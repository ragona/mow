use std::{collections::VecDeque, sync::Arc};

#[cfg(not(target_arch = "wasm32"))]
use lawn_core::PlanetGenerator;
#[cfg(not(target_arch = "wasm32"))]
use std::{sync::mpsc, thread};
use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(not(target_arch = "wasm32"))]
use winit::dpi::LogicalSize;

use anyhow::{Context, Result};
use egui::{Align2, Color32, RichText};
use egui_wgpu::wgpu;
use lawn_core::{
    GameConfig, WorldSeed,
    camera::{CameraRig, CameraState},
    config::GeneratorConfig,
    flow::GameState,
    input::{Action, Binding},
    planet::TUTORIAL_SEED,
    profile::{Profile, QualityPreset, RecordKey},
    race::RaceOutcome,
    run::{GameMode, RunEvent, RunState, TutorialStage},
    score::Results,
    simulation::FixedStepClock,
};
use lawn_render::{FrameAcquireError, FrameStats, Renderer};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow},
    keyboard::PhysicalKey,
    window::{Fullscreen, Window, WindowAttributes, WindowId},
};

use crate::{input_adapter::InputAdapter, profile_store::ProfileStore};

mod garden_ui;
mod race_ui;
#[cfg(test)]
mod ui_capture_tests;
use garden_ui::{
    Icon, action_label, boost_meter, coverage_dial, display, icon, icon_button, keycap, mow_button,
    preset_card,
};

#[derive(Clone, Copy, Debug)]
enum ConfirmAction {
    Restart,
    ReturnToEditor,
    RandomPlanet,
}

#[derive(Clone, Debug)]
enum UiCommand {
    Mow,
    OpenEditor,
    OpenSettings,
    Back,
    Start(WorldSeed),
    Random,
    Resume,
    Pause,
    Restart,
    ReturnToEditor,
    Submit,
    Retry,
    #[cfg(not(target_arch = "wasm32"))]
    Quit,
    ToggleFullscreen,
    ToggleFavorite(WorldSeed),
    SkipArrival,
    KeepMowing,
    ShowVictory,
}

const PREVIEW_INTERVAL: Duration = Duration::from_millis(100);
const EDITOR_SCENE_INSET: f32 = 356.0;
const TITLE_SCENE_INSET: f32 = 400.0;
const GARDEN_CREAM: Color32 = Color32::from_rgb(255, 249, 231);
const GARDEN_PINE: Color32 = Color32::from_rgb(34, 66, 57);
const GARDEN_MUTED: Color32 = Color32::from_rgb(93, 108, 91);
const GARDEN_SAGE: Color32 = Color32::from_rgb(206, 222, 182);
const GARDEN_CORAL: Color32 = Color32::from_rgb(248, 147, 111);
const GARDEN_RIVAL: Color32 = Color32::from_rgb(105, 155, 215);
const GARDEN_ERROR: Color32 = Color32::from_rgb(161, 57, 42);
const ARRIVAL_SECONDS: f32 = 0.9;

#[derive(Clone, Copy, Debug)]
struct SceneTransition {
    from: CameraState,
    to: CameraState,
    elapsed: f32,
    from_inset: f32,
    to_editor: bool,
}

impl SceneTransition {
    fn fraction(self) -> f32 {
        let t = (self.elapsed / ARRIVAL_SECONDS).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    fn pose(self) -> CameraState {
        let t = self.fraction();
        // Interpolate radial direction and distance separately: a straight
        // segment between opposite sides of the planet would pass through it.
        let from_direction = self.from.position.normalize_or(glam::Vec3::Y);
        let to_direction = self.to.position.normalize_or(from_direction);
        let arc = glam::Quat::from_rotation_arc(from_direction, to_direction);
        let radial_rotation = glam::Quat::IDENTITY.slerp(arc, t);
        let direction = radial_rotation * from_direction;
        let distance = self.from.position.length()
            + (self.to.position.length() - self.from.position.length()) * t;
        let position = direction * distance;
        let target = self.from.target.lerp(self.to.target, t);
        let view = (target - position).normalize_or(-direction);
        let from_view = (self.from.target - self.from.position).normalize_or(-from_direction);
        let to_view = (self.to.target - self.to.position).normalize_or(-to_direction);
        let from_up = self
            .from
            .up
            .reject_from_normalized(from_view)
            .normalize_or(from_view.any_orthonormal_vector());
        let to_up = self
            .to
            .up
            .reject_from_normalized(to_view)
            .normalize_or(to_view.any_orthonormal_vector());
        // Carry the camera's frame along its radial arc and align it with the
        // changing target, then interpolate only the remaining signed roll.
        // Opposite endpoint ups make a continuous half turn instead of
        // cancelling to zero halfway through a normalized vector lerp.
        let transport_up = |rotation: glam::Quat, look: glam::Vec3| {
            let align_view = glam::Quat::from_rotation_arc(rotation * from_view, look);
            (align_view * rotation * from_up).normalize()
        };
        let transported_end_up = transport_up(arc, to_view);
        let roll = to_view
            .dot(transported_end_up.cross(to_up))
            .atan2(transported_end_up.dot(to_up));
        let up = glam::Quat::from_axis_angle(view, roll * t) * transport_up(radial_rotation, view);
        CameraState {
            position,
            target,
            up,
            field_of_view_degrees: self.from.field_of_view_degrees
                + (self.to.field_of_view_degrees - self.from.field_of_view_degrees) * t,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GenerationPurpose {
    Preview,
    Play {
        mode: GameMode,
        return_state: GameState,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorldRecipe {
    settings: WorldEditorSettings,
    seed: WorldSeed,
}

struct PendingGeneration {
    recipe: WorldRecipe,
    purpose: GenerationPurpose,
    #[cfg(not(target_arch = "wasm32"))]
    receiver: mpsc::Receiver<Result<RunState, String>>,
    #[cfg(target_arch = "wasm32")]
    receiver: crate::generation::GenerationJob,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorldEditorSettings {
    planet_radius: f32,
    rock_coverage_percent: f32,
    peak_clusters: u8,
    peak_height: f32,
    rolling_amplitude: f32,
}

impl WorldEditorSettings {
    const RADIUS_SLIDER_RANGE: std::ops::RangeInclusive<f32> = 5.0..=20.0;
    const ROCKINESS_SLIDER_RANGE: std::ops::RangeInclusive<f32> = 0.0..=40.0;
    const PEAK_CLUSTERS_SLIDER_RANGE: std::ops::RangeInclusive<u8> = 0..=10;
    const PEAK_HEIGHT_SLIDER_MAX: f32 = 6.0;
    const ROLLING_SLIDER_MAX: f32 = 1.5;

    fn random(seed: WorldSeed) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed.0);
        let planet_radius = rng.random_range(Self::RADIUS_SLIDER_RANGE);
        Self {
            planet_radius,
            rock_coverage_percent: f32::from(rng.random_range(
                *Self::ROCKINESS_SLIDER_RANGE.start() as u8
                    ..=*Self::ROCKINESS_SLIDER_RANGE.end() as u8,
            )),
            peak_clusters: rng.random_range(Self::PEAK_CLUSTERS_SLIDER_RANGE),
            peak_height: rng.random_range(0.0..=Self::PEAK_HEIGHT_SLIDER_MAX.min(planet_radius)),
            rolling_amplitude: rng
                .random_range(0.0..=Self::ROLLING_SLIDER_MAX.min(planet_radius * 0.9)),
        }
    }

    fn from_generator(config: &GeneratorConfig) -> Self {
        Self {
            planet_radius: config.base_radius,
            rock_coverage_percent: (1.0
                - (config.mowable_ratio_min + config.mowable_ratio_max) * 0.5)
                * 100.0,
            peak_clusters: (u16::from(config.mountain_count_min)
                + u16::from(config.mountain_count_max))
            .div_ceil(2) as u8,
            peak_height: (config.mountain_height_min + config.mountain_height_max) * 0.5,
            rolling_amplitude: config.rolling_amplitude,
        }
    }

    fn sanitized(mut self) -> Self {
        let finite_clamp = |value: f32, min: f32, max: f32| {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                min
            }
        };
        self.planet_radius = finite_clamp(self.planet_radius, 1.0, 128.0);
        self.rock_coverage_percent = finite_clamp(self.rock_coverage_percent, 0.0, 90.0);
        self.peak_height = finite_clamp(self.peak_height, 0.0, self.planet_radius);
        // Keep every radial sample positive, including the sum of noise octaves.
        self.rolling_amplitude =
            finite_clamp(self.rolling_amplitude, 0.0, self.planet_radius * 0.9);
        self
    }

    fn is_experimental(self) -> bool {
        // These original tuning bounds govern duration validation, independently
        // of slider travel, so larger custom worlds can take longer to mow.
        !(12.0..=22.0).contains(&self.planet_radius)
            || !(0.0..=24.0).contains(&self.rock_coverage_percent)
            || !(2..=9).contains(&self.peak_clusters)
            || !(2.0..=7.0).contains(&self.peak_height)
            || !(0.0..=1.2).contains(&self.rolling_amplitude)
    }

    fn generator_config(self, baseline: &GeneratorConfig) -> GeneratorConfig {
        let settings = self.sanitized();
        let mut config = baseline.clone();
        config.base_radius = settings.planet_radius;
        config.rolling_amplitude = settings.rolling_amplitude;

        let peak_clusters = settings.peak_clusters;
        if (2..=9).contains(&peak_clusters) {
            config.mountain_count_min = peak_clusters.saturating_sub(2).max(1);
            config.mountain_count_max = peak_clusters.saturating_add(2);
        } else {
            config.mountain_count_min = peak_clusters;
            config.mountain_count_max = peak_clusters;
        }
        let peak_height = settings.peak_height;
        let spread = peak_height.min(1.25);
        config.mountain_height_min = peak_height - spread;
        config.mountain_height_max = (peak_height + spread).min(config.base_radius);
        config.mountain_separation_radians = (baseline.mountain_separation_radians
            * (5.0 / f32::from(peak_clusters.max(1))).sqrt())
        .clamp(0.035, 0.75);

        let rock_coverage = settings.rock_coverage_percent / 100.0;
        if rock_coverage == 0.0 {
            config.mountain_count_min = 0;
            config.mountain_count_max = 0;
            config.mowable_ratio_min = 1.0;
            config.mowable_ratio_max = 1.0;
        } else {
            let tolerance = (rock_coverage * 0.5).min(0.05);
            config.mowable_ratio_min = 1.0 - rock_coverage - tolerance;
            config.mowable_ratio_max = 1.0 - rock_coverage + tolerance;
            // Small rock amounts also taper the outcroppings into the lawn.
            let peak_scale = (rock_coverage / 0.08).min(1.0);
            config.mountain_height_min *= peak_scale;
            config.mountain_height_max *= peak_scale;
        }

        let radius_scale = config.base_radius / baseline.base_radius.max(1.0);
        config.pass_clearance = (baseline.pass_clearance * radius_scale).clamp(0.05, 6.2);
        config.spawn_clearance = (baseline.spawn_clearance * radius_scale).clamp(0.05, 5.2);
        config.ideal_time_min_seconds = if settings.is_experimental() {
            0.0
        } else {
            45.0
        };
        config.ideal_time_max_seconds = if settings.is_experimental() {
            1_000_000.0
        } else {
            900.0
        };
        config.maximum_generation_attempts = baseline.maximum_generation_attempts.max(16);
        config
    }

    fn meadow() -> Self {
        Self {
            planet_radius: 14.0,
            rock_coverage_percent: 0.0,
            peak_clusters: 3,
            peak_height: 2.8,
            rolling_amplitude: 0.3,
        }
    }

    fn craggy() -> Self {
        Self {
            planet_radius: 17.0,
            rock_coverage_percent: 20.0,
            peak_clusters: 8,
            peak_height: 6.0,
            rolling_amplitude: 1.0,
        }
    }
}

pub struct LawnOrbitApp {
    window: Option<Arc<Window>>,
    #[cfg(target_arch = "wasm32")]
    browser_platform: Option<crate::browser_platform::BrowserPlatform>,
    #[cfg(target_arch = "wasm32")]
    redraw_pending: bool,
    renderer: Option<Renderer>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    run: RunState,
    game_config: GameConfig,
    world_editor: WorldEditorSettings,
    selected_mode: GameMode,
    state: GameState,
    settings_return_state: GameState,
    settings_open: bool,
    clock: FixedStepClock,
    last_frame: Instant,
    input: InputAdapter,
    profile: Profile,
    profile_store: ProfileStore,
    profile_write_enabled: bool,
    occluded: bool,
    surface_retry_at: Option<Instant>,
    results: Option<Results>,
    victory_card_open: bool,
    pending_generation: Option<PendingGeneration>,
    preview_recipe: Option<WorldRecipe>,
    preview_attempt: Option<WorldRecipe>,
    preview_queued_at: Option<Instant>,
    seed_text: String,
    confirmation: Option<ConfirmAction>,
    last_stats: FrameStats,
    status_message: Option<String>,
    scene_transition: Option<SceneTransition>,
    editor_zoom: f32,
    editor_orbit: f32,
    hud_milestone: Option<(u8, f32)>,
    boost_was_ready: bool,
    boost_ready_age: f32,
    app_started: Instant,
    show_diagnostics: bool,
    frame_times_ms: VecDeque<f32>,
}

impl LawnOrbitApp {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new() -> Result<Self> {
        let game_config = GameConfig::shipping().context("shipping gameplay config is invalid")?;
        Self::load_with_config(ProfileStore::discover(), game_config)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_with_config(profile_store: ProfileStore, game_config: GameConfig) -> Result<Self> {
        let (profile, load_error) = match profile_store.load() {
            Ok(profile) => (profile, None),
            Err(error) => {
                tracing::warn!(%error, "profile could not be loaded; original file will be preserved");
                (
                    Profile::default(),
                    Some(format!(
                        "Profile could not be loaded: {error}. The original file is preserved; changes will not be saved this session."
                    )),
                )
            }
        };
        let mut app = Self::with_config(profile_store, profile, game_config)?;
        app.profile_write_enabled = load_error.is_none();
        app.status_message = load_error;
        Ok(app)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn with_config(
        profile_store: ProfileStore,
        mut profile: Profile,
        game_config: GameConfig,
    ) -> Result<Self> {
        game_config
            .validate()
            .map_err(anyhow::Error::msg)
            .context("gameplay config is invalid")?;
        profile.sanitize();
        let planet = PlanetGenerator::new(
            lawn_core::planet::CURRENT_GENERATOR_VERSION,
            game_config.generator.clone(),
        )
        .generate(TUTORIAL_SEED)
        .context("tutorial planet generation failed")?;
        let run = RunState::new(
            planet,
            GameMode::FreeMow,
            game_config.vehicle.clone(),
            game_config.job.clone(),
            &profile.settings.accessibility,
            false,
        );
        Ok(Self::with_run(profile_store, profile, game_config, run))
    }

    fn with_run(
        profile_store: ProfileStore,
        profile: Profile,
        game_config: GameConfig,
        run: RunState,
    ) -> Self {
        let world_editor = WorldEditorSettings::from_generator(&game_config.generator);
        Self {
            window: None,
            #[cfg(target_arch = "wasm32")]
            browser_platform: None,
            #[cfg(target_arch = "wasm32")]
            redraw_pending: false,
            renderer: None,
            egui_state: None,
            egui_renderer: None,
            run,
            game_config,
            world_editor,
            selected_mode: GameMode::TurfRace,
            state: GameState::Title,
            settings_return_state: GameState::Title,
            settings_open: false,
            clock: FixedStepClock::default(),
            last_frame: Instant::now(),
            input: InputAdapter::default(),
            profile,
            profile_store,
            profile_write_enabled: true,
            occluded: false,
            surface_retry_at: None,
            results: None,
            victory_card_open: false,
            pending_generation: None,
            preview_recipe: None,
            preview_attempt: None,
            preview_queued_at: None,
            seed_text: TUTORIAL_SEED.to_string(),
            confirmation: None,
            last_stats: FrameStats::default(),
            status_message: None,
            scene_transition: None,
            editor_zoom: 1.0,
            editor_orbit: 0.5,
            hud_milestone: None,
            boost_was_ready: true,
            boost_ready_age: 10.0,
            app_started: Instant::now(),
            show_diagnostics: false,
            frame_times_ms: VecDeque::with_capacity(240),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn create_window_and_gpu(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if self.window.is_some() {
            return Ok(());
        }
        let attributes = WindowAttributes::default()
            .with_title(crate::GAME_TITLE)
            .with_inner_size(LogicalSize::new(1280.0, 720.0))
            .with_min_inner_size(LogicalSize::new(960.0, 540.0));
        let window = Arc::new(event_loop.create_window(attributes)?);
        if self.profile.settings.fullscreen {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        pollster::block_on(self.initialize_gpu(window, None))
    }

    async fn initialize_gpu(
        &mut self,
        window: Arc<Window>,
        sky: Option<&lawn_render::PreparedSky>,
    ) -> Result<()> {
        let mut renderer = Renderer::new_with_sky(
            window.clone(),
            &self.run,
            self.profile.settings.msaa_samples,
            self.profile.settings.quality,
            self.profile.settings.render_scale,
            self.profile.settings.accessibility.high_contrast_grass,
            self.profile.settings.accessibility.reduced_particles
                || self.profile.settings.accessibility.reduced_motion,
            self.profile.settings.grass_height_multiplier,
            sky,
        )
        .await?;
        renderer.set_motion_reduction(self.profile.settings.accessibility.reduced_motion);
        tracing::info!(
            adapter = %renderer.capabilities().adapter_name,
            backend = ?renderer.capabilities().backend,
            tier = ?renderer.capabilities().tier,
            "graphics initialized"
        );
        let egui_context = egui::Context::default();
        configure_egui_style(&egui_context);
        let egui_state = egui_winit::State::new(
            egui_context,
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(renderer.capabilities().maximum_texture_dimension.min(4_096) as usize),
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            renderer.device(),
            renderer.surface_format(),
            egui_wgpu::RendererOptions::default(),
        );
        self.renderer = Some(renderer);
        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);
        self.window = Some(window);
        self.last_frame = Instant::now();
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn start_web() -> Result<()> {
        use crate::generation::{GenerationJob, GenerationRequest, GenerationResult};
        use wasm_bindgen::JsCast;
        use winit::{
            event_loop::EventLoop,
            platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys},
        };

        let document = web_sys::window()
            .and_then(|window| window.document())
            .context("browser document is unavailable")?;
        let canvas = document
            .get_element_by_id("lawn-canvas")
            .context("game canvas is missing")?
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|_| anyhow::anyhow!("game canvas has the wrong element type"))?;
        let game_config = GameConfig::shipping()?;
        let profile_store = ProfileStore::discover();
        let (mut profile, load_error) = match profile_store.load() {
            Ok(profile) => (profile, None),
            Err(error) => (
                Profile::default(),
                Some(format!(
                    "Profile could not be loaded: {error}. The saved profile is preserved; changes will not be saved this session."
                )),
            ),
        };
        profile.sanitize();
        let result = GenerationJob::spawn(&GenerationRequest::Startup {
            config: game_config.generator.clone(),
            seed: TUTORIAL_SEED,
        })
        .map_err(anyhow::Error::msg)?
        .finish()
        .await
        .map_err(anyhow::Error::msg)?;
        let GenerationResult::Startup { world, sky } = result else {
            anyhow::bail!("worker returned an unexpected startup response");
        };
        let run = world.into_run(
            GameMode::FreeMow,
            game_config.vehicle.clone(),
            game_config.job.clone(),
            &profile.settings.accessibility,
            false,
        );
        let mut app = Self::with_run(profile_store, profile, game_config, run);
        app.profile_write_enabled = load_error.is_none();
        app.status_message = load_error;
        app.browser_platform = Some(crate::browser_platform::BrowserPlatform::new(&canvas)?);
        if let Some(status) = document.get_element_by_id("loading-status") {
            status.set_text_content(Some("Preparing the garden's lighting…"));
        }
        let event_loop = EventLoop::new()?;
        let attributes = WindowAttributes::default()
            .with_title(crate::GAME_TITLE)
            .with_canvas(Some(canvas.clone()))
            .with_prevent_default(false);
        // On the web, creation before spawn_app lets GPU initialization await
        // browser promises without blocking winit's event callbacks.
        #[allow(deprecated)]
        let window = Arc::new(event_loop.create_window(attributes)?);
        app.initialize_gpu(window, Some(&sky)).await?;
        drop(sky);
        // The browser owns fullscreen. A saved desktop-style preference must
        // not request fullscreen without a user gesture on a fresh page load.
        app.profile.settings.fullscreen = false;
        canvas
            .focus()
            .map_err(|error| anyhow::anyhow!("could not focus game canvas: {error:?}"))?;
        event_loop.spawn_app(app);
        Ok(())
    }

    fn update_simulation(&mut self, elapsed: Duration) {
        let _span = tracing::debug_span!("fixed_simulation_batch").entered();
        if self.state == GameState::WorldEditor && !self.settings_open {
            if self.profile.settings.accessibility.reduced_motion {
                self.scene_transition = None;
            } else {
                self.editor_orbit += elapsed.as_secs_f32().min(0.05) * 0.08;
            }
            if let Some(transition) = &mut self.scene_transition {
                transition.elapsed += elapsed.as_secs_f32().min(0.05);
                if transition.elapsed >= ARRIVAL_SECONDS {
                    self.scene_transition = None;
                }
            }
        }
        if self.state != GameState::Playing {
            return;
        }
        if self.input.take_pause_pressed() {
            self.pause();
            return;
        }
        if self.scene_transition.is_some() {
            let snapshot = self.input.snapshot(
                &self.profile.settings.controls,
                &self.profile.settings.accessibility,
            );
            let skip = snapshot.steer.abs() > 0.1
                || snapshot.accelerate > 0.1
                || snapshot.brake_reverse > 0.1
                || snapshot.boost_held
                || snapshot.recover_held
                || snapshot.look_behind
                || snapshot.recenter_pressed
                || snapshot.camera_orbit.iter().any(|value| value.abs() > 0.1)
                || snapshot.submit_pressed;
            let transition = self.scene_transition.as_mut().expect("arrival exists");
            transition.elapsed += elapsed.as_secs_f32().min(0.05);
            if skip
                || transition.elapsed >= ARRIVAL_SECONDS
                || self.profile.settings.accessibility.reduced_motion
            {
                self.finish_arrival();
            } else {
                let pose = transition.pose();
                self.run
                    .camera
                    .snap_to_pose(pose.position, pose.target, pose.up);
                self.run.camera.state.field_of_view_degrees = pose.field_of_view_degrees;
            }
            // Camera arrival never advances mowing or the authoritative body.
            self.run.clear_frame_events();
            return;
        }
        self.run.clear_frame_events();
        let was_victory_lap = self.run.is_victory_lap();
        let input = &mut self.input;
        let run = &mut self.run;
        let controls = &self.profile.settings.controls;
        let accessibility = &self.profile.settings.accessibility;
        let mut submit = false;
        // Sample when a fixed tick actually consumes input. This preserves
        // mouse movement on faster display frames and consumes it only once
        // when a slower display frame executes multiple simulation ticks.
        self.clock.advance(elapsed, || {
            let snapshot = input.snapshot(controls, accessibility);
            run.tick(snapshot, accessibility);
            submit |= snapshot.submit_pressed;
        });
        if self.run.mode == GameMode::TurfRace
            && self
                .run
                .race
                .as_ref()
                .is_some_and(|race| race.outcome.is_some())
        {
            if !self.run.is_victory_lap() {
                self.finish_run();
                return;
            }
            if !was_victory_lap {
                self.finish_run();
            }
        }
        if self.run.tutorial_enabled
            && self.run.tutorial_stage == TutorialStage::Complete
            && !self.profile.tutorial_completed
        {
            self.profile.tutorial_completed = true;
            self.profile.tutorial_reset_requested = false;
            self.save_profile();
        }
        if self.run.mode == GameMode::Standard && submit && self.run.completion_available {
            self.finish_run();
        }
        self.input.update_feedback(&self.run);
        let presentation_dt = elapsed.as_secs_f32().min(0.05);
        self.boost_ready_age += presentation_dt;
        if let Some((_, age)) = &mut self.hud_milestone {
            *age += presentation_dt;
            if *age >= 3.0 {
                self.hud_milestone = None;
            }
        }
        for event in self.run.events() {
            if let RunEvent::CoverageMilestone(percent) = event {
                self.hud_milestone = Some((*percent, 0.0));
            }
        }
        let ready = self.run.vehicle.state.boost_charge
            >= self.run.vehicle_tuning.boost_capacity_seconds * 0.995;
        if ready && !self.boost_was_ready {
            self.boost_ready_age = 0.0;
        }
        self.boost_was_ready = ready;
    }

    fn pause(&mut self) {
        if self.state == GameState::Playing {
            self.state = GameState::Paused;
            self.run.paused = true;
        }
        self.clock = FixedStepClock::default();
        self.input.clear();
    }

    fn resume(&mut self) {
        self.state = GameState::Playing;
        self.run.paused = false;
        self.clock = FixedStepClock::default();
        self.input.clear_transient();
        self.confirmation = None;
        self.clear_ui_keyboard_focus();
        self.last_frame = Instant::now();
        if !self.input.is_focused() {
            self.pause();
        }
    }

    fn clear_ui_keyboard_focus(&self) {
        if let Some(state) = &self.egui_state {
            release_keyboard_focus(state.egui_ctx());
        }
    }

    fn keep_mowing(&mut self) {
        if self.run.is_victory_lap() {
            self.victory_card_open = false;
            self.clear_ui_keyboard_focus();
            if self.state == GameState::Paused {
                self.resume();
            }
        }
    }

    fn show_victory(&mut self) {
        if self.run.is_victory_lap() {
            self.victory_card_open = true;
            if self.state == GameState::Paused {
                self.resume();
            }
        }
    }

    fn window_focus_changed(&mut self, focused: bool) {
        tracing::debug!(focused, state = ?self.state, "window focus changed");
        self.input.handle_window_event(
            &WindowEvent::Focused(focused),
            &mut self.profile.settings.controls,
        );
        if !focused {
            self.pause();
        }
    }

    fn gameplay_owns_key(&self, key: PhysicalKey, text_edit_focused: bool) -> bool {
        if self.state != GameState::Playing
            || self.settings_open
            || self.confirmation.is_some()
            || self.input.rebind_action.is_some()
            || text_edit_focused
        {
            return false;
        }
        let PhysicalKey::Code(code) = key else {
            return false;
        };
        let binding = Binding::Key(format!("{code:?}"));
        self.profile
            .settings
            .controls
            .bindings
            .iter()
            .any(|(action, bindings)| {
                !matches!(
                    action,
                    Action::Confirm | Action::Cancel | Action::ToggleMower
                ) && bindings.contains(&binding)
            })
    }

    fn finish_run(&mut self) {
        if self.run.mode == GameMode::TurfRace {
            if self.run.is_victory_lap() {
                // The core has removed the rival, but driving and mowing keep
                // their current camera, clock, velocity, and held input.
                self.victory_card_open = true;
                self.results = None;
                return;
            }
            if self
                .run
                .race
                .as_ref()
                .is_some_and(|race| race.outcome.is_some())
            {
                self.run.paused = true;
                self.results = None;
                self.victory_card_open = false;
                self.state = GameState::Results;
                self.scene_transition = None;
                self.clock = FixedStepClock::default();
                self.input.clear();
            }
            return;
        }
        if self.run.tutorial_enabled && self.run.tutorial_stage == TutorialStage::Submit {
            self.run.tutorial_stage = TutorialStage::Complete;
            self.profile.tutorial_completed = true;
            self.profile.tutorial_reset_requested = false;
        }
        if let Some(results) = self.run.submit() {
            self.profile.record_result(&results);
            self.results = Some(results);
            self.state = GameState::Results;
            self.save_profile();
        }
    }

    fn poll_generation(&mut self) {
        let Some(pending) = &mut self.pending_generation else {
            return;
        };
        #[cfg(not(target_arch = "wasm32"))]
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(
                "The planet worker stopped before finishing. Try another setting or seed.".into(),
            ),
        };
        #[cfg(target_arch = "wasm32")]
        let result = match pending.receiver.poll() {
            None => return,
            Some(result) => result.and_then(|prepared| match prepared {
                crate::generation::GenerationResult::World(world) => Ok(world.into_run(
                    GameMode::FreeMow,
                    self.game_config.vehicle.clone(),
                    self.game_config.job.clone(),
                    &self.profile.settings.accessibility,
                    false,
                )),
                _ => Err("The planet worker returned an unexpected response.".into()),
            }),
        };
        let recipe = pending.recipe;
        let purpose = pending.purpose;
        self.pending_generation = None;
        // Leaving the editor must never let a late preview replace a live run
        // or navigate back into the editor on completion.
        if purpose == GenerationPurpose::Preview && self.state != GameState::WorldEditor {
            return;
        }
        match result {
            Ok(run) => {
                self.run = run;
                self.results = None;
                self.clock = FixedStepClock::default();
                self.status_message = None;
                if let GenerationPurpose::Play { mode, .. } = purpose {
                    self.begin_game(mode);
                } else {
                    if let Some(renderer) = &mut self.renderer {
                        renderer.upload_planet(&self.run);
                    }
                    self.run.paused = true;
                    self.preview_recipe = Some(recipe);
                }
            }
            Err(error) => {
                if matches!(purpose, GenerationPurpose::Play { .. })
                    || self.editor_recipe() == Some(recipe)
                {
                    self.status_message = Some(format!("Planet generation failed: {error}"));
                }
                self.state = match purpose {
                    GenerationPurpose::Preview => GameState::WorldEditor,
                    GenerationPurpose::Play { return_state, .. } => return_state,
                };
                if !self.input.is_focused() {
                    self.pause();
                }
            }
        }
    }

    fn editor_recipe(&self) -> Option<WorldRecipe> {
        self.seed_text.parse().ok().map(|seed| WorldRecipe {
            settings: self.world_editor,
            seed,
        })
    }

    fn update_editor_preview(&mut self) {
        if self.state != GameState::WorldEditor || self.settings_open {
            self.preview_queued_at = None;
            return;
        }
        let Some(recipe) = self.editor_recipe() else {
            self.preview_queued_at = None;
            return;
        };
        if self.preview_recipe == Some(recipe) {
            self.preview_queued_at = None;
            return;
        }
        // Coalesce slider events into one latest request. A running worker is
        // allowed to finish, so continuous dragging still shows intermediate
        // worlds instead of indefinitely postponing every preview.
        let queued_at = self.preview_queued_at.get_or_insert_with(Instant::now);
        if self.pending_generation.is_some()
            || self.preview_attempt == Some(recipe)
            || queued_at.elapsed() < PREVIEW_INTERVAL
        {
            return;
        }
        self.preview_queued_at = None;
        self.preview_attempt = Some(recipe);
        self.spawn_generation(recipe, GenerationPurpose::Preview);
    }

    fn start_mow(&mut self) {
        // Quick play has its own recipe and mode. A custom lawn (including an
        // unfinished preview or a Free Mow selection) stays in the editor.
        let recipe = WorldRecipe {
            settings: WorldEditorSettings::from_generator(&self.game_config.generator),
            seed: Self::random_seed(),
        };
        self.start_play_generation(recipe, GameMode::TurfRace);
    }

    fn start_generation(&mut self, seed: WorldSeed) {
        let recipe = WorldRecipe {
            settings: self.world_editor,
            seed,
        };
        self.seed_text = seed.to_string();
        if self.preview_recipe == Some(recipe) && self.pending_generation.is_none() {
            // The visible world already has full grass, mowing, and collision
            // data. Enter that exact world without another generation or upload.
            self.begin_game(self.selected_mode);
            return;
        }
        self.start_play_generation(recipe, self.selected_mode);
    }

    fn start_play_generation(&mut self, recipe: WorldRecipe, mode: GameMode) {
        let return_state = self.state;
        // Drop any editor worker before attempting a new one, including when
        // starting a replacement worker fails. Its result must never enter play.
        self.pending_generation = None;
        self.scene_transition = None;
        self.spawn_generation(recipe, GenerationPurpose::Play { mode, return_state });
        if self.pending_generation.is_some() {
            self.state = GameState::Loading;
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_generation(&mut self, recipe: WorldRecipe, purpose: GenerationPurpose) {
        let config = recipe
            .settings
            .generator_config(&self.game_config.generator);
        let generator = PlanetGenerator::new(lawn_core::planet::CURRENT_GENERATOR_VERSION, config);
        let vehicle = self.game_config.vehicle.clone();
        let job = self.game_config.job.clone();
        let accessibility = self.profile.settings.accessibility.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("planet-generator".into())
            .spawn(move || {
                let started = Instant::now();
                let result = generator
                    .generate_with_root_budget(recipe.seed, crate::EDITOR_ROOT_BUDGET)
                    .map(|planet| {
                        RunState::new(
                            planet,
                            GameMode::FreeMow,
                            vehicle,
                            job,
                            &accessibility,
                            false,
                        )
                    })
                    .map_err(|error| error.to_string());
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis(),
                    "planet prepared"
                );
                let _ = sender.send(result);
            });
        if let Err(error) = worker {
            self.status_message = Some(format!("Could not start planet generation: {error}"));
            return;
        }
        self.pending_generation = Some(PendingGeneration {
            recipe,
            purpose,
            receiver,
        });
        self.status_message = None;
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn_generation(&mut self, recipe: WorldRecipe, purpose: GenerationPurpose) {
        let request = crate::generation::GenerationRequest::World {
            config: recipe
                .settings
                .generator_config(&self.game_config.generator),
            seed: recipe.seed,
        };
        match crate::generation::GenerationJob::spawn(&request) {
            Ok(receiver) => {
                self.pending_generation = Some(PendingGeneration {
                    recipe,
                    purpose,
                    receiver,
                });
                self.status_message = None;
            }
            Err(error) => {
                self.status_message = Some(format!("Could not start planet generation: {error}"));
            }
        }
    }

    fn begin_game(&mut self, mode: GameMode) {
        // Mode changes reset claims and vehicles on the exact previewed planet.
        // Keep the preview camera as the start of the arrival animation.
        let camera = self.run.camera.state;
        match mode {
            GameMode::TurfRace => self.run.start_race(&self.profile.settings.accessibility),
            GameMode::FreeMow => self
                .run
                .start_free_mow(&self.profile.settings.accessibility),
            GameMode::Standard => self.run.restart(&self.profile.settings.accessibility),
        }
        self.run
            .camera
            .snap_to_pose(camera.position, camera.target, camera.up);
        self.run.camera.state.field_of_view_degrees = camera.field_of_view_degrees;
        if let Some(renderer) = &mut self.renderer {
            renderer.upload_planet(&self.run);
        }
        self.results = None;
        self.enter_sandbox();
    }

    fn enter_sandbox(&mut self) {
        self.victory_card_open = false;
        let from = self.run.camera.state;
        let from_inset = if self.state == GameState::WorldEditor {
            EDITOR_SCENE_INSET
        } else {
            0.0
        };
        let arrival = CameraRig::new(
            self.run.vehicle.state.transform,
            self.run.planet.config.base_radius,
            &self.profile.settings.accessibility,
        );
        let moving_to_game =
            from.position.distance(arrival.state.position) > 0.5 || from_inset > 0.0;
        self.scene_transition = (!self.profile.settings.accessibility.reduced_motion
            && moving_to_game)
            .then_some(SceneTransition {
                from,
                to: arrival.state,
                elapsed: 0.0,
                from_inset,
                to_editor: false,
            });
        if self.scene_transition.is_none() {
            self.run.camera = arrival;
        }
        self.profile.record_seed(
            self.run.planet.generator_version,
            self.run.planet.world_seed,
        );
        self.run.paused = false;
        self.run.tutorial_enabled =
            self.run.mode != GameMode::TurfRace && !self.profile.tutorial_completed;
        self.state = GameState::Playing;
        self.preview_recipe = None;
        self.preview_attempt = None;
        self.preview_queued_at = None;
        self.hud_milestone = None;
        self.boost_ready_age = 10.0;
        self.boost_was_ready = true;
        self.clock = FixedStepClock::default();
        self.input.clear_transient();
        self.clear_ui_keyboard_focus();
        self.last_frame = Instant::now();
        if !self.input.is_focused() {
            self.pause();
        }
        self.save_profile();
    }

    fn finish_arrival(&mut self) {
        self.scene_transition = None;
        self.run.camera = CameraRig::new(
            self.run.vehicle.state.transform,
            self.run.planet.config.base_radius,
            &self.profile.settings.accessibility,
        );
        self.clock = FixedStepClock::default();
        self.input.clear_transient();
    }

    fn enter_editor(&mut self) {
        self.victory_card_open = false;
        let from = self.run.camera.state;
        let from_inset = if self.state == GameState::Title {
            TITLE_SCENE_INSET
        } else {
            0.0
        };
        self.state = GameState::WorldEditor;
        self.run.paused = true;
        self.preview_attempt = None;
        self.status_message = None;
        self.editor_orbit = from.position.z.atan2(from.position.x);
        self.scene_transition =
            (!self.profile.settings.accessibility.reduced_motion).then_some(SceneTransition {
                from,
                to: from,
                elapsed: 0.0,
                from_inset,
                to_editor: true,
            });
    }

    fn return_to_title(&mut self) {
        self.state = GameState::Title;
        self.scene_transition = None;
    }

    fn random_seed() -> WorldSeed {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        WorldSeed((nanos as u64) ^ (nanos >> 64) as u64 ^ 0xA17C_9E37_5EED_1234)
    }

    fn animate_planet_camera(&mut self, speed: f32) {
        let angle = if self.profile.settings.accessibility.reduced_motion {
            0.5
        } else {
            self.app_started.elapsed().as_secs_f32() * speed
        };
        let planet_radius = self.run.planet.config.base_radius;
        let extent = self.planet_preview_extent();
        let ordinary_distance = egui::vec2(planet_radius * 1.8, planet_radius * 0.72).length();
        let radius = if ordinary_distance > extent * 1.1 {
            planet_radius * 1.8
        } else {
            extent * 1.35
        };
        let position = glam::Vec3::new(
            angle.cos() * radius,
            planet_radius * 0.72,
            angle.sin() * radius,
        );
        self.run
            .camera
            .snap_to_pose(position, glam::Vec3::ZERO, glam::Vec3::Y);
    }

    fn planet_preview_extent(&self) -> f32 {
        // Include overlapping peaks rather than assuming base radius bounds the
        // world. Padding covers unsampled tips and cosmetic surface relief.
        self.run
            .planet
            .terrain
            .iter()
            .map(|cell| cell.radius)
            .fold(0.0, f32::max)
            * 1.08
            + 0.5
    }

    fn animate_editor_camera(&mut self, context: &egui::Context) {
        let screen = context.content_rect();
        let aspect = ((screen.width() - EDITOR_SCENE_INSET) / screen.height()).max(0.25);
        let vertical_half_fov = self.run.camera.state.field_of_view_degrees.to_radians() * 0.5;
        let half_fov = vertical_half_fov.min((vertical_half_fov.tan() * aspect).atan());
        // Keep the common comparison scale for ordinary planets. Experimental
        // sizes get enough room for their actual surface, including dense peaks.
        let base_radius = self.run.planet.config.base_radius;
        let extent = self.planet_preview_extent();
        let reference_extent = 32.0 * (base_radius / 12.0).min(1.0);
        let envelope = reference_extent.max(extent);
        let minimum_distance = (36.0 * (base_radius / 12.0).min(1.0)).max(extent * 1.15);
        let distance = (envelope / half_fov.sin() * 1.08 / self.editor_zoom).max(minimum_distance);
        let angle = self.editor_orbit;
        let direction = glam::Vec3::new(angle.cos(), 0.4, angle.sin()).normalize();
        let to = CameraState {
            position: direction * distance,
            target: glam::Vec3::ZERO,
            up: glam::Vec3::Y,
            field_of_view_degrees: self.run.camera.state.field_of_view_degrees,
        };
        let pose = if let Some(transition) = &mut self.scene_transition {
            transition.to = to;
            transition.pose()
        } else {
            to
        };
        self.run
            .camera
            .snap_to_pose(pose.position, pose.target, pose.up);
    }

    fn draw_ui(&mut self, context: &egui::Context) -> Vec<UiCommand> {
        let mut commands = Vec::new();
        match self.state {
            GameState::Title => Self::draw_title(context, &mut commands),
            GameState::WorldEditor => self.draw_world_editor(context, &mut commands),
            GameState::Loading => Self::draw_loading(context),
            GameState::Playing => self.draw_hud(context, &mut commands),
            GameState::Paused => self.draw_pause(context, &mut commands),
            GameState::Results => self.draw_results(context, &mut commands),
            GameState::Boot => {}
        }
        if self.state == GameState::WorldEditor {
            self.draw_editor_inspection(context);
            self.animate_editor_camera(context);
        } else if matches!(self.state, GameState::Title | GameState::Results) {
            self.animate_planet_camera(0.08);
        }
        if self.state == GameState::Loading {
            context.request_repaint_after(Duration::from_millis(16));
        }
        if let Some(message) = &self.status_message {
            egui::Area::new("status".into())
                .anchor(Align2::LEFT_BOTTOM, [16.0, -16.0])
                .show(context, |ui| {
                    garden_card().show(ui, |ui| {
                        ui.colored_label(GARDEN_ERROR, message);
                    });
                });
        }
        self.draw_confirmation(context, &mut commands);
        commands
    }

    fn draw_title(context: &egui::Context, commands: &mut Vec<UiCommand>) {
        egui::Window::new(crate::GAME_TITLE)
            .anchor(Align2::LEFT_CENTER, [32.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(
                garden_card()
                    .inner_margin(24)
                    .corner_radius(2)
                    .stroke(egui::Stroke::new(1.5, GARDEN_PINE)),
            )
            .show(context, |ui| {
                ui.set_width(292.0);
                ui.spacing_mut().item_spacing.y = 5.0;
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("PLANETARY LAWN CARE")
                            .font(egui::FontId::monospace(10.0))
                            .extra_letter_spacing(1.5)
                            .color(GARDEN_MUTED),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("M.O.W.")
                            .font(egui::FontId::new(
                                96.0,
                                egui::FontFamily::Name("mow-mark".into()),
                            ))
                            .line_height(Some(96.0))
                            .extra_letter_spacing(0.4)
                            .color(GARDEN_PINE),
                    );
                    ui.label(
                        RichText::new("MOWER OF WORLDS")
                            .font(display(24.0))
                            .extra_letter_spacing(1.7),
                    );
                    let (rule, _) =
                        ui.allocate_exact_size(egui::vec2(292.0, 5.0), egui::Sense::hover());
                    ui.painter().rect_filled(rule, 0, GARDEN_PINE);
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Race a rival around a tiny planet.\nClaim more than half the lawn to win.",
                        )
                        .color(GARDEN_MUTED),
                    );
                    ui.add_space(10.0);
                    let mow = mow_button(ui, "Mow", [292.0, 68.0]);
                    if !context.egui_wants_keyboard_input() {
                        mow.request_focus();
                    }
                    if mow.clicked() {
                        commands.push(UiCommand::Mow);
                    }
                    ui.add_space(6.0);
                    if ui.button("Customize planet").clicked() {
                        commands.push(UiCommand::OpenEditor);
                    }
                    ui.small("Shape your world or settle in for Free Mow.");
                    ui.add_space(8.0);
                    if ui.button("Settings & Accessibility").clicked() {
                        commands.push(UiCommand::OpenSettings);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button("Quit").clicked() {
                        commands.push(UiCommand::Quit);
                    }
                });
            });
    }

    fn draw_world_editor(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        self.world_editor = self.world_editor.sanitized();
        let classic = WorldEditorSettings::from_generator(&self.game_config.generator);
        let opacity = self
            .scene_transition
            .filter(|transition| transition.to_editor)
            .map_or(1.0, |transition| (transition.fraction() * 2.0).min(1.0));
        egui::Window::new("World Editor")
            .id(egui::Id::new("world-editor"))
            .fixed_pos([16.0, 16.0])
            .fixed_size([324.0, context.content_rect().height() - 32.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(
                garden_card()
                    .inner_margin(10)
                    .multiply_with_opacity(opacity),
            )
            .show(context, |ui| {
                ui.set_opacity(opacity);
                ui.set_width(302.0);
                ui.spacing_mut().item_spacing.y = 5.0;
                ui.label(
                    RichText::new("M.O.W. · WORLD EDITOR")
                        .size(11.0)
                        .color(GARDEN_MUTED),
                );
                ui.heading("Shape a tiny planet");
                ui.label("Slide to tune. Click a number to experiment.");
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height((context.content_rect().height() - 286.0).max(140.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for (label, preset, peaks) in [
                                ("Meadow", WorldEditorSettings::meadow(), 0),
                                ("Classic", classic, 2),
                                ("Craggy", WorldEditorSettings::craggy(), 3),
                            ] {
                                if preset_card(ui, label, self.world_editor == preset, peaks)
                                    .clicked()
                                {
                                    self.world_editor = preset;
                                }
                            }
                        });
                        if ui
                            .add_sized([ui.available_width(), 30.0], egui::Button::new("Random"))
                            .on_hover_text(
                                "Randomize the planet settings within the slider ranges.",
                            )
                            .clicked()
                        {
                            self.world_editor = WorldEditorSettings::random(Self::random_seed());
                        }
                        ui.separator();
                        ui.spacing_mut().slider_width = 208.0;
                        ui.label("Planet radius");
                        ui.add(
                            editor_slider(
                                &mut self.world_editor.planet_radius,
                                WorldEditorSettings::RADIUS_SLIDER_RANGE,
                                1.0..=128.0,
                            )
                            .suffix(" m"),
                        );
                        self.world_editor = self.world_editor.sanitized();
                        ui.add_space(6.0);
                        ui.label("Rockiness");
                        ui.add(
                            editor_slider(
                                &mut self.world_editor.rock_coverage_percent,
                                WorldEditorSettings::ROCKINESS_SLIDER_RANGE,
                                0.0..=90.0,
                            )
                            .step_by(1.0)
                            .suffix("%"),
                        );
                        ui.small("0% gives you an uninterrupted grassy world.");
                        ui.add_space(6.0);
                        ui.add_enabled_ui(self.world_editor.rock_coverage_percent > 0.0, |ui| {
                            ui.label("Peak clusters");
                            ui.add(editor_slider(
                                &mut self.world_editor.peak_clusters,
                                WorldEditorSettings::PEAK_CLUSTERS_SLIDER_RANGE,
                                0..=255,
                            ));
                            ui.add_space(6.0);
                            ui.label("Peak size");
                            ui.add(
                                editor_slider(
                                    &mut self.world_editor.peak_height,
                                    0.0..=WorldEditorSettings::PEAK_HEIGHT_SLIDER_MAX
                                        .min(self.world_editor.planet_radius),
                                    0.0..=self.world_editor.planet_radius,
                                )
                                .suffix(" m"),
                            );
                        });
                        ui.add_space(6.0);
                        ui.label("Rolling terrain");
                        ui.add(
                            editor_slider(
                                &mut self.world_editor.rolling_amplitude,
                                0.0..=WorldEditorSettings::ROLLING_SLIDER_MAX
                                    .min(self.world_editor.planet_radius * 0.9),
                                0.0..=self.world_editor.planet_radius * 0.9,
                            )
                            .suffix(" m"),
                        );
                        ui.separator();
                        ui.label(RichText::new("Planet seed").strong());
                        ui.add(
                            egui::TextEdit::singleline(&mut self.seed_text).desired_width(290.0),
                        );
                        ui.horizontal(|ui| {
                            if ui.button("New seed").clicked() {
                                self.seed_text = Self::random_seed().to_string();
                            }
                            if ui.button("Copy").clicked() {
                                context.copy_text(self.seed_text.clone());
                            }
                            if let Some(recipe) = self.editor_recipe() {
                                let favorite = self.profile.favorite_seeds.contains(&(
                                    lawn_core::planet::CURRENT_GENERATOR_VERSION.0,
                                    recipe.seed.0,
                                ));
                                if ui
                                    .button(if favorite {
                                        "★ Saved"
                                    } else {
                                        "☆ Favorite"
                                    })
                                    .clicked()
                                {
                                    commands.push(UiCommand::ToggleFavorite(recipe.seed));
                                }
                            }
                        });
                        ui.small("A number, hexadecimal seed, or memorable phrase.");
                        let recent: Vec<_> =
                            self.profile.recent_seeds.iter().take(4).copied().collect();
                        if !recent.is_empty() {
                            ui.label("Recent seeds");
                            for key in recent {
                                if ui.small_button(key.seed.to_string()).clicked() {
                                    self.seed_text = key.seed.to_string();
                                }
                            }
                        }
                    });
                ui.separator();
                let recipe = self.editor_recipe();
                let ready = recipe.is_some()
                    && recipe == self.preview_recipe
                    && self.pending_generation.is_none();
                ui.horizontal(|ui| {
                    if ready {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(9.0, 18.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 3.0, GARDEN_PINE);
                        ui.colored_label(GARDEN_PINE, "Live preview");
                    } else if recipe.is_none() {
                        ui.colored_label(GARDEN_ERROR, "Enter a seed to preview your planet.");
                    } else if self.pending_generation.is_none() && self.preview_attempt == recipe {
                        if ui.button("Retry preview").clicked() {
                            self.preview_attempt = None;
                        }
                    } else {
                        ui.spinner();
                        ui.label("Updating planet…");
                    }
                });
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.selected_mode, GameMode::TurfRace, "Turf race");
                    ui.selectable_value(&mut self.selected_mode, GameMode::FreeMow, "Free mow");
                });
                ui.small(if self.selected_mode == GameMode::TurfRace {
                    "Fresh grass is yours. First past 50% wins."
                } else {
                    "Just you and a lovely lawn. Take your time."
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let label = if self.selected_mode == GameMode::TurfRace {
                        "Start Race"
                    } else {
                        "Start Mowing"
                    };
                    if garden_button(ui, label, [200.0, 34.0], ready).clicked() {
                        commands.push(UiCommand::Start(recipe.unwrap().seed));
                    }
                    if ui.button("Back").clicked() {
                        commands.push(UiCommand::Back);
                    }
                });
            });
    }

    fn draw_editor_inspection(&mut self, context: &egui::Context) {
        let screen = context.content_rect();
        egui::Area::new("planet-inspection".into())
            .fixed_pos([
                EDITOR_SCENE_INSET + (screen.width() - EDITOR_SCENE_INSET - 310.0) * 0.5,
                screen.bottom() - 70.0,
            ])
            .show(context, |ui| {
                garden_card().inner_margin(10).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Inspect").color(GARDEN_MUTED).size(12.0));
                        if ui.small_button("−").on_hover_text("Zoom out").clicked() {
                            self.editor_zoom = (self.editor_zoom - 0.15).max(0.85);
                        }
                        ui.spacing_mut().slider_width = 78.0;
                        ui.add(
                            egui::Slider::new(&mut self.editor_zoom, 0.85..=1.8).show_value(false),
                        );
                        if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                            self.editor_zoom = (self.editor_zoom + 0.15).min(1.8);
                        }
                        if ui
                            .small_button("Reset")
                            .on_hover_text("Restore the shared scale for comparing planet sizes")
                            .clicked()
                        {
                            self.editor_zoom = 1.0;
                        }
                        if icon_button(ui, Icon::Rotate(false), "Rotate planet left").clicked() {
                            self.editor_orbit -= 0.35;
                        }
                        if icon_button(ui, Icon::Rotate(true), "Rotate planet right").clicked() {
                            self.editor_orbit += 0.35;
                        }
                    });
                });
            });
    }

    fn draw_loading(context: &egui::Context) {
        egui::Area::new("loading".into())
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                garden_card().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.heading("Growing a tiny planet…");
                    });
                    ui.label("One small mower. One giant lawn.");
                });
            });
    }

    fn draw_hud(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let victory_lap = self.run.is_victory_lap();
        let reduced_motion = self.profile.settings.accessibility.reduced_motion;
        let arrival = self.scene_transition.map_or(1.0, SceneTransition::fraction);
        let hud_opacity = if reduced_motion {
            1.0
        } else {
            ((arrival - 0.25) / 0.75).clamp(0.0, 1.0)
        };
        let gamepad = self.input.last_device_label == "Gamepad";
        let coverage = self.run.mowing.display_coverage_percent() as f32;
        let boost = (self.run.vehicle.state.boost_charge
            / self.run.vehicle_tuning.boost_capacity_seconds)
            .clamp(0.0, 1.0);
        let active = self.run.vehicle.state.boost_active;
        let boost_label = action_label(&self.profile.settings.controls, Action::Boost, gamepad);
        let accent = if reduced_motion {
            0.0
        } else {
            self.hud_milestone
                .map_or(0.0, |(_, age)| (1.0 - age).max(0.0))
        };
        egui::Area::new("hud".into())
            .fixed_pos([18.0 - (1.0 - hud_opacity) * 12.0, 18.0])
            .show(context, |ui| {
                ui.set_opacity(hud_opacity);
                garden_card().inner_margin(12).show(ui, |ui| {
                    if self.run.mode == GameMode::TurfRace && !victory_lap {
                        ui.label(RichText::new("YOUR MOWER").size(11.0).color(GARDEN_MUTED));
                    } else {
                        coverage_dial(ui, coverage, accent);
                    }
                    if self.profile.settings.accessibility.boost_enabled {
                        let flash = if reduced_motion {
                            0.0
                        } else {
                            (1.0 - self.boost_ready_age / 0.8).max(0.0)
                        };
                        boost_meter(ui, boost, active, flash);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            keycap(ui, &boost_label);
                            ui.label(
                                RichText::new(if active {
                                    "A little extra zip"
                                } else if boost >= 0.995 {
                                    "Boost ready"
                                } else {
                                    "Recharging"
                                })
                                .size(11.0)
                                .color(GARDEN_MUTED),
                            );
                        });
                    }
                });
            });
        egui::Area::new("pause-button".into())
            .anchor(Align2::RIGHT_TOP, [-18.0, 18.0])
            .show(context, |ui| {
                garden_card().inner_margin(5).show(ui, |ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::click());
                    if response.hovered() || response.has_focus() {
                        ui.painter().rect_filled(rect, 9, GARDEN_SAGE);
                    }
                    icon(ui.painter(), rect.center(), 22.0, Icon::Pause, GARDEN_PINE);
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Pause")
                    });
                    if response.on_hover_text("Pause").clicked() {
                        commands.push(UiCommand::Pause);
                    }
                });
            });
        egui::Area::new("telemetry".into())
            .anchor(Align2::RIGHT_BOTTOM, [-20.0, -20.0])
            .show(context, |ui| {
                ui.set_opacity(hud_opacity);
                egui::Frame::NONE
                    .fill(GARDEN_CREAM.gamma_multiply(0.92))
                    .corner_radius(20)
                    .inner_margin(egui::Margin::symmetric(12, 7))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{:.0}",
                                    self.run.vehicle.state.speed() * 3.6
                                ))
                                .font(display(22.0)),
                            );
                            ui.label(RichText::new("km/h").size(10.0).color(GARDEN_MUTED));
                        });
                    });
            });
        if victory_lap {
            self.draw_victory_hud(context, commands);
        } else if self.run.mode == GameMode::TurfRace {
            self.draw_race_hud(context, hud_opacity);
        }
        if self.run.mode != GameMode::TurfRace || victory_lap {
            if let Some((percent, age)) = self.hud_milestone {
                egui::Area::new("milestone".into())
                    .anchor(Align2::CENTER_TOP, [0.0, 24.0])
                    .show(context, |ui| {
                        let fade = (age / 0.15).min(1.0) * ((3.0 - age) / 0.4).min(1.0);
                        ui.set_opacity(if reduced_motion { 1.0 } else { fade });
                        garden_card().inner_margin(12).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(22.0, 22.0),
                                    egui::Sense::hover(),
                                );
                                icon(ui.painter(), rect.center(), 20.0, Icon::Leaf, GARDEN_PINE);
                                ui.label(
                                    RichText::new(match percent {
                                        100 => "World mown.".to_owned(),
                                        50 => "Half a world, freshly mown.".to_owned(),
                                        _ => format!("{percent}% — a lovely little lawn."),
                                    })
                                    .font(display(20.0)),
                                );
                            });
                        });
                    });
            } else if let Some(direction) = self.run.locator_direction() {
                let camera = self.run.camera.state;
                let camera_forward =
                    (camera.target - camera.position).normalize_or(glam::Vec3::NEG_Z);
                let screen_right = camera_forward.cross(camera.up).normalize_or(glam::Vec3::X);
                let screen_up = screen_right.cross(camera_forward);
                let forward = direction.dot(screen_up);
                let side = direction.dot(screen_right);
                egui::Area::new("locator".into())
                    .anchor(Align2::CENTER_TOP, [0.0, 24.0])
                    .show(context, |ui| {
                        let size = if self.profile.settings.accessibility.enlarged_locator {
                            27.0
                        } else {
                            18.0
                        };
                        garden_card().inner_margin(10).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(size, size),
                                    egui::Sense::hover(),
                                );
                                icon(
                                    ui.painter(),
                                    rect.center(),
                                    size,
                                    Icon::Arrow(side.atan2(forward)),
                                    GARDEN_PINE,
                                );
                                ui.label(
                                    RichText::new("A little grass this way")
                                        .size(size * 0.72)
                                        .color(GARDEN_PINE),
                                );
                            });
                        });
                    });
            }
        }
        if self.scene_transition.is_some() {
            egui::Area::new("arrival".into())
                .anchor(Align2::CENTER_BOTTOM, [0.0, -28.0])
                .show(context, |ui| {
                    garden_card().inner_margin(12).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(if self.run.mode == GameMode::TurfRace {
                                    "One world. Two mowers."
                                } else {
                                    "A world awaits its mower."
                                })
                                .font(display(21.0)),
                            );
                            if ui.button("Start now").clicked() {
                                commands.push(UiCommand::SkipArrival);
                            }
                        });
                    });
                });
        } else if self.run.tutorial_enabled
            && let Some(prompt) = self.run.tutorial_stage.prompt()
            && (self.run.tutorial_stage != TutorialStage::Recovery
                || self.run.vehicle.state.stuck_seconds >= 1.0)
        {
            egui::Area::new("tutorial".into())
                .anchor(Align2::CENTER_BOTTOM, [0.0, -28.0])
                .show(context, |ui| {
                    garden_card().inner_margin(12).show(ui, |ui| {
                        ui.label(RichText::new(prompt).size(16.0));
                    });
                });
        }
        if self.run.mode == GameMode::Standard && self.run.completion_available {
            egui::Area::new("submit".into())
                .anchor(Align2::CENTER_BOTTOM, [0.0, -90.0])
                .show(context, |ui| {
                    garden_card().show(ui, |ui| {
                        if garden_button(ui, "Submit Job (Enter)", [260.0, 40.0], true).clicked() {
                            commands.push(UiCommand::Submit);
                        }
                        ui.label("or continue mowing to 100%");
                    });
                });
        }
    }

    fn draw_pause(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let victory_lap = self.run.is_victory_lap();
        egui::Window::new("Paused")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(garden_card().inner_margin(24))
            .show(context, |ui| {
                ui.set_min_width(300.0);
                ui.label(
                    RichText::new("M.O.W. · PAUSED")
                        .size(11.0)
                        .color(GARDEN_MUTED),
                );
                ui.label(RichText::new("The world can wait.").font(display(32.0)));
                if let Some(race) = &self.run.race {
                    ui.label(
                        RichText::new(format!(
                            "You {:.1}% · Rival {:.1}% · {} bumps",
                            race.player_coverage * 100.0,
                            race.rival_coverage * 100.0,
                            race.bumps
                        ))
                        .size(12.0)
                        .color(GARDEN_MUTED),
                    );
                } else {
                    ui.label(
                        RichText::new(format!(
                            "{:.1}% mown · {} impacts · {} recoveries",
                            self.run.mowing.display_coverage_percent(),
                            self.run.metrics.substantial_collision_count(),
                            self.run.metrics.recoveries,
                        ))
                        .size(12.0)
                        .color(GARDEN_MUTED),
                    );
                }
                ui.add_space(8.0);
                ui.label("Hold Recover at any time if the mower is stuck or overturned.");
                let resume_button = garden_button(
                    ui,
                    if victory_lap { "Keep mowing" } else { "Resume" },
                    [300.0, 38.0],
                    true,
                );
                if self.input.last_device_label == "Gamepad"
                    && !ui.ctx().egui_wants_keyboard_input()
                {
                    resume_button.request_focus();
                }
                if resume_button.clicked() {
                    commands.push(if victory_lap {
                        UiCommand::KeepMowing
                    } else {
                        UiCommand::Resume
                    });
                }
                if victory_lap && ui.button("View race result").clicked() {
                    commands.push(UiCommand::ShowVictory);
                }
                if ui.button("Settings & Accessibility").clicked() {
                    commands.push(UiCommand::OpenSettings);
                }
                if ui
                    .button(if victory_lap {
                        "Rematch"
                    } else if self.run.mode == GameMode::TurfRace {
                        "Restart race"
                    } else {
                        "Regrow this planet"
                    })
                    .clicked()
                {
                    self.confirmation = Some(ConfirmAction::Restart);
                }
                if ui.button("New random planet").clicked() {
                    self.confirmation = Some(ConfirmAction::RandomPlanet);
                }
                if ui.button("Customize planet").clicked() {
                    self.confirmation = Some(ConfirmAction::ReturnToEditor);
                }
            });
    }

    fn draw_results(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        if self.run.mode == GameMode::TurfRace {
            self.draw_race_results(context, commands);
            return;
        }
        let Some(results) = &self.results else { return };
        egui::Window::new("Job complete")
            .anchor(Align2::RIGHT_CENTER, [-54.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(garden_card().inner_margin(24))
            .show(context, |ui| {
                ui.set_min_width(370.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("M.O.W. · JOB COMPLETE")
                            .size(11.0)
                            .color(GARDEN_MUTED),
                    );
                    ui.label(RichText::new("Planetary service complete.").font(display(30.0)));
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(150.0, 42.0), egui::Sense::hover());
                    for index in 0..3 {
                        icon(
                            ui.painter(),
                            rect.center() + egui::vec2((index as f32 - 1.0) * 40.0, 0.0),
                            30.0,
                            Icon::Star,
                            if index < results.stars {
                                GARDEN_CORAL
                            } else {
                                GARDEN_SAGE
                            },
                        );
                    }
                    ui.label(
                        RichText::new(format!("Planet {}", results.world_seed))
                            .size(12.0)
                            .color(GARDEN_MUTED),
                    );
                });
                ui.add_space(12.0);
                egui::Grid::new("results-grid")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("Coverage");
                        ui.label(format!("{:.2}%", results.metrics.coverage * 100.0));
                        ui.end_row();
                        ui.label("Time");
                        ui.label(format_time(results.metrics.elapsed_seconds));
                        ui.end_row();
                        ui.label("Control");
                        ui.label(format!(
                            "{} substantial impacts",
                            results.metrics.substantial_collision_count()
                        ));
                        ui.end_row();
                        ui.label("Efficiency");
                        ui.label(format!("{:.1}%", results.metrics.efficiency() * 100.0));
                        ui.end_row();
                        ui.label("Recoveries");
                        ui.label(results.metrics.recoveries.to_string());
                        ui.end_row();
                        ui.label("Distance");
                        ui.label(format!("{:.0} m", results.metrics.distance_traveled));
                        ui.end_row();
                    });
                if let Some(best) = self.profile.records.get(&RecordKey {
                    version: results.generator_version,
                    seed: results.world_seed,
                }) {
                    ui.separator();
                    ui.label(RichText::new("Personal best").strong());
                    ui.label(format!(
                        "{} stars · {} · {:.1}% coverage · {:.0}% efficiency",
                        best.best_rating,
                        best.best_time_seconds
                            .map_or_else(|| "—".into(), format_time),
                        best.highest_coverage * 100.0,
                        best.best_efficiency * 100.0,
                    ));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Retry seed").clicked() {
                        commands.push(UiCommand::Retry);
                    }
                    if ui.button("Customize planet").clicked() {
                        commands.push(UiCommand::ReturnToEditor);
                    }
                });
            });
    }

    fn draw_settings(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let content_width = (context.content_rect().width() - 80.0).clamp(300.0, 600.0);
        egui::Window::new("Settings & Accessibility")
            .id(egui::Id::new("garden-settings"))
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .default_width(content_width)
            .title_bar(false)
            .frame(garden_card().inner_margin(22))
            .show(context, |ui| {
                ui.set_width(content_width);
                ui.label(RichText::new("M.O.W. · SETTINGS & ACCESSIBILITY").size(11.0).color(GARDEN_MUTED));
                ui.label(RichText::new("Make yourself at home.").font(display(32.0)));
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .id_salt("settings-scroll")
                    .max_height((context.content_rect().height() - 300.0).max(160.0))
                    .max_width(content_width)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    ui.set_max_width(content_width);
                    if !self.profile_write_enabled {
                        ui.colored_label(GARDEN_ERROR, "Changes apply for this session only: the existing profile could not be loaded.");
                    }
                    ui.heading("Camera & controls");
                    let a = &mut self.profile.settings.accessibility;
                    ui.add(egui::Slider::new(&mut a.camera_shake, 0.0..=1.0).text("Camera shake"));
                    ui.add(egui::Slider::new(&mut a.camera_tilt_degrees, 0.0..=18.0).text("Camera tilt"));
                    ui.add(egui::Slider::new(&mut a.field_of_view_degrees, 60.0..=120.0).text("Field of view"));
                    ui.add(egui::Slider::new(&mut a.camera_follow_stiffness, 1.0..=20.0).text("Follow stiffness"));
                    ui.add(egui::Slider::new(&mut a.steering_sensitivity, 0.25..=2.0).text("Horizontal movement sensitivity"));
                    ui.checkbox(&mut a.invert_steering, "Invert horizontal movement");
                    ui.checkbox(&mut a.invert_camera_y, "Invert camera Y");
                    ui.checkbox(&mut a.fixed_horizon, "Fixed-horizon comfort mode");
                    ui.checkbox(&mut a.boost_enabled, "Enable boost");
                    ui.separator();
                    ui.heading("Visual accessibility");
                    ui.checkbox(&mut a.high_contrast_grass, "High-contrast cut grass");
                    ui.checkbox(&mut a.reduced_particles, "Reduced particles");
                    ui.checkbox(&mut a.reduced_motion, "Reduced motion")
                        .on_hover_text("Skip camera arrivals and stop decorative motion, camera shake, and interface pulses.");
                    ui.checkbox(&mut a.enlarged_locator, "Enlarged uncut-grass locator");
                    ui.add(
                        egui::Slider::new(
                            &mut self.profile.settings.grass_height_multiplier,
                            0.4..=1.6,
                        )
                        .text("Grass height")
                        .suffix("×"),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Quality");
                        egui::ComboBox::from_id_salt("quality").selected_text(format!("{:?}", self.profile.settings.quality)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.profile.settings.quality, QualityPreset::Low, "Low");
                            ui.selectable_value(&mut self.profile.settings.quality, QualityPreset::Standard, "Standard");
                            ui.selectable_value(&mut self.profile.settings.quality, QualityPreset::High, "High");
                        });
                    });
                    ui.add(
                        egui::Slider::new(&mut self.profile.settings.render_scale, 0.5..=1.0)
                            .text("World render scale"),
                    );
                    ui.horizontal(|ui| {
                        ui.label("MSAA").on_hover_text("The renderer uses a supported sample count if the selected value is unavailable on this GPU.");
                        for samples in [1, 2, 4] {
                            ui.selectable_value(
                                &mut self.profile.settings.msaa_samples,
                                samples,
                                format!("{samples}×"),
                            );
                        }
                    });
                    let restart_action = if cfg!(target_arch = "wasm32") {
                        "Reload the page"
                    } else {
                        "Restart the game"
                    };
                    ui.small(format!("{restart_action} to apply render scale and MSAA. Quality changes immediately; UI stays at full resolution."));
                    if let Some(renderer) = &self.renderer {
                        let active = renderer.diagnostics();
                        ui.small(format!(
                            "Rendering now: {:?} · {:.0}% world scale · {}× MSAA",
                            active.quality,
                            active.render_scale * 100.0,
                            active.msaa_samples,
                        ));
                    }
                    ui.checkbox(&mut self.profile.settings.fullscreen, "Borderless fullscreen");
                    if ui.button("Apply fullscreen").clicked() { commands.push(UiCommand::ToggleFullscreen); }
                    ui.separator();
                    ui.heading("Remap controls");
                    ui.label("Choose an action, then press any keyboard or gamepad control.");
                    for action in [
                            Action::SteerLeft, Action::SteerRight, Action::Accelerate, Action::BrakeReverse,
                            Action::Boost, Action::LookBehind, Action::Recover,
                            Action::CameraLeft, Action::CameraRight, Action::CameraUp, Action::CameraDown,
                            Action::RecenterCamera, Action::Pause,
                        ] {
                            let name = match action {
                                Action::SteerLeft => "Move left",
                                Action::SteerRight => "Move right",
                                Action::Accelerate => "Move forward",
                                Action::BrakeReverse => "Move backward",
                                Action::Boost => "Boost",
                                Action::LookBehind => "Look behind",
                                Action::Recover => "Recover mower",
                                Action::CameraLeft => "Camera left",
                                Action::CameraRight => "Camera right",
                                Action::CameraUp => "Camera up",
                                Action::CameraDown => "Camera down",
                                Action::RecenterCamera => "Center camera",
                                Action::Pause => "Pause",
                                _ => unreachable!("only displayed actions are listed"),
                            };
                            let label = if self.input.rebind_action == Some(action) {
                                "Press a control…".into()
                            } else {
                                self.profile.settings.controls.bindings.get(&action).map_or_else(|| "Unbound".into(), |list| {
                                    let mut labels = Vec::new();
                                    for label in list.iter().map(settings_binding_label) {
                                        if !labels.contains(&label) { labels.push(label); }
                                    }
                                    labels.join(" · ")
                                })
                            };
                            ui.horizontal(|ui| {
                                ui.add_sized([134.0, 28.0], egui::Label::new(name));
                                let response = ui.add_sized([ui.available_width(), 28.0], egui::Button::new(label).wrap());
                                if response.on_hover_text(format!("Change {name}")).clicked() {
                                    self.input.rebind_action = Some(action);
                                }
                            });
                        }
                    if ui.button("Reset tutorial prompts").clicked() {
                        self.profile.tutorial_completed = false;
                        self.profile.tutorial_reset_requested = true;
                    }
                });
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if garden_button(ui, "Done", [100.0, 34.0], true).clicked() {
                        commands.push(UiCommand::Back);
                    }
                    ui.label(RichText::new("Make it comfortable. Make it yours.").size(12.0).color(GARDEN_MUTED));
                });
            });
    }

    fn draw_confirmation(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let Some(action) = self.confirmation else {
            return;
        };
        egui::Modal::new(egui::Id::new("discard-current-mowing")).show(context, |ui| {
            let racing = self.run.mode == GameMode::TurfRace;
            ui.heading(if racing {
                "Leave this race?"
            } else {
                "Discard current mowing?"
            });
            ui.label(if racing {
                "Both mowers' claims in this race will be cleared."
            } else {
                "Current mowing progress will be lost."
            });
            ui.horizontal(|ui| {
                if ui
                    .button(if racing { "Keep racing" } else { "Keep mowing" })
                    .clicked()
                {
                    self.confirmation = None;
                }
                if ui.button("Discard progress").clicked() {
                    commands.push(match action {
                        ConfirmAction::Restart => UiCommand::Restart,
                        ConfirmAction::ReturnToEditor => UiCommand::ReturnToEditor,
                        ConfirmAction::RandomPlanet => UiCommand::Random,
                    });
                    self.confirmation = None;
                }
            });
        });
    }

    fn draw_diagnostics(&self, context: &egui::Context) {
        if !self.show_diagnostics {
            return;
        }
        let mut times: Vec<_> = self.frame_times_ms.iter().copied().collect();
        times.sort_by(f32::total_cmp);
        let percentile = |fraction: f32| {
            let index = ((times.len().saturating_sub(1)) as f32 * fraction).round() as usize;
            times.get(index).copied().unwrap_or_default()
        };
        egui::Window::new("M.O.W. diagnostics (F3)")
            .anchor(Align2::RIGHT_BOTTOM, [-12.0, -12.0])
            .resizable(false)
            .vscroll(true)
            .show(context, |ui| {
                ui.monospace(format!(
                    "frame interval p50 {:>5.2}  p95 {:>5.2} ms",
                    percentile(0.50),
                    percentile(0.95)
                ));
                ui.monospace(format!(
                    "surface acquire    {:>5.2} ms",
                    self.last_stats.cpu_acquire_milliseconds
                ));
                ui.monospace(format!(
                    "renderer CPU       {:>5.2} ms",
                    self.last_stats.cpu_encode_milliseconds
                ))
                .on_hover_text("CPU work inside the world renderer; excludes simulation, UI, and presentation.");
                if let Some(renderer) = &self.renderer {
                    let active = renderer.diagnostics();
                    let capabilities = renderer.capabilities();
                    if !capabilities.timestamp_queries {
                        ui.monospace("GPU timing         unavailable")
                            .on_hover_text("This adapter or browser does not expose GPU timestamp queries. CPU timings are not GPU timings.");
                    } else if !active.gpu_profiling_enabled {
                        ui.monospace("GPU timing         off");
                    } else if self.last_stats.gpu_frame_milliseconds > 0.0 {
                        ui.monospace(format!(
                            "renderer GPU       {:>5.2} ms",
                            self.last_stats.gpu_frame_milliseconds
                        ))
                        .on_hover_text("GPU timestamps cover the world renderer, including its post-processing. They exclude egui and presentation; results arrive after the measured frame.");
                    } else {
                        ui.monospace("GPU timing         waiting for sample");
                    }
                    if active.gpu_profiling_enabled && self.last_stats.gpu_world_milliseconds > 0.0 {
                        ui.monospace(format!(
                            "GPU passes  i {:>4.2}  s {:>4.2}  w {:>4.2}  c {:>4.2} ms",
                            self.last_stats.gpu_interaction_milliseconds,
                            self.last_stats.gpu_shadow_milliseconds,
                            self.last_stats.gpu_world_milliseconds,
                            self.last_stats.gpu_composite_milliseconds,
                        ))
                        .on_hover_text(
                            "Interaction, shadow, world, composite. Pass spans can overlap and need not sum to the total renderer GPU time.",
                        );
                    }
                    ui.separator();
                    ui.monospace(format!(
                        "framebuffer  {} × {} px",
                        active.surface_size.width, active.surface_size.height,
                    ))
                    .on_hover_text("Physical presentation pixels; the UI renders at this resolution.");
                    ui.monospace(format!(
                        "world        {} × {} px",
                        active.world_size.width, active.world_size.height,
                    ));
                    if let Some(window) = &self.window {
                        ui.monospace(format!("window scale {:.2}×", window.scale_factor()));
                    }
                    ui.monospace(format!(
                        "active       {:?} · {:.0}% scale · {}× MSAA",
                        active.quality,
                        active.render_scale * 100.0,
                        active.msaa_samples,
                    ))
                    .on_hover_text("The configuration currently used by the renderer. Pending render-scale and MSAA selections in Settings apply after restarting or reloading.");
                    ui.separator();
                }
                ui.monospace(format!("patches   {:>7}", self.last_stats.visible_patches));
                ui.monospace(format!("grass draws {:>5}", self.last_stats.grass_draw_calls))
                    .on_hover_text("Grass draw commands submitted by the world pass; excludes terrain, vehicles, shadows, and UI.");
                ui.monospace(format!("tufts     {:>7}", self.last_stats.visible_tufts));
                ui.monospace(format!(
                    "triangles {:>7}",
                    self.last_stats.generated_triangles
                ));
                ui.monospace(format!(
                    "uploads   {:>7}",
                    self.last_stats.mowing_tile_uploads
                ));
                ui.monospace(format!(
                    "clippings {:>7}",
                    self.last_stats.clipping_particles
                ));
                ui.monospace(format!(
                    "heap vecs {:>7}",
                    self.last_stats.transient_allocations
                ));
                if let Some(capabilities) = self
                    .renderer
                    .as_ref()
                    .map(lawn_render::Renderer::capabilities)
                {
                    ui.separator();
                    ui.monospace(format!(
                        "backend   {:?} · {:?} · timestamps {}",
                        capabilities.backend,
                        capabilities.tier,
                        if capabilities.timestamp_queries {
                            "supported"
                        } else {
                            "unavailable"
                        }
                    ));
                    let adapter = &capabilities.adapter;
                    ui.monospace(format!(
                        "GPU       {}",
                        capabilities.adapter_name,
                    ));
                    ui.monospace(format!(
                        "vendor    {} · device {}",
                        adapter.vendor.as_deref().unwrap_or("unavailable"),
                        adapter.device.as_deref().unwrap_or("unavailable"),
                    ));
                    ui.monospace(format!("arch      {}", adapter.architecture.as_deref().unwrap_or("unavailable")));
                    ui.monospace(format!("type      {}", adapter.device_type.as_deref().unwrap_or("unavailable")));
                    ui.monospace(format!("driver    {}", adapter.driver.as_deref().unwrap_or("unavailable")));
                    ui.monospace(format!("driver info {}", adapter.driver_info.as_deref().unwrap_or("unavailable")));
                    ui.monospace(format!(
                        "fallback  {}",
                        match adapter.fallback {
                            Some(true) => "yes",
                            Some(false) => "no",
                            None => "unavailable",
                        },
                    ));
                    ui.small(match adapter.source {
                        lawn_render::AdapterMetadataSource::WgpuDevice => "GPU details reported by the rendering device.",
                        lawn_render::AdapterMetadataSource::BrowserConfiguredDevice => "GPU details reported by the configured browser device. Browsers may hide these fields.",
                    });
                }
                ui.monospace(format!("seed      {}", self.run.planet.world_seed));
                ui.monospace(format!(
                    "hash      {:016X}",
                    self.run.planet.deterministic_hash
                ));
            });
    }

    fn process_commands(&mut self, commands: Vec<UiCommand>, event_loop: &ActiveEventLoop) {
        #[cfg(target_arch = "wasm32")]
        let _ = event_loop;
        for command in commands {
            match command {
                UiCommand::Mow => self.start_mow(),
                UiCommand::OpenEditor | UiCommand::ReturnToEditor => {
                    self.enter_editor();
                }
                UiCommand::OpenSettings => {
                    self.settings_return_state = self.state;
                    self.settings_open = true;
                }
                UiCommand::Back => {
                    if self.settings_open {
                        self.settings_open = false;
                        self.input.rebind_action = None;
                        self.input.clear_transient();
                        self.state = self.settings_return_state;
                    } else {
                        self.return_to_title();
                    }
                    if self.state == GameState::Paused {
                        self.run.paused = true;
                    }
                    self.save_profile();
                }
                UiCommand::Start(seed) => self.start_generation(seed),
                UiCommand::Random => {
                    if self.run.mode == GameMode::TurfRace {
                        self.start_mow();
                    } else {
                        self.start_generation(Self::random_seed());
                    }
                }
                UiCommand::Resume => self.resume(),
                UiCommand::Pause => self.pause(),
                UiCommand::SkipArrival => self.finish_arrival(),
                UiCommand::KeepMowing => self.keep_mowing(),
                UiCommand::ShowVictory => self.show_victory(),
                UiCommand::Restart => self.begin_game(self.run.mode),
                UiCommand::Submit => self.finish_run(),
                UiCommand::Retry => {
                    self.start_generation(self.run.planet.world_seed);
                }
                #[cfg(not(target_arch = "wasm32"))]
                UiCommand::Quit => {
                    self.save_profile();
                    event_loop.exit();
                }
                UiCommand::ToggleFullscreen => {
                    if let Some(window) = &self.window {
                        window.set_fullscreen(
                            self.profile
                                .settings
                                .fullscreen
                                .then_some(Fullscreen::Borderless(None)),
                        );
                    }
                }
                UiCommand::ToggleFavorite(seed) => {
                    self.profile
                        .toggle_favorite(lawn_core::planet::CURRENT_GENERATOR_VERSION, seed);
                    self.save_profile();
                }
            }
        }
        if let Some(renderer) = &mut self.renderer {
            renderer.set_motion_reduction(self.profile.settings.accessibility.reduced_motion);
            renderer.set_visual_options(
                self.profile.settings.quality,
                self.profile.settings.accessibility.high_contrast_grass,
                self.profile.settings.accessibility.reduced_particles
                    || self.profile.settings.accessibility.reduced_motion,
                self.profile.settings.grass_height_multiplier,
            );
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let _span = tracing::debug_span!("display_frame").entered();
        if self.occluded
            || self.window.as_ref().is_some_and(|window| {
                let size = window.inner_size();
                size.width == 0 || size.height == 0
            })
        {
            return;
        }
        if self
            .surface_retry_at
            .is_some_and(|deadline| deadline > Instant::now())
        {
            return;
        }
        self.surface_retry_at = None;
        // Browser gamepad state is sampled once per animation frame, before
        // processing menu actions and simulation input from that same frame.
        #[cfg(target_arch = "wasm32")]
        self.input
            .poll_gamepads(&mut self.profile.settings.controls);
        self.poll_generation();
        if self.state == GameState::Playing {
            // East is recovery during play, not a menu-cancel action.
            let _ = self.input.take_menu_cancel();
        } else {
            let pause_or_back = self.input.take_pause_pressed();
            let cancel = self.input.take_menu_cancel();
            if pause_or_back || cancel {
                let command = if self.confirmation.take().is_some() {
                    None
                } else if self.settings_open {
                    Some(UiCommand::Back)
                } else {
                    match self.state {
                        GameState::Paused => Some(UiCommand::Resume),
                        GameState::WorldEditor => Some(UiCommand::Back),
                        GameState::Results => Some(UiCommand::ReturnToEditor),
                        _ => None,
                    }
                };
                if let Some(command) = command {
                    self.process_commands(vec![command], event_loop);
                }
            }
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_frame);
        self.last_frame = now;
        if self.frame_times_ms.len() == 240 {
            self.frame_times_ms.pop_front();
        }
        self.frame_times_ms
            .push_back(elapsed.as_secs_f32() * 1_000.0);
        self.update_simulation(elapsed);
        let Some(window) = self.window.clone() else {
            return;
        };
        let mut egui_state = self.egui_state.take().expect("egui state initialized");
        let mut raw_input = egui_state.take_egui_input(&window);
        #[cfg(target_arch = "wasm32")]
        if let Some(platform) = &mut self.browser_platform {
            platform.append_input(&mut raw_input);
        }
        if matches!(self.state, GameState::Playing | GameState::Loading) && !self.settings_open {
            self.input.discard_menu_events();
        } else {
            self.input.append_egui_gamepad_events(&mut raw_input);
        }
        let context = egui_state.egui_ctx().clone();
        let mut ui_commands = Vec::new();
        let full_output = context.run_ui(raw_input, |root_ui| {
            let context = root_ui.ctx().clone();
            if self.settings_open {
                self.draw_settings(&context, &mut ui_commands);
            } else {
                ui_commands.extend(self.draw_ui(&context));
            }
            self.draw_diagnostics(&context);
        });
        #[cfg(target_arch = "wasm32")]
        if let Some(platform) = &self.browser_platform {
            platform.handle_output(&full_output.platform_output);
        }
        egui_state.handle_platform_output(&window, full_output.platform_output);
        self.egui_state = Some(egui_state);
        self.process_commands(ui_commands, event_loop);
        self.update_editor_preview();
        let pixels_per_point = context.pixels_per_point();
        let paint_jobs = context.tessellate(full_output.shapes, pixels_per_point);

        let Some(renderer) = &mut self.renderer else {
            return;
        };
        renderer.set_gpu_profiling(self.show_diagnostics);
        let scene_inset = if let Some(transition) = self.scene_transition {
            let to_inset = if transition.to_editor {
                EDITOR_SCENE_INSET
            } else {
                0.0
            };
            transition.from_inset + (to_inset - transition.from_inset) * transition.fraction()
        } else {
            match self.state {
                GameState::WorldEditor => EDITOR_SCENE_INSET,
                GameState::Title => TITLE_SCENE_INSET,
                _ => 0.0,
            }
        };
        renderer.set_scene_left_inset(scene_inset / context.content_rect().width());
        // Texture deltas (especially the first font atlas) are independent of
        // surface acquisition. Upload them before any recoverable early return,
        // or one initial Outdated/Occluded frame would desynchronize egui's
        // texture manager from the GPU renderer permanently.
        {
            let egui_renderer = self
                .egui_renderer
                .as_mut()
                .expect("egui renderer initialized");
            for (id, delta) in &full_output.textures_delta.set {
                egui_renderer.update_texture(renderer.device(), renderer.queue(), *id, delta);
            }
        }
        let acquired = renderer.begin_frame(&mut self.run, self.clock.interpolation_alpha());
        if acquired.is_err() {
            for id in &full_output.textures_delta.free {
                self.egui_renderer
                    .as_mut()
                    .expect("egui renderer initialized")
                    .free_texture(id);
            }
            self.surface_retry_at = Some(Instant::now() + Duration::from_millis(100));
        }
        let mut frame = match acquired {
            Ok(frame) => frame,
            Err(FrameAcquireError::Outdated | FrameAcquireError::Lost) => {
                renderer.resize(renderer.size());
                return;
            }
            Err(FrameAcquireError::Timeout | FrameAcquireError::Occluded) => return,
            Err(FrameAcquireError::Validation) => {
                self.status_message =
                    Some("The graphics surface reported a validation error.".into());
                return;
            }
        };
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [renderer.size().width, renderer.size().height],
            pixels_per_point,
        };
        let egui_renderer = self
            .egui_renderer
            .as_mut()
            .expect("egui renderer initialized");
        let extra = egui_renderer.update_buffers(
            renderer.device(),
            renderer.queue(),
            &mut frame.encoder,
            &paint_jobs,
            &screen,
        );
        renderer.queue().submit(extra);
        {
            let pass = frame
                .encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("user interface"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &frame.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            egui_renderer.render(&mut pass.forget_lifetime(), &paint_jobs, &screen);
        }
        for id in &full_output.textures_delta.free {
            egui_renderer.free_texture(id);
        }
        self.last_stats = frame.stats;
        renderer.finish_frame(frame);
    }

    fn save_profile(&mut self) {
        if !self.profile_write_enabled {
            return;
        }
        self.profile.sanitize();
        if let Err(error) = self.profile_store.save(&self.profile) {
            tracing::warn!(%error, path = %self.profile_store.path().display(), "profile save failed");
            self.status_message = Some(format!("Changes could not be saved: {error}"));
        }
    }
}

impl ApplicationHandler for LawnOrbitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = event_loop;
            self.redraw_pending = false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(error) = self.create_window_and_gpu(event_loop) {
            tracing::error!(%error, "application initialization failed");
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = &self.window else { return };
        if window.id() != window_id {
            return;
        }
        // Keep app and egui focus in agreement. winit 0.30 can emit a false
        // startup notification on macOS even though this is the key window.
        let native_focus = (cfg!(target_os = "macos") && matches!(&event, WindowEvent::Focused(_)))
            .then(|| window.has_focus());
        let event = normalize_focus_event(event, native_focus);
        let rebinding = self.input.rebind_action.is_some();
        let gameplay_key = match &event {
            WindowEvent::KeyboardInput { event, .. }
                if event.state == winit::event::ElementState::Pressed =>
            {
                self.gameplay_owns_key(
                    event.physical_key,
                    self.egui_state
                        .as_ref()
                        .is_some_and(|state| state.egui_ctx().text_edit_focused()),
                )
            }
            _ => false,
        };
        // A focused HUD button must not swallow driving or turn Space/boost
        // into a click. Tab and unbound Enter still navigate/activate the HUD.
        // Releases always reach egui and the adapter to clear held state.
        let rebinding_key = rebinding
            && matches!(&event, WindowEvent::KeyboardInput { event, .. }
                if event.state == winit::event::ElementState::Pressed);
        let consumed = !(gameplay_key || rebinding_key)
            && self.egui_state.as_mut().is_some_and(|state| {
                let response = state.on_window_event(window, &event);
                #[cfg(target_arch = "wasm32")]
                if let Some(platform) = &self.browser_platform {
                    platform.normalize_modifiers(&event, state.egui_input_mut());
                }
                response.consumed
            });
        let release = matches!(&event,
            WindowEvent::KeyboardInput { event, .. } if event.state == winit::event::ElementState::Released
        ) || matches!(
            &event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                ..
            }
        );
        if !matches!(&event, WindowEvent::Focused(_)) && (!consumed || rebinding || release) {
            self.input
                .handle_window_event(&event, &mut self.profile.settings.controls);
        }
        match event {
            WindowEvent::CloseRequested => {
                self.save_profile();
                event_loop.exit();
            }
            WindowEvent::Focused(focused) => self.window_focus_changed(focused),
            WindowEvent::Occluded(occluded) => {
                #[cfg(target_arch = "wasm32")]
                {
                    self.redraw_pending = false;
                }
                self.occluded = occluded;
                if occluded {
                    self.pause();
                } else {
                    self.last_frame = Instant::now();
                    self.surface_retry_at = None;
                }
            }
            WindowEvent::Resized(size) => {
                self.surface_retry_at = None;
                if size.width == 0 || size.height == 0 {
                    self.pause();
                }
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size);
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(window.inner_size());
                }
            }
            WindowEvent::RedrawRequested => {
                #[cfg(target_arch = "wasm32")]
                {
                    self.redraw_pending = false;
                }
                self.redraw(event_loop);
            }
            WindowEvent::KeyboardInput { event, .. }
                if !event.repeat
                    && !rebinding
                    && event.state == winit::event::ElementState::Pressed
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F3) =>
            {
                self.show_diagnostics = !self.show_diagnostics;
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = &self.window else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };
        let size = window.inner_size();
        if self.occluded || size.width == 0 || size.height == 0 {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        if let Some(deadline) = self.surface_retry_at
            && deadline > Instant::now()
        {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.input
                .poll_gamepads(&mut self.profile.settings.controls);
            event_loop.set_control_flow(ControlFlow::Poll);
            window.request_redraw();
        }
        #[cfg(target_arch = "wasm32")]
        {
            // winit schedules request_redraw through requestAnimationFrame.
            // Re-requesting it on every input event cancels and replaces that
            // callback, so keep a single request until it actually arrives.
            event_loop.set_control_flow(ControlFlow::Wait);
            if !self.redraw_pending {
                self.redraw_pending = true;
                window.request_redraw();
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        #[cfg(target_arch = "wasm32")]
        {
            self.redraw_pending = false;
        }
        self.pause();
        self.save_profile();
    }
}

fn normalize_focus_event(event: WindowEvent, native_focus: Option<bool>) -> WindowEvent {
    match event {
        WindowEvent::Focused(reported) => {
            let focused = native_focus.unwrap_or(reported);
            if focused != reported {
                tracing::debug!(
                    reported,
                    focused,
                    "corrected native window focus notification"
                );
            }
            WindowEvent::Focused(focused)
        }
        event => event,
    }
}

fn release_keyboard_focus(context: &egui::Context) {
    context.memory_mut(|memory| {
        if let Some(id) = memory.focused() {
            memory.surrender_focus(id);
        }
    });
}

fn garden_card() -> egui::Frame {
    egui::Frame::NONE
        .fill(GARDEN_CREAM)
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(228, 222, 202)))
        .corner_radius(18)
        .inner_margin(14)
        .shadow(egui::epaint::Shadow {
            offset: [0, 5],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(32),
        })
}

/// Slider travel covers a broad creative range; the number field accepts a
/// wider finite range. Commit typed edits once so typing "100" does not
/// generate intermediate worlds for "1" and "10".
fn editor_slider<Num: egui::emath::Numeric>(
    value: &mut Num,
    slider_range: std::ops::RangeInclusive<Num>,
    input_range: std::ops::RangeInclusive<Num>,
) -> egui::Slider<'_> {
    let min = input_range.start().to_f64();
    let max = input_range.end().to_f64();
    let slider = egui::Slider::from_get_set(
        slider_range.start().to_f64()..=slider_range.end().to_f64(),
        move |new| {
            if let Some(new) = new.filter(|number| number.is_finite()) {
                *value = Num::from_f64(new.clamp(min, max));
            }
            value.to_f64()
        },
    )
    .clamping(egui::SliderClamping::Never)
    .update_while_editing(false);
    if Num::INTEGRAL {
        slider.integer()
    } else {
        slider
    }
}

fn garden_button(ui: &mut egui::Ui, label: &str, size: [f32; 2], enabled: bool) -> egui::Response {
    ui.scope(|ui| {
        // Style each interaction state rather than overriding the button's fill,
        // so hover and keyboard/gamepad focus remain clearly visible.
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.weak_bg_fill = GARDEN_CORAL;
        widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(218, 119, 89));
        widgets.hovered.weak_bg_fill = Color32::from_rgb(255, 172, 127);
        widgets.hovered.bg_stroke = egui::Stroke::new(2.0, GARDEN_PINE);
        widgets.active.weak_bg_fill = Color32::from_rgb(234, 130, 95);
        widgets.active.bg_stroke = egui::Stroke::new(2.0, GARDEN_PINE);
        ui.add_enabled(
            enabled,
            egui::Button::new(RichText::new(label).strong()).min_size(size.into()),
        )
    })
    .inner
}

fn configure_egui_style(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Barlow Condensed Bold".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/BarlowCondensed-Bold.ttf"))
            .into(),
    );
    fonts.font_data.insert(
        "Barlow Condensed Black".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/BarlowCondensed-Black.ttf"))
            .into(),
    );
    fonts.families.insert(
        egui::FontFamily::Name("mow-display".into()),
        vec!["Barlow Condensed Bold".into()],
    );
    fonts.families.insert(
        egui::FontFamily::Name("mow-mark".into()),
        vec!["Barlow Condensed Black".into()],
    );
    context.set_fonts(fonts);
    let mut visuals = egui::Visuals::light();
    visuals.override_text_color = Some(GARDEN_PINE);
    visuals.window_fill = GARDEN_CREAM;
    visuals.panel_fill = GARDEN_CREAM;
    visuals.window_corner_radius = egui::CornerRadius::same(18);
    visuals.menu_corner_radius = egui::CornerRadius::same(10);
    visuals.window_stroke = egui::Stroke::new(1.0, Color32::from_rgb(228, 222, 202));
    visuals.window_shadow = garden_card().shadow;
    visuals.weak_text_color = Some(GARDEN_MUTED);
    visuals.selection.bg_fill = GARDEN_SAGE;
    visuals.selection.stroke = egui::Stroke::new(1.0, GARDEN_PINE);
    visuals.hyperlink_color = GARDEN_PINE;
    visuals.extreme_bg_color = Color32::from_rgb(246, 239, 218);
    visuals.text_edit_bg_color = Some(Color32::from_rgb(255, 253, 244));
    visuals.faint_bg_color = Color32::from_rgb(241, 235, 214);
    visuals.warn_fg_color = Color32::from_rgb(138, 85, 24);
    visuals.error_fg_color = GARDEN_ERROR;
    visuals.slider_trailing_fill = true;
    visuals.text_cursor.stroke = egui::Stroke::new(2.0, GARDEN_PINE);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(9);
        widget.fg_stroke = egui::Stroke::new(1.0, GARDEN_PINE);
        widget.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(214, 217, 189));
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(232, 229, 207);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(234, 237, 215);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(234, 237, 215);
    visuals.widgets.hovered.bg_fill = GARDEN_SAGE;
    visuals.widgets.hovered.weak_bg_fill = GARDEN_SAGE;
    visuals.widgets.active.bg_fill = Color32::from_rgb(180, 203, 148);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(180, 203, 148);
    visuals.widgets.open.bg_fill = GARDEN_SAGE;
    visuals.widgets.open.weak_bg_fill = GARDEN_SAGE;
    context.set_visuals_of(egui::Theme::Dark, visuals.clone());
    context.set_visuals_of(egui::Theme::Light, visuals);
    context.all_styles_mut(|style| {
        style.interaction.selectable_labels = false;
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size.y = 26.0;
        for (text_style, size) in [
            (egui::TextStyle::Heading, 22.0),
            (egui::TextStyle::Body, 15.0),
            (egui::TextStyle::Button, 15.0),
            (egui::TextStyle::Small, 12.0),
        ] {
            style
                .text_styles
                .insert(text_style, egui::FontId::proportional(size));
        }
        style
            .text_styles
            .insert(egui::TextStyle::Heading, display(24.0));
    });
}

fn settings_binding_label(binding: &lawn_core::input::Binding) -> String {
    use lawn_core::input::Binding;
    match binding {
        Binding::Key(key) => match key.as_str() {
            "ShiftLeft" | "ShiftRight" => "Shift",
            "ControlLeft" | "ControlRight" => "Ctrl",
            "AltLeft" | "AltRight" => "Alt",
            "SuperLeft" | "SuperRight" => "Command",
            "ArrowLeft" => "Left arrow",
            "ArrowRight" => "Right arrow",
            "ArrowUp" => "Up arrow",
            "ArrowDown" => "Down arrow",
            "Escape" => "Esc",
            _ => key
                .strip_prefix("Key")
                .or_else(|| key.strip_prefix("Digit"))
                .unwrap_or(key),
        }
        .to_owned(),
        Binding::MouseButton(button) => format!("Mouse {button}"),
        Binding::GamepadButton(button) => match button.as_str() {
            "South" => "A / Cross",
            "East" => "B / Circle",
            "West" => "X / Square",
            "North" => "Y / Triangle",
            "LeftTrigger" => "LB / L1",
            "RightTrigger" => "RB / R1",
            "LeftTrigger2" => "LT / L2",
            "RightTrigger2" => "RT / R2",
            "LeftThumb" => "Left stick press",
            "RightThumb" => "Right stick press",
            "DPadLeft" => "D-pad left",
            "DPadRight" => "D-pad right",
            "DPadUp" => "D-pad up",
            "DPadDown" => "D-pad down",
            "Start" => "Menu",
            "Select" => "View / Share",
            _ => button,
        }
        .to_owned(),
        Binding::GamepadAxis { axis, direction } => {
            let positive = *direction >= 0;
            match axis.as_str() {
                "LeftStickX" => format!("Left stick {}", if positive { "right" } else { "left" }),
                "LeftStickY" => format!("Left stick {}", if positive { "up" } else { "down" }),
                "RightStickX" => format!("Right stick {}", if positive { "right" } else { "left" }),
                "RightStickY" => format!("Right stick {}", if positive { "up" } else { "down" }),
                "LeftZ" | "ButtonLeftTrigger2" => "LT / L2".into(),
                "RightZ" | "ButtonRightTrigger2" => "RT / R2".into(),
                _ => format!("{axis} {}", if positive { "+" } else { "−" }),
            }
        }
    }
}

fn format_time(seconds: f32) -> String {
    let total = seconds.max(0.0) as u32;
    format!(
        "{:02}:{:02}.{:01}",
        total / 60,
        total % 60,
        ((seconds.fract() * 10.0) as u32).min(9)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lawn_core::{
        config::{JobConfig, VehicleTuning},
        input::InputSnapshot,
        planet::{CURRENT_GENERATOR_VERSION, SurfaceMaterial},
        profile::AccessibilitySettings,
    };

    fn preview_app() -> LawnOrbitApp {
        let mut app = LawnOrbitApp::with_config(
            ProfileStore::temporary("editor-preview"),
            Profile::default(),
            GameConfig {
                generator: GeneratorConfig::test_quality(),
                ..GameConfig::default()
            },
        )
        .unwrap();
        app.state = GameState::WorldEditor;
        app
    }

    fn request_preview(app: &mut LawnOrbitApp) {
        app.preview_queued_at = Instant::now().checked_sub(PREVIEW_INTERVAL);
        app.update_editor_preview();
    }

    fn finish_preview(app: &mut LawnOrbitApp) {
        // Wait for the real worker, then deliver its result through the normal
        // polling path, without relying on frame timings or sleeps.
        let pending = app.pending_generation.as_mut().expect("preview requested");
        let result = pending
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send(result).unwrap();
        pending.receiver = receiver;
        app.poll_generation();
    }

    fn drag_camera(app: &mut LawnOrbitApp) {
        use winit::{
            dpi::PhysicalPosition,
            event::{DeviceId, ElementState, MouseButton},
        };
        for event in [
            WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Right,
            },
            WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(0.0, 0.0),
            },
            WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(25.0, 10.0),
            },
        ] {
            app.input
                .handle_window_event(&event, &mut app.profile.settings.controls);
        }
    }

    #[test]
    fn mouse_input_is_consumed_once_independent_of_display_tick_batching() {
        let mut split = preview_app();
        let mut batched = preview_app();
        for app in [&mut split, &mut batched] {
            app.state = GameState::Playing;
            app.run.paused = false;
            drag_camera(app);
        }
        split.update_simulation(Duration::from_millis(5));
        assert_eq!(split.run.simulation_seconds, 0.0);
        split.update_simulation(Duration::from_millis(5));
        batched.update_simulation(Duration::from_millis(10));
        assert_eq!(split.run.camera.state, batched.run.camera.state);

        drag_camera(&mut split);
        drag_camera(&mut batched);
        split.update_simulation(Duration::from_millis(10));
        split.update_simulation(Duration::from_millis(10));
        batched.update_simulation(Duration::from_millis(20));
        assert_eq!(split.run.camera.state, batched.run.camera.state);
    }

    #[test]
    fn focus_pause_stops_simulation_and_discards_queued_input() {
        let mut app = preview_app();
        app.state = GameState::Playing;
        app.run.paused = false;
        drag_camera(&mut app);
        app.update_simulation(Duration::from_millis(5));
        app.pause();
        app.update_simulation(Duration::from_secs(1));
        assert_eq!(app.state, GameState::Paused);
        assert!(app.run.paused);
        assert_eq!(app.run.simulation_seconds, 0.0);
        assert_eq!(app.clock.interpolation_alpha(), 0.0);
        assert_eq!(
            app.input.snapshot(
                &app.profile.settings.controls,
                &app.profile.settings.accessibility
            ),
            InputSnapshot::default()
        );
    }

    fn egui_key_pulse(key: egui::Key) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect()
    }

    fn draw_keyboard_test_frame(
        app: &mut LawnOrbitApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<UiCommand> {
        let mut commands = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 540.0),
                )),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| commands.extend(app.draw_ui(ui.ctx())),
        );
        commands
    }

    #[test]
    fn focused_hud_keeps_driving_keys_and_boost_out_of_button_activation() {
        use winit::keyboard::KeyCode;

        let mut app = preview_app();
        app.state = GameState::Playing;
        let context = egui::Context::default();
        configure_egui_style(&context);
        draw_keyboard_test_frame(&mut app, &context, Vec::new());
        draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Tab));
        // The real Pause button owns egui focus. Previously this made
        // egui-winit consume every driving key until focus was cleared.
        assert!(context.egui_wants_keyboard_input());
        assert!(!context.text_edit_focused());
        for key in [
            KeyCode::KeyW,
            KeyCode::ArrowLeft,
            KeyCode::Space,
            KeyCode::Escape,
        ] {
            assert!(app.gameplay_owns_key(PhysicalKey::Code(key), context.text_edit_focused()));
        }

        // Reproduce the old second failure: sending boost to egui also
        // activates the focused Pause button. Owned presses must bypass it.
        let old_commands =
            draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Space));
        assert!(
            old_commands
                .iter()
                .any(|command| matches!(command, UiCommand::Pause))
        );
        let events = if app.gameplay_owns_key(PhysicalKey::Code(KeyCode::Space), false) {
            Vec::new()
        } else {
            egui_key_pulse(egui::Key::Space)
        };
        let commands = draw_keyboard_test_frame(&mut app, &context, events);
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, UiCommand::Pause))
        );

        // Keyboard access to the HUD survives: Tab focuses and Enter activates.
        for key in [KeyCode::Tab, KeyCode::Enter] {
            assert!(!app.gameplay_owns_key(PhysicalKey::Code(key), false));
        }
        let commands =
            draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Enter));
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, UiCommand::Pause))
        );
    }

    #[test]
    fn gameplay_priority_respects_text_menus_modals_and_remapped_controls() {
        use winit::keyboard::KeyCode;

        let mut app = preview_app();
        app.state = GameState::Playing;
        let key = PhysicalKey::Code(KeyCode::KeyW);
        let context = egui::Context::default();
        let mut seed = String::from("garden seed");
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            ui.text_edit_singleline(&mut seed).request_focus();
        });
        assert!(context.text_edit_focused());
        assert!(!app.gameplay_owns_key(key, context.text_edit_focused()));
        release_keyboard_focus(&context);
        assert!(!context.egui_wants_keyboard_input());
        assert!(app.gameplay_owns_key(key, context.text_edit_focused()));

        app.settings_open = true;
        assert!(!app.gameplay_owns_key(key, false));
        app.settings_open = false;
        app.confirmation = Some(ConfirmAction::Restart);
        assert!(!app.gameplay_owns_key(key, false));
        app.confirmation = None;
        app.input.rebind_action = Some(Action::Accelerate);
        assert!(!app.gameplay_owns_key(key, false));
        app.input.rebind_action = None;
        for state in [
            GameState::Paused,
            GameState::WorldEditor,
            GameState::Results,
        ] {
            app.state = state;
            assert!(!app.gameplay_owns_key(key, false));
        }
        app.state = GameState::Playing;
        app.profile
            .settings
            .controls
            .bindings
            .insert(Action::Accelerate, vec![Binding::Key("KeyZ".into())]);
        assert!(!app.gameplay_owns_key(key, false));
        assert!(app.gameplay_owns_key(PhysicalKey::Code(KeyCode::KeyZ), false));
    }

    #[test]
    fn native_focus_correction_preserves_play_but_real_focus_loss_clears_input() {
        let mut app = preview_app();
        app.state = GameState::Playing;
        app.run.paused = false;
        drag_camera(&mut app);
        let apply_focus = |app: &mut LawnOrbitApp, reported, native| {
            let WindowEvent::Focused(focused) =
                normalize_focus_event(WindowEvent::Focused(reported), native)
            else {
                unreachable!();
            };
            app.window_focus_changed(focused);
        };

        // macOS startup can report false while the native window is key.
        apply_focus(&mut app, false, Some(true));
        assert!(app.input.is_focused());
        assert_eq!(app.state, GameState::Playing);
        assert!(!app.run.paused);
        let snapshot = app.input.snapshot(
            &app.profile.settings.controls,
            &app.profile.settings.accessibility,
        );
        assert_ne!(snapshot.camera_orbit, [0.0; 2]);

        drag_camera(&mut app);
        apply_focus(&mut app, true, Some(false));
        assert!(!app.input.is_focused());
        assert_eq!(app.state, GameState::Paused);
        assert!(app.run.paused);
        assert_eq!(
            app.input.snapshot(
                &app.profile.settings.controls,
                &app.profile.settings.accessibility,
            ),
            InputSnapshot::default()
        );
        app.resume();
        assert_eq!(app.state, GameState::Paused);
        apply_focus(&mut app, true, Some(true));
        assert_eq!(app.state, GameState::Paused);
        app.resume();
        assert_eq!(app.state, GameState::Playing);

        // Platforms without the macOS workaround retain their event value.
        apply_focus(&mut app, false, None);
        assert!(!app.input.is_focused());
        assert_eq!(app.state, GameState::Paused);
    }

    #[test]
    fn title_defaults_to_mow_and_keeps_customization_separate() {
        let mut app = preview_app();
        app.return_to_title();
        let context = egui::Context::default();
        configure_egui_style(&context);
        draw_keyboard_test_frame(&mut app, &context, Vec::new());
        draw_keyboard_test_frame(&mut app, &context, Vec::new());
        let commands =
            draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Enter));
        assert!(matches!(commands.as_slice(), [UiCommand::Mow]));
        draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Tab));
        let commands =
            draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Enter));
        assert!(matches!(commands.as_slice(), [UiCommand::OpenEditor]));
        assert!(app.pending_generation.is_none());
    }

    #[test]
    fn quick_mow_replaces_pending_preview_without_changing_the_custom_lawn() {
        let mut app = preview_app();
        app.profile_write_enabled = false;
        app.world_editor = WorldEditorSettings::meadow();
        app.selected_mode = GameMode::FreeMow;
        app.seed_text = "my quiet garden".into();
        let custom_recipe = app.editor_recipe().unwrap();
        request_preview(&mut app);
        app.return_to_title();
        app.start_mow();
        assert_eq!(app.state, GameState::Loading);
        let quick_recipe = app.pending_generation.as_ref().unwrap().recipe;
        assert_eq!(
            quick_recipe.settings,
            WorldEditorSettings::from_generator(&app.game_config.generator)
        );
        assert_ne!(quick_recipe.seed, custom_recipe.seed);
        finish_preview(&mut app);
        assert_eq!(app.state, GameState::Playing);
        assert_eq!(app.run.mode, GameMode::TurfRace);
        assert_eq!(app.run.planet.world_seed, quick_recipe.seed);
        assert!(app.run.race.is_some() && app.run.rival.is_some());
        assert!(!app.run.tutorial_enabled);
        assert_eq!(app.run.simulation_seconds, 0.0);
        assert_eq!(app.editor_recipe(), Some(custom_recipe));
        assert_eq!(app.seed_text, "my quiet garden");
        assert_eq!(app.selected_mode, GameMode::FreeMow);
        assert_eq!(app.profile.recent_seeds[0].seed, quick_recipe.seed);

        // A rematch uses the active race even though the editor prefers Free Mow.
        app.begin_game(app.run.mode);
        assert_eq!(app.run.mode, GameMode::TurfRace);
        assert_eq!(app.run.planet.world_seed, quick_recipe.seed);
        assert_eq!(app.selected_mode, GameMode::FreeMow);

        app.start_mow();
        finish_preview(&mut app);
        assert_eq!(app.run.mode, GameMode::TurfRace);
        assert_ne!(app.run.planet.world_seed, quick_recipe.seed);
        app.enter_editor();
        request_preview(&mut app);
        finish_preview(&mut app);
        assert_eq!(app.run.planet.world_seed, custom_recipe.seed);
        assert!(app.run.planet.mountains.is_empty());
        let custom_hash = app.run.planet.deterministic_hash;
        app.start_generation(custom_recipe.seed);
        assert_eq!(app.state, GameState::Playing);
        assert_eq!(app.run.mode, GameMode::FreeMow);
        assert_eq!(app.run.planet.deterministic_hash, custom_hash);
        assert!(app.pending_generation.is_none());
    }

    #[test]
    fn quick_mow_failure_returns_to_its_menu_and_preserves_the_previous_lawn() {
        for state in [GameState::Title, GameState::Results] {
            let mut app = preview_app();
            app.profile_write_enabled = false;
            app.state = state;
            let original = app.run.planet.deterministic_hash;
            let custom = app.editor_recipe();
            app.start_mow();
            let (sender, receiver) = mpsc::channel();
            sender.send(Err("test generation failure".into())).unwrap();
            app.pending_generation.as_mut().unwrap().receiver = receiver;
            app.poll_generation();
            assert_eq!(app.state, state);
            assert_eq!(app.run.planet.deterministic_hash, original);
            assert_eq!(app.editor_recipe(), custom);
            assert!(
                app.status_message
                    .as_ref()
                    .unwrap()
                    .contains("test generation failure")
            );
            assert!(app.pending_generation.is_none());
            app.start_mow();
            finish_preview(&mut app);
            assert_eq!(app.state, GameState::Playing);
            assert_eq!(app.run.mode, GameMode::TurfRace);
        }
    }

    #[test]
    fn failed_mow_again_keeps_an_unfocused_victory_lap_paused() {
        let mut app = win_moving_race();
        let seconds = app.run.simulation_seconds;
        app.start_mow();
        app.window_focus_changed(false);
        let (sender, receiver) = mpsc::channel();
        sender.send(Err("test generation failure".into())).unwrap();
        app.pending_generation.as_mut().unwrap().receiver = receiver;
        app.poll_generation();
        assert_eq!(app.state, GameState::Paused);
        assert!(app.run.paused && app.run.is_victory_lap());
        assert!(app.victory_card_open);
        app.update_simulation(Duration::from_secs(1));
        assert_eq!(app.run.simulation_seconds, seconds);
    }

    #[test]
    fn background_quick_mow_does_not_resume_an_unfocused_game() {
        let mut app = preview_app();
        app.profile_write_enabled = false;
        app.return_to_title();
        app.start_mow();
        app.window_focus_changed(false);
        finish_preview(&mut app);
        assert_eq!(app.state, GameState::Paused);
        assert_eq!(app.run.mode, GameMode::TurfRace);
        assert!(app.run.paused);
        app.update_simulation(Duration::from_secs(1));
        assert_eq!(app.run.simulation_seconds, 0.0);
    }

    #[test]
    fn background_generation_does_not_resume_an_unfocused_game() {
        let mut app = preview_app();
        app.start_generation(WorldSeed(42));
        app.input.handle_window_event(
            &WindowEvent::Focused(false),
            &mut app.profile.settings.controls,
        );
        app.pause();
        finish_preview(&mut app);
        assert_eq!(app.state, GameState::Paused);
        assert!(app.run.paused);
        app.update_simulation(Duration::from_secs(1));
        assert_eq!(app.run.simulation_seconds, 0.0);
        std::fs::remove_file(app.profile_store.path()).unwrap();
    }

    #[test]
    fn invalid_simulation_config_is_rejected_before_startup() {
        let mut config = GameConfig {
            generator: GeneratorConfig::test_quality(),
            ..GameConfig::default()
        };
        config.vehicle.mower_width = 0.0;
        assert!(
            LawnOrbitApp::with_config(
                ProfileStore::temporary("bad-config"),
                Profile::default(),
                config
            )
            .is_err()
        );
    }

    #[test]
    fn unreadable_profiles_are_preserved_when_defaults_are_used() {
        for source in [
            "invalid profile".to_owned(),
            ron::to_string(&Profile {
                version: lawn_core::profile::PROFILE_VERSION + 1,
                ..Profile::default()
            })
            .unwrap(),
        ] {
            let store = ProfileStore::temporary("preserve-profile");
            std::fs::write(store.path(), &source).unwrap();
            let mut app = LawnOrbitApp::load_with_config(
                store,
                GameConfig {
                    generator: GeneratorConfig::test_quality(),
                    ..GameConfig::default()
                },
            )
            .unwrap();
            assert!(!app.profile_write_enabled);
            assert!(app.status_message.is_some());
            app.profile.tutorial_completed = true;
            app.save_profile();
            assert_eq!(
                std::fs::read_to_string(app.profile_store.path()).unwrap(),
                source
            );
            std::fs::remove_file(app.profile_store.path()).unwrap();
        }
    }

    #[test]
    fn live_preview_coalesces_edits_and_enters_the_displayed_planet() {
        let mut app = preview_app();
        app.selected_mode = GameMode::FreeMow;
        request_preview(&mut app);
        let first = app.pending_generation.as_ref().unwrap().recipe;
        for seed in 40..50 {
            app.seed_text = seed.to_string();
            app.world_editor = WorldEditorSettings::meadow();
            request_preview(&mut app);
            assert_eq!(app.pending_generation.as_ref().unwrap().recipe, first);
        }
        finish_preview(&mut app);
        assert_eq!(app.preview_recipe, Some(first));
        request_preview(&mut app);
        finish_preview(&mut app);
        assert_eq!(app.run.planet.world_seed, WorldSeed(49));
        assert!(app.run.planet.mountains.is_empty());
        assert_eq!(app.preview_recipe, app.editor_recipe());
        assert_eq!(app.state, GameState::WorldEditor);
        assert!(app.run.paused);
        assert!(!app.run.tutorial_enabled);
        assert!(app.profile.recent_seeds.is_empty());
        assert!(!app.profile_store.path().exists());

        let hash = app.run.planet.deterministic_hash;
        app.start_generation(WorldSeed(49));
        assert_eq!(app.state, GameState::Playing);
        assert!(app.pending_generation.is_none());
        assert_eq!(app.run.planet.deterministic_hash, hash);
        assert!(!app.run.paused);
        assert!(app.run.tutorial_enabled);
        assert!(app.preview_recipe.is_none());
        assert_eq!(app.profile.recent_seeds.len(), 1);

        // Re-entering the editor prepares a fresh lawn even at the same seed.
        app.state = GameState::WorldEditor;
        app.run.simulation_seconds = 12.0;
        request_preview(&mut app);
        finish_preview(&mut app);
        assert_eq!(app.run.simulation_seconds, 0.0);
        assert_eq!(app.run.planet.deterministic_hash, hash);
        std::fs::remove_file(app.profile_store.path()).unwrap();
    }

    #[test]
    fn leaving_the_editor_ignores_inflight_previews() {
        let mut app = preview_app();
        let original = app.run.planet.deterministic_hash;
        app.world_editor = WorldEditorSettings::meadow();
        request_preview(&mut app);
        app.state = GameState::Title;
        finish_preview(&mut app);
        assert_eq!(app.state, GameState::Title);
        assert_eq!(app.run.planet.deterministic_hash, original);
        assert!(app.preview_recipe.is_none());
        assert!(!app.profile_store.path().exists());
    }

    #[test]
    fn preview_worker_failure_does_not_loop_and_a_changed_seed_recovers() {
        let mut app = preview_app();
        let recipe = app.editor_recipe().unwrap();
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        app.preview_attempt = Some(recipe);
        app.pending_generation = Some(PendingGeneration {
            recipe,
            purpose: GenerationPurpose::Preview,
            receiver,
        });
        app.poll_generation();
        assert!(app.status_message.is_some());
        request_preview(&mut app);
        assert!(app.pending_generation.is_none());
        app.seed_text.clear();
        request_preview(&mut app);
        assert!(app.pending_generation.is_none());
        app.seed_text = "a new world".into();
        request_preview(&mut app);
        finish_preview(&mut app);
        assert!(app.status_message.is_none());
        assert_eq!(app.preview_recipe, app.editor_recipe());
    }

    #[test]
    fn zero_rockiness_removes_all_outcroppings_and_retains_mowable_grass() {
        let baseline = GeneratorConfig {
            grass_roots_per_square_meter: 1.0,
            ..GeneratorConfig::test_quality()
        };
        for radius in [12.0, 22.0] {
            for roll in [0.0, 1.2] {
                let settings = WorldEditorSettings {
                    planet_radius: radius,
                    rolling_amplitude: roll,
                    ..WorldEditorSettings::meadow()
                };
                let generator = PlanetGenerator::new(
                    CURRENT_GENERATOR_VERSION,
                    settings.generator_config(&baseline),
                );
                for seed in 0..4 {
                    let planet = generator.generate(WorldSeed(seed)).unwrap();
                    assert!(planet.mountains.is_empty());
                    assert!(
                        planet
                            .terrain
                            .iter()
                            .all(|cell| cell.material == SurfaceMaterial::Grass
                                && cell.mountain_influence == 0.0)
                    );
                    assert_eq!(planet.validation.mowable_ratio, 1.0);
                    assert_eq!(planet.validation.reachable_ratio, 1.0);
                    assert!(!planet.grass_roots.is_empty());
                    assert!(planet.spawn.position.is_finite());
                    let accessibility = AccessibilitySettings {
                        boost_enabled: false,
                        ..AccessibilitySettings::default()
                    };
                    let mut run = RunState::new(
                        planet,
                        GameMode::FreeMow,
                        VehicleTuning::default(),
                        JobConfig::default(),
                        &accessibility,
                        true,
                    );
                    run.tutorial_stage = TutorialStage::Boost;
                    run.tick(InputSnapshot::default(), &accessibility);
                    assert_eq!(run.tutorial_stage, TutorialStage::WaitForLocator);
                }
            }
        }
    }

    #[test]
    fn race_is_default_and_mode_changes_reset_only_the_current_planet() {
        let mut app = preview_app();
        app.profile_write_enabled = false;
        app.profile.settings.accessibility.reduced_motion = true;
        assert_eq!(app.selected_mode, GameMode::TurfRace);
        let planet_hash = app.run.planet.deterministic_hash;
        app.begin_game(app.selected_mode);
        assert_eq!(app.run.mode, GameMode::TurfRace);
        assert!(app.run.rival.is_some());
        assert!(app.run.race.is_some());
        assert_eq!(app.state, GameState::Playing);
        assert!(!app.run.tutorial_enabled);
        app.run.race.as_mut().unwrap().bumps = 7;
        app.run.simulation_seconds = 18.0;
        app.begin_game(app.selected_mode);
        assert_eq!(app.run.race.as_ref().unwrap().bumps, 0);
        assert_eq!(app.run.simulation_seconds, 0.0);
        assert_eq!(app.run.planet.deterministic_hash, planet_hash);
        app.selected_mode = GameMode::FreeMow;
        app.begin_game(app.selected_mode);
        assert_eq!(app.run.mode, GameMode::FreeMow);
        assert!(app.run.rival.is_none());
        assert!(app.run.race.is_none());
        assert!(app.run.tutorial_enabled);
        assert_eq!(app.run.planet.deterministic_hash, planet_hash);
    }

    #[test]
    fn losses_and_draws_open_results_without_job_ratings() {
        for outcome in [RaceOutcome::RivalWon, RaceOutcome::Draw] {
            let mut app = preview_app();
            app.profile_write_enabled = false;
            app.profile.settings.accessibility.reduced_motion = true;
            app.begin_game(app.selected_mode);
            app.run.race.as_mut().unwrap().outcome = Some(outcome);
            app.run.active = false;
            app.update_simulation(Duration::from_millis(17));
            assert_eq!(app.state, GameState::Results);
            assert!(app.run.paused);
            assert_eq!(app.run.race.as_ref().unwrap().outcome, Some(outcome));
            assert!(app.results.is_none());
            assert!(app.profile.records.is_empty());
            assert!(!app.profile.tutorial_completed);
            app.begin_game(app.selected_mode);
            assert_eq!(app.state, GameState::Playing);
            assert_eq!(app.run.race.as_ref().unwrap().outcome, None);
            assert!(app.run.active);
        }
    }

    fn win_moving_race() -> LawnOrbitApp {
        use lawn_core::mowing::MowingStamp;
        use winit::event::{DeviceId, ElementState, MouseButton};

        let mut app = preview_app();
        app.profile_write_enabled = false;
        app.profile.settings.accessibility.reduced_motion = true;
        app.begin_game(app.selected_mode);
        // Hold a real supported binding through the adapter, so a win that
        // clears input would be caught without platform-specific KeyEvent data.
        app.profile
            .settings
            .controls
            .bindings
            .get_mut(&Action::Accelerate)
            .unwrap()
            .push(Binding::MouseButton(2));
        app.input.handle_window_event(
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Right,
            },
            &mut app.profile.settings.controls,
        );
        for _ in 0..60 {
            app.update_simulation(Duration::from_millis(17));
        }
        let previous = app.run.vehicle.state.clone();
        let camera = app.run.camera.state;
        let radius = app.run.planet.config.base_radius;
        let away = -previous.transform.position.normalize() * radius;
        app.run.mowing.stamp_owned(
            MowingStamp {
                from: away,
                to: away,
                comb_direction: glam::Vec3::Y,
                deck_width: 2.0 * radius * (1.8 - 1.5 / app.run.mowing.resolution() as f32),
                cut_delta: 1.0,
                recent_epoch: 0,
            },
            1,
        );
        assert!(app.run.mowing.owned_coverage(1) > 0.5);
        assert!(!app.run.is_victory_lap());
        app.update_simulation(Duration::from_millis(17));
        assert!(app.run.is_victory_lap());
        assert!(app.victory_card_open);
        assert_eq!(app.state, GameState::Playing);
        assert!(app.run.active && !app.run.paused);
        assert!(app.run.rival.is_none());
        assert!(app.scene_transition.is_none());
        assert!(app.clock.interpolation_alpha() > 0.0);
        assert!(app.run.vehicle.state.speed() > 1.0);
        assert!(
            app.run
                .vehicle
                .state
                .transform
                .position
                .distance(previous.transform.position)
                < 1.0
        );
        assert!(app.run.camera.state.position.distance(camera.position) < 1.0);
        assert!(
            app.input
                .snapshot(
                    &app.profile.settings.controls,
                    &app.profile.settings.accessibility
                )
                .accelerate
                > 0.9
        );
        assert_eq!(
            app.run
                .events()
                .iter()
                .filter(|event| matches!(event, RunEvent::RivalDefeated { .. }))
                .count(),
            1
        );
        app
    }

    #[test]
    fn victory_card_keeps_live_mowing_and_frozen_results_then_dismisses_once() {
        let mut app = win_moving_race();
        let finished = app.run.race.as_ref().unwrap();
        let results = (
            finished.player_coverage,
            finished.rival_coverage,
            finished.finished_seconds,
            finished.bumps,
        );
        let coverage = app.run.mowing.coverage();
        let seconds = app.run.simulation_seconds;
        for _ in 0..60 {
            app.update_simulation(Duration::from_millis(17));
        }
        assert!(app.victory_card_open);
        assert!(app.run.mowing.coverage() > coverage);
        assert!(app.run.simulation_seconds > seconds + 1.0);
        let finished = app.run.race.as_ref().unwrap();
        assert_eq!(
            (
                finished.player_coverage,
                finished.rival_coverage,
                finished.finished_seconds,
                finished.bumps
            ),
            results
        );
        assert!(app.results.is_none() && app.profile.records.is_empty());
        assert!(
            !app.run
                .events()
                .iter()
                .any(|event| matches!(event, RunEvent::RivalDefeated { .. }))
        );
        app.keep_mowing();
        for _ in 0..3 {
            app.update_simulation(Duration::from_millis(17));
        }
        assert!(!app.victory_card_open);
        assert_eq!(app.state, GameState::Playing);
        assert!(
            app.input
                .snapshot(
                    &app.profile.settings.controls,
                    &app.profile.settings.accessibility
                )
                .accelerate
                > 0.9
        );
        app.show_victory();
        assert!(app.victory_card_open);
    }

    #[test]
    fn victory_pause_rematch_and_mode_change_reset_presentation_correctly() {
        let mut app = win_moving_race();
        let planet = app.run.planet.deterministic_hash;
        app.pause();
        let seconds = app.run.simulation_seconds;
        app.update_simulation(Duration::from_secs(1));
        assert_eq!(app.run.simulation_seconds, seconds);
        app.keep_mowing();
        assert_eq!(app.state, GameState::Playing);
        assert!(!app.run.paused && !app.victory_card_open);
        app.pause();
        app.show_victory();
        assert_eq!(app.state, GameState::Playing);
        assert!(app.victory_card_open);
        app.begin_game(app.selected_mode);
        assert!(!app.victory_card_open);
        assert!(!app.run.is_victory_lap());
        assert!(app.run.rival.is_some());
        assert_eq!(app.run.race.as_ref().unwrap().finished_seconds, None);
        assert_eq!(app.run.mowing.coverage(), 0.0);
        assert_eq!(app.run.planet.deterministic_hash, planet);
        app.victory_card_open = true;
        app.enter_editor();
        assert!(!app.victory_card_open);
        app.selected_mode = GameMode::FreeMow;
        app.begin_game(app.selected_mode);
        assert!(!app.victory_card_open);
        assert!(app.run.race.is_none() && app.run.rival.is_none());
    }

    #[test]
    fn victory_buttons_preserve_driving_and_gamepad_pause_can_dismiss_the_card() {
        use winit::keyboard::KeyCode;

        let mut app = win_moving_race();
        let context = egui::Context::default();
        configure_egui_style(&context);
        draw_keyboard_test_frame(&mut app, &context, Vec::new());
        draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Tab));
        draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Tab));
        draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Tab));
        assert!(context.egui_wants_keyboard_input());
        assert!(app.gameplay_owns_key(
            PhysicalKey::Code(KeyCode::KeyW),
            context.text_edit_focused()
        ));
        assert!(app.gameplay_owns_key(
            PhysicalKey::Code(KeyCode::Space),
            context.text_edit_focused()
        ));
        let commands =
            draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Enter));
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, UiCommand::KeepMowing))
        );

        app.pause();
        let context = egui::Context::default();
        configure_egui_style(&context);
        app.input.last_device_label = "Gamepad";
        draw_keyboard_test_frame(&mut app, &context, Vec::new());
        assert!(context.egui_wants_keyboard_input());
        let commands =
            draw_keyboard_test_frame(&mut app, &context, egui_key_pulse(egui::Key::Enter));
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, UiCommand::KeepMowing))
        );
        app.keep_mowing();
        assert_eq!(app.state, GameState::Playing);
        assert!(!app.victory_card_open);
    }

    #[test]
    fn arrival_keeps_simulation_and_mowing_frozen_and_can_be_skipped() {
        let mut app = preview_app();
        app.profile_write_enabled = false;
        let context = egui::Context::default();
        app.animate_editor_camera(&context);
        let camera_before = app.run.camera.state;
        let coverage_before = app.run.mowing.coverage();
        let cells_before = app.run.mowing.snapshot().cells;
        let vehicle_before = app.run.vehicle.state.transform;
        app.enter_sandbox();
        for _ in 0..12 {
            app.update_simulation(Duration::from_millis(33));
        }
        assert!(app.scene_transition.is_some());
        assert_ne!(app.run.camera.state.position, camera_before.position);
        assert_eq!(app.run.simulation_seconds, 0.0);
        assert_eq!(app.run.mowing.coverage(), coverage_before);
        assert_eq!(app.run.mowing.snapshot().cells, cells_before);
        assert_eq!(app.run.vehicle.state.transform, vehicle_before);
        app.finish_arrival();
        assert!(app.scene_transition.is_none());
        let camera = CameraRig::new(
            vehicle_before,
            app.run.planet.config.base_radius,
            &app.profile.settings.accessibility,
        );
        assert_eq!(app.run.camera.state, camera.state);
        app.update_simulation(Duration::from_millis(17));
        assert!(app.run.simulation_seconds > 0.0);
    }

    #[test]
    fn returning_to_title_abandons_an_unfinished_editor_transition() {
        let mut app = preview_app();
        app.state = GameState::Title;
        app.enter_editor();
        app.update_simulation(Duration::from_millis(50));
        assert!(app.scene_transition.is_some());
        app.return_to_title();
        app.update_simulation(Duration::from_millis(50));
        assert_eq!(app.state, GameState::Title);
        assert!(app.scene_transition.is_none());
    }

    #[test]
    fn reduced_motion_skips_arrival_and_holds_editor_orbit() {
        let mut app = preview_app();
        app.profile_write_enabled = false;
        app.profile.settings.accessibility.reduced_motion = true;
        let orbit = app.editor_orbit;
        app.update_simulation(Duration::from_millis(50));
        assert_eq!(app.editor_orbit, orbit);
        app.enter_sandbox();
        assert!(app.scene_transition.is_none());
        app.update_simulation(Duration::from_millis(17));
        assert!(app.run.simulation_seconds > 0.0);
        app.enter_editor();
        assert!(app.scene_transition.is_none());
    }

    #[test]
    fn editor_zoom_keeps_a_shared_scale_and_camera_outside_the_world() {
        let mut app = preview_app();
        let context = egui::Context::default();
        app.run.planet.config.base_radius = 12.0;
        app.animate_editor_camera(&context);
        let small_distance = app.run.camera.state.position.length();
        app.run.planet.config.base_radius = 22.0;
        app.animate_editor_camera(&context);
        assert!((small_distance - app.run.camera.state.position.length()).abs() < 0.001);
        app.editor_zoom = 1.8;
        app.animate_editor_camera(&context);
        let inspection_distance = app.run.camera.state.position.length();
        assert!(inspection_distance < small_distance);
        assert!(inspection_distance >= 35.99);
    }

    #[test]
    fn transition_roll_stays_continuous_between_opposite_endpoint_ups() {
        let from = CameraState {
            position: glam::Vec3::Z * 50.0,
            target: glam::Vec3::ZERO,
            up: glam::Vec3::NEG_Y,
            field_of_view_degrees: 90.0,
        };
        // Include a pure roll, an orbit with an off-center look target, and
        // the opposite hemisphere. Every endpoint up is valid and opposite.
        for (position, target) in [
            (glam::Vec3::Z * 38.0, glam::Vec3::ZERO),
            (
                glam::Vec3::new(40.0, 0.0, -30.0),
                glam::Vec3::new(5.0, 0.0, -3.0),
            ),
            (glam::Vec3::NEG_Z * 40.0, glam::Vec3::ZERO),
        ] {
            let to = CameraState {
                position,
                target,
                up: glam::Vec3::Y,
                ..from
            };
            let projected_frame = |pose: CameraState| {
                let view = (pose.target - pose.position).normalize();
                let right = view.cross(pose.up).normalize();
                let up = right.cross(view).normalize();
                (view, right, up)
            };
            let (_, mut previous_right, mut previous_up) = projected_frame(from);
            for sample in 0..=256 {
                let transition = SceneTransition {
                    from,
                    to,
                    elapsed: ARRIVAL_SECONDS * sample as f32 / 256.0,
                    from_inset: 0.0,
                    to_editor: true,
                };
                let pose = transition.pose();
                let (view, right, up) = projected_frame(pose);
                assert!(pose.up.is_normalized());
                assert!(pose.up.dot(view).abs() < 0.0001);
                assert!(
                    right.dot(previous_right) > 0.995,
                    "right vector jumped at sample {sample}"
                );
                assert!(
                    up.dot(previous_up) > 0.995,
                    "up vector jumped at sample {sample}"
                );
                previous_right = right;
                previous_up = up;
            }
            let (_, expected_right, expected_up) = projected_frame(to);
            assert!(previous_right.dot(expected_right) > 0.9999);
            assert!(previous_up.dot(expected_up) > 0.9999);
        }
    }

    #[test]
    fn transition_goes_around_the_planet_even_between_opposite_hemispheres() {
        let from = CameraState {
            position: glam::Vec3::Y * 50.0,
            target: glam::Vec3::ZERO,
            up: glam::Vec3::Z,
            field_of_view_degrees: 90.0,
        };
        let to = CameraState {
            position: glam::Vec3::NEG_Y * 30.0,
            ..from
        };
        for step in 0..=20 {
            let transition = SceneTransition {
                from,
                to,
                elapsed: ARRIVAL_SECONDS * step as f32 / 20.0,
                from_inset: 0.0,
                to_editor: false,
            };
            let pose = transition.pose();
            assert!(pose.position.length() >= 29.99);
            assert!(pose.position.is_finite());
            assert!(pose.up.is_normalized());
        }
    }

    #[test]
    fn settings_window_and_done_action_fit_supported_window_sizes() {
        fn text_rect(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    (text.galley.text() == label).then_some(text.visual_bounding_rect())
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| text_rect(shape, label))
                }
                _ => None,
            }
        }
        let mut app = preview_app();
        // Several readable bindings still require wrapping; retain all of
        // them rather than truncating the remapping information to fit.
        app.profile
            .settings
            .controls
            .bindings
            .get_mut(&Action::Accelerate)
            .unwrap()
            .push(lawn_core::input::Binding::GamepadButton(
                "Additional controller forward control".into(),
            ));
        for size in [egui::vec2(960.0, 540.0), egui::vec2(1280.0, 720.0)] {
            let context = egui::Context::default();
            configure_egui_style(&context);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            for frame in 0..8 {
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        time: Some(f64::from(frame)),
                        ..Default::default()
                    },
                    |ui| {
                        app.draw_settings(ui.ctx(), &mut Vec::new());
                    },
                );
                if frame < 7 {
                    continue;
                }
                let window = context
                    .memory(|memory| memory.area_rect(egui::Id::new("garden-settings")))
                    .unwrap();
                assert!(
                    screen.contains_rect(window),
                    "settings overflows {size:?}: {window:?}"
                );
                assert!(
                    window.width() <= 646.0,
                    "bindings expanded the settings window: {window:?}"
                );
                for label in ["M.O.W. · SETTINGS & ACCESSIBILITY", "Done"] {
                    let (clip, text) = output
                        .shapes
                        .iter()
                        .find_map(|shape| {
                            text_rect(&shape.shape, label).map(|rect| (shape.clip_rect, rect))
                        })
                        .expect("settings action is painted");
                    assert!(clip.contains_rect(text), "{label} is clipped at {size:?}");
                    assert!(
                        screen.contains_rect(text),
                        "{label} is offscreen at {size:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn editor_layout_keeps_controls_and_play_action_beside_the_planet() {
        fn play_text_rect(shape: &egui::epaint::Shape) -> Option<egui::Rect> {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    matches!(text.galley.text(), "Start Race" | "Start Mowing")
                        .then_some(text.visual_bounding_rect())
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(play_text_rect),
                _ => None,
            }
        }
        let mut app = preview_app();
        app.preview_recipe = app.editor_recipe();
        for seed in 0..4 {
            app.profile
                .record_seed(CURRENT_GENERATOR_VERSION, WorldSeed(seed));
        }
        for size in [
            egui::vec2(960.0, 540.0),
            egui::vec2(1280.0, 720.0),
            egui::vec2(1920.0, 1080.0),
        ] {
            let context = egui::Context::default();
            configure_egui_style(&context);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            for frame in 0..3 {
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        time: Some(f64::from(frame)),
                        ..egui::RawInput::default()
                    },
                    |ui| {
                        app.draw_ui(ui.ctx());
                    },
                );
                if frame < 2 {
                    continue;
                }
                let panel = context
                    .memory(|memory| memory.area_rect(egui::Id::new("world-editor")))
                    .unwrap();
                assert!(
                    panel.right() < EDITOR_SCENE_INSET,
                    "panel overlaps planet at {size:?}: {panel:?}"
                );
                assert!(
                    screen.contains_rect(panel),
                    "panel clipped at {size:?}: {panel:?}"
                );
                let play_text = output
                    .shapes
                    .iter()
                    .find_map(|shape| {
                        play_text_rect(&shape.shape).map(|rect| (shape.clip_rect, rect))
                    })
                    .expect("play action is painted");
                assert!(
                    play_text.0.contains_rect(play_text.1),
                    "play action is clipped at {size:?}: panel {panel:?}, text/clip {play_text:?}"
                );
                assert!(
                    screen.contains_rect(play_text.1),
                    "play action is offscreen at {size:?}"
                );
            }
        }
    }

    #[test]
    fn editor_number_entry_exceeds_slider_travel_but_keeps_finite_limits() {
        let context = egui::Context::default();
        configure_egui_style(&context);
        let frame = |value: &mut f64, events| {
            let mut rect = egui::Rect::NOTHING;
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 200.0),
                    )),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| {
                    rect = ui
                        .add(editor_slider(value, 2.0..=9.0, 0.0..=255.0).integer())
                        .rect;
                },
            );
            rect
        };
        let mut value = 5.0;
        frame(&mut value, Vec::new());
        for (text, expected) in [
            ("100", 100.0),
            ("9999", 255.0),
            ("NaN", 255.0),
            ("-10", 0.0),
        ] {
            let rect = frame(&mut value, Vec::new());
            let pos = rect.right_center() - egui::vec2(10.0, 0.0);
            for pressed in [true, false] {
                frame(
                    &mut value,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            let previous = value;
            frame(&mut value, vec![egui::Event::Text(text.into())]);
            assert_eq!(value, previous, "typing should wait for commit");
            frame(&mut value, egui_key_pulse(egui::Key::Enter));
            frame(&mut value, Vec::new());
            assert_eq!(value, expected, "typed {text}");
        }
        let rect = frame(&mut value, Vec::new());
        let pos = rect.left_center() + egui::vec2(100.0, 0.0);
        for pressed in [true, false] {
            frame(
                &mut value,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!((2.0..=9.0).contains(&value));
    }

    #[test]
    fn expanded_editor_values_generate_and_remain_visible() {
        let baseline = GeneratorConfig::test_quality();
        for (radius, peaks) in [
            (1.0, 5),
            (1.0, 100),
            (15.0, 100),
            (15.0, 255),
            (64.0, 5),
            (128.0, 100),
        ] {
            let settings = WorldEditorSettings {
                planet_radius: radius,
                peak_clusters: peaks,
                ..WorldEditorSettings::from_generator(&baseline)
            }
            .sanitized();
            let config = settings.generator_config(&baseline);
            config.validate().unwrap();
            assert_eq!(config.base_radius, radius);
            let mut app = preview_app();
            app.world_editor = settings;
            app.profile_write_enabled = false;
            for seed in [0, 1, 42] {
                let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config.clone())
                    .generate_with_root_budget(WorldSeed(seed), crate::EDITOR_ROOT_BUDGET)
                    .unwrap_or_else(|error| panic!("{settings:?} seed {seed}: {error}"));
                if peaks > 9 {
                    assert_eq!(planet.mountains.len(), usize::from(peaks));
                }
                app.run = RunState::new(
                    planet,
                    GameMode::FreeMow,
                    app.game_config.vehicle.clone(),
                    app.game_config.job.clone(),
                    &app.profile.settings.accessibility,
                    false,
                );
                let context = egui::Context::default();
                for zoom in [0.85, 1.0, 1.8] {
                    app.editor_zoom = zoom;
                    app.animate_editor_camera(&context);
                    assert!(app.run.camera.state.position.is_finite());
                    assert!(app.run.camera.state.position.length() > app.planet_preview_extent());
                }
                app.animate_planet_camera(0.08);
                assert!(app.run.camera.state.position.length() > app.planet_preview_extent());
            }
        }
    }

    #[test]
    fn editor_numeric_safety_keeps_dependent_relief_in_valid_domains() {
        let baseline = GeneratorConfig::test_quality();
        for radius in [f32::NAN, f32::INFINITY, -1.0, 0.0, 1.0, 128.0, f32::MAX] {
            for height in [f32::NAN, -1.0, 0.0, f32::MAX] {
                let settings = WorldEditorSettings {
                    planet_radius: radius,
                    peak_height: height,
                    rolling_amplitude: height,
                    rock_coverage_percent: height,
                    peak_clusters: 255,
                }
                .sanitized();
                settings.generator_config(&baseline).validate().unwrap();
                assert!((1.0..=128.0).contains(&settings.planet_radius));
                assert!(settings.peak_height <= settings.planet_radius);
                assert!(settings.rolling_amplitude * 1.06 < settings.planet_radius);
            }
        }
    }

    #[test]
    fn randomized_editor_settings_fit_sliders_and_generator_domains() {
        let baseline = GeneratorConfig::test_quality();
        for seed in 0..1024 {
            let settings = WorldEditorSettings::random(WorldSeed(seed));
            assert!(WorldEditorSettings::RADIUS_SLIDER_RANGE.contains(&settings.planet_radius));
            assert!(
                WorldEditorSettings::ROCKINESS_SLIDER_RANGE
                    .contains(&settings.rock_coverage_percent)
            );
            assert_eq!(settings.rock_coverage_percent.fract(), 0.0);
            assert!(
                WorldEditorSettings::PEAK_CLUSTERS_SLIDER_RANGE.contains(&settings.peak_clusters)
            );
            assert!(
                (0.0..=WorldEditorSettings::PEAK_HEIGHT_SLIDER_MAX).contains(&settings.peak_height)
            );
            assert!(
                (0.0..=WorldEditorSettings::ROLLING_SLIDER_MAX)
                    .contains(&settings.rolling_amplitude)
            );
            assert_eq!(settings, settings.sanitized());
            settings.generator_config(&baseline).validate().unwrap();
        }
    }

    #[test]
    fn classic_world_editor_values_preserve_shipping_shape() {
        let baseline = GeneratorConfig::default();
        let edited = WorldEditorSettings::from_generator(&baseline).generator_config(&baseline);

        assert_eq!(edited.base_radius, baseline.base_radius);
        assert_eq!(edited.rolling_amplitude, baseline.rolling_amplitude);
        assert_eq!(edited.mountain_count_min, baseline.mountain_count_min);
        assert_eq!(edited.mountain_count_max, baseline.mountain_count_max);
        assert_eq!(edited.mountain_height_min, baseline.mountain_height_min);
        assert_eq!(edited.mountain_height_max, baseline.mountain_height_max);
        assert!((edited.mowable_ratio_min - baseline.mowable_ratio_min).abs() < 1.0e-6);
        assert!((edited.mowable_ratio_max - baseline.mowable_ratio_max).abs() < 1.0e-6);
    }

    #[test]
    fn classic_editor_combinations_generate_valid_worlds() {
        let baseline = GeneratorConfig::test_quality();
        for radius in [12.0, 22.0] {
            for rockiness in [0.0, 1.0, 24.0] {
                for peaks in [2, 9] {
                    for height in [2.0, 7.0] {
                        for rolling in [0.0, 1.2] {
                            let settings = WorldEditorSettings {
                                planet_radius: radius,
                                rock_coverage_percent: rockiness,
                                peak_clusters: peaks,
                                peak_height: height,
                                rolling_amplitude: rolling,
                            };
                            let generator = PlanetGenerator::new(
                                CURRENT_GENERATOR_VERSION,
                                settings.generator_config(&baseline),
                            );
                            for seed in [0, 1, 42] {
                                generator
                                    .generate_with_roots(WorldSeed(seed), false)
                                    .unwrap_or_else(|error| {
                                        panic!("{settings:?}, seed {seed}: {error}")
                                    });
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn editor_slider_endpoints_generate_valid_worlds() {
        let baseline = GeneratorConfig::test_quality();
        let defaults = WorldEditorSettings::from_generator(&baseline);
        // Test each control at both ends against the default world. Simultaneous
        // extremes can still fail the generator's route and spawn validation.
        for upper in [false, true] {
            let endpoint = |range: std::ops::RangeInclusive<f32>| {
                if upper { *range.end() } else { *range.start() }
            };
            for settings in [
                WorldEditorSettings {
                    planet_radius: endpoint(WorldEditorSettings::RADIUS_SLIDER_RANGE),
                    ..defaults
                },
                WorldEditorSettings {
                    rock_coverage_percent: endpoint(WorldEditorSettings::ROCKINESS_SLIDER_RANGE),
                    ..defaults
                },
                WorldEditorSettings {
                    peak_clusters: if upper {
                        *WorldEditorSettings::PEAK_CLUSTERS_SLIDER_RANGE.end()
                    } else {
                        *WorldEditorSettings::PEAK_CLUSTERS_SLIDER_RANGE.start()
                    },
                    ..defaults
                },
                WorldEditorSettings {
                    peak_height: endpoint(
                        0.0..=WorldEditorSettings::PEAK_HEIGHT_SLIDER_MAX
                            .min(defaults.planet_radius),
                    ),
                    ..defaults
                },
                WorldEditorSettings {
                    rolling_amplitude: endpoint(0.0..=WorldEditorSettings::ROLLING_SLIDER_MAX),
                    ..defaults
                },
            ] {
                for seed in [0, 1, 42] {
                    PlanetGenerator::new(
                        CURRENT_GENERATOR_VERSION,
                        settings.generator_config(&baseline),
                    )
                    .generate_with_roots(WorldSeed(seed), false)
                    .unwrap_or_else(|error| panic!("{settings:?}, seed {seed}: {error}"));
                }
            }
        }
        // Exercise root allocation with shipping settings and the editor budget,
        // including the largest fully grassy world, beyond metadata generation.
        for rockiness in [0.0, *WorldEditorSettings::ROCKINESS_SLIDER_RANGE.end()] {
            let settings = WorldEditorSettings {
                planet_radius: *WorldEditorSettings::RADIUS_SLIDER_RANGE.end(),
                rock_coverage_percent: rockiness,
                peak_clusters: *WorldEditorSettings::PEAK_CLUSTERS_SLIDER_RANGE.end(),
                peak_height: WorldEditorSettings::PEAK_HEIGHT_SLIDER_MAX,
                rolling_amplitude: WorldEditorSettings::ROLLING_SLIDER_MAX,
            };
            let shipping = GameConfig::shipping().unwrap().generator;
            let planet = PlanetGenerator::new(
                CURRENT_GENERATOR_VERSION,
                settings.generator_config(&shipping),
            )
            .generate_with_root_budget(WorldSeed(42), crate::EDITOR_ROOT_BUDGET)
            .unwrap();
            assert!(!planet.grass_roots.is_empty());
            assert!(planet.grass_roots.len() <= crate::EDITOR_ROOT_BUDGET);
        }
    }

    #[test]
    fn world_editor_presets_generate_playable_planets() {
        let baseline = GeneratorConfig::test_quality();
        let presets = [
            WorldEditorSettings::meadow(),
            WorldEditorSettings::from_generator(&baseline),
            WorldEditorSettings::craggy(),
            WorldEditorSettings {
                rock_coverage_percent: 1.0,
                ..WorldEditorSettings::craggy()
            },
            WorldEditorSettings {
                rock_coverage_percent: 2.0,
                ..WorldEditorSettings::craggy()
            },
            WorldEditorSettings {
                rock_coverage_percent: 7.0,
                ..WorldEditorSettings::craggy()
            },
            WorldEditorSettings {
                planet_radius: 12.0,
                rock_coverage_percent: 24.0,
                peak_clusters: 9,
                peak_height: 7.0,
                rolling_amplitude: 1.2,
            },
            WorldEditorSettings {
                planet_radius: 22.0,
                rock_coverage_percent: 8.0,
                peak_clusters: 2,
                peak_height: 2.0,
                rolling_amplitude: 0.0,
            },
        ];
        for (preset_index, preset) in presets.into_iter().enumerate() {
            let generator = PlanetGenerator::new(
                CURRENT_GENERATOR_VERSION,
                preset.generator_config(&baseline),
            );
            for seed in 0..12 {
                generator
                    .generate_with_roots(WorldSeed(seed), false)
                    .unwrap_or_else(|error| {
                        panic!("preset {preset_index}, seed {seed} failed: {error}")
                    });
            }
        }
    }
}
