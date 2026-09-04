//! Local-radial top-down camera without geographic poles.

use glam::{Quat, Vec3};

use crate::{planet::Planet, profile::AccessibilitySettings, vehicle::VehicleTransform};

const TOP_DOWN_HEIGHT: f32 = 9.5;
const TOP_DOWN_ZOOM_RANGE: f32 = 4.0;

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
    orbit_yaw: f32,
    orbit_pitch: f32,
    elapsed_seconds: f32,
}

impl CameraRig {
    #[must_use]
    pub fn new(vehicle: VehicleTransform, settings: &AccessibilitySettings) -> Self {
        let state = CameraState {
            position: vehicle.position + vehicle.up * TOP_DOWN_HEIGHT,
            target: vehicle.position,
            up: vehicle.forward,
            field_of_view_degrees: settings.field_of_view_degrees,
        };
        Self {
            state,
            orbit_yaw: 0.0,
            orbit_pitch: 0.0,
            elapsed_seconds: 0.0,
        }
    }

    pub fn update(
        &mut self,
        _planet: &Planet,
        vehicle: VehicleTransform,
        velocity: Vec3,
        orbit: [f32; 2],
        look_behind: bool,
        recenter: bool,
        settings: &AccessibilitySettings,
        dt: f32,
    ) {
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

        let height = (TOP_DOWN_HEIGHT + self.orbit_pitch * TOP_DOWN_ZOOM_RANGE).max(5.5);
        self.state.position = vehicle.position + local_up * height;
        self.state.target = vehicle.position;
        let stiffness = settings.camera_follow_stiffness;
        let follow = 1.0 - (-stiffness * dt).exp();
        let smoothed_up = self.state.up.lerp(screen_up, follow);
        self.state.up = (smoothed_up - local_up * smoothed_up.dot(local_up))
            .try_normalize()
            .unwrap_or(screen_up);
        self.state.field_of_view_degrees = settings.field_of_view_degrees;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION};

    #[test]
    fn default_camera_is_exactly_top_down() {
        let vehicle = VehicleTransform {
            position: Vec3::Y * 15.8,
            rotation: glam::Quat::IDENTITY,
            forward: Vec3::Z,
            up: Vec3::Y,
        };
        let camera = CameraRig::new(vehicle, &AccessibilitySettings::default());
        let position_offset = camera.state.position - vehicle.position;
        let view_direction = (camera.state.target - camera.state.position).normalize();

        assert!((position_offset.dot(vehicle.up) - TOP_DOWN_HEIGHT).abs() < 1.0e-5);
        assert!((position_offset - vehicle.up * TOP_DOWN_HEIGHT).length() < 1.0e-5);
        assert!(view_direction.dot(-vehicle.up) > 0.999_99);
        assert!(camera.state.up.dot(vehicle.up).abs() < 1.0e-5);
        assert!(camera.state.up.dot(vehicle.forward) > 0.999_99);

        let nominal_planet_radius = 15.0_f32;
        let angular_diameter = 2.0
            * (nominal_planet_radius / camera.state.position.length())
                .asin()
                .to_degrees();
        let vertical_fill = angular_diameter / camera.state.field_of_view_degrees;
        assert!(
            (0.7..1.0).contains(&vertical_fill),
            "planet should mostly fill without cropping the view; angular fill was {vertical_fill:.3}"
        );
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
        let mut camera = CameraRig::new(vehicle, &settings);
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
        let mut slow = CameraRig::new(vehicle, &settings);
        let mut fast = CameraRig::new(vehicle, &settings);
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
}
