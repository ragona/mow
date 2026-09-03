//! Spherical arcade hover-vehicle controller.

use glam::{Mat3, Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::{
    config::VehicleTuning,
    cube_map::tangent_frame,
    input::InputSnapshot,
    physics::{PhysicsWorld, VehiclePhysicsInput},
    planet::{Planet, SpawnPoint, SurfaceMaterial},
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VehicleTransform {
    pub position: Vec3,
    pub rotation: Quat,
    pub forward: Vec3,
    pub up: Vec3,
}

impl VehicleTransform {
    #[must_use]
    pub fn interpolated(self, next: Self, alpha: f32) -> Self {
        let alpha = alpha.clamp(0.0, 1.0);
        let up = self.up.lerp(next.up, alpha).normalize();
        let forward = self.forward.lerp(next.forward, alpha);
        let forward = (forward - up * forward.dot(up))
            .try_normalize()
            .unwrap_or_else(|| tangent_frame(up).0);
        Self {
            position: self.position.lerp(next.position, alpha),
            rotation: self.rotation.slerp(next.rotation, alpha),
            forward,
            up,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VehicleState {
    pub transform: VehicleTransform,
    pub linear_velocity: Vec3,
    pub mower_enabled: bool,
    pub boost_charge: f32,
    pub boost_active: bool,
    pub grounded: bool,
    pub recovery_hold: f32,
    pub recoveries: u32,
    pub stuck_seconds: f32,
    pub last_boost_seconds: f32,
    pub collision_cooldown: f32,
}

impl VehicleState {
    #[must_use]
    pub fn at_spawn(spawn: SpawnPoint, tuning: &VehicleTuning) -> Self {
        let transform = make_transform(spawn.position, spawn.forward, spawn.up);
        Self {
            transform,
            linear_velocity: Vec3::ZERO,
            mower_enabled: true,
            boost_charge: tuning.boost_capacity_seconds,
            boost_active: false,
            grounded: true,
            recovery_hold: 0.0,
            recoveries: 0,
            stuck_seconds: 0.0,
            last_boost_seconds: f32::INFINITY,
            collision_cooldown: 0.0,
        }
    }

    #[must_use]
    pub fn speed(&self) -> f32 {
        self.linear_velocity.length()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VehicleTickResult {
    pub traveled_distance: f32,
    pub collision_impulse: Option<f32>,
    pub recovered: bool,
    pub deck_from: Vec3,
    pub deck_to: Vec3,
    pub reverse_cut_multiplier: f32,
}

#[derive(Debug)]
pub struct HoverVehicle {
    pub state: VehicleState,
    previous_transform: VehicleTransform,
    /// Integrated steering target. Keeping this independent of the physical
    /// body's lag makes steering input accumulate immediately instead of
    /// presenting Rapier with a target only one simulation tick ahead.
    steering_forward: Vec3,
    physics: PhysicsWorld,
}

impl HoverVehicle {
    #[must_use]
    pub fn new(planet: &Planet, tuning: &VehicleTuning) -> Self {
        let state = VehicleState::at_spawn(planet.spawn, tuning);
        let physics = PhysicsWorld::new(planet, planet.spawn, tuning);
        Self {
            previous_transform: state.transform,
            steering_forward: state.transform.forward,
            state,
            physics,
        }
    }

    #[must_use]
    pub const fn previous_transform(&self) -> VehicleTransform {
        self.previous_transform
    }

    #[must_use]
    pub fn interpolated_transform(&self, alpha: f32) -> VehicleTransform {
        self.previous_transform
            .interpolated(self.state.transform, alpha)
    }

    pub fn tick(
        &mut self,
        planet: &Planet,
        tuning: &VehicleTuning,
        input: InputSnapshot,
        boost_allowed: bool,
        dt: f32,
    ) -> VehicleTickResult {
        self.previous_transform = self.state.transform;
        let old_deck = self.deck_position(tuning);
        // The deck is an always-on part of driving now. Preserve the state bit
        // for renderer/audio snapshots while making it an invariant each tick.
        self.state.mower_enabled = true;
        self.state.collision_cooldown = (self.state.collision_cooldown - dt).max(0.0);

        if input.recover_held {
            self.state.recovery_hold += dt;
        } else {
            self.state.recovery_hold = 0.0;
        }
        let radial_distance = self.state.transform.position.length();
        let invalid_distance = !(planet.config.base_radius * 0.7
            ..=planet.config.base_radius + 14.0)
            .contains(&radial_distance);
        let overturned = self
            .state
            .transform
            .up
            .dot(self.state.transform.position.normalize_or(Vec3::Y))
            < -0.2;
        let manual_recovery = self.state.recovery_hold >= tuning.recovery_hold_time;
        let automatic_recovery = invalid_distance
            || overturned
            || self.state.stuck_seconds >= tuning.automatic_recovery_delay;
        if manual_recovery || automatic_recovery {
            self.recover(planet, tuning);
            return VehicleTickResult {
                recovered: true,
                deck_from: old_deck,
                deck_to: self.deck_position(tuning),
                ..VehicleTickResult::default()
            };
        }

        let position = self.state.transform.position;
        let radial_up = position.normalize_or_zero();
        let terrain = planet.terrain_cell(radial_up);
        let up = self
            .state
            .transform
            .up
            .lerp(terrain.normal, 1.0 - (-9.0 * dt).exp())
            .normalize();
        let body_forward = (self.state.transform.forward
            - up * self.state.transform.forward.dot(up))
        .try_normalize()
        .unwrap_or_else(|| tangent_frame(up).0);
        self.steering_forward = (self.steering_forward - up * self.steering_forward.dot(up))
            .try_normalize()
            .unwrap_or(body_forward);
        let right = self.steering_forward.cross(up).normalize();

        let tangent_velocity = self.state.linear_velocity - up * self.state.linear_velocity.dot(up);
        let forward_speed = tangent_velocity.dot(self.steering_forward);
        let side_speed = tangent_velocity.dot(right);
        let normalized_speed = (forward_speed.abs() / tuning.max_forward_speed).clamp(0.0, 1.0);
        let turn_radius = tuning.low_speed_turn_radius
            + (tuning.full_speed_turn_radius - tuning.low_speed_turn_radius) * normalized_speed;
        let reference_speed = forward_speed.abs().max(3.0);
        let yaw_rate = input.steer * reference_speed / turn_radius * tuning.steering_yaw_fraction;
        let travel_sign = if forward_speed < -0.2 { -1.0 } else { 1.0 };
        self.steering_forward = Quat::from_axis_angle(up, -yaw_rate * travel_sign * dt)
            .mul_vec3(self.steering_forward)
            .normalize();
        let right = self.steering_forward.cross(up).normalize();

        let movement_requested =
            input.accelerate > 0.05 || input.brake_reverse > 0.05 || input.steer.abs() > 0.05;
        let boost_requested = boost_allowed && input.boost_held && movement_requested;
        self.state.boost_active = boost_requested && self.state.boost_charge > 0.0;
        if self.state.boost_active {
            self.state.boost_charge = (self.state.boost_charge - dt).max(0.0);
            self.state.last_boost_seconds = 0.0;
        } else {
            self.state.last_boost_seconds += dt;
            if self.state.last_boost_seconds >= tuning.boost_recharge_delay {
                self.state.boost_charge = (self.state.boost_charge
                    + dt * tuning.boost_capacity_seconds / tuning.boost_recharge_seconds)
                    .min(tuning.boost_capacity_seconds);
            }
        }

        let desired_speed = if input.accelerate >= input.brake_reverse {
            let top_speed = if self.state.boost_active {
                tuning.boost_max_speed
            } else {
                tuning.max_forward_speed
            };
            input.accelerate * top_speed
        } else {
            -input.brake_reverse * tuning.max_reverse_speed
        };
        let acceleration_rate = std::f32::consts::LN_10 / tuning.acceleration_time_90_percent
            * if self.state.boost_active {
                tuning.boost_acceleration_multiplier
            } else {
                1.0
            };
        let new_forward_speed = forward_speed
            + (desired_speed - forward_speed) * (1.0 - (-acceleration_rate * dt).exp());
        // Steering is primarily lateral hover thrust. A small yaw fraction keeps
        // the deck readable while the chassis can translate dramatically across
        // its own facing direction.
        let strafe_boost = if self.state.boost_active { 1.8 } else { 1.0 };
        let desired_side_speed = input.steer * tuning.strafe_speed * strafe_boost;
        let lateral_response = 1.0 - (-tuning.strafe_response * dt).exp();
        let new_side_speed = side_speed + (desired_side_speed - side_speed) * lateral_response;
        let desired_velocity = self.steering_forward * new_forward_speed + right * new_side_speed;
        let physics = self.physics.step(
            VehiclePhysicsInput {
                desired_velocity,
                desired_forward: self.steering_forward,
                desired_up: terrain.normal,
                drive_response: 15.0
                    * if self.state.boost_active {
                        tuning.boost_acceleration_multiplier
                    } else {
                        1.0
                    },
            },
            dt,
        );
        let new_position = physics.position;

        let traveled_distance = position
            .normalize()
            .dot(new_position.normalize())
            .clamp(-1.0, 1.0)
            .acos()
            * terrain.radius;
        self.state.linear_velocity = physics.linear_velocity;
        let transform_up = physics.up.normalize_or(terrain.normal);
        let transform_forward = (physics.forward
            - transform_up * physics.forward.dot(transform_up))
        .normalize_or(self.steering_forward);
        self.state.transform = VehicleTransform {
            position: new_position,
            rotation: physics.rotation,
            forward: transform_forward,
            up: transform_up,
        };
        self.state.grounded = physics.grounded;
        let contact_direction = physics
            .collision_position
            .unwrap_or(new_position)
            .normalize_or(up);
        let on_rock = planet.terrain_cell(contact_direction).material == SurfaceMaterial::Rock;
        let collision_impulse = physics.collision_impulse.filter(|_| {
            if on_rock && self.state.collision_cooldown <= 0.0 {
                self.state.collision_cooldown = 0.7;
                true
            } else {
                false
            }
        });
        self.state.stuck_seconds =
            if (input.accelerate > 0.5 || input.brake_reverse > 0.5) && self.state.speed() < 0.4 {
                self.state.stuck_seconds + dt
            } else {
                0.0
            };
        let reverse_cut_multiplier = if new_forward_speed < -0.1 { 0.7 } else { 1.0 };
        VehicleTickResult {
            traveled_distance,
            collision_impulse,
            recovered: false,
            deck_from: old_deck,
            deck_to: self.deck_position(tuning),
            reverse_cut_multiplier,
        }
    }

    pub fn recover(&mut self, planet: &Planet, tuning: &VehicleTuning) {
        let safe = planet.nearest_safe_point(self.state.transform.position.normalize_or(Vec3::Y));
        self.state.transform = make_transform(safe.position, self.state.transform.forward, safe.up);
        self.previous_transform = self.state.transform;
        self.steering_forward = self.state.transform.forward;
        self.state.linear_velocity = Vec3::ZERO;
        self.physics.teleport(self.state.transform, Vec3::ZERO);
        self.state.recovery_hold = 0.0;
        self.state.stuck_seconds = 0.0;
        self.state.recoveries += 1;
        self.state.grounded = true;
        self.state.boost_active = false;
        self.state.boost_charge = self.state.boost_charge.min(tuning.boost_capacity_seconds);
    }

    #[must_use]
    pub fn deck_position(&self, tuning: &VehicleTuning) -> Vec3 {
        self.state.transform.position + self.state.transform.forward * (tuning.car_length * 0.36)
            - self.state.transform.up * (tuning.hover_height * 0.72)
    }
}

fn make_transform(position: Vec3, forward: Vec3, up: Vec3) -> VehicleTransform {
    let up = up.normalize();
    let forward = (forward - up * forward.dot(up))
        .try_normalize()
        .unwrap_or_else(|| tangent_frame(up).0);
    let right = forward.cross(up).normalize();
    // Local +X is right, +Y is up, and local -Z is forward.
    let rotation = Quat::from_mat3(&Mat3::from_cols(right, up, -forward));
    VehicleTransform {
        position,
        rotation,
        forward,
        up,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION};

    fn planet() -> Planet {
        PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
            .generate_with_roots(WorldSeed(123), false)
            .unwrap()
    }

    #[test]
    fn vehicle_stays_aligned_during_circumnavigation() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        for step in 0..120 * 25 {
            vehicle.tick(
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    steer: 0.15,
                    ..InputSnapshot::default()
                },
                true,
                1.0 / 120.0,
            );
            let direction = vehicle.state.transform.position.normalize();
            let alignment = vehicle
                .state
                .transform
                .up
                .dot(planet.terrain_cell(direction).normal);
            assert!(
                alignment > 0.75,
                "orientation lost at step {step}: alignment {alignment}, speed {}",
                vehicle.state.speed()
            );
            assert!(
                vehicle
                    .state
                    .transform
                    .forward
                    .dot(vehicle.state.transform.up)
                    .abs()
                    < 1.0e-4
            );
        }
    }

    #[test]
    fn manual_recovery_preserves_progress_independent_state() {
        let planet = planet();
        let tuning = VehicleTuning {
            recovery_hold_time: 0.02,
            ..VehicleTuning::default()
        };
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        vehicle.tick(
            &planet,
            &tuning,
            InputSnapshot {
                recover_held: true,
                ..InputSnapshot::default()
            },
            true,
            0.03,
        );
        assert_eq!(vehicle.state.recoveries, 1);
        assert!(vehicle.state.grounded);
    }

    #[test]
    fn steering_is_dominantly_lateral() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        let initial = vehicle.state.transform;
        let initial_right = initial.forward.cross(initial.up).normalize();
        for _ in 0..120 {
            vehicle.tick(
                &planet,
                &tuning,
                InputSnapshot {
                    steer: 1.0,
                    ..InputSnapshot::default()
                },
                true,
                1.0 / 120.0,
            );
        }
        let lateral_distance =
            (vehicle.state.transform.position - initial.position).dot(initial_right);
        let heading_change = initial
            .forward
            .dot(vehicle.state.transform.forward)
            .clamp(-1.0, 1.0)
            .acos();
        assert!(
            lateral_distance > 3.0,
            "one second of full strafe moved only {lateral_distance} meters sideways"
        );
        assert!(
            heading_change < 0.65,
            "strafe rotated the chassis by {heading_change} radians"
        );
        let lateral_velocity = vehicle.state.linear_velocity.dot(initial_right).abs();
        let forward_velocity = vehicle
            .state
            .linear_velocity
            .dot(vehicle.state.transform.forward)
            .abs();
        assert!(
            lateral_velocity > forward_velocity * 2.0,
            "strafe velocity {lateral_velocity} was not dominant over forward velocity {forward_velocity}"
        );
    }

    #[test]
    fn boost_produces_a_major_speed_step() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut normal = HoverVehicle::new(&planet, &tuning);
        let mut boosted = HoverVehicle::new(&planet, &tuning);
        let mut normal_peak = 0.0_f32;
        let mut boosted_peak = 0.0_f32;
        for _ in 0..120 {
            normal.tick(
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                true,
                1.0 / 120.0,
            );
            boosted.tick(
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    boost_held: true,
                    ..InputSnapshot::default()
                },
                true,
                1.0 / 120.0,
            );
            normal_peak = normal_peak.max(normal.state.speed());
            boosted_peak = boosted_peak.max(boosted.state.speed());
        }
        assert!(
            boosted_peak > normal_peak * 1.8,
            "peak boost speed {boosted_peak} was not dramatically above normal speed {normal_peak}"
        );
        assert!(
            boosted_peak > 18.0,
            "boost peaked at only {boosted_peak} m/s (final {}, normal peak {normal_peak})",
            boosted.state.speed()
        );
    }

    #[test]
    fn mower_is_restored_to_always_on_each_tick() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        vehicle.state.mower_enabled = false;
        vehicle.tick(
            &planet,
            &tuning,
            InputSnapshot::default(),
            true,
            1.0 / 120.0,
        );
        assert!(vehicle.state.mower_enabled);
    }
}
