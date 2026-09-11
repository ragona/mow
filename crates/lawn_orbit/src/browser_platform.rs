//! Browser services that egui-winit's desktop platform adapter cannot provide.

use std::{cell::RefCell, rc::Rc};

use anyhow::{Context, Result};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{ClipboardEvent, Event, EventTarget, HtmlCanvasElement, KeyboardEvent};

use crate::input_adapter::BrowserKeyboardPlatform;

/// Keep DOM listeners alive for exactly as long as the game window.
pub struct BrowserPlatform {
    keyboard_platform: BrowserKeyboardPlatform,
    pending_events: Rc<RefCell<Vec<egui::Event>>>,
    listeners: Vec<Listener>,
}

impl std::fmt::Debug for BrowserPlatform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserPlatform")
            .field("keyboard_platform", &self.keyboard_platform)
            .field("pending_events", &self.pending_events)
            .field("listener_count", &self.listeners.len())
            .finish()
    }
}

impl BrowserPlatform {
    pub fn new(canvas: &HtmlCanvasElement) -> Result<Self> {
        let window = web_sys::window().context("browser window is unavailable")?;
        let navigator = window.navigator();
        let keyboard_platform = BrowserKeyboardPlatform::from_browser(
            &navigator.platform().unwrap_or_default(),
            &navigator.user_agent().unwrap_or_default(),
        );
        let document = window
            .document()
            .context("browser document is unavailable")?;
        let pending_events = Rc::new(RefCell::new(Vec::new()));
        let mut listeners = Vec::new();
        for name in ["copy", "cut", "paste"] {
            let document = document.clone();
            let target: EventTarget = document.clone().into();
            let canvas = canvas.clone();
            let pending = pending_events.clone();
            listeners.push(Listener::new(target, name, move |event| {
                // Leave browser controls and any surrounding page text alone.
                if !document
                    .active_element()
                    .is_some_and(|active| active.is_same_node(Some(&canvas)))
                {
                    return;
                }
                let Some(clipboard_event) = event.dyn_ref::<ClipboardEvent>() else {
                    return;
                };
                let input_event = match name {
                    "copy" => egui::Event::Copy,
                    "cut" => egui::Event::Cut,
                    "paste" => {
                        let Some(data) = clipboard_event.clipboard_data() else {
                            return;
                        };
                        let Ok(text) = data.get_data("text/plain") else {
                            return;
                        };
                        if text.is_empty() {
                            return;
                        }
                        egui::Event::Paste(text.replace("\r\n", "\n"))
                    }
                    _ => return,
                };
                pending.borrow_mut().push(input_event);
                // Egui supplies copied text in the next frame's output. The
                // browser's default canvas selection is otherwise empty.
                event.prevent_default();
            })?);
        }
        listeners.push(Listener::new(
            canvas.clone().into(),
            "contextmenu",
            |event| event.prevent_default(),
        )?);
        listeners.push(Listener::new(canvas.clone().into(), "wheel", |event| {
            event.prevent_default();
        })?);
        let focus_canvas = canvas.clone();
        listeners.push(Listener::new(
            canvas.clone().into(),
            "pointerdown",
            move |event| {
                event.prevent_default();
                let _ = focus_canvas.focus();
            },
        )?);
        listeners.push(Listener::new(canvas.clone().into(), "keydown", |event| {
            let Some(key) = event.dyn_ref::<KeyboardEvent>() else {
                return;
            };
            // winit's blanket preventDefault must be disabled on the web
            // window: it suppresses browser paste events. Keep shortcuts
            // available while preventing arrows, Space and Tab from
            // scrolling the page or moving focus away from egui.
            let browser_shortcut =
                key.ctrl_key() || key.meta_key() || (key.shift_key() && key.key() == "Insert");
            if !browser_shortcut {
                event.prevent_default();
            }
        })?);
        Ok(Self {
            keyboard_platform,
            pending_events,
            listeners,
        })
    }

    /// Call immediately after egui-winit processes each window event. Applying
    /// modifiers before the next event preserves their keydown-time value,
    /// even when Command is released again before the next animation frame.
    pub fn normalize_modifiers(
        &self,
        event: &winit::event::WindowEvent,
        input: &mut egui::RawInput,
    ) {
        self.keyboard_platform.normalize_modifiers(event, input);
    }

    pub fn append_input(&mut self, input: &mut egui::RawInput) {
        // The desktop clipboard fallback can contain an old copied string.
        // Only DOM paste events carry the browser's actual clipboard data.
        input
            .events
            .retain(|event| !matches!(event, egui::Event::Paste(_)));
        for event in self.pending_events.borrow_mut().drain(..) {
            // Keyboard shortcuts can also produce copy/cut in egui-winit.
            // Processing cut twice would remove text beyond the selection.
            if matches!(event, egui::Event::Copy | egui::Event::Cut)
                && input.events.contains(&event)
            {
                continue;
            }
            input.events.push(event);
        }
    }

    // Keep all browser output handling behind the platform instance's API.
    #[allow(clippy::unused_self)]
    pub fn handle_output(&self, output: &egui::PlatformOutput) {
        for command in &output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                write_clipboard(text);
            }
        }
    }
}

fn write_clipboard(text: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    // The Clipboard API is optional outside secure contexts. Check the getter
    // before invoking writeText so an unavailable API cannot throw into WASM.
    let Ok(value) = js_sys::Reflect::get(&window.navigator(), &"clipboard".into()) else {
        return;
    };
    if value.is_null() || value.is_undefined() {
        tracing::warn!("browser clipboard API is unavailable; use HTTPS or localhost");
        return;
    }
    let clipboard: web_sys::Clipboard = value.unchecked_into();
    let promise = clipboard.write_text(text);
    spawn_local(async move {
        if let Err(error) = JsFuture::from(promise).await {
            tracing::warn!(?error, "browser refused clipboard write");
        }
    });
}

struct Listener {
    target: EventTarget,
    name: &'static str,
    callback: Closure<dyn FnMut(Event)>,
}

impl Listener {
    fn new(
        target: EventTarget,
        name: &'static str,
        callback: impl FnMut(Event) + 'static,
    ) -> Result<Self> {
        let callback = Closure::wrap(Box::new(callback) as Box<dyn FnMut(Event)>);
        target
            .add_event_listener_with_callback(name, callback.as_ref().unchecked_ref())
            .map_err(|error| anyhow::anyhow!("could not listen for {name}: {error:?}"))?;
        Ok(Self {
            target,
            name,
            callback,
        })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self
            .target
            .remove_event_listener_with_callback(self.name, self.callback.as_ref().unchecked_ref());
    }
}
