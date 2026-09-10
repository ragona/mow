use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use gilrs::{
    Button, EventType, Gilrs,
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

#[derive(Default, Debug)]
struct GamepadInput {
    buttons: HashSet<String>,
    axes: HashMap<String, f32>,
    navigation_axis_latched: [bool; 2],
}

pub struct InputAdapter {
    keys: HashSet<String>,
    gamepads: HashMap<usize, GamepadInput>,
    focused: bool,
    right_mouse: bool,
    last_cursor: Option<(f64, f64)>,
    mouse_delta: [f32; 2],
    recenter: bool,
    pause: bool,
    submit: bool,
    menu_cancel: bool,
    menu_keys: Vec<egui::Key>,
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
            .field("gamepads", &self.gamepads)
            .field("rebind_action", &self.rebind_action)
            .finish_non_exhaustive()
    }
}

impl Default for InputAdapter {
    fn default() -> Self {
        Self {
            keys: HashSet::new(),
            gamepads: HashMap::new(),
            focused: true,
            right_mouse: false,
            last_cursor: None,
            mouse_delta: [0.0; 2],
            recenter: false,
            pause: false,
            submit: false,
            menu_cancel: false,
            menu_keys: Vec::new(),
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
        if let WindowEvent::Focused(focused) = event {
            self.focused = *focused;
            if !focused {
                self.clear();
            }
            return;
        }
        if !self.focused {
            return;
        }
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let name = format!("{code:?}");
                if event.state == ElementState::Pressed {
                    if event.repeat && self.rebind_action.is_some() {
                        return;
                    }
                    if let Some(action) = self.rebind_action.take() {
                        self.keys.insert(name.clone());
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
                let before = self.edge_actions(controls);
                self.right_mouse = *state == ElementState::Pressed;
                self.last_cursor = None;
                self.capture_edges(controls, before);
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
            _ => {}
        }
    }

    pub fn poll_gamepads(&mut self, controls: &mut ControlMap) {
        while let Some(event) = self.gilrs.as_mut().and_then(Gilrs::next_event) {
            let id = usize::from(event.id);
            if matches!(event.event, EventType::Disconnected) {
                self.gamepads.remove(&id);
                continue;
            }
            if !self.focused {
                continue;
            }
            match event.event {
                EventType::ButtonPressed(button, _) => {
                    self.gamepad_button(id, button, true, controls);
                }
                EventType::ButtonReleased(button, _) => {
                    self.gamepad_button(id, button, false, controls);
                }
                EventType::AxisChanged(axis, value, _) => {
                    self.gamepad_axis(id, format!("{axis:?}"), value, controls);
                }
                EventType::ButtonChanged(button, value, _) => {
                    self.gamepad_axis(id, format!("Button{button:?}"), value, controls);
                }
                _ => {}
            }
        }
    }

    fn gamepad_button(
        &mut self,
        id: usize,
        button: Button,
        pressed: bool,
        controls: &mut ControlMap,
    ) {
        let name = format!("{button:?}");
        if pressed && let Some(action) = self.rebind_action.take() {
            self.gamepads
                .entry(id)
                .or_default()
                .buttons
                .insert(name.clone());
            controls
                .bindings
                .insert(action, vec![Binding::GamepadButton(name)]);
            return;
        }
        let before = self.edge_actions(controls);
        let state = self.gamepads.entry(id).or_default();
        if pressed {
            if state.buttons.insert(name) {
                self.last_device_label = "Gamepad";
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
        } else {
            state.buttons.remove(&name);
        }
        self.capture_edges(controls, before);
    }

    fn gamepad_axis(&mut self, id: usize, name: String, value: f32, controls: &mut ControlMap) {
        let value = if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        if let Some(action) = self.rebind_action
            && value.abs() > 0.65
        {
            self.gamepads
                .entry(id)
                .or_default()
                .axes
                .insert(name.clone(), value);
            controls.bindings.insert(
                action,
                vec![Binding::GamepadAxis {
                    axis: name,
                    direction: if value >= 0.0 { 1 } else { -1 },
                }],
            );
            self.rebind_action = None;
            return;
        }
        let before = self.edge_actions(controls);
        let state = self.gamepads.entry(id).or_default();
        state.axes.insert(name.clone(), value);
        if value.abs() > 0.35 {
            self.last_device_label = "Gamepad";
        }
        let navigation = match name.as_str() {
            "LeftStickX" => Some((0, egui::Key::ArrowRight, egui::Key::ArrowLeft)),
            "LeftStickY" => Some((1, egui::Key::ArrowUp, egui::Key::ArrowDown)),
            _ => None,
        };
        if self.rebind_action.is_none()
            && let Some((axis, positive, negative)) = navigation
        {
            if value.abs() > 0.72 && !state.navigation_axis_latched[axis] {
                self.menu_keys
                    .push(if value > 0.0 { positive } else { negative });
                state.navigation_axis_latched[axis] = true;
            } else if value.abs() < 0.35 {
                state.navigation_axis_latched[axis] = false;
            }
        }
        self.capture_edges(controls, before);
    }

    fn edge_actions(&self, controls: &ControlMap) -> [bool; 2] {
        [
            self.action_held(controls, Action::Pause),
            self.action_held(controls, Action::RecenterCamera),
        ]
    }

    fn capture_edges(&mut self, controls: &ControlMap, before: [bool; 2]) {
        let after = self.edge_actions(controls);
        self.pause |= after[0] && !before[0];
        self.recenter |= after[1] && !before[1];
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn clear_transient(&mut self) {
        self.mouse_delta = [0.0; 2];
        self.recenter = false;
        self.pause = false;
        self.submit = false;
        self.menu_cancel = false;
        self.menu_keys.clear();
    }

    pub fn clear(&mut self) {
        self.keys.clear();
        self.gamepads.clear();
        self.right_mouse = false;
        self.last_cursor = None;
        self.clear_transient();
    }

    pub fn discard_menu_events(&mut self) {
        self.menu_keys.clear();
        self.menu_cancel = false;
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
            // Menu navigation is a pulse. A press without its release leaves
            // egui believing Enter/arrows are held after gameplay resumes.
            for pressed in [true, false] {
                input.events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
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
                RunEvent::MowerBump { impulse, .. } => {
                    strength = strength.max((impulse / 18.0).clamp(0.25, 0.6));
                    duration_ms = duration_ms.max(95);
                }
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
                    if self
                        .gamepads
                        .values()
                        .any(|state| state.buttons.contains(name))
                    {
                        1.0
                    } else {
                        0.0
                    }
                }
                Binding::GamepadAxis { axis, direction } => self
                    .gamepads
                    .values()
                    .filter_map(|state| state.axes.get(axis))
                    .map(|value| (*value * f32::from(*direction)).clamp(0.0, 1.0))
                    .fold(0.0, f32::max),
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
    use gilrs::Axis;

    #[test]
    fn focus_loss_clears_drag_buttons_and_queued_actions() {
        let mut input = InputAdapter::default();
        input.keys.insert("KeyW".into());
        input.right_mouse = true;
        input.last_cursor = Some((2.0, 3.0));
        input.mouse_delta = [0.2, 0.3];
        input.recenter = true;
        input.pause = true;
        input.submit = true;
        input.menu_keys.push(egui::Key::Enter);
        input.menu_cancel = true;
        input.handle_window_event(&WindowEvent::Focused(false), &mut ControlMap::default());
        assert_eq!(
            input.snapshot(&ControlMap::default(), &AccessibilitySettings::default()),
            InputSnapshot::default()
        );
        assert!(!input.right_mouse);
        assert!(input.last_cursor.is_none());
        assert!(input.menu_keys.is_empty());
        assert!(!input.take_menu_cancel());
    }

    #[test]
    fn gamepad_axis_rebinding_supports_pressed_actions() {
        let mut input = InputAdapter::default();
        let mut controls = ControlMap::default();
        input.rebind_action = Some(Action::Pause);
        input.gamepad_axis(0, "LeftStickX".into(), 0.8, &mut controls);
        assert!(
            !input.take_pause_pressed(),
            "binding an action must not activate it"
        );
        assert!(input.menu_keys.is_empty());
        input.gamepad_axis(0, "LeftStickX".into(), 0.0, &mut controls);
        input.gamepad_axis(0, "LeftStickX".into(), 0.8, &mut controls);
        assert!(input.take_pause_pressed());
        input.gamepad_axis(0, "LeftStickX".into(), 0.9, &mut controls);
        assert!(
            !input.take_pause_pressed(),
            "holding an axis must not toggle repeatedly"
        );
    }

    #[test]
    fn controller_releases_do_not_cancel_another_controllers_buttons() {
        let mut input = InputAdapter::default();
        let mut controls = ControlMap::default();
        input.gamepad_button(0, Button::South, true, &mut controls);
        input.gamepad_button(1, Button::South, true, &mut controls);
        input.gamepad_button(1, Button::South, false, &mut controls);
        assert!(input.action_held(&controls, Action::Boost));
        input.gamepads.remove(&0);
        assert!(!input.action_held(&controls, Action::Boost));
    }

    #[test]
    fn gamepad_menu_pulses_release_the_egui_key() {
        let mut input = InputAdapter::default();
        input.menu_keys.push(egui::Key::Enter);
        let mut raw_input = egui::RawInput::default();
        input.append_egui_gamepad_events(&mut raw_input);
        let context = egui::Context::default();
        let _ = context.run_ui(raw_input, |ui| {
            ui.input(|state| {
                assert!(state.key_pressed(egui::Key::Enter));
                assert!(!state.key_down(egui::Key::Enter));
            });
        });
    }

    #[test]
    fn analog_movement_values_are_not_quantized() {
        let mut input = InputAdapter::default();
        input
            .gamepads
            .entry(0)
            .or_default()
            .axes
            .insert(format!("Button{:?}", Button::RightTrigger2), 0.37);
        let snapshot = input.snapshot(&ControlMap::default(), &AccessibilitySettings::default());
        assert!((snapshot.accelerate - 0.37).abs() < 1.0e-6);
    }

    #[test]
    fn left_stick_supplies_both_movement_axes() {
        let mut input = InputAdapter::default();
        input
            .gamepads
            .entry(0)
            .or_default()
            .axes
            .insert(format!("{:?}", Axis::LeftStickX), 0.6);
        input
            .gamepads
            .entry(0)
            .or_default()
            .axes
            .insert(format!("{:?}", Axis::LeftStickY), -0.8);
        let snapshot = input.snapshot(&ControlMap::default(), &AccessibilitySettings::default());
        assert!((snapshot.steer - 0.6).abs() < 1.0e-6);
        assert!((snapshot.brake_reverse - 0.8).abs() < 1.0e-6);
    }

    #[test]
    fn replacing_a_stick_binding_removes_the_default_stick_path() {
        let mut input = InputAdapter::default();
        input
            .gamepads
            .entry(0)
            .or_default()
            .axes
            .insert(format!("{:?}", Axis::LeftStickX), 0.8);
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
    fn horizontal_inversion_applies_after_analog_mapping() {
        let mut input = InputAdapter::default();
        input
            .gamepads
            .entry(0)
            .or_default()
            .axes
            .insert(format!("{:?}", Axis::LeftStickX), 0.6);
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
