//! Platform-independent action mappings and per-tick input snapshots.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Action {
    SteerLeft,
    SteerRight,
    Accelerate,
    BrakeReverse,
    Boost,
    /// Retained only so profiles from the toggle-mower build still deserialize.
    /// The shipping vehicle now mows continuously.
    #[serde(alias = "ToggleFisheye")]
    ToggleMower,
    LookBehind,
    Recover,
    CameraLeft,
    CameraRight,
    CameraUp,
    CameraDown,
    RecenterCamera,
    Pause,
    Confirm,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Binding {
    Key(String),
    MouseButton(u8),
    GamepadButton(String),
    GamepadAxis { axis: String, direction: i8 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlMap {
    pub bindings: BTreeMap<Action, Vec<Binding>>,
}

impl Default for ControlMap {
    fn default() -> Self {
        use Action::{
            Accelerate, Boost, BrakeReverse, CameraDown, CameraLeft, CameraRight, CameraUp,
            LookBehind, Pause, RecenterCamera, Recover, SteerLeft, SteerRight,
        };
        use Binding::{GamepadAxis, GamepadButton, Key};
        let mut bindings = BTreeMap::new();
        bindings.insert(
            SteerLeft,
            vec![
                Key("KeyA".into()),
                Key("ArrowLeft".into()),
                GamepadAxis {
                    axis: "LeftStickX".into(),
                    direction: -1,
                },
            ],
        );
        bindings.insert(
            SteerRight,
            vec![
                Key("KeyD".into()),
                Key("ArrowRight".into()),
                GamepadAxis {
                    axis: "LeftStickX".into(),
                    direction: 1,
                },
            ],
        );
        bindings.insert(
            Accelerate,
            vec![
                Key("KeyW".into()),
                Key("ArrowUp".into()),
                GamepadAxis {
                    axis: "RightZ".into(),
                    direction: 1,
                },
                GamepadAxis {
                    axis: "ButtonRightTrigger2".into(),
                    direction: 1,
                },
            ],
        );
        bindings.insert(
            BrakeReverse,
            vec![
                Key("KeyS".into()),
                Key("ArrowDown".into()),
                GamepadAxis {
                    axis: "LeftZ".into(),
                    direction: 1,
                },
                GamepadAxis {
                    axis: "ButtonLeftTrigger2".into(),
                    direction: 1,
                },
            ],
        );
        bindings.insert(
            Boost,
            vec![Key("Space".into()), GamepadButton("South".into())],
        );
        bindings.insert(
            LookBehind,
            vec![Key("KeyQ".into()), GamepadButton("North".into())],
        );
        bindings.insert(
            Recover,
            vec![Key("KeyR".into()), GamepadButton("East".into())],
        );
        bindings.insert(
            CameraLeft,
            vec![
                Key("KeyJ".into()),
                GamepadAxis {
                    axis: "RightStickX".into(),
                    direction: -1,
                },
            ],
        );
        bindings.insert(
            CameraRight,
            vec![
                Key("KeyL".into()),
                GamepadAxis {
                    axis: "RightStickX".into(),
                    direction: 1,
                },
            ],
        );
        bindings.insert(
            CameraUp,
            vec![
                Key("KeyI".into()),
                GamepadAxis {
                    axis: "RightStickY".into(),
                    direction: 1,
                },
            ],
        );
        bindings.insert(
            CameraDown,
            vec![
                Key("KeyK".into()),
                GamepadAxis {
                    axis: "RightStickY".into(),
                    direction: -1,
                },
            ],
        );
        bindings.insert(
            RecenterCamera,
            vec![Key("KeyC".into()), GamepadButton("RightThumb".into())],
        );
        bindings.insert(
            Pause,
            vec![Key("Escape".into()), GamepadButton("Start".into())],
        );
        Self { bindings }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputSnapshot {
    /// Analog steering in `[-1, 1]`.
    pub steer: f32,
    /// Preserved analog trigger value in `[0, 1]`.
    pub accelerate: f32,
    /// Preserved analog trigger value in `[0, 1]`.
    pub brake_reverse: f32,
    pub camera_orbit: [f32; 2],
    pub boost_held: bool,
    pub look_behind: bool,
    pub recover_held: bool,
    pub recenter_pressed: bool,
    pub pause_pressed: bool,
    pub submit_pressed: bool,
}

impl InputSnapshot {
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.steer = self.steer.clamp(-1.0, 1.0);
        self.accelerate = self.accelerate.clamp(0.0, 1.0);
        self.brake_reverse = self.brake_reverse.clamp(0.0, 1.0);
        self.camera_orbit[0] = self.camera_orbit[0].clamp(-1.0, 1.0);
        self.camera_orbit[1] = self.camera_orbit[1].clamp(-1.0, 1.0);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_fisheye_action_deserializes_to_the_existing_tombstone() {
        let action: Action = ron::from_str("ToggleFisheye").unwrap();
        assert_eq!(action, Action::ToggleMower);
    }
}
