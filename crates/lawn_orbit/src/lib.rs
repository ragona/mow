//! Native and browser entry points for M.O.W. — Mower Of Worlds.

const GAME_TITLE: &str = "M.O.W. — Mower Of Worlds";

mod app;
mod benchmark;
#[cfg(target_arch = "wasm32")]
mod browser_platform;
#[cfg(any(target_arch = "wasm32", test))]
mod generation;
mod input_adapter;
mod profile_store;

/// Starts the desktop application.
///
/// # Errors
/// Returns initialization or event-loop failures.
#[cfg(not(target_arch = "wasm32"))]
pub fn run() -> anyhow::Result<()> {
    use winit::event_loop::{ControlFlow, EventLoop};
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lawn_orbit=info,lawn_render=info,wgpu=warn".into()),
        )
        .init();
    if std::env::args().any(|arg| arg == "--benchmark") {
        return benchmark::run_native();
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = app::LawnOrbitApp::new()?;
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Runs the reproducible renderer benchmark without reading or writing saves.
///
/// # Errors
/// Rejects invalid scenes or failures preparing the world or graphics device.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn benchmark(
    scene: String,
    gpu_timing: bool,
    resolution: String,
) -> Result<(), wasm_bindgen::JsValue> {
    benchmark::run_web(&scene, gpu_timing, &resolution)
        .await
        .map_err(|error| wasm_bindgen::JsValue::from_str(&format!("{error:#}")))
}

#[cfg(target_arch = "wasm32")]
pub use generation::prepare_worker;

/// Loads the game into the page's `lawn-canvas` canvas.
/// Workers initialize the same WASM module without invoking this entry point.
///
/// # Errors
/// Rejects if preparation or graphics initialization fails.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn start() -> Result<(), wasm_bindgen::JsValue> {
    std::panic::set_hook(Box::new(|info| {
        let message = format!("{GAME_TITLE} stopped: {info}");
        web_sys::console::error_1(&message.clone().into());
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            if let Some(status) = document.get_element_by_id("loading-status") {
                status.set_text_content(Some(&message));
            }
            if let Some(loading) = document.get_element_by_id("loading") {
                let _ = loading.remove_attribute("hidden");
            }
        }
    }));
    let _ = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_env_filter("lawn_orbit=info,lawn_render=info,wgpu=warn")
        .with_writer(|| ConsoleWriter(Vec::new()))
        .try_init();
    app::LawnOrbitApp::start_web()
        .await
        .map_err(|error| wasm_bindgen::JsValue::from_str(&format!("{error:#}")))
}

#[cfg(target_arch = "wasm32")]
struct ConsoleWriter(Vec<u8>);

#[cfg(target_arch = "wasm32")]
impl std::io::Write for ConsoleWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for ConsoleWriter {
    fn drop(&mut self) {
        web_sys::console::log_1(&String::from_utf8_lossy(&self.0).as_ref().into());
    }
}

// Bound cosmetic CPU/worker/GPU buffers even on unusually large custom worlds.
const EDITOR_ROOT_BUDGET: usize = 2_000_000;
