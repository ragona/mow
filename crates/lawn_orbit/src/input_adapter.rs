use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use gilrs::{
    Axis, Button, EventType, Gilrs,
    ff::{BaseEffect, BaseEffectType, Effect, EffectBuilder, Envelope, Repeat, Replay, Ticks},
};
use lawn_core::{
    input::{Action, Binding, ControlMap, InputSnapshot},
    profile::AccessibilitySettings,
    run::{RunEvent, RunState},
};
use winit::{
    event::{ElementState, MouseButton, WindowEvent},
    keyboard::{KeyCode, PhysicalKey},
};

pub struct InputAdapter {
    keys: HashSet<String>,
    gamepad_buttons: HashSet<String>,
    axes: HashMap<String, f32>,
    right_mouse: bool,
    last_cursor: Option<(f64, f64)>,
    mouse_delta: [f32; 2],
    recenter: bool,
    pause: bool,
    submit: bool,
    menu_cancel: bool,
    menu_keys: Vec<egui::Key>,
    navigation_axis_latched: [bool; 2],
    rumble_effects: VecDeque<Effect>,
    last_cut_rumble: Instant,
    pub gilrs: Option<Gilrs>,
    pub rebind_action: Option<Action>,
    pub last_device_label: &'static str,
}

impl std::fmt::Debug for InputAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputAdapter")
            .field("keys", &self.keys)
            .field("gamepad_buttons", &self.gamepad_buttons)
            .field("rebind_action", &self.rebind_action)
            .finish_non_exhaustive()
    }
}

impl Default for InputAdapter {
    fn default() -> Self {
        Self {
            keys: HashSet::new(),
            gamepad_buttons: HashSet::new(),
            axes: HashMap::new(),
            right_mouse: false,
            last_cursor: None,
            mouse_delta: [0.0; 2],
            recenter: false,
            pause: false,
            submit: false,
            menu_cancel: false,
            menu_keys: Vec::new(),
            navigation_axis_latched: [false; 2],
            rumble_effects: VecDeque::with_capacity(8),
            last_cut_rumble: Instant::now(),
            gilrs: Gilrs::new().ok(),
            rebind_action: None,
            last_device_label: "Keyboard",
        }
    }
}

impl InputAdapter {
    pub fn handle_window_event(&mut self, event: &WindowEvent, controls: &mut ControlMap) {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let name = format!("{code:?}");
                if event.state == ElementState::Pressed {
                    if let Some(action) = self.rebind_action.take() {
                        controls.bindings.insert(action, vec![Binding::Key(name)]);
                        return;
                    }
                    let first_press = self.keys.insert(name.clone());
                    if first_press {
                        self.recenter |= matches_binding(
                            controls,
                            Action::RecenterCamera,
                            &Binding::Key(name.clone()),
                        );
                        self.pause |=
                            matches_binding(controls, Action::Pause, &Binding::Key(name.clone()));
                        self.submit |= code == KeyCode::Enter;
                    }
                    self.last_device_label = "Keyboard";
                } else {
                    self.keys.remove(&name);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Right,
                ..
            } => {
                self.right_mouse = *state == ElementState::Pressed;
                self.last_cursor = None;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let current = (position.x, position.y);
                if self.right_mouse
                    && let Some(previous) = self.last_cursor
                {
                    self.mouse_delta[0] += ((current.0 - previous.0) * 0.012) as f32;
                    self.mouse_delta[1] += ((current.1 - previous.1) * 0.012) as f32;
                }
                self.last_cursor = Some(current);
            }
            WindowEvent::Focused(false) => {
                self.keys.clear();
                self.gamepad_buttons.clear();
                self.axes.clear();
            }
            _ => {}
        }
    }

    pub fn poll_gamepads(&mut self, controls: &mut ControlMap) {
        let Some(gilrs) = &mut self.gilrs else { return };
        while let Some(event) = gilrs.next_event() {
            self.last_device_label = "Gamepad";
            match event.event {
                EventType::ButtonPressed(button, _) => {
                    let name = format!("{button:?}");
                    if let Some(action) = self.rebind_action.take() {
                        controls
                            .bindings
                            .insert(action, vec![Binding::GamepadButton(name)]);
                    } else {
                        self.gamepad_buttons.insert(name.clone());
                        self.recenter |= matches_binding(
                            controls,
                            Action::RecenterCamera,
                            &Binding::GamepadButton(name.clone()),
                        );
                        self.pause |=
                            matches_binding(controls, Action::Pause, &Binding::GamepadButton(name));
                        match button {
                            Button::DPadUp => self.menu_keys.push(egui::Key::ArrowUp),
                            Button::DPadDown => self.menu_keys.push(egui::Key::ArrowDown),
                            Button::DPadLeft => self.menu_keys.push(egui::Key::ArrowLeft),
                            Button::DPadRight => self.menu_keys.push(egui::Key::ArrowRight),
                            Button::South => self.menu_keys.push(egui::Key::Enter),
                            Button::East => self.menu_cancel = true,
                            _ => {}
                        }
                    }
                }
                EventType::ButtonReleased(button, _) => {
                    self.gamepad_buttons.remove(&format!("{button:?}"));
                }
                EventType::AxisChanged(axis, value, _) => {
                    let name = format!("{axis:?}");
                    self.axes.insert(name.clone(), value);
                    match axis {
                        Axis::LeftStickX => {
                            if value.abs() > 0.72 && !self.navigation_axis_latched[0] {
                                self.menu_keys.push(if value > 0.0 {
                                    egui::Key::ArrowRight
                                } else {
                                    egui::Key::ArrowLeft
                                });
                                self.navigation_axis_latched[0] = true;
                            } else if value.abs() < 0.35 {
                                self.navigation_axis_latched[0] = false;
                            }
                        }
                        Axis::LeftStickY => {
                            if value.abs() > 0.72 && !self.navigation_axis_latched[1] {
                                self.menu_keys.push(if value > 0.0 {
                                    egui::Key::ArrowUp
                                } else {
                                    egui::Key::ArrowDown
                                });
                                self.navigation_axis_latched[1] = true;
                            } else if value.abs() < 0.35 {
                                self.navigation_axis_latched[1] = false;
                            }
                        }
                        _ => {}
                    }
                    if let Some(action) = self.rebind_action
                        && value.abs() > 0.65
                    {
                        controls.bindings.insert(
                            action,
                            vec![Binding::GamepadAxis {
                                axis: name,
                                direction: if value >= 0.0 { 1 } else { -1 },
                            }],
                        );
                        self.rebind_action = None;
                    }
                }
                EventType::ButtonChanged(button, value, _) => {
                    let axis = format!("Button{button:?}");
                    self.axes.insert(axis.clone(), value);
                    if let Some(action) = self.rebind_action
                        && value > 0.65
                    {
                        controls
                            .bindings
                            .insert(action, vec![Binding::GamepadAxis { axis, direction: 1 }]);
                        self.rebind_action = None;
                    }
                }
                _ => {}
            }
        }
    }

    pub fn snapshot(
        &mut self,
        controls: &ControlMap,
        accessibility: &AccessibilitySettings,
    ) -> InputSnapshot {
        let steer = self.action_value(controls, Action::SteerRight)
            - self.action_value(controls, Action::SteerLeft);
        let camera = [
            self.action_value(controls, Action::CameraRight)
                - self.action_value(controls, Action::CameraLeft),
            self.action_value(controls, Action::CameraDown)
                - self.action_value(controls, Action::CameraUp),
        ];
        let result = InputSnapshot {
            steer: steer
                * accessibility.steering_sensitivity
                * if accessibility.invert_steering {
                    -1.0
                } else {
                    1.0
                },
            accelerate: self.action_value(controls, Action::Accelerate),
            brake_reverse: self.action_value(controls, Action::BrakeReverse),
            camera_orbit: [
                (camera[0] + self.mouse_delta[0]).clamp(-1.0, 1.0),
                (camera[1] + self.mouse_delta[1]).clamp(-1.0, 1.0),
            ],
            boost_held: self.action_held(controls, Action::Boost),
            look_behind: self.action_held(controls, Action::LookBehind),
            recover_held: self.action_held(controls, Action::Recover),
            recenter_pressed: std::mem::take(&mut self.recenter),
            pause_pressed: std::mem::take(&mut self.pause),
            submit_pressed: std::mem::take(&mut self.submit),
        };
        self.mouse_delta = [0.0; 2];
        result.sanitized()
    }

    pub fn append_egui_gamepad_events(&mut self, input: &mut egui::RawInput) {
        for key in self.menu_keys.drain(..) {
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }

    pub fn take_menu_cancel(&mut self) -> bool {
        std::mem::take(&mut self.menu_cancel)
    }

    pub fn take_pause_pressed(&mut self) -> bool {
        std::mem::take(&mut self.pause)
    }

    pub fn update_feedback(&mut self, run: &RunState) {
        let mut strength = 0.0_f32;
        let mut duration_ms = 0_u32;
        for event in run.events() {
            match event {
                RunEvent::SubstantialCollision { impulse } => {
                    strength = strength.max((impulse / 12.0).clamp(0.35, 1.0));
                    duration_ms = duration_ms.max(170);
                }
                RunEvent::RockScrape => {
                    strength = strength.max(0.24);
                    duration_ms = duration_ms.max(75);
                }
                RunEvent::GrassCut { .. }
                    if self.last_cut_rumble.elapsed() >= Duration::from_millis(110) =>
                {
                    strength = strength.max(0.075);
                    duration_ms = duration_ms.max(90);
                    self.last_cut_rumble = Instant::now();
                }
                _ => {}
            }
        }
        if duration_ms > 0 {
            self.play_rumble(strength, duration_ms);
        }
    }

    fn play_rumble(&mut self, strength: f32, duration_ms: u32) {
        let Some(gilrs) = &mut self.gilrs else { return };
        let gamepads: Vec<_> = gilrs
            .gamepads()
            .filter_map(|(id, gamepad)| gamepad.is_ff_supported().then_some(id))
            .collect();
        if gamepads.is_empty() {
            return;
        }
        let duration = Ticks::from_ms(duration_ms);
        let magnitude = (strength.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16;
        let mut builder = EffectBuilder::new();
        builder
            .add_effect(BaseEffect {
                kind: BaseEffectType::Strong { magnitude },
                scheduling: Replay {
                    play_for: duration,
                    ..Replay::default()
                },
                envelope: Envelope::default(),
            })
            .add_effect(BaseEffect {
                kind: BaseEffectType::Weak {
                    magnitude: magnitude / 2,
                },
                scheduling: Replay {
                    play_for: duration,
                    ..Replay::default()
                },
                envelope: Envelope::default(),
            })
            .gamepads(&gamepads)
            .repeat(Repeat::For(duration));
        if let Ok(effect) = builder.finish(gilrs) {
            let _ = effect.play();
            if self.rumble_effects.len() == 8 {
                self.rumble_effects.pop_front();
            }
            self.rumble_effects.push_back(effect);
        }
    }

    fn action_held(&self, controls: &ControlMap, action: Action) -> bool {
        self.action_value(controls, action) > 0.35
    }

    fn action_value(&self, controls: &ControlMap, action: Action) -> f32 {
        controls
            .bindings
            .get(&action)
            .into_iter()
            .flatten()
            .map(|binding| match binding {
                Binding::Key(name) => {
                    if self.keys.contains(name) {
                        1.0
                    } else {
                        0.0
                    }
                }
                Binding::GamepadButton(name) => {
                    if self.gamepad_buttons.contains(name) {
                        1.0
                    } else {
                        0.0
                    }
                }
                Binding::GamepadAxis { axis, direction } => self
                    .axes
                    .get(axis)
                    .map_or(0.0, |value| (*value * *direction as f32).clamp(0.0, 1.0)),
                Binding::MouseButton(button) => {
                    if *button == 2 && self.right_mouse {
                        1.0
                    } else {
                        0.0
                    }
                }
            })
            .fold(0.0, f32::max)
    }
}

fn matches_binding(controls: &ControlMap, action: Action, candidate: &Binding) -> bool {
    controls
        .bindings
        .get(&action)
        .is_some_and(|bindings| bindings.contains(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analog_trigger_values_are_not_quantized() {
        let mut input = InputAdapter::default();
        input
            .axes
            .insert(format!("Button{:?}", Button::RightTrigger2), 0.37);
        let snapshot = input.snapshot(&ControlMap::default(), &AccessibilitySettings::default());
        assert!((snapshot.accelerate - 0.37).abs() < 1.0e-6);
    }

    #[test]
    fn replacing_a_stick_binding_removes_the_default_stick_path() {
        let mut input = InputAdapter::default();
        input.axes.insert(format!("{:?}", Axis::LeftStickX), 0.8);
        let mut controls = ControlMap::default();
        controls
            .bindings
            .insert(Action::SteerRight, vec![Binding::Key("KeyD".into())]);
        controls
            .bindings
            .insert(Action::SteerLeft, vec![Binding::Key("KeyA".into())]);

        let snapshot = input.snapshot(&controls, &AccessibilitySettings::default());
        assert_eq!(snapshot.steer, 0.0);
    }

    #[test]
    fn steering_inversion_applies_after_analog_mapping() {
        let mut input = InputAdapter::default();
        input.axes.insert(format!("{:?}", Axis::LeftStickX), 0.6);
        let accessibility = AccessibilitySettings {
            invert_steering: true,
            ..AccessibilitySettings::default()
        };
        let snapshot = input.snapshot(&ControlMap::default(), &accessibility);
        assert!((snapshot.steer + 0.6).abs() < 1.0e-6);
    }

    #[test]
    fn keyboard_camera_bindings_are_independent_actions() {
        let mut input = InputAdapter::default();
        let controls = ControlMap::default();
        input.keys.insert("KeyJ".into());
        input.keys.insert("KeyI".into());
        let snapshot = input.snapshot(&controls, &AccessibilitySettings::default());
        assert_eq!(snapshot.camera_orbit, [-1.0, -1.0]);
    }
}
