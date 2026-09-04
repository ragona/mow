//! Render-only hover motion layered over the authoritative vehicle pose.

use glam::{Mat3, Quat, Vec3};
use lawn_core::vehicle::VehicleTransform;

const CHASSIS_LIFT: f32 = 0.14;
const HOVER_BOB_AMPLITUDE: f32 = 0.035;
const HOVER_BOB_FREQUENCY: f32 = 2.4;
const ATTITUDE_RESPONSE: f32 = 14.0;
const LEAN_RESPONSE: f32 = 7.5;
const LEAN_DAMPING_RATIO: f32 = 0.62;
const LEAN_PER_ACCELERATION: f32 = 0.0045;
const MAX_LEAN: f32 = 0.2;

#[derive(Clone, Debug)]
pub(crate) struct VehiclePresentation {
    base_up: Vec3,
    base_forward: Vec3,
    lean: Vec3,
    lean_velocity: Vec3,
    previous_velocity: Vec3,
}

impl VehiclePresentation {
    pub(crate) fn new(transform: VehicleTransform, velocity: Vec3) -> Self {
        Self {
            base_up: transform.up,
            base_forward: transform.forward,
            lean: Vec3::ZERO,
            lean_velocity: Vec3::ZERO,
            previous_velocity: velocity,
        }
    }

    pub(crate) fn reset(&mut self, transform: VehicleTransform, velocity: Vec3) {
        *self = Self::new(transform, velocity);
    }

    /// Filters the small attitude corrections produced by the physical hover
    /// pads, then adds a soft, under-damped lean driven by acceleration. The
    /// authoritative position and mower deck remain untouched.
    pub(crate) fn update(
        &mut self,
        target: VehicleTransform,
        velocity: Vec3,
        elapsed_seconds: f32,
        dt: f32,
    ) -> VehicleTransform {
        let dt = dt.clamp(1.0 / 240.0, 1.0 / 30.0);
        if self.base_up.dot(target.up) < 0.5 {
            self.reset(target, velocity);
        }

        let attitude_follow = 1.0 - (-ATTITUDE_RESPONSE * dt).exp();
        self.base_up = self
            .base_up
            .lerp(target.up, attitude_follow)
            .normalize_or(target.up);
        let target_forward = (target.forward - self.base_up * target.forward.dot(self.base_up))
            .normalize_or(self.base_forward);
        self.base_forward = self
            .base_forward
            .lerp(target_forward, attitude_follow)
            .reject_from_normalized(self.base_up)
            .normalize_or(target_forward);

        let acceleration = (velocity - self.previous_velocity) / dt;
        self.previous_velocity = velocity;
        let tangent_acceleration = acceleration - target.up * acceleration.dot(target.up);
        let desired_lean = tangent_acceleration * LEAN_PER_ACCELERATION;
        let desired_lean = desired_lean.clamp_length_max(MAX_LEAN);
        let lean_acceleration = (desired_lean - self.lean) * LEAN_RESPONSE * LEAN_RESPONSE
            - self.lean_velocity * (2.0 * LEAN_DAMPING_RATIO * LEAN_RESPONSE);
        self.lean_velocity += lean_acceleration * dt;
        self.lean += self.lean_velocity * dt;
        self.lean -= self.base_up * self.lean.dot(self.base_up);
        self.lean = self.lean.clamp_length_max(MAX_LEAN);

        // Tilting local up opposite acceleration makes the deck plane lean into
        // thrust, like a helicopter disc, while the spring supplies overshoot.
        let up = (self.base_up - self.lean).normalize_or(self.base_up);
        let forward = (self.base_forward - up * self.base_forward.dot(up))
            .try_normalize()
            .unwrap_or_else(|| up.any_orthonormal_vector());
        let right = forward.cross(up).normalize();
        let rotation = Quat::from_mat3(&Mat3::from_cols(right, up, -forward));
        let hover_bob = (elapsed_seconds * HOVER_BOB_FREQUENCY).sin() * HOVER_BOB_AMPLITUDE;

        VehicleTransform {
            position: target.position + target.up * (CHASSIS_LIFT + hover_bob),
            rotation,
            forward,
            up,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level_transform() -> VehicleTransform {
        VehicleTransform {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            forward: Vec3::NEG_Z,
            up: Vec3::Y,
        }
    }

    #[test]
    fn presentation_lifts_and_leans_without_moving_authoritative_pose() {
        let target = level_transform();
        let mut presentation = VehiclePresentation::new(target, Vec3::ZERO);
        let visual = presentation.update(target, Vec3::Z * 2.0, 0.0, 1.0 / 60.0);

        assert_eq!(target.position, Vec3::ZERO);
        assert!(visual.position.y >= CHASSIS_LIFT - HOVER_BOB_AMPLITUDE);
        assert!(visual.up.z < 0.0);
        assert!(visual.up.is_normalized());
        assert!(visual.forward.is_normalized());
    }

    #[test]
    fn presentation_lean_settles_after_acceleration_ends() {
        let target = level_transform();
        let mut presentation = VehiclePresentation::new(target, Vec3::ZERO);
        let mut visual = presentation.update(target, Vec3::X * 4.0, 0.0, 1.0 / 120.0);
        assert!(visual.up.x < 0.0);

        for step in 1..480 {
            visual = presentation.update(target, Vec3::X * 4.0, step as f32 / 120.0, 1.0 / 120.0);
        }
        assert!(visual.up.dot(Vec3::Y) > 0.999_9);
    }
}
