//! Narrow Rapier integration for the one dynamic mower body and generated terrain.
//!
//! Gameplay chooses desired tangent motion; Rapier owns integration, continuous
//! collision detection, contact response, hover-pad queries, and contact impulses.

use std::fmt;

use glam::{Quat, Vec2, Vec3};
use rapier3d::{
    math::{Pose, Rotation, Vector},
    prelude::{
        ColliderBuilder, ColliderHandle, PhysicsWorld as RapierWorld, QueryFilter, Ray,
        RigidBodyBuilder, RigidBodyHandle,
    },
};

use crate::{
    FIXED_DT,
    config::VehicleTuning,
    cube_map::{CubeFace, face_uv_to_direction},
    planet::{Planet, SpawnPoint},
    vehicle::VehicleTransform,
};

const COLLISION_FACE_RESOLUTION: u32 = 32;
const BODY_HALF_WIDTH: f32 = 0.95;
const BODY_HALF_HEIGHT: f32 = 0.28;
const BODY_HALF_LENGTH: f32 = 0.95;

/// Desired behavior supplied by the arcade controller for one fixed tick.
#[derive(Clone, Copy, Debug)]
pub struct VehiclePhysicsInput {
    pub desired_velocity: Vec3,
    pub desired_forward: Vec3,
    pub desired_up: Vec3,
    /// Tangent velocity servo strength in inverse seconds.
    pub drive_response: f32,
}

/// Authoritative body state produced by Rapier after one fixed tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct VehiclePhysicsOutput {
    pub position: Vec3,
    pub rotation: Quat,
    pub linear_velocity: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
    pub grounded: bool,
    /// Contact impulse normalized by vehicle mass, approximately a delta-speed.
    pub collision_impulse: Option<f32>,
    /// World-space terrain point associated with the substantial contact.
    pub collision_position: Option<Vec3>,
}

/// Owns all collision state. Nothing outside this type depends on Rapier handles.
pub struct PhysicsWorld {
    world: RapierWorld,
    vehicle_body: RigidBodyHandle,
    vehicle_collider: ColliderHandle,
    terrain_collider: ColliderHandle,
    pad_target_distance: f32,
    surface_glue_acceleration: f32,
    surface_glue_damping: f32,
    grounded: bool,
}

impl fmt::Debug for PhysicsWorld {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PhysicsWorld")
            .field("rigid_bodies", &self.world.bodies.len())
            .field("colliders", &self.world.colliders.len())
            .field("vehicle_body", &self.vehicle_body)
            .field("vehicle_collider", &self.vehicle_collider)
            .field("terrain_collider", &self.terrain_collider)
            .field("pad_target_distance", &self.pad_target_distance)
            .field("surface_glue_acceleration", &self.surface_glue_acceleration)
            .field("surface_glue_damping", &self.surface_glue_damping)
            .field("grounded", &self.grounded)
            .finish()
    }
}

impl PhysicsWorld {
    /// Builds the low-frequency static planet and dynamic hover-mower body.
    ///
    /// # Panics
    ///
    /// Panics only if the generator-produced cube-sphere cannot be represented
    /// as a valid Rapier triangle mesh, which indicates an internal invariant
    /// violation.
    #[must_use]
    pub fn new(planet: &Planet, spawn: SpawnPoint, tuning: &VehicleTuning) -> Self {
        let mut world = RapierWorld::new();
        world.gravity = Vector::ZERO;
        world.integration_parameters.dt = FIXED_DT;
        world.integration_parameters.max_ccd_substeps = 2;

        let (vertices, indices) = build_collision_mesh(planet);
        let terrain_builder = ColliderBuilder::trimesh(vertices, indices)
            .expect("generated collision mesh must be a valid manifold")
            .friction(0.45)
            .restitution(0.02);
        let terrain_collider = world.insert_collider(terrain_builder, None);

        let transform = spawn_transform(spawn);
        let body = RigidBodyBuilder::dynamic()
            .pose(glam_pose(transform.position, transform.rotation))
            .linear_damping(0.05)
            .angular_damping(1.8)
            .ccd_enabled(true)
            .can_sleep(false);
        let collider = ColliderBuilder::round_cuboid(
            BODY_HALF_WIDTH,
            BODY_HALF_HEIGHT,
            BODY_HALF_LENGTH,
            0.12,
        )
        .density(38.0)
        .friction(0.28)
        .restitution(0.04)
        .contact_force_event_threshold(0.0);
        let (vehicle_body, vehicle_collider) = world.insert(body, collider);

        // Populate broad/narrow-phase structures before the first hover query.
        world.step();
        let mut result = Self {
            world,
            vehicle_body,
            vehicle_collider,
            terrain_collider,
            pad_target_distance: (tuning.hover_height - 0.18).max(0.25),
            surface_glue_acceleration: tuning.surface_glue_acceleration,
            surface_glue_damping: tuning.surface_glue_damping,
            grounded: true,
        };
        result.teleport(transform, Vec3::ZERO);
        result.world.integration_parameters.dt = FIXED_DT;
        // Keep the API tuning-dependent from the outset; this also documents
        // that the body starts at the configured hover height.
        debug_assert!(tuning.hover_height > BODY_HALF_HEIGHT);
        result
    }

    /// Advance the dynamic body using four hover-pad springs and PD orientation.
    pub fn step(&mut self, input: VehiclePhysicsInput, dt: f32) -> VehiclePhysicsOutput {
        if !dt.is_finite() || dt <= 0.0 {
            return self.output(self.grounded);
        }
        self.world.integration_parameters.dt = dt;
        let body = &self.world.bodies[self.vehicle_body];
        let position = vector_to_glam(body.translation());
        let rotation = rapier_rotation_to_glam(body.rotation());
        let current_up = rotation.mul_vec3(Vec3::Y).normalize_or_zero();
        let current_forward = rotation.mul_vec3(Vec3::NEG_Z).normalize_or_zero();
        let up = input.desired_up.normalize_or(current_up);
        let desired_forward = (input.desired_forward - up * input.desired_forward.dot(up))
            .normalize_or(current_forward);
        let pad_samples = self.hover_pad_samples(position, rotation, up);

        {
            let body = &mut self.world.bodies[self.vehicle_body];
            let mass = body.mass();
            body.reset_forces(true);
            body.reset_torques(true);
            let velocity = vector_to_glam(body.linvel());
            let tangent_velocity = velocity - up * velocity.dot(up);
            let desired_tangent = input.desired_velocity - up * input.desired_velocity.dot(up);
            // Integrate the servo exponentially so aggressive tuning cannot
            // overshoot or reverse the velocity within one fixed step.
            let response = -(-input.drive_response.max(0.0) * dt).exp_m1() / dt;
            let drive_force = (desired_tangent - tangent_velocity) * (mass * response);
            body.add_force(glam_vector(drive_force), true);

            // A light radial preload holds normal hover height. When every pad
            // loses the surface, strong damped magnetic attraction takes over
            // so a high-speed crest cannot launch the mower into space.
            let contacted_pads = pad_samples
                .iter()
                .filter(|sample| sample.hit.is_some())
                .count();
            let outward_speed = velocity.dot(up).max(0.0);
            let airborne_glue = if contacted_pads == 0 {
                self.surface_glue_acceleration
            } else {
                0.0
            };
            let radial_acceleration =
                2.2 + airborne_glue + outward_speed * self.surface_glue_damping;
            body.add_force(glam_vector(-up * mass * radial_acceleration), true);
            for sample in &pad_samples {
                if let Some(hit) = sample.hit {
                    let point_velocity =
                        vector_to_glam(body.velocity_at_point(glam_vector(sample.anchor)));
                    let normal_speed = point_velocity.dot(hit.normal);
                    // Signed suspension is the near-surface magnetic constraint:
                    // compressed pads push out and stretched pads pull inward.
                    let compression = sample.target_distance - hit.distance;
                    let spring = mass * 48.0 * compression;
                    let damping = mass * 3.8 * normal_speed;
                    let preload = mass * 2.2 / 4.0;
                    let force = (spring - damping + preload)
                        .clamp(-mass * self.surface_glue_acceleration / 4.0, mass * 22.0);
                    body.add_force_at_point(
                        glam_vector(hit.normal * force),
                        glam_vector(sample.anchor),
                        true,
                    );
                }
            }

            let orientation_error =
                current_up.cross(up) * 18.0 + current_forward.cross(desired_forward) * 4.5;
            let angular_velocity = vector_to_glam(body.angvel());
            body.add_torque(
                glam_vector((orientation_error - angular_velocity) * mass * 3.0),
                true,
            );
        }

        self.world.step();
        self.grounded = pad_samples
            .iter()
            .filter(|sample| {
                sample
                    .hit
                    .is_some_and(|hit| hit.distance <= sample.target_distance + 0.45)
            })
            .count()
            >= 2;
        self.output(self.grounded)
    }

    /// Place the body at a known-safe pose for manual/automatic recovery.
    pub fn teleport(&mut self, transform: VehicleTransform, velocity: Vec3) {
        let body = &mut self.world.bodies[self.vehicle_body];
        body.set_position(glam_pose(transform.position, transform.rotation), true);
        body.set_linvel(glam_vector(velocity), true);
        body.set_angvel(Vector::ZERO, true);
        body.reset_forces(true);
        body.reset_torques(true);
    }

    #[must_use]
    pub fn body_count(&self) -> usize {
        self.world.bodies.len()
    }

    #[must_use]
    pub fn collider_count(&self) -> usize {
        self.world.colliders.len()
    }

    fn hover_pad_samples(&self, position: Vec3, rotation: Quat, up: Vec3) -> [PadSample; 4] {
        let mut samples = [PadSample::default(); 4];
        let anchors = [
            Vec3::new(-0.68, -0.18, -0.68),
            Vec3::new(0.68, -0.18, -0.68),
            Vec3::new(-0.68, -0.18, 0.68),
            Vec3::new(0.68, -0.18, 0.68),
        ];
        for (sample, local) in samples.iter_mut().zip(anchors) {
            let anchor = position + rotation.mul_vec3(local);
            let ray = Ray::new(glam_vector(anchor), glam_vector(-up));
            let filter = QueryFilter::default().exclude_collider(self.vehicle_collider);
            let hit = self
                .world
                .cast_ray_and_get_normal(&ray, 2.8, true, filter)
                .and_then(|(handle, intersection)| {
                    (handle == self.terrain_collider).then(|| PadHit {
                        distance: intersection.time_of_impact,
                        normal: vector_to_glam(intersection.normal).normalize_or(up),
                    })
                });
            *sample = PadSample {
                anchor,
                target_distance: self.pad_target_distance,
                hit,
            };
        }
        samples
    }

    fn output(&self, grounded: bool) -> VehiclePhysicsOutput {
        let body = &self.world.bodies[self.vehicle_body];
        let position = vector_to_glam(body.translation());
        let rotation = rapier_rotation_to_glam(body.rotation());
        let up = rotation.mul_vec3(Vec3::Y).normalize_or(Vec3::Y);
        let forward = rotation.mul_vec3(Vec3::NEG_Z).normalize_or(Vec3::NEG_Z);
        let mass = body.mass().max(f32::EPSILON);
        let pair = self
            .world
            .narrow_phase
            .contact_pair(self.vehicle_collider, self.terrain_collider);
        let collision_impulse = pair
            .map(|pair| pair.total_impulse_magnitude() / mass)
            .filter(|impulse| *impulse >= 3.0);
        let collision_position = collision_impulse.and_then(|_| {
            let pair = pair?;
            let (_, contact) = pair.find_deepest_contact()?;
            let terrain_is_first = pair.collider1 == self.terrain_collider;
            let (collider, local_point) = if terrain_is_first {
                (&self.world.colliders[pair.collider1], contact.local_p1)
            } else {
                (&self.world.colliders[pair.collider2], contact.local_p2)
            };
            Some(vector_to_glam(collider.position() * local_point))
        });
        VehiclePhysicsOutput {
            position,
            rotation,
            linear_velocity: vector_to_glam(body.linvel()),
            forward,
            up,
            grounded,
            collision_impulse,
            collision_position,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PadSample {
    anchor: Vec3,
    target_distance: f32,
    hit: Option<PadHit>,
}

#[derive(Clone, Copy, Debug)]
struct PadHit {
    distance: f32,
    normal: Vec3,
}

fn build_collision_mesh(planet: &Planet) -> (Vec<Vector>, Vec<[u32; 3]>) {
    let resolution = planet.terrain.resolution().min(COLLISION_FACE_RESOLUTION);
    let vertices_per_face = (resolution + 1) * (resolution + 1);
    let mut vertices = Vec::with_capacity((vertices_per_face * 6) as usize);
    let mut indices = Vec::with_capacity((resolution * resolution * 12) as usize);
    for face in CubeFace::ALL {
        let face_start = vertices.len() as u32;
        for y in 0..=resolution {
            for x in 0..=resolution {
                let uv = Vec2::new(
                    x as f32 * 2.0 / resolution as f32 - 1.0,
                    y as f32 * 2.0 / resolution as f32 - 1.0,
                );
                vertices.push(glam_vector(
                    planet.surface_point(face_uv_to_direction(face, uv)),
                ));
            }
        }
        let stride = resolution + 1;
        for y in 0..resolution {
            for x in 0..resolution {
                let i0 = face_start + y * stride + x;
                let i1 = i0 + 1;
                let i2 = i0 + stride;
                let i3 = i2 + 1;
                indices.push([i0, i2, i1]);
                indices.push([i1, i2, i3]);
            }
        }
    }
    (vertices, indices)
}

fn spawn_transform(spawn: SpawnPoint) -> VehicleTransform {
    let up = spawn.up.normalize();
    let forward = (spawn.forward - up * spawn.forward.dot(up)).normalize();
    let right = forward.cross(up).normalize();
    let rotation = Quat::from_mat3(&glam::Mat3::from_cols(right, up, -forward));
    VehicleTransform {
        position: spawn.position,
        rotation,
        forward,
        up,
    }
}

fn glam_vector(value: Vec3) -> Vector {
    Vector::new(value.x, value.y, value.z)
}

fn vector_to_glam(value: Vector) -> Vec3 {
    Vec3::new(value.x, value.y, value.z)
}

fn glam_pose(position: Vec3, rotation: Quat) -> Pose {
    Pose::from_parts(
        glam_vector(position),
        Rotation::from_xyzw(rotation.x, rotation.y, rotation.z, rotation.w).normalize(),
    )
}

fn rapier_rotation_to_glam(rotation: &Rotation) -> Quat {
    Quat::from_xyzw(rotation.x, rotation.y, rotation.z, rotation.w).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION};

    fn planet() -> Planet {
        PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
            .generate_with_roots(WorldSeed(707), false)
            .unwrap()
    }

    #[test]
    fn world_contains_one_dynamic_body_and_static_terrain() {
        let planet = planet();
        let physics = PhysicsWorld::new(&planet, planet.spawn, &VehicleTuning::default());
        assert_eq!(physics.body_count(), 1);
        assert_eq!(physics.collider_count(), 2);
    }

    #[test]
    fn four_pad_hover_remains_near_the_surface() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut physics = PhysicsWorld::new(&planet, planet.spawn, &tuning);
        let up = planet.spawn.up;
        for _ in 0..480 {
            let output = physics.step(
                VehiclePhysicsInput {
                    desired_velocity: Vec3::ZERO,
                    desired_forward: planet.spawn.forward,
                    desired_up: up,
                    drive_response: 15.0,
                },
                FIXED_DT,
            );
            assert!(output.position.is_finite());
        }
        let output = physics.output(true);
        let surface = planet.surface_radius(output.position.normalize());
        let altitude = output.position.length() - surface;
        assert!(
            (0.35..1.4).contains(&altitude),
            "hover altitude was {altitude}"
        );
    }

    #[test]
    fn teleport_resets_velocity_for_recovery() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut physics = PhysicsWorld::new(&planet, planet.spawn, &tuning);
        let transform = spawn_transform(planet.spawn);
        physics.teleport(transform, Vec3::ZERO);
        let output = physics.output(true);
        assert!(output.linear_velocity.length() < 1.0e-5);
        assert!(output.position.distance(transform.position) < 1.0e-4);
    }

    #[test]
    fn distant_hover_ray_hits_do_not_allow_airborne_mowing() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut physics = PhysicsWorld::new(&planet, planet.spawn, &tuning);
        let mut transform = spawn_transform(planet.spawn);
        transform.position += transform.up * 1.0;
        physics.teleport(transform, Vec3::ZERO);
        let output = physics.step(
            VehiclePhysicsInput {
                desired_velocity: Vec3::ZERO,
                desired_forward: transform.forward,
                desired_up: transform.up,
                drive_response: 15.0,
            },
            FIXED_DT,
        );
        assert!(!output.grounded);
    }

    #[test]
    fn drive_servo_ignores_radial_requests_and_remains_stable_at_high_response() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut physics = PhysicsWorld::new(&planet, planet.spawn, &tuning);
        let output = physics.step(
            VehiclePhysicsInput {
                desired_velocity: planet.spawn.forward * 10.0 + planet.spawn.up * 100.0,
                desired_forward: planet.spawn.forward,
                desired_up: planet.spawn.up,
                drive_response: 10_000.0,
            },
            FIXED_DT,
        );
        assert!(output.linear_velocity.dot(planet.spawn.forward) > 8.0);
        assert!(output.linear_velocity.dot(planet.spawn.forward) < 10.1);
        assert!(output.linear_velocity.dot(planet.spawn.up).abs() < 1.0);
    }

    #[test]
    fn invalid_time_steps_leave_physics_unchanged() {
        let planet = planet();
        let mut physics = PhysicsWorld::new(&planet, planet.spawn, &VehicleTuning::default());
        let input = VehiclePhysicsInput {
            desired_velocity: planet.spawn.forward * 10.0,
            desired_forward: planet.spawn.forward,
            desired_up: planet.spawn.up,
            drive_response: 15.0,
        };
        let before = physics.step(input, FIXED_DT);
        for dt in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let output = physics.step(input, dt);
            assert_eq!(output.position, before.position);
            assert_eq!(output.rotation, before.rotation);
            assert_eq!(output.linear_velocity, before.linear_velocity);
            assert_eq!(output.grounded, before.grounded);
        }
        assert!(physics.step(input, FIXED_DT).position.is_finite());
    }

    #[test]
    fn magnetic_glue_catches_a_high_speed_outward_launch() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut physics = PhysicsWorld::new(&planet, planet.spawn, &tuning);
        let transform = spawn_transform(planet.spawn);
        physics.teleport(transform, planet.spawn.up * 25.0);

        let mut maximum_altitude = 0.0_f32;
        let mut output = physics.output(true);
        for _ in 0..240 {
            output = physics.step(
                VehiclePhysicsInput {
                    desired_velocity: Vec3::ZERO,
                    desired_forward: planet.spawn.forward,
                    desired_up: output.position.normalize_or(planet.spawn.up),
                    drive_response: 15.0,
                },
                FIXED_DT,
            );
            let direction = output.position.normalize();
            let altitude = output.position.length() - planet.surface_radius(direction);
            maximum_altitude = maximum_altitude.max(altitude);
        }
        let final_direction = output.position.normalize();
        let final_altitude = output.position.length() - planet.surface_radius(final_direction);
        assert!(
            maximum_altitude < 3.0,
            "25 m/s outward launch reached {maximum_altitude} meters"
        );
        assert!(
            final_altitude < 1.5,
            "vehicle did not settle back onto the surface: {final_altitude} meters"
        );
    }
}
