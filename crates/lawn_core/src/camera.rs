//! Mostly top-down local-radial chase camera without geographic poles.

use glam::{Quat, Vec3};

use crate::{planet::Planet, profile::AccessibilitySettings, vehicle::VehicleTransform};

const CHASE_HEIGHT: f32 = 10.2;
const CHASE_TARGET_DEPTH: f32 = 2.2;
const CHASE_ZOOM_RANGE: f32 = 4.0;
const REFERENCE_PLANET_RADIUS: f32 = 15.0;
const FOLLOW_DAMPING_RATIO: f32 = 0.72;
const FOLLOW_TELEPORT_DISTANCE: f32 = CHASE_HEIGHT * 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraState {
    pub position: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub field_of_view_degrees: f32,
}

#[derive(Clone, Debug)]
pub struct CameraRig {
    pub state: CameraState,
    previous_state: CameraState,
    position_velocity: Vec3,
    target_velocity: Vec3,
    orbit_yaw: f32,
    orbit_pitch: f32,
    elapsed_seconds: f32,
}

impl CameraRig {
    #[must_use]
    pub fn new(
        vehicle: VehicleTransform,
        planet_radius: f32,
        settings: &AccessibilitySettings,
    ) -> Self {
        let scale = chase_scale(planet_radius);
        let (position, target) = chase_pose(
            vehicle,
            vehicle.forward,
            CHASE_HEIGHT * scale,
            CHASE_TARGET_DEPTH * scale,
            settings.camera_tilt_degrees,
        );
        let state = CameraState {
            position,
            target,
            up: vehicle.forward,
            field_of_view_degrees: settings.field_of_view_degrees,
        };
        Self {
            state,
            previous_state: state,
            position_velocity: Vec3::ZERO,
            target_velocity: Vec3::ZERO,
            orbit_yaw: 0.0,
            orbit_pitch: 0.0,
            elapsed_seconds: 0.0,
        }
    }

    /// Returns a camera pose on the same render timeline as the interpolated
    /// vehicle, avoiding fixed-tick judder between the two presentation layers.
    #[must_use]
    pub fn interpolated_state(&self, alpha: f32) -> CameraState {
        let alpha = alpha.clamp(0.0, 1.0);
        CameraState {
            position: self
                .previous_state
                .position
                .lerp(self.state.position, alpha),
            target: self.previous_state.target.lerp(self.state.target, alpha),
            up: self
                .previous_state
                .up
                .lerp(self.state.up, alpha)
                .normalize_or(self.state.up),
            field_of_view_degrees: self.previous_state.field_of_view_degrees
                + (self.state.field_of_view_degrees - self.previous_state.field_of_view_degrees)
                    * alpha,
        }
    }

    /// Immediately establishes a non-gameplay camera, such as the title-screen
    /// planet orbit, without leaving spring energy behind for the next frame.
    pub fn snap_to_pose(&mut self, position: Vec3, target: Vec3, up: Vec3) {
        self.state.position = position;
        self.state.target = target;
        self.state.up = up.normalize_or(Vec3::Y);
        self.previous_state = self.state;
        self.position_velocity = Vec3::ZERO;
        self.target_velocity = Vec3::ZERO;
    }

    pub fn update(
        &mut self,
        planet: &Planet,
        vehicle: VehicleTransform,
        velocity: Vec3,
        orbit: [f32; 2],
        look_behind: bool,
        recenter: bool,
        settings: &AccessibilitySettings,
        dt: f32,
    ) {
        self.previous_state = self.state;
        self.elapsed_seconds += dt;
        if recenter {
            self.orbit_yaw = 0.0;
            self.orbit_pitch = 0.0;
        }
        self.orbit_yaw = (self.orbit_yaw + orbit[0] * dt * 2.2).clamp(-1.4, 1.4);
        let pitch_input = if settings.invert_camera_y {
            orbit[1]
        } else {
            -orbit[1]
        };
        self.orbit_pitch = (self.orbit_pitch + pitch_input * dt * 1.7).clamp(-0.35, 0.7);
        self.orbit_yaw *= (-1.8 * dt).exp();
        self.orbit_pitch *= (-1.8 * dt).exp();

        let local_up = vehicle.up;
        let fixed_heading = Vec3::Y - local_up * Vec3::Y.dot(local_up);
        let base_screen_up = if settings.fixed_horizon {
            fixed_heading.try_normalize().unwrap_or(vehicle.forward)
        } else {
            vehicle.forward
        };
        let heading_rotation = self.orbit_yaw
            + if look_behind {
                std::f32::consts::PI
            } else {
                0.0
            };
        let mut screen_up = Quat::from_axis_angle(local_up, heading_rotation)
            .mul_vec3(base_screen_up)
            .normalize();
        let speed_shake = (velocity.length() / 17.0).clamp(0.0, 1.0);
        let shake_angle =
            settings.camera_shake * speed_shake * (self.elapsed_seconds * 31.0).sin() * 0.008;
        screen_up = Quat::from_axis_angle(local_up, shake_angle).mul_vec3(screen_up);

        let scale = chase_scale(planet.config.base_radius);
        let height =
            (CHASE_HEIGHT * scale + self.orbit_pitch * CHASE_ZOOM_RANGE * scale).max(5.5 * scale);
        let (desired_position, desired_target) = chase_pose(
            vehicle,
            screen_up,
            height,
            CHASE_TARGET_DEPTH * scale,
            settings.camera_tilt_degrees,
        );
        let stiffness = settings.camera_follow_stiffness;
        if self.state.position.distance(desired_position) > FOLLOW_TELEPORT_DISTANCE {
            self.state.position = desired_position;
            self.state.target = desired_target;
            self.position_velocity = Vec3::ZERO;
            self.target_velocity = Vec3::ZERO;
        } else {
            spring_follow(
                &mut self.state.position,
                &mut self.position_velocity,
                desired_position,
                stiffness,
                FOLLOW_DAMPING_RATIO,
                dt,
            );
            spring_follow(
                &mut self.state.target,
                &mut self.target_velocity,
                desired_target,
                stiffness,
                FOLLOW_DAMPING_RATIO,
                dt,
            );
        }
        let follow = 1.0 - (-stiffness * dt).exp();
        let smoothed_up = self.state.up.lerp(screen_up, follow);
        self.state.up = (smoothed_up - local_up * smoothed_up.dot(local_up))
            .try_normalize()
            .unwrap_or(screen_up);
        self.state.field_of_view_degrees = settings.field_of_view_degrees;
    }
}

fn spring_follow(
    value: &mut Vec3,
    velocity: &mut Vec3,
    target: Vec3,
    angular_frequency: f32,
    damping_ratio: f32,
    dt: f32,
) {
    let frequency = angular_frequency.max(0.01);
    let acceleration =
        (target - *value) * frequency * frequency - *velocity * (2.0 * damping_ratio * frequency);
    *velocity += acceleration * dt;
    *value += *velocity * dt;
}

fn chase_pose(
    vehicle: VehicleTransform,
    screen_up: Vec3,
    height: f32,
    target_depth: f32,
    tilt_degrees: f32,
) -> (Vec3, Vec3) {
    let target = vehicle.position - vehicle.up * target_depth;
    let vertical_span = height + target_depth;
    let back_offset = vertical_span * tilt_degrees.clamp(0.0, 18.0).to_radians().tan();
    let position = vehicle.position + vehicle.up * height - screen_up * back_offset;
    (position, target)
}

fn chase_scale(planet_radius: f32) -> f32 {
    (planet_radius / REFERENCE_PLANET_RADIUS).clamp(0.8, 1.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION};

    #[test]
    fn default_camera_is_slightly_tipped_and_keeps_the_planet_in_frame() {
        let vehicle = VehicleTransform {
            position: Vec3::Y * 15.8,
            rotation: glam::Quat::IDENTITY,
            forward: Vec3::Z,
            up: Vec3::Y,
        };
        let camera = CameraRig::new(
            vehicle,
            REFERENCE_PLANET_RADIUS,
            &AccessibilitySettings::default(),
        );
        let position_offset = camera.state.position - vehicle.position;
        let view_direction = (camera.state.target - camera.state.position).normalize();

        assert!((position_offset.dot(vehicle.up) - CHASE_HEIGHT).abs() < 1.0e-5);
        let tilt = view_direction.dot(-vehicle.up).acos().to_degrees();
        assert!((tilt - 12.0).abs() < 0.01);
        assert!(camera.state.up.dot(vehicle.up).abs() < 1.0e-5);
        assert!(camera.state.up.dot(vehicle.forward) > 0.999_99);

        let nominal_planet_radius = 15.0_f32;
        let angular_radius = (nominal_planet_radius / camera.state.position.length())
            .asin()
            .to_degrees();
        let planet_center = (-camera.state.position).normalize();
        let framing_offset = view_direction.dot(planet_center).acos().to_degrees();
        assert!(
            angular_radius + framing_offset < camera.state.field_of_view_degrees * 0.5,
            "planet should remain fully framed: radius {angular_radius:.2}°, offset {framing_offset:.2}°"
        );
    }

    #[test]
    fn zero_tilt_restores_the_exact_top_down_view() {
        let vehicle = VehicleTransform {
            position: Vec3::Y * 15.8,
            rotation: glam::Quat::IDENTITY,
            forward: Vec3::Z,
            up: Vec3::Y,
        };
        let settings = AccessibilitySettings {
            camera_tilt_degrees: 0.0,
            ..AccessibilitySettings::default()
        };
        let camera = CameraRig::new(vehicle, REFERENCE_PLANET_RADIUS, &settings);
        let view_direction = (camera.state.target - camera.state.position).normalize();
        assert!(view_direction.dot(-vehicle.up) > 0.999_99);
    }

    #[test]
    fn chase_distance_scales_to_keep_editor_planet_sizes_framed() {
        let settings = AccessibilitySettings::default();
        for planet_radius in [12.0_f32, 15.0, 22.0] {
            let vehicle = VehicleTransform {
                position: Vec3::Y * (planet_radius + 0.8),
                rotation: glam::Quat::IDENTITY,
                forward: Vec3::Z,
                up: Vec3::Y,
            };
            let camera = CameraRig::new(vehicle, planet_radius, &settings);
            let view_direction = (camera.state.target - camera.state.position).normalize();
            let angular_radius = (planet_radius / camera.state.position.length())
                .asin()
                .to_degrees();
            let planet_center = (-camera.state.position).normalize();
            let framing_offset = view_direction.dot(planet_center).acos().to_degrees();
            assert!(
                angular_radius + framing_offset < camera.state.field_of_view_degrees * 0.5,
                "radius {planet_radius} was not fully framed"
            );
        }
    }

    #[test]
    fn camera_remains_finite_across_arbitrary_coordinate_poles() {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(313), false)
                .unwrap();
        let settings = AccessibilitySettings::default();
        let mut vehicle = VehicleTransform {
            position: Vec3::X * 31.0,
            rotation: glam::Quat::IDENTITY,
            forward: Vec3::Y,
            up: Vec3::X,
        };
        let mut camera = CameraRig::new(vehicle, planet.config.base_radius, &settings);
        for step in 0..720 {
            let angle = step as f32 * std::f32::consts::TAU / 720.0;
            let up = Vec3::new(angle.cos(), angle.sin(), 0.0);
            vehicle.position = up * (planet.surface_radius(up) + 0.8);
            vehicle.up = up;
            vehicle.forward = Vec3::new(-angle.sin(), angle.cos(), 0.0);
            camera.update(
                &planet,
                vehicle,
                vehicle.forward * 8.0,
                [0.0; 2],
                false,
                false,
                &settings,
                1.0 / 120.0,
            );
            assert!(camera.state.position.is_finite());
            assert!(camera.state.target.is_finite());
            assert!(camera.state.up.is_normalized());
        }
    }

    #[test]
    fn zero_camera_shake_is_motion_independent() {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(17), false)
                .unwrap();
        let settings = AccessibilitySettings {
            camera_shake: 0.0,
            ..AccessibilitySettings::default()
        };
        let vehicle = VehicleTransform {
            position: planet.spawn.position,
            rotation: glam::Quat::IDENTITY,
            forward: planet.spawn.forward,
            up: planet.spawn.up,
        };
        let mut slow = CameraRig::new(vehicle, planet.config.base_radius, &settings);
        let mut fast = CameraRig::new(vehicle, planet.config.base_radius, &settings);
        slow.update(
            &planet,
            vehicle,
            vehicle.forward,
            [0.0; 2],
            false,
            false,
            &settings,
            1.0 / 60.0,
        );
        fast.update(
            &planet,
            vehicle,
            vehicle.forward * 17.0,
            [0.0; 2],
            false,
            false,
            &settings,
            1.0 / 60.0,
        );
        assert!(slow.state.position.distance(fast.state.position) < 1.0e-6);
        assert!(slow.state.up.distance(fast.state.up) < 1.0e-6);
    }

    #[test]
    fn follow_spring_lags_then_settles_on_a_moving_vehicle() {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(91), false)
                .unwrap();
        let settings = AccessibilitySettings::default();
        let mut vehicle = VehicleTransform {
            position: planet.spawn.position,
            rotation: glam::Quat::IDENTITY,
            forward: planet.spawn.forward,
            up: planet.spawn.up,
        };
        let mut camera = CameraRig::new(vehicle, planet.config.base_radius, &settings);
        let initial_position = camera.state.position;
        vehicle.position += vehicle.forward * 2.0;
        let desired_position = chase_pose(
            vehicle,
            vehicle.forward,
            CHASE_HEIGHT,
            CHASE_TARGET_DEPTH,
            settings.camera_tilt_degrees,
        )
        .0;

        camera.update(
            &planet,
            vehicle,
            vehicle.forward * 8.0,
            [0.0; 2],
            false,
            false,
            &settings,
            1.0 / 120.0,
        );
        assert!(camera.state.position.distance(initial_position) > 0.0);
        assert!(camera.state.position.distance(desired_position) > 0.5);
        let interpolated = camera.interpolated_state(0.5);
        assert!(interpolated.position.distance(initial_position) > 0.0);
        assert!(interpolated.position.distance(camera.state.position) > 0.0);

        for _ in 0..240 {
            camera.update(
                &planet,
                vehicle,
                Vec3::ZERO,
                [0.0; 2],
                false,
                false,
                &settings,
                1.0 / 120.0,
            );
        }
        assert!(camera.state.position.distance(desired_position) < 0.01);
    }
}
