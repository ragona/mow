//! A shared native/browser renderer workload, independent of user saves and DPI.

use std::sync::{Arc, Mutex};

#[cfg(target_arch = "wasm32")]
use anyhow::Context;
use anyhow::Result;
use lawn_core::{
    FIXED_DT,
    config::{GameConfig, GeneratorConfig},
    input::InputSnapshot,
    planet::WorldSeed,
    profile::{AccessibilitySettings, QualityPreset},
    run::{GameMode, PreparedWorld, RunState},
};
use lawn_render::{FrameStats, PreparedSky, Renderer};
use serde_json::json;
use web_time::Instant;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

const WARMUP: usize = 90;
const SAMPLES: usize = 360;
const SEED: WorldSeed = WorldSeed(42);

fn resolution(value: &str) -> Result<PhysicalSize<u32>> {
    match value {
        "720p" => Ok(PhysicalSize::new(1280, 720)),
        "1440p" => Ok(PhysicalSize::new(2560, 1440)),
        _ => anyhow::bail!("benchmark resolution must be 720p or 1440p"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scene {
    Stationary,
    Race,
}

impl Scene {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "stationary" => Ok(Self::Stationary),
            "race" => Ok(Self::Race),
            _ => anyhow::bail!("benchmark scene must be stationary or race"),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Stationary => "stationary",
            Self::Race => "race",
        }
    }
}

fn generator_config() -> Result<GeneratorConfig> {
    Ok(GeneratorConfig {
        base_radius: 17.0,
        mountain_count_min: 6,
        mountain_count_max: 10,
        mountain_height_min: 4.95,
        mountain_height_max: 7.45,
        rolling_amplitude: 1.0,
        mowable_ratio_min: 0.75,
        mowable_ratio_max: 0.85,
        mountain_separation_radians: 0.46,
        maximum_generation_attempts: 16,
        ..GameConfig::shipping()?.generator
    })
}

/// Callbacks may run during a poll or on another thread. Keep the first failure
/// latched so a later queue-completion callback cannot make the run valid again.
#[derive(Clone, Debug, Default)]
struct GpuFailure(Arc<Mutex<Option<String>>>);

impl GpuFailure {
    fn record(&self, message: String) {
        self.0
            .lock()
            .expect("GPU failure lock")
            .get_or_insert(message);
    }

    fn message(&self) -> Option<String> {
        self.0.lock().expect("GPU failure lock").clone()
    }
}

#[derive(Debug)]
struct BenchmarkApp {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    run: RunState,
    scene: Scene,
    gpu_timing: bool,
    size: PhysicalSize<u32>,
    frame: usize,
    frame_prepared: bool,
    startup: Instant,
    #[cfg(target_arch = "wasm32")]
    redraw_pending: bool,
    last_frame: Instant,
    intervals: Vec<f64>,
    cpu_work: Vec<f64>,
    simulation: Vec<f64>,
    encode: Vec<f64>,
    gpu: Vec<f64>,
    final_stats: FrameStats,
    completion: Arc<Mutex<Option<Instant>>>,
    gpu_failure: GpuFailure,
    waiting_for_gpu: bool,
    measurement_started: Option<Instant>,
    gpu_completed_ms: Option<f64>,
    done: bool,
    failure: Option<String>,
}

impl BenchmarkApp {
    fn new(
        world: PreparedWorld,
        scene: Scene,
        gpu_timing: bool,
        size: PhysicalSize<u32>,
    ) -> Result<Self> {
        let config = GameConfig::shipping()?;
        let accessibility = AccessibilitySettings::default();
        let mut run = world.into_run(
            if scene == Scene::Race {
                GameMode::TurfRace
            } else {
                GameMode::FreeMow
            },
            config.vehicle,
            config.job,
            &accessibility,
            false,
        );
        for _ in 0..120 {
            run.tick(InputSnapshot::default(), &accessibility);
        }
        run.clear_frame_events();
        run.paused = scene == Scene::Stationary;
        Ok(Self {
            window: None,
            renderer: None,
            run,
            scene,
            gpu_timing,
            size,
            frame: 0,
            frame_prepared: false,
            startup: Instant::now(),
            #[cfg(target_arch = "wasm32")]
            redraw_pending: false,
            last_frame: Instant::now(),
            intervals: Vec::with_capacity(SAMPLES),
            cpu_work: Vec::with_capacity(SAMPLES),
            simulation: Vec::with_capacity(SAMPLES),
            encode: Vec::with_capacity(SAMPLES),
            gpu: Vec::with_capacity(SAMPLES),
            final_stats: FrameStats::default(),
            completion: Arc::new(Mutex::new(None)),
            gpu_failure: GpuFailure::default(),
            waiting_for_gpu: false,
            measurement_started: None,
            gpu_completed_ms: None,
            done: false,
            failure: None,
        })
    }

    async fn initialize(&mut self, window: Arc<Window>, sky: Option<&PreparedSky>) -> Result<()> {
        let mut renderer = Renderer::new_with_sky(
            Arc::clone(&window),
            &self.run,
            4,
            QualityPreset::High,
            1.0,
            false,
            false,
            1.0,
            sky,
        )
        .await?;
        let gpu_failure = self.gpu_failure.clone();
        renderer
            .device()
            .set_device_lost_callback(move |reason, message| {
                gpu_failure.record(format!("benchmark GPU device lost ({reason:?}): {message}"));
            });
        let gpu_failure = self.gpu_failure.clone();
        renderer
            .device()
            .on_uncaptured_error(Arc::new(move |error: egui_wgpu::wgpu::Error| {
                gpu_failure.record(format!("benchmark GPU error: {error}"));
            }));
        renderer.resize(self.size);
        renderer.set_gpu_profiling(self.gpu_timing);
        #[cfg(not(target_arch = "wasm32"))]
        window.focus_window();
        self.renderer = Some(renderer);
        self.window = Some(window);
        self.last_frame = Instant::now();
        self.startup = Instant::now();
        Ok(())
    }

    fn render(&mut self, event_loop: &ActiveEventLoop) {
        if self.done {
            return;
        }
        if self.reject_gpu_failure(event_loop) {
            return;
        }
        if self.waiting_for_gpu {
            let renderer = self
                .renderer
                .as_ref()
                .expect("benchmark renderer initialized");
            let poll_result = renderer.device().poll(egui_wgpu::wgpu::PollType::Poll);
            // Poll can deliver both the completion callback and a device error.
            // Reject the error before accepting completion as a valid result.
            if self.reject_gpu_failure(event_loop) {
                return;
            }
            if let Err(error) = poll_result {
                self.fail(
                    &format!("benchmark GPU polling failed: {error}"),
                    event_loop,
                );
                return;
            }
            let completed = self.completion.lock().expect("completion lock").take();
            let Some(completed) = completed else {
                if self.last_frame.elapsed().as_secs() >= 60 {
                    self.fail("GPU work did not complete within 60 seconds", event_loop);
                }
                return;
            };
            self.waiting_for_gpu = false;
            if self.frame == WARMUP {
                // Start with an empty GPU queue. Otherwise warmup backlog can
                // make CPU callback FPS look healthy while GPU work falls behind.
                let now = Instant::now();
                self.measurement_started = Some(now);
                self.last_frame = now;
                return;
            }
            self.gpu_completed_ms = Some(
                completed
                    .duration_since(self.measurement_started.expect("measurement started"))
                    .as_secs_f64()
                    * 1000.0,
            );
            self.done = true;
            let report = self.report();
            #[cfg(not(target_arch = "wasm32"))]
            {
                println!("{report}");
                event_loop.exit();
            }
            #[cfg(target_arch = "wasm32")]
            {
                show_report(&report);
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            return;
        }
        let now = Instant::now();
        let interval = now.duration_since(self.last_frame).as_secs_f64() * 1000.0;
        self.last_frame = now;
        let started = Instant::now();
        if !self.frame_prepared {
            self.run.clear_frame_events();
            if self.scene == Scene::Race {
                self.run.tick(
                    scripted_input(self.frame),
                    &AccessibilitySettings::default(),
                );
            }
            self.frame_prepared = true;
        }
        let simulation = started.elapsed().as_secs_f64() * 1000.0;
        let renderer = self
            .renderer
            .as_mut()
            .expect("benchmark renderer initialized");
        let frame = match renderer.begin_frame_with_delta(&mut self.run, 1.0, FIXED_DT) {
            Ok(frame) => frame,
            Err(
                lawn_render::FrameAcquireError::Occluded | lawn_render::FrameAcquireError::Timeout,
            ) if self.frame == 0 && self.startup.elapsed().as_secs() < 5 => return,
            Err(error) => {
                self.fail(&format!("benchmark surface failed: {error}"), event_loop);
                return;
            }
        };
        self.final_stats = frame.stats;
        renderer.finish_frame(frame);
        self.frame_prepared = false;
        let cpu_work = started.elapsed().as_secs_f64() * 1000.0;
        if self.frame >= WARMUP {
            self.intervals.push(interval);
            self.cpu_work.push(cpu_work);
            self.simulation.push(simulation);
            self.encode
                .push(f64::from(self.final_stats.cpu_encode_milliseconds));
            if self.final_stats.gpu_frame_milliseconds > 0.0 {
                self.gpu
                    .push(f64::from(self.final_stats.gpu_frame_milliseconds));
            }
        }
        self.frame += 1;
        if self.frame == WARMUP || self.frame == WARMUP + SAMPLES {
            self.waiting_for_gpu = true;
            let completion = Arc::clone(&self.completion);
            renderer.queue().on_submitted_work_done(move || {
                *completion.lock().expect("completion lock") = Some(Instant::now());
            });
        }
    }

    fn report(&self) -> String {
        let renderer = self.renderer.as_ref().expect("renderer initialized");
        let caps = renderer.capabilities();
        let settings = renderer.diagnostics();
        serde_json::to_string_pretty(&json!({
            "benchmark_version": 2,
            "scene": self.scene.name(), "seed": SEED.0,
            "world_hash": format!("{:016x}", self.run.planet.deterministic_hash),
            "backend": format!("{:?}", caps.backend), "adapter": caps.adapter_name,
            "adapter_metadata": caps.adapter,
            "surface": [settings.surface_size.width, settings.surface_size.height],
            "world_target": [settings.world_size.width, settings.world_size.height],
            "quality": format!("{:?}", settings.quality),
            "msaa": settings.msaa_samples, "render_scale": settings.render_scale,
            "gpu_timing_requested": self.gpu_timing,
            "gpu_timing_active": settings.gpu_profiling_enabled,
            "warmup_frames": WARMUP, "sample_frames": self.intervals.len(),
            "fixed_dt_seconds": FIXED_DT,
            "frame_ms": distribution(&self.intervals),
            "cpu_frame_including_present_ms": distribution(&self.cpu_work),
            "gpu_completed_elapsed_ms": self.gpu_completed_ms,
            "gpu_completed_frames_per_second": self.gpu_completed_ms.map(|ms| SAMPLES as f64 * 1000.0 / ms),
            "simulation_ms": distribution(&self.simulation),
            "encode_ms": distribution(&self.encode),
            "gpu_ms": distribution(&self.gpu),
            "final_visible_patches": self.final_stats.visible_patches,
            "final_visible_tufts": self.final_stats.visible_tufts,
            "final_triangles": self.final_stats.generated_triangles,
            "final_grass_draw_calls": self.final_stats.grass_draw_calls,
            "final_total_coverage": self.run.mowing.coverage(),
            "final_clipping_particles": self.final_stats.clipping_particles,
            "scope": "Shared renderer and scripted simulation. Frame intervals count CPU redraw callbacks, not display flips. GPU completed throughput drains the queue before and after measured frames. Excludes egui, loading and compositor display timing. GPU spans are asynchronous latest samples, not per-frame correlated."
        })).expect("finite benchmark values")
    }

    fn fail(&mut self, error: &str, event_loop: &ActiveEventLoop) {
        self.done = true;
        self.failure = Some(error.to_owned());
        #[cfg(not(target_arch = "wasm32"))]
        event_loop.exit();
        #[cfg(target_arch = "wasm32")]
        {
            show_report(error);
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn reject_gpu_failure(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if let Some(error) = self.gpu_failure.message() {
            self.fail(&error, event_loop);
            true
        } else {
            false
        }
    }
}

fn scripted_input(frame: usize) -> InputSnapshot {
    InputSnapshot {
        accelerate: 1.0,
        steer: if frame < 225 { 0.25 } else { -0.25 },
        boost_held: (150..210).contains(&frame),
        ..InputSnapshot::default()
    }
}

fn distribution(values: &[f64]) -> serde_json::Value {
    if values.is_empty() {
        return serde_json::Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({
        "mean": sorted.iter().sum::<f64>() / sorted.len() as f64,
        "p50": sorted[(sorted.len() - 1) / 2],
        "p95": sorted[((sorted.len() as f64 * 0.95).ceil() as usize - 1).min(sorted.len()-1)],
    })
}

impl ApplicationHandler for BenchmarkApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_arch = "wasm32")]
        let _ = event_loop;
        #[cfg(not(target_arch = "wasm32"))]
        if self.window.is_none() {
            let result = (|| {
                let window = Arc::new(
                    event_loop.create_window(
                        Window::default_attributes()
                            .with_title("Lawn Orbit benchmark")
                            .with_inner_size(self.size)
                            .with_resizable(false),
                    )?,
                );
                pollster::block_on(self.initialize(window, None))
            })();
            if let Err(error) = result {
                self.fail(&format!("{error:#}"), event_loop);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                #[cfg(target_arch = "wasm32")]
                {
                    self.redraw_pending = false;
                }
                self.render(event_loop);
            }
            WindowEvent::CloseRequested => self.fail("benchmark canceled", event_loop),
            // The benchmark owns a fixed physical render target, independent of CSS/display scaling.
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(self.size);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.done {
            event_loop.set_control_flow(ControlFlow::Wait);
        } else if let Some(window) = &self.window {
            #[cfg(not(target_arch = "wasm32"))]
            {
                event_loop.set_control_flow(ControlFlow::Poll);
                window.request_redraw();
            }
            #[cfg(target_arch = "wasm32")]
            {
                event_loop.set_control_flow(ControlFlow::Wait);
                if !self.redraw_pending {
                    self.redraw_pending = true;
                    window.request_redraw();
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() -> Result<()> {
    use lawn_core::planet::{CURRENT_GENERATOR_VERSION, PlanetGenerator};
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut scene = Scene::Stationary;
    let mut gpu_timing = false;
    let mut size = resolution("720p")?;
    for arg in &args {
        match arg.as_str() {
            "--benchmark" => {}
            "--gpu-timing" => gpu_timing = true,
            value if value.starts_with("--scene=") => scene = Scene::parse(&value[8..])?,
            value if value.starts_with("--resolution=") => size = resolution(&value[13..])?,
            _ => anyhow::bail!("unknown benchmark argument: {arg}"),
        }
    }
    let generator = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, generator_config()?);
    let world = PreparedWorld::generate(&generator, SEED)?;
    let mut app = BenchmarkApp::new(world, scene, gpu_timing, size)?;
    EventLoop::new()?.run_app(&mut app)?;
    if let Some(error) = app.failure {
        anyhow::bail!(error);
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn run_web(scene: &str, gpu_timing: bool, size: &str) -> Result<()> {
    use crate::generation::{GenerationJob, GenerationRequest, GenerationResult};
    use wasm_bindgen::JsCast;
    use winit::platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys};
    let scene = Scene::parse(scene)?;
    let size = resolution(size)?;
    let prepared = GenerationJob::spawn(&GenerationRequest::Startup {
        config: generator_config()?,
        seed: SEED,
    })
    .map_err(anyhow::Error::msg)?
    .finish()
    .await
    .map_err(anyhow::Error::msg)?;
    let GenerationResult::Startup { world, sky } = prepared else {
        anyhow::bail!("unexpected benchmark worker result")
    };
    let mut app = BenchmarkApp::new(world, scene, gpu_timing, size)?;
    let document = web_sys::window()
        .and_then(|window| window.document())
        .context("document unavailable")?;
    let canvas = document
        .get_element_by_id("lawn-canvas")
        .context("benchmark canvas missing")?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| anyhow::anyhow!("invalid benchmark canvas"))?;
    let event_loop = EventLoop::new()?;
    #[allow(deprecated)]
    let window =
        Arc::new(event_loop.create_window(Window::default_attributes().with_canvas(Some(canvas)))?);
    app.initialize(window, Some(&sky)).await?;
    show_report(
        "Benchmark running: 90 warmup frames + 360 measured frames. Keep this tab visible.",
    );
    event_loop.spawn_app(app);
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn show_report(report: &str) {
    if let Some(document) = web_sys::window().and_then(|window| window.document())
        && let Some(output) = document.get_element_by_id("benchmark-results")
    {
        output.set_text_content(Some(report));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_failure_is_shared_and_keeps_the_first_error_latched() {
        let failure = GpuFailure::default();
        let callback_state = failure.clone();
        assert!(failure.message().is_none());
        callback_state.record("device lost".into());
        failure.record("later validation error".into());
        assert_eq!(failure.message().as_deref(), Some("device lost"));
        assert_eq!(callback_state.message().as_deref(), Some("device lost"));
    }

    #[test]
    fn distribution_uses_nearest_rank_p95_and_handles_no_gpu_samples() {
        assert_eq!(distribution(&[]), serde_json::Value::Null);
        let values: Vec<_> = (1..=100).rev().map(f64::from).collect();
        assert_eq!(
            distribution(&values),
            json!({"mean":50.5,"p50":50.0,"p95":95.0})
        );
    }

    #[test]
    fn scene_and_input_sequence_are_explicit() {
        assert_eq!(Scene::parse("race").unwrap(), Scene::Race);
        assert!(Scene::parse("custom").is_err());
        assert_eq!(resolution("1440p").unwrap(), PhysicalSize::new(2560, 1440));
        assert!(resolution("huge").is_err());
        assert!(!scripted_input(149).boost_held);
        assert!(scripted_input(150).boost_held);
        assert!(!scripted_input(210).boost_held);
        assert!(scripted_input(225).steer < 0.0);
    }
}
