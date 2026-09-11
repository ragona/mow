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
        let rotation = self.rotation.slerp(next.rotation, alpha).normalize();
        Self {
            position: self.position.lerp(next.position, alpha),
            rotation,
            forward: rotation * Vec3::NEG_Z,
            up: rotation * Vec3::Y,
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
}

#[derive(Debug)]
pub struct HoverVehicle {
    pub state: VehicleState,
    previous_transform: VehicleTransform,
    previous_linear_velocity: Vec3,
    physics: PhysicsWorld,
    bump_control_seconds: f32,
}

impl HoverVehicle {
    #[must_use]
    pub fn new(planet: &Planet, tuning: &VehicleTuning) -> Self {
        Self::from_spawn(planet, planet.spawn, tuning)
    }

    #[must_use]
    pub fn from_spawn(planet: &Planet, spawn: SpawnPoint, tuning: &VehicleTuning) -> Self {
        let state = VehicleState::at_spawn(spawn, tuning);
        let physics = PhysicsWorld::new(planet, spawn, tuning);
        Self {
            previous_transform: state.transform,
            previous_linear_velocity: state.linear_velocity,
            state,
            physics,
            bump_control_seconds: 0.0,
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

    #[must_use]
    pub fn interpolated_velocity(&self, alpha: f32) -> Vec3 {
        self.previous_linear_velocity
            .lerp(self.state.linear_velocity, alpha.clamp(0.0, 1.0))
    }

    pub fn tick(
        &mut self,
        planet: &Planet,
        tuning: &VehicleTuning,
        input: InputSnapshot,
        movement_forward: Vec3,
        boost_allowed: bool,
        dt: f32,
    ) -> VehicleTickResult {
        let input = input.sanitized();
        self.bump_control_seconds = (self.bump_control_seconds - dt).max(0.0);
        self.previous_transform = self.state.transform;
        self.previous_linear_velocity = self.state.linear_velocity;
        let old_deck = self.deck_position(tuning);
        // The deck is an always-on part of driving now. Preserve the state bit
        // for renderer snapshots while making it an invariant each tick.
        self.state.mower_enabled = true;
        self.state.collision_cooldown = (self.state.collision_cooldown - dt).max(0.0);

        if input.recover_held {
            self.state.recovery_hold += dt;
        } else {
            self.state.recovery_hold = 0.0;
        }
        let position = self.state.transform.position;
        let radial_up = position.normalize_or(Vec3::Y);
        let terrain = planet.terrain_cell(radial_up);
        // Keep the original escape envelope, extending it around local terrain
        // so experimental peaks and valleys are not mistaken for a lost mower.
        let minimum_radius = planet.config.base_radius.min(terrain.radius) * 0.7;
        let maximum_radius = planet.config.base_radius.max(terrain.radius) + 14.0;
        let invalid_distance = !(minimum_radius..=maximum_radius).contains(&position.length());
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
        let screen_forward = (movement_forward - up * movement_forward.dot(up))
            .try_normalize()
            .unwrap_or(body_forward);
        let screen_right = screen_forward.cross(up).normalize();

        let tangent_velocity = self.state.linear_velocity - up * self.state.linear_velocity.dot(up);
        let movement_input = glam::Vec2::new(input.steer, input.accelerate - input.brake_reverse)
            .clamp_length_max(1.0);
        let movement_requested = movement_input.length_squared() > 0.05 * 0.05;
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

        let top_speed = if self.state.boost_active {
            tuning.boost_max_speed
        } else {
            tuning.max_speed
        };
        let desired_velocity =
            (screen_right * movement_input.x + screen_forward * movement_input.y) * top_speed;
        let alignment = tangent_velocity
            .try_normalize()
            .zip(desired_velocity.try_normalize())
            .map_or(1.0, |(current, desired)| current.dot(desired));
        let slowing_down = alignment >= 0.8
            && desired_velocity.length_squared() + 0.01 < tangent_velocity.length_squared();
        let response_time = if !movement_requested || slowing_down {
            tuning.braking_time_90_percent
        } else if alignment < 0.8 {
            tuning.direction_change_time_90_percent
        } else {
            tuning.acceleration_time_90_percent
        };
        let contact_control = 1.0 - 0.78 * (self.bump_control_seconds / 0.28).clamp(0.0, 1.0);
        let drive_response = contact_control * std::f32::consts::LN_10 / response_time.max(0.01)
            * if self.state.boost_active {
                tuning.boost_acceleration_multiplier
            } else {
                1.0
            };
        let physics = self.physics.step(
            VehiclePhysicsInput {
                desired_velocity,
                // Translation is omnidirectional. The chassis only maintains
                // its heading while conforming to the changing surface normal.
                desired_forward: body_forward,
                desired_up: terrain.normal,
                drive_response,
            },
            dt,
        );
        let new_position = physics.position;

        let traveled_distance = surface_distance(position, new_position, terrain.radius);
        self.state.linear_velocity = physics.linear_velocity;
        let transform_up = physics.up.normalize_or(terrain.normal);
        let transform_forward = (physics.forward
            - transform_up * physics.forward.dot(transform_up))
        .normalize_or(body_forward);
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
            if movement_input.length_squared() > 0.25 && self.state.speed() < 0.4 {
                self.state.stuck_seconds + dt
            } else {
                0.0
            };
        VehicleTickResult {
            traveled_distance,
            collision_impulse,
            recovered: false,
            deck_from: old_deck,
            deck_to: self.deck_position(tuning),
        }
    }

    pub fn recover(&mut self, planet: &Planet, tuning: &VehicleTuning) {
        self.bump_control_seconds = 0.0;
        let safe = planet.nearest_safe_point(self.state.transform.position.normalize_or(Vec3::Y));
        self.state.transform = make_transform(safe.position, self.state.transform.forward, safe.up);
        self.previous_transform = self.state.transform;
        self.state.linear_velocity = Vec3::ZERO;
        self.previous_linear_velocity = Vec3::ZERO;
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
        self.state.transform.position - self.state.transform.up * (tuning.hover_height * 0.72)
    }

    /// Apply a tangent bumper impulse without changing authoritative cut history.
    pub(crate) fn bumper_contact(&mut self, planet: &Planet, correction: Vec3, impulse: Vec3) {
        let old_position = self.state.transform.position;
        let old_up = old_position.normalize();
        let new_up = (old_position + correction).normalize_or(old_up);
        let altitude = old_position.length() - planet.surface_radius(old_up);
        let rotation = Quat::from_rotation_arc(old_up, new_up);
        let transform = &mut self.state.transform;
        transform.position = new_up * (planet.surface_radius(new_up) + altitude);
        transform.rotation = (rotation * transform.rotation).normalize();
        transform.up = transform.rotation * Vec3::Y;
        transform.forward = transform.rotation * Vec3::NEG_Z;
        let tangent_impulse = impulse - new_up * impulse.dot(new_up);
        self.state.linear_velocity = rotation * self.state.linear_velocity + tangent_impulse;
        if impulse.length_squared() > 1.0 {
            self.bump_control_seconds = 0.28;
        }
        self.physics
            .correct_contact(*transform, self.state.linear_velocity);
    }
}

/// Equal-mass rounded bumpers. Swept contact catches two mowers crossing during
/// one fixed tick; separation and impulses stay tangent to the planet.
pub(crate) fn resolve_mower_contact(
    planet: &Planet,
    player: &mut HoverVehicle,
    rival: &mut HoverVehicle,
    player_before: Vec3,
    rival_before: Vec3,
) -> Option<(f32, Vec3)> {
    const DIAMETER: f32 = 2.18;
    let p = player.state.transform.position;
    let r = rival.state.transform.position;
    if (p.length() - r.length()).abs() > 0.9 {
        return None;
    }
    let from = rival_before - player_before;
    let to = r - p;
    let delta = to - from;
    let t = (-from.dot(delta) / delta.length_squared().max(1.0e-8)).clamp(0.0, 1.0);
    let closest = from + delta * t;
    if closest.length_squared() >= DIAMETER * DIAMETER {
        return None;
    }
    let up = (p + r).normalize_or(player.state.transform.up);
    let relative_velocity = rival.state.linear_velocity - player.state.linear_velocity;
    let direction = if closest.length_squared() > 0.0001 {
        closest
    } else {
        from
    };
    let normal = (direction - up * direction.dot(up))
        .try_normalize()
        .unwrap_or_else(|| {
            (player.state.transform.forward - up * player.state.transform.forward.dot(up))
                .normalize()
        });
    let closing = (-relative_velocity.dot(normal)).max(0.0);
    let impulse = (closing * 0.825).min(30.0);
    let separation = if to.dot(normal) < 0.0 {
        // The pair crossed between samples; place each back on its own side.
        (DIAMETER - to.dot(normal)).max(0.0) * 0.5
    } else {
        (DIAMETER - to.length()).max(0.0) * 0.5
    };
    player.bumper_contact(planet, -normal * separation, -normal * impulse);
    rival.bumper_contact(planet, normal * separation, normal * impulse);
    (closing > 1.0).then_some((impulse, (p + r) * 0.5))
}

pub(crate) fn surface_distance(from: Vec3, to: Vec3, radius: f32) -> f32 {
    let from = from.normalize_or_zero();
    let to = to.normalize_or_zero();
    // atan2 retains sub-texel movement; acos(dot) rounds slow movement to zero.
    from.cross(to).length().atan2(from.dot(to)) * radius
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

    #[test]
    fn slow_travel_remains_measurable() {
        let radius = 15.0;
        let angle = 0.000_01;
        let from = Vec3::X * radius;
        let to = Quat::from_rotation_z(angle) * from;
        assert!((surface_distance(from, to, radius) - angle * radius).abs() < 1.0e-7);
    }

    #[test]
    fn opposite_up_vectors_interpolate_to_a_valid_pose() {
        let from = make_transform(Vec3::Y, Vec3::NEG_Z, Vec3::Y);
        let to = make_transform(Vec3::NEG_Y, Vec3::NEG_Z, Vec3::NEG_Y);
        let middle = from.interpolated(to, 0.5);
        assert!(middle.up.is_normalized());
        assert!(middle.forward.is_normalized());
        assert!(middle.up.dot(middle.forward).abs() < 1.0e-6);
        assert!(middle.up.distance(middle.rotation * Vec3::Y) < 1.0e-6);
    }

    fn planet() -> Planet {
        PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
            .generate_with_roots(WorldSeed(123), false)
            .unwrap()
    }

    fn bumper_pair(
        up: Vec3,
        speed_player: f32,
        speed_rival: f32,
    ) -> (Planet, HoverVehicle, HoverVehicle, Vec3) {
        let config = GeneratorConfig {
            rolling_amplitude: 0.0,
            mountain_count_min: 0,
            mountain_count_max: 0,
            mowable_ratio_min: 1.0,
            mowable_ratio_max: 1.0,
            ..GeneratorConfig::test_quality()
        };
        let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config)
            .generate_with_roots(WorldSeed(7), false)
            .unwrap();
        let tuning = VehicleTuning::default();
        let right = tangent_frame(up).0;
        let spawn = |side: f32| {
            let normal = (up * planet.config.base_radius + right * side).normalize();
            let forward = (right - normal * right.dot(normal)).normalize();
            SpawnPoint {
                position: normal * (planet.config.base_radius + 0.8),
                up: normal,
                forward,
                clearance: 0.0,
            }
        };
        let mut player = HoverVehicle::from_spawn(&planet, spawn(-0.95), &tuning);
        let mut rival = HoverVehicle::from_spawn(&planet, spawn(0.95), &tuning);
        player.state.linear_velocity = player.state.transform.forward * speed_player;
        rival.state.linear_velocity = rival.state.transform.forward * speed_rival;
        player
            .physics
            .teleport(player.state.transform, player.state.linear_velocity);
        rival
            .physics
            .teleport(rival.state.transform, rival.state.linear_velocity);
        (planet, player, rival, right)
    }

    #[test]
    fn mower_bumpers_exchange_momentum_and_separate_on_any_hemisphere() {
        for up in [Vec3::Y, Vec3::NEG_X, Vec3::new(1.0, -1.0, 1.0).normalize()] {
            for (player_speed, rival_speed) in [(8.0, -8.0), (12.0, 0.0)] {
                let (planet, mut player, mut rival, right) =
                    bumper_pair(up, player_speed, rival_speed);
                let before_player = player.state.transform.position;
                let before_rival = rival.state.transform.position;
                let momentum_before = player.state.linear_velocity + rival.state.linear_velocity;
                let (impulse, _) = resolve_mower_contact(
                    &planet,
                    &mut player,
                    &mut rival,
                    before_player,
                    before_rival,
                )
                .unwrap();
                assert!(impulse > 5.0);
                assert!(
                    player
                        .state
                        .transform
                        .position
                        .distance(rival.state.transform.position)
                        >= 2.175
                );
                assert!(
                    rival.state.linear_velocity.dot(right) > 4.0,
                    "contact must move the opponent"
                );
                if rival_speed < 0.0 {
                    assert!(player.state.linear_velocity.dot(right) < -3.0);
                }
                let momentum_after = player.state.linear_velocity + rival.state.linear_velocity;
                assert!((momentum_after - momentum_before).dot(right).abs() < 0.12);
                for vehicle in [&player, &rival] {
                    let normal = vehicle.state.transform.position.normalize();
                    assert!(
                        (vehicle.state.transform.position.length()
                            - planet.config.base_radius
                            - 0.8)
                            .abs()
                            < 0.001
                    );
                    assert!(vehicle.state.linear_velocity.dot(normal).abs() < 0.001);
                    assert!(vehicle.state.transform.rotation.is_finite());
                }
                // Resting/separating contact cannot repeatedly kick either car.
                assert!(
                    resolve_mower_contact(
                        &planet,
                        &mut player,
                        &mut rival,
                        before_player,
                        before_rival
                    )
                    .is_none()
                );
            }
        }
    }

    #[test]
    fn bumper_collision_is_swept_and_steering_control_returns() {
        let (planet, mut player, mut rival, right) = bumper_pair(Vec3::Y, 12.0, -12.0);
        let player_before = player.state.transform.position - right * 2.0;
        let rival_before = rival.state.transform.position + right * 2.0;
        let p = player.state.transform.position;
        let r = rival.state.transform.position;
        // Deliberately cross both centers in one sample to exercise sweep handling.
        player.state.transform.position = r;
        rival.state.transform.position = p;
        assert!(
            resolve_mower_contact(
                &planet,
                &mut player,
                &mut rival,
                player_before,
                rival_before
            )
            .is_some()
        );
        assert!(
            (rival.state.transform.position - player.state.transform.position).dot(right) > 2.1
        );
        let tuning = VehicleTuning::default();
        for _ in 0..90 {
            tick(
                &mut player,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                crate::FIXED_DT,
            );
        }
        assert_eq!(player.bump_control_seconds, 0.0);
        assert!(player.state.speed() > tuning.max_speed * 0.9);
        assert!(player.state.grounded);
    }

    fn tick(
        vehicle: &mut HoverVehicle,
        planet: &Planet,
        tuning: &VehicleTuning,
        input: InputSnapshot,
        dt: f32,
    ) -> VehicleTickResult {
        let movement_forward = vehicle.state.transform.forward;
        vehicle.tick(planet, tuning, input, movement_forward, true, dt)
    }

    #[test]
    fn vehicle_stays_aligned_during_circumnavigation() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        for step in 0..120 * 25 {
            tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    steer: 0.15,
                    ..InputSnapshot::default()
                },
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
        tick(
            &mut vehicle,
            &planet,
            &tuning,
            InputSnapshot {
                recover_held: true,
                ..InputSnapshot::default()
            },
            0.03,
        );
        assert_eq!(vehicle.state.recoveries, 1);
        assert!(vehicle.state.grounded);
    }

    #[test]
    fn tall_terrain_does_not_repeat_recovery_but_escaped_mowers_still_recover() {
        let config = GeneratorConfig {
            rolling_amplitude: 0.0,
            mountain_count_min: 1,
            mountain_count_max: 1,
            mountain_height_min: 15.0,
            mountain_height_max: 15.0,
            mowable_ratio_min: 1.0,
            mowable_ratio_max: 1.0,
            spawn_max_slope_degrees: 45.0,
            ideal_time_min_seconds: 0.0,
            ideal_time_max_seconds: 10_000.0,
            ..GeneratorConfig::test_quality()
        };
        let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config)
            .generate_with_roots(WorldSeed(7), false)
            .unwrap();
        let tuning = VehicleTuning::default();
        let spawn = planet.nearest_safe_point(planet.mountains[0].center);
        assert!(spawn.position.length() > planet.config.base_radius + 14.0);
        let mut vehicle = HoverVehicle::from_spawn(&planet, spawn, &tuning);
        for _ in 0..120 {
            let result = tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot::default(),
                crate::FIXED_DT,
            );
            assert!(!result.recovered, "a valid peak must not cause recovery");
        }
        assert_eq!(vehicle.state.recoveries, 0);
        assert!(vehicle.state.transform.position.is_finite());

        for radius in [
            1.0,
            planet.surface_radius(spawn.position.normalize()) + 20.0,
        ] {
            vehicle.state.transform.position = spawn.position.normalize() * radius;
            let result = tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot::default(),
                crate::FIXED_DT,
            );
            assert!(result.recovered, "an escaped mower must recover");
            assert!(vehicle.state.grounded);
            assert!(vehicle.state.transform.position.length() > planet.config.base_radius + 14.0);
            let result = tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot::default(),
                crate::FIXED_DT,
            );
            assert!(!result.recovered, "the recovered peak must remain playable");
        }
        assert_eq!(vehicle.state.recoveries, 2);
    }

    #[test]
    fn low_rolling_terrain_is_not_mistaken_for_falling_through_the_planet() {
        let config = GeneratorConfig {
            rolling_amplitude: 14.0,
            mountain_count_min: 0,
            mountain_count_max: 0,
            mowable_ratio_min: 1.0,
            mowable_ratio_max: 1.0,
            spawn_max_slope_degrees: 45.0,
            ideal_time_min_seconds: 0.0,
            ideal_time_max_seconds: 10_000.0,
            ..GeneratorConfig::test_quality()
        };
        let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config)
            .generate_with_roots(WorldSeed(42), false)
            .unwrap();
        let tuning = VehicleTuning::default();
        assert!(planet.spawn.position.length() < planet.config.base_radius * 0.7);
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        for _ in 0..120 {
            assert!(
                !tick(
                    &mut vehicle,
                    &planet,
                    &tuning,
                    InputSnapshot::default(),
                    crate::FIXED_DT,
                )
                .recovered,
                "a valid low valley must not cause recovery"
            );
        }
        assert_eq!(vehicle.state.recoveries, 0);
        assert!(vehicle.state.transform.position.is_finite());
    }

    #[test]
    fn forward_backward_and_sideways_have_the_same_speed() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut forward = HoverVehicle::new(&planet, &tuning);
        let mut backward = HoverVehicle::new(&planet, &tuning);
        let mut sideways = HoverVehicle::new(&planet, &tuning);
        for _ in 0..48 {
            tick(
                &mut forward,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
            tick(
                &mut backward,
                &planet,
                &tuning,
                InputSnapshot {
                    brake_reverse: 1.0,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
            tick(
                &mut sideways,
                &planet,
                &tuning,
                InputSnapshot {
                    steer: 1.0,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
        }
        let speeds = [
            forward.state.speed(),
            backward.state.speed(),
            sideways.state.speed(),
        ];
        let slowest = speeds.into_iter().fold(f32::INFINITY, f32::min);
        let fastest = speeds.into_iter().fold(0.0_f32, f32::max);
        assert!(
            slowest > fastest * 0.9,
            "directional speeds diverged: {speeds:?}"
        );

        let local_right = sideways
            .state
            .transform
            .forward
            .cross(sideways.state.transform.up)
            .normalize();
        let lateral_speed = sideways.state.linear_velocity.dot(local_right).abs();
        let chassis_forward_speed = sideways
            .state
            .linear_velocity
            .dot(sideways.state.transform.forward)
            .abs();
        assert!(lateral_speed > chassis_forward_speed * 4.0);
    }

    #[test]
    fn boost_is_a_controlled_speed_step_and_releases_quickly() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut normal = HoverVehicle::new(&planet, &tuning);
        let mut boosted = HoverVehicle::new(&planet, &tuning);
        let mut normal_peak = 0.0_f32;
        let mut boosted_peak = 0.0_f32;
        for _ in 0..120 {
            tick(
                &mut normal,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
            tick(
                &mut boosted,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    boost_held: true,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
            normal_peak = normal_peak.max(normal.state.speed());
            boosted_peak = boosted_peak.max(boosted.state.speed());
        }
        assert!(
            boosted_peak > normal_peak * 1.35 && boosted_peak < normal_peak * 1.6,
            "peak boost speed {boosted_peak} was outside the controlled burst range above {normal_peak}"
        );
        assert!(
            boosted_peak > tuning.boost_max_speed * 0.93,
            "boost peaked at only {boosted_peak} m/s (final {}, normal peak {normal_peak})",
            boosted.state.speed()
        );
        for _ in 0..36 {
            tick(
                &mut boosted,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                crate::FIXED_DT,
            );
        }
        assert!(boosted.state.speed() < tuning.max_speed + 0.5);
        for _ in 0..36 {
            tick(
                &mut boosted,
                &planet,
                &tuning,
                InputSnapshot::default(),
                crate::FIXED_DT,
            );
        }
        assert!(
            boosted.state.speed() < 1.0,
            "boost release should brake promptly"
        );
    }

    #[test]
    fn boost_follows_small_and_rolling_planets() {
        let mut reports = Vec::new();
        for radius in [12.0, 22.0] {
            for rolling in [0.0, 1.2] {
                let config = GeneratorConfig {
                    base_radius: radius,
                    rolling_amplitude: rolling,
                    mountain_count_min: 0,
                    mountain_count_max: 0,
                    mowable_ratio_min: 1.0,
                    mowable_ratio_max: 1.0,
                    terrain_resolution: 64,
                    ..GeneratorConfig::test_quality()
                };
                let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config)
                    .generate_with_roots(WorldSeed(123), false)
                    .unwrap();
                for sideways in [false, true] {
                    for stress in [false, true] {
                        let tuning = if stress {
                            VehicleTuning {
                                boost_max_speed: 28.0,
                                boost_capacity_seconds: 10.0,
                                ..VehicleTuning::default()
                            }
                        } else {
                            VehicleTuning::default()
                        };
                        let mut vehicle = HoverVehicle::new(&planet, &tuning);
                        let mut airborne_ticks = 0;
                        let mut max_altitude = 0.0_f32;
                        let mut peak_speed = 0.0_f32;
                        let mut minimum_alignment = 1.0_f32;
                        for step in 0..960 {
                            tick(
                                &mut vehicle,
                                &planet,
                                &tuning,
                                InputSnapshot {
                                    accelerate: if sideways { 0.0 } else { 1.0 },
                                    steer: if sideways { 1.0 } else { 0.0 },
                                    boost_held: stress || step % 720 < 180,
                                    ..InputSnapshot::default()
                                },
                                crate::FIXED_DT,
                            );
                            let position = vehicle.state.transform.position;
                            let altitude =
                                position.length() - planet.surface_radius(position.normalize());
                            max_altitude = max_altitude.max(altitude);
                            peak_speed = peak_speed.max(vehicle.state.speed());
                            airborne_ticks += usize::from(!vehicle.state.grounded);
                            minimum_alignment = minimum_alignment.min(
                                vehicle
                                    .state
                                    .transform
                                    .up
                                    .dot(planet.terrain_cell(position.normalize()).normal),
                            );
                        }
                        eprintln!(
                            "boost radius={radius} rolling={rolling} sideways={sideways} stress={stress}: airborne={airborne_ticks}/960 max_altitude={max_altitude:.3} peak_speed={peak_speed:.3} alignment={minimum_alignment:.3}"
                        );
                        reports.push((
                            stress,
                            airborne_ticks,
                            max_altitude,
                            vehicle.state.recoveries,
                        ));
                        assert!(minimum_alignment > 0.90, "boost lost surface alignment");
                    }
                }
            }
        }
        for (stress, airborne, altitude, recoveries) in reports {
            // Shipping boost must keep cutting throughout. The sustained old
            // 28 m/s stress case may briefly unload at the sharpest crests,
            // but must remain near the lawn and recover contact immediately.
            assert!(
                airborne <= if stress { 10 } else { 0 },
                "boost lost ground contact for {airborne} ticks (stress={stress})"
            );
            assert!(
                altitude < if stress { 1.5 } else { 1.05 },
                "boost floated {altitude} metres above the lawn (stress={stress})"
            );
            assert_eq!(recoveries, 0);
        }
    }

    #[test]
    fn forward_launch_reaches_cruising_speed_within_one_second() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        let mut peak_speed = 0.0_f32;
        let mut traveled = 0.0_f32;
        for _ in 0..120 {
            let result = tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
            peak_speed = peak_speed.max(vehicle.state.speed());
            traveled += result.traveled_distance;
        }
        let forward_speed = vehicle
            .state
            .linear_velocity
            .dot(vehicle.state.transform.forward);
        assert!(
            forward_speed > tuning.max_speed * 0.9,
            "forward launch only reached {forward_speed} m/s after one second (peak speed {peak_speed}, final speed {}, distance {traveled})",
            vehicle.state.speed()
        );
    }

    #[test]
    fn releasing_input_brakes_quickly() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        for _ in 0..120 {
            tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
        }
        let cruising_speed = vehicle.state.speed();
        for _ in 0..24 {
            tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot::default(),
                1.0 / 120.0,
            );
        }
        assert!(
            vehicle.state.speed() < cruising_speed * 0.25,
            "mower retained {} m/s from a {cruising_speed} m/s cruise",
            vehicle.state.speed()
        );
    }

    #[test]
    fn reversing_direction_is_fast_but_not_instantaneous() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        for _ in 0..120 {
            tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
        }
        let initial_forward_speed = vehicle
            .state
            .linear_velocity
            .dot(vehicle.state.transform.forward);
        tick(
            &mut vehicle,
            &planet,
            &tuning,
            InputSnapshot {
                brake_reverse: 1.0,
                ..InputSnapshot::default()
            },
            1.0 / 120.0,
        );
        assert!(
            vehicle
                .state
                .linear_velocity
                .dot(vehicle.state.transform.forward)
                > 0.0,
            "direction change should sweep through momentum rather than snap"
        );
        for _ in 1..72 {
            tick(
                &mut vehicle,
                &planet,
                &tuning,
                InputSnapshot {
                    brake_reverse: 1.0,
                    ..InputSnapshot::default()
                },
                1.0 / 120.0,
            );
        }
        let reversed_speed = vehicle
            .state
            .linear_velocity
            .dot(vehicle.state.transform.forward);
        assert!(initial_forward_speed > tuning.max_speed * 0.9);
        assert!(
            reversed_speed < -tuning.max_speed * 0.8,
            "reversal reached only {reversed_speed} m/s after 0.6 seconds"
        );
    }

    #[test]
    fn mower_deck_is_centered_under_the_chassis() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let vehicle = HoverVehicle::new(&planet, &tuning);
        let offset = vehicle.deck_position(&tuning) - vehicle.state.transform.position;
        assert!(offset.dot(vehicle.state.transform.forward).abs() < 1.0e-6);
        let right = vehicle
            .state
            .transform
            .forward
            .cross(vehicle.state.transform.up);
        assert!(offset.dot(right).abs() < 1.0e-6);
        assert!(offset.dot(vehicle.state.transform.up) < 0.0);
    }

    #[test]
    fn mower_is_restored_to_always_on_each_tick() {
        let planet = planet();
        let tuning = VehicleTuning::default();
        let mut vehicle = HoverVehicle::new(&planet, &tuning);
        vehicle.state.mower_enabled = false;
        tick(
            &mut vehicle,
            &planet,
            &tuning,
            InputSnapshot::default(),
            1.0 / 120.0,
        );
        assert!(vehicle.state.mower_enabled);
    }
}
