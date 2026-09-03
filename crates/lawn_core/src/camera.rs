//! Local-up chase camera without geographic poles.

use glam::Vec3;

use crate::{planet::Planet, profile::AccessibilitySettings, vehicle::VehicleTransform};

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
            position: vehicle.position - vehicle.forward * 7.0 + vehicle.up * 4.2,
            target: vehicle.position + vehicle.forward * 2.0,
            up: vehicle.up,
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
        planet: &Planet,
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
        let velocity_forward = (velocity - local_up * velocity.dot(local_up)).try_normalize();
        let favored_forward = velocity_forward.map_or(vehicle.forward, |direction| {
            vehicle.forward.lerp(direction, 0.55).normalize()
        });
        let side = favored_forward.cross(local_up).normalize();
        let behind_sign = if look_behind { 1.0 } else { -1.0 };
        let desired = vehicle.position
            + favored_forward * (7.4 * behind_sign + self.orbit_yaw.sin() * 4.0)
            + side * (self.orbit_yaw.sin() * 5.0)
            + local_up * (4.1 + self.orbit_pitch * 3.0);
        let target = vehicle.position + favored_forward * if look_behind { -2.0 } else { 2.0 };

        // Resolve terrain obstruction by sampling the desired boom and selecting
        // the furthest unobstructed point toward the vehicle.
        let mut unobstructed = desired;
        for index in (1..=12).rev() {
            let t = index as f32 / 12.0;
            let point = target.lerp(desired, t);
            let direction = point.normalize();
            let terrain = planet.terrain_cell(direction);
            if point.length() >= terrain.radius + 0.35 {
                unobstructed = point;
                break;
            }
        }
        let speed_shake = (velocity.length() / 17.0).clamp(0.0, 1.0);
        let shake_amplitude = settings.camera_shake * speed_shake * 0.055;
        unobstructed += side * (self.elapsed_seconds * 31.0).sin() * shake_amplitude
            + local_up * (self.elapsed_seconds * 43.0).cos() * shake_amplitude * 0.55;
        let stiffness = settings.camera_follow_stiffness;
        let follow = 1.0 - (-stiffness * dt).exp();
        self.state.position = self.state.position.lerp(unobstructed, follow);
        self.state.target = self.state.target.lerp(target, follow);
        let desired_up = if settings.fixed_horizon {
            // Use a projected global reference where stable, gracefully falling
            // back to local up near its singularity.
            let projected = Vec3::Y - favored_forward * Vec3::Y.dot(favored_forward);
            projected.try_normalize().unwrap_or(local_up)
        } else {
            local_up
        };
        self.state.up = self.state.up.lerp(desired_up, follow).normalize();
        self.state.field_of_view_degrees = settings.field_of_view_degrees;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION};

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
    }
}
