use std::{
    collections::VecDeque,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use egui::{Align2, Color32, RichText};
use egui_wgpu::wgpu;
use lawn_core::{
    GameConfig, PlanetGenerator, WorldSeed,
    config::GeneratorConfig,
    flow::GameState,
    input::Action,
    planet::TUTORIAL_SEED,
    profile::{Profile, QualityPreset, RecordKey},
    run::{GameMode, RunState, TutorialStage},
    score::Results,
    simulation::FixedStepClock,
};
use lawn_render::{FrameAcquireError, FrameStats, Renderer};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow},
    window::{Fullscreen, Window, WindowAttributes, WindowId},
};

use crate::{input_adapter::InputAdapter, profile_store::ProfileStore};

#[derive(Clone, Copy, Debug)]
enum ConfirmAction {
    Restart,
    ReturnToEditor,
    RandomPlanet,
}

#[derive(Clone, Debug)]
enum UiCommand {
    OpenEditor,
    OpenSettings,
    Back,
    Start(WorldSeed),
    Random,
    Resume,
    Restart,
    ReturnToEditor,
    Submit,
    Retry,
    Quit,
    ToggleFullscreen,
    ToggleFavorite(WorldSeed),
}

const PREVIEW_INTERVAL: Duration = Duration::from_millis(100);
const EDITOR_SCENE_INSET: f32 = 356.0;
const TITLE_SCENE_INSET: f32 = 400.0;
const GARDEN_CREAM: Color32 = Color32::from_rgb(255, 249, 231);
const GARDEN_PINE: Color32 = Color32::from_rgb(34, 66, 57);
const GARDEN_MUTED: Color32 = Color32::from_rgb(93, 108, 91);
const GARDEN_SAGE: Color32 = Color32::from_rgb(206, 222, 182);
const GARDEN_CORAL: Color32 = Color32::from_rgb(248, 147, 111);
const GARDEN_ERROR: Color32 = Color32::from_rgb(161, 57, 42);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GenerationPurpose {
    Preview,
    Play,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorldRecipe {
    settings: WorldEditorSettings,
    seed: WorldSeed,
}

struct PendingGeneration {
    recipe: WorldRecipe,
    purpose: GenerationPurpose,
    receiver: mpsc::Receiver<Result<RunState, String>>,
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

    fn generator_config(self, baseline: &GeneratorConfig) -> GeneratorConfig {
        let mut config = baseline.clone();
        config.base_radius = self.planet_radius.clamp(12.0, 22.0);
        config.rolling_amplitude = self.rolling_amplitude.clamp(0.0, 1.2);

        let peak_clusters = self.peak_clusters.clamp(2, 9);
        config.mountain_count_min = peak_clusters.saturating_sub(2).max(1);
        config.mountain_count_max = peak_clusters.saturating_add(2);
        let peak_height = self.peak_height.clamp(2.0, 7.0);
        config.mountain_height_min = (peak_height - 1.25).max(0.75);
        config.mountain_height_max = peak_height + 1.25;
        config.mountain_separation_radians = (baseline.mountain_separation_radians
            * (5.0 / f32::from(peak_clusters)).sqrt())
        .clamp(0.4, 0.75);

        let rock_coverage = (self.rock_coverage_percent / 100.0).clamp(0.0, 0.24);
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
        config.pass_clearance = (baseline.pass_clearance * radius_scale).clamp(3.4, 6.2);
        config.spawn_clearance = (baseline.spawn_clearance * radius_scale).clamp(2.8, 5.2);
        config.ideal_time_min_seconds = 45.0;
        config.ideal_time_max_seconds = 900.0;
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
            peak_height: 6.2,
            rolling_amplitude: 1.0,
        }
    }
}

pub struct LawnOrbitApp {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    run: RunState,
    game_config: GameConfig,
    world_editor: WorldEditorSettings,
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
    pending_generation: Option<PendingGeneration>,
    preview_recipe: Option<WorldRecipe>,
    preview_attempt: Option<WorldRecipe>,
    preview_queued_at: Option<Instant>,
    seed_text: String,
    confirmation: Option<ConfirmAction>,
    last_stats: FrameStats,
    status_message: Option<String>,
    survey_started: Option<Instant>,
    app_started: Instant,
    show_diagnostics: bool,
    frame_times_ms: VecDeque<f32>,
}

impl LawnOrbitApp {
    pub fn new() -> Result<Self> {
        let game_config = GameConfig::shipping().context("shipping gameplay config is invalid")?;
        Self::load_with_config(ProfileStore::discover(), game_config)
    }

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
        let world_editor = WorldEditorSettings::from_generator(&game_config.generator);
        Ok(Self {
            window: None,
            renderer: None,
            egui_state: None,
            egui_renderer: None,
            run,
            game_config,
            world_editor,
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
            pending_generation: None,
            preview_recipe: None,
            preview_attempt: None,
            preview_queued_at: None,
            seed_text: TUTORIAL_SEED.to_string(),
            confirmation: None,
            last_stats: FrameStats::default(),
            status_message: None,
            survey_started: None,
            app_started: Instant::now(),
            show_diagnostics: false,
            frame_times_ms: VecDeque::with_capacity(240),
        })
    }

    fn create_window_and_gpu(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if self.window.is_some() {
            return Ok(());
        }
        let attributes = WindowAttributes::default()
            .with_title("Lawn Orbit")
            .with_inner_size(LogicalSize::new(1280.0, 720.0))
            .with_min_inner_size(LogicalSize::new(960.0, 540.0));
        let window = Arc::new(event_loop.create_window(attributes)?);
        if self.profile.settings.fullscreen {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let renderer = pollster::block_on(Renderer::new(
            window.clone(),
            &self.run,
            self.profile.settings.msaa_samples,
            self.profile.settings.quality,
            self.profile.settings.render_scale,
            self.profile.settings.accessibility.high_contrast_grass,
            self.profile.settings.accessibility.reduced_particles,
            self.profile.settings.grass_height_multiplier,
        ))?;
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

    fn update_simulation(&mut self, elapsed: Duration) {
        let _span = tracing::debug_span!("fixed_simulation_batch").entered();
        if self.state != GameState::Playing {
            return;
        }
        if self.input.take_pause_pressed() {
            self.pause();
            return;
        }
        if self
            .survey_started
            .is_some_and(|started| started.elapsed() < Duration::from_secs(4))
        {
            self.input.clear_transient();
            self.animate_planet_camera(0.12);
            return;
        }
        self.survey_started = None;
        self.run.clear_frame_events();
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
    }

    fn pause(&mut self) {
        if self.state == GameState::Playing {
            self.state = GameState::Paused;
            self.run.paused = true;
        }
        self.clock = FixedStepClock::default();
        self.input.clear();
    }

    fn finish_run(&mut self) {
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
        let Some(pending) = &self.pending_generation else {
            return;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(
                "The planet worker stopped before finishing. Try another setting or seed.".into(),
            ),
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
                if let Some(renderer) = &mut self.renderer {
                    renderer.upload_planet(&self.run);
                }
                self.results = None;
                self.clock = FixedStepClock::default();
                self.status_message = None;
                if purpose == GenerationPurpose::Play {
                    self.enter_sandbox();
                } else {
                    self.run.paused = true;
                    self.preview_recipe = Some(recipe);
                    self.survey_started = None;
                }
            }
            Err(error) => {
                if purpose == GenerationPurpose::Play || self.editor_recipe() == Some(recipe) {
                    self.status_message = Some(format!("Planet generation failed: {error}"));
                }
                self.state = GameState::WorldEditor;
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

    fn start_generation(&mut self, seed: WorldSeed) {
        let recipe = WorldRecipe {
            settings: self.world_editor,
            seed,
        };
        if self.preview_recipe == Some(recipe) && self.pending_generation.is_none() {
            // The visible world already has full grass, mowing, and collision
            // data. Enter that exact world without another generation or upload.
            self.enter_sandbox();
            return;
        }
        self.spawn_generation(recipe, GenerationPurpose::Play);
        self.state = if self.pending_generation.is_some() {
            GameState::Loading
        } else {
            GameState::WorldEditor
        };
    }

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
                    .generate(recipe.seed)
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

    fn enter_sandbox(&mut self) {
        self.profile.record_seed(
            self.run.planet.generator_version,
            self.run.planet.world_seed,
        );
        self.seed_text = self.run.planet.world_seed.to_string();
        self.run.paused = false;
        self.run.tutorial_enabled = !self.profile.tutorial_completed;
        self.state = GameState::Playing;
        self.preview_recipe = None;
        self.preview_attempt = None;
        self.preview_queued_at = None;
        self.survey_started = Some(Instant::now());
        self.clock = FixedStepClock::default();
        self.input.clear_transient();
        self.last_frame = Instant::now();
        if !self.input.is_focused() {
            self.pause();
        }
        self.save_profile();
    }

    fn random_seed() -> WorldSeed {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        WorldSeed((nanos as u64) ^ (nanos >> 64) as u64 ^ 0xA17C_9E37_5EED_1234)
    }

    fn animate_planet_camera(&mut self, speed: f32) {
        let angle = self.app_started.elapsed().as_secs_f32() * speed;
        let planet_radius = self.run.planet.config.base_radius;
        let radius = planet_radius * 1.8;
        let position = glam::Vec3::new(
            angle.cos() * radius,
            planet_radius * 0.72,
            angle.sin() * radius,
        );
        self.run
            .camera
            .snap_to_pose(position, glam::Vec3::ZERO, glam::Vec3::Y);
    }

    fn animate_editor_camera(&mut self, context: &egui::Context) {
        let screen = context.content_rect();
        let aspect = ((screen.width() - EDITOR_SCENE_INSET) / screen.height()).max(0.25);
        let vertical_half_fov = self.run.camera.state.field_of_view_degrees.to_radians() * 0.5;
        let half_fov = vertical_half_fov.min((vertical_half_fov.tan() * aspect).atan());
        // A fixed envelope fits even the largest craggy planet. Keeping the
        // distance independent of the sliders makes radius changes visible.
        let distance = 32.0 / half_fov.sin() * 1.08;
        let angle = self.app_started.elapsed().as_secs_f32() * 0.08;
        let direction = glam::Vec3::new(angle.cos(), 0.4, angle.sin()).normalize();
        self.run
            .camera
            .snap_to_pose(direction * distance, glam::Vec3::ZERO, glam::Vec3::Y);
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
        egui::Window::new("Lawn Orbit")
            .anchor(Align2::LEFT_CENTER, [32.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(garden_card().inner_margin(24))
            .show(context, |ui| {
                ui.set_width(292.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("YOUR LITTLE CORNER OF THE COSMOS")
                            .size(11.0)
                            .color(GARDEN_MUTED),
                    );
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new("Lawn Orbit")
                            .size(48.0)
                            .strong()
                            .color(GARDEN_PINE),
                    );
                    ui.label(RichText::new("A little world. A lovely lawn.").size(18.0));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Grow a tiny planet, hop on your mower,\nand make yourself a little patch of happy.",
                        )
                        .color(GARDEN_MUTED),
                    );
                    ui.add_space(22.0);
                    if garden_button(ui, "Create a Planet", [292.0, 46.0], true).clicked() {
                        commands.push(UiCommand::OpenEditor);
                    }
                    if ui.button("Settings & Accessibility").clicked() {
                        commands.push(UiCommand::OpenSettings);
                    }
                    if ui.button("Quit").clicked() {
                        commands.push(UiCommand::Quit);
                    }
                    ui.add_space(4.0);
                });
            });
    }

    fn draw_world_editor(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let classic = WorldEditorSettings::from_generator(&self.game_config.generator);
        egui::Window::new("World Editor")
            .id(egui::Id::new("world-editor"))
            .fixed_pos([16.0, 16.0])
            .fixed_size([324.0, context.content_rect().height() - 32.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(garden_card().inner_margin(10))
            .show(context, |ui| {
                ui.set_width(302.0);
                ui.spacing_mut().item_spacing.y = 5.0;
                ui.label(
                    RichText::new("THE PLANET PATCH")
                        .size(11.0)
                        .color(GARDEN_MUTED),
                );
                ui.heading("Shape a tiny planet");
                ui.label("A little more meadow? A few more peaks?");
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height((context.content_rect().height() - 270.0).max(160.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (label, preset) in [
                                ("Meadow", WorldEditorSettings::meadow()),
                                ("Classic", classic),
                                ("Craggy", WorldEditorSettings::craggy()),
                            ] {
                                if ui
                                    .selectable_label(self.world_editor == preset, label)
                                    .clicked()
                                {
                                    self.world_editor = preset;
                                }
                            }
                        });
                        ui.separator();
                        ui.spacing_mut().slider_width = 208.0;
                        ui.label("Planet radius");
                        ui.add(
                            egui::Slider::new(&mut self.world_editor.planet_radius, 12.0..=22.0)
                                .suffix(" m"),
                        );
                        ui.add_space(6.0);
                        ui.label("Rockiness");
                        ui.add(
                            egui::Slider::new(
                                &mut self.world_editor.rock_coverage_percent,
                                0.0..=24.0,
                            )
                            .step_by(1.0)
                            .suffix("%"),
                        );
                        ui.small("0% gives you an uninterrupted grassy world.");
                        ui.add_space(6.0);
                        ui.add_enabled_ui(self.world_editor.rock_coverage_percent > 0.0, |ui| {
                            ui.label("Peak clusters");
                            ui.add(egui::Slider::new(
                                &mut self.world_editor.peak_clusters,
                                2..=9,
                            ));
                            ui.add_space(6.0);
                            ui.label("Peak size");
                            ui.add(
                                egui::Slider::new(&mut self.world_editor.peak_height, 2.0..=7.0)
                                    .suffix(" m"),
                            );
                        });
                        ui.add_space(6.0);
                        ui.label("Rolling terrain");
                        ui.add(
                            egui::Slider::new(&mut self.world_editor.rolling_amplitude, 0.0..=1.2)
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
                        ui.colored_label(GARDEN_PINE, "● Live preview");
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
                let circumference = std::f32::consts::TAU * self.world_editor.planet_radius;
                ui.small(format!(
                    "≈ {circumference:.0} m around · mow at your own pace"
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if garden_button(ui, "Start Mowing", [200.0, 34.0], ready).clicked() {
                        commands.push(UiCommand::Start(recipe.unwrap().seed));
                    }
                    if ui.button("Back").clicked() {
                        commands.push(UiCommand::Back);
                    }
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
                    ui.label("A little sunshine. A lot of grass. Almost ready.");
                });
            });
    }

    fn draw_hud(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        egui::Area::new("hud".into())
            .fixed_pos([18.0, 18.0])
            .show(context, |ui| {
                garden_card().show(ui, |ui| {
                    ui.label(
                        RichText::new("A LITTLE TIDIER")
                            .size(11.0)
                            .color(GARDEN_MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{:.1}%",
                                self.run.mowing.display_coverage_percent()
                            ))
                            .size(34.0)
                            .strong(),
                        );
                        ui.label(
                            RichText::new("of your planet\nfreshly mown")
                                .size(12.0)
                                .color(GARDEN_MUTED),
                        );
                    });
                    ui.add(
                        egui::ProgressBar::new(
                            (self.run.mowing.display_coverage_percent() / 100.0) as f32,
                        )
                        .desired_width(196.0)
                        .desired_height(5.0)
                        .fill(GARDEN_PINE),
                    );
                    ui.add_space(2.0);
                    let boost = self.run.vehicle.state.boost_charge
                        / self.run.vehicle_tuning.boost_capacity_seconds;
                    ui.add(
                        egui::ProgressBar::new(boost)
                            .desired_width(196.0)
                            .desired_height(16.0)
                            .fill(GARDEN_CORAL)
                            .text("Boost"),
                    );
                    ui.small(format!(
                        "Always mowing · {} impacts",
                        self.run.metrics.substantial_collision_count()
                    ));
                });
            });
        egui::Area::new("telemetry".into())
            .anchor(Align2::RIGHT_TOP, [-18.0, 18.0])
            .show(context, |ui| {
                garden_card().inner_margin(10).show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} · {:.0} km/h",
                            self.input.last_device_label,
                            self.run.vehicle.state.speed() * 3.6
                        ))
                        .color(GARDEN_PINE),
                    );
                });
            });
        if let Some(direction) = self.run.locator_direction() {
            let transform = self.run.vehicle.state.transform;
            let forward = direction.dot(transform.forward);
            let side = direction.dot(transform.forward.cross(transform.up));
            egui::Area::new("locator".into())
                .anchor(Align2::CENTER_TOP, [0.0, 24.0])
                .show(context, |ui| {
                    let arrow = if side.abs() > forward.abs() {
                        if side > 0.0 { "▶" } else { "◀" }
                    } else if forward >= 0.0 {
                        "▲"
                    } else {
                        "▼"
                    };
                    let size = if self.profile.settings.accessibility.enlarged_locator {
                        27.0
                    } else {
                        17.0
                    };
                    garden_card().inner_margin(10).show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{arrow} UNCUT GRASS"))
                                .size(size)
                                .strong()
                                .color(GARDEN_PINE),
                        );
                    });
                });
        }
        if self.run.tutorial_enabled
            && let Some(prompt) = self.run.tutorial_stage.prompt()
            && (self.run.tutorial_stage != TutorialStage::Recovery
                || self.run.vehicle.state.stuck_seconds >= 1.0)
        {
            egui::Area::new("tutorial".into())
                .anchor(Align2::CENTER_BOTTOM, [0.0, -42.0])
                .show(context, |ui| {
                    garden_card().show(ui, |ui| {
                        ui.label(RichText::new(prompt).size(18.0));
                    });
                });
        }
        if self.survey_started.is_some() {
            egui::Area::new("survey".into())
                .anchor(Align2::CENTER_TOP, [0.0, 70.0])
                .show(context, |ui| {
                    garden_card().show(ui, |ui| {
                        ui.label(RichText::new("Surveying mountain routes…").size(20.0));
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
        egui::Window::new("Paused")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.set_min_width(300.0);
                ui.label("Hold Recover at any time if the mower is stuck or overturned.");
                if garden_button(ui, "Resume", [300.0, 38.0], true).clicked() {
                    commands.push(UiCommand::Resume);
                }
                if ui.button("Settings & Accessibility").clicked() {
                    commands.push(UiCommand::OpenSettings);
                }
                if ui.button("Regrow this planet").clicked() {
                    self.confirmation = Some(ConfirmAction::Restart);
                }
                if ui.button("New random planet").clicked() {
                    self.confirmation = Some(ConfirmAction::RandomPlanet);
                }
                if ui.button("Return to world editor").clicked() {
                    self.confirmation = Some(ConfirmAction::ReturnToEditor);
                }
            });
    }

    fn draw_results(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let Some(results) = &self.results else { return };
        egui::Window::new("Job complete")
            .anchor(Align2::RIGHT_CENTER, [-54.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.set_min_width(370.0);
                ui.vertical_centered(|ui| {
                    ui.heading("★".repeat(results.stars as usize));
                    ui.label(format!("Seed {}", results.world_seed));
                });
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
                    if ui.button("World editor").clicked() {
                        commands.push(UiCommand::ReturnToEditor);
                    }
                });
            });
    }

    fn draw_settings(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        egui::Window::new("Settings & Accessibility")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .default_width(600.0)
            .show(context, |ui| {
                egui::ScrollArea::vertical().max_height(610.0).show(ui, |ui| {
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
                        ui.label("MSAA");
                        for samples in [1, 2, 4] {
                            ui.selectable_value(
                                &mut self.profile.settings.msaa_samples,
                                samples,
                                format!("{samples}×"),
                            );
                        }
                    });
                    ui.small("Render scale and MSAA apply on the next launch; UI remains native resolution.");
                    ui.checkbox(&mut self.profile.settings.fullscreen, "Borderless fullscreen");
                    if ui.button("Apply fullscreen").clicked() { commands.push(UiCommand::ToggleFullscreen); }
                    ui.separator();
                    ui.heading("Remap controls");
                    ui.label("Choose an action, then press any keyboard or gamepad control.");
                    egui::Grid::new("bindings").striped(true).show(ui, |ui| {
                        for action in [
                            Action::SteerLeft, Action::SteerRight, Action::Accelerate, Action::BrakeReverse,
                            Action::Boost, Action::LookBehind, Action::Recover,
                            Action::CameraLeft, Action::CameraRight, Action::CameraUp, Action::CameraDown,
                            Action::RecenterCamera, Action::Pause,
                        ] {
                            ui.label(match action {
                                Action::SteerLeft => "Move left".to_owned(),
                                Action::SteerRight => "Move right".to_owned(),
                                Action::Accelerate => "Move forward".to_owned(),
                                Action::BrakeReverse => "Move backward".to_owned(),
                                _ => format!("{action:?}"),
                            });
                            let label = if self.input.rebind_action == Some(action) {
                                "Press a control…".into()
                            } else {
                                self.profile.settings.controls.bindings.get(&action).map_or_else(|| "Unbound".into(), |list| list.iter().map(|binding| format!("{binding:?}")).collect::<Vec<_>>().join(" / "))
                            };
                            if ui.button(label).clicked() { self.input.rebind_action = Some(action); }
                            ui.end_row();
                        }
                    });
                    if ui.button("Reset tutorial prompts").clicked() {
                        self.profile.tutorial_completed = false;
                        self.profile.tutorial_reset_requested = true;
                    }
                    ui.add_space(10.0);
                    if ui.button("Done").clicked() { commands.push(UiCommand::Back); }
                });
            });
    }

    fn draw_confirmation(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let Some(action) = self.confirmation else {
            return;
        };
        egui::Modal::new(egui::Id::new("discard-current-mowing")).show(context, |ui| {
            ui.heading("Discard current mowing?");
            ui.label("Current mowing progress will be lost.");
            ui.horizontal(|ui| {
                if ui.button("Keep mowing").clicked() {
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
        egui::Window::new("Lawn Orbit diagnostics (F3)")
            .anchor(Align2::RIGHT_BOTTOM, [-12.0, -12.0])
            .resizable(false)
            .show(context, |ui| {
                ui.monospace(format!(
                    "frame p50 {:>5.2} ms  p95 {:>5.2} ms",
                    percentile(0.50),
                    percentile(0.95)
                ));
                ui.monospace(format!(
                    "acquire   {:>5.2} ms",
                    self.last_stats.cpu_acquire_milliseconds
                ));
                ui.monospace(format!(
                    "encode    {:>5.2} ms",
                    self.last_stats.cpu_encode_milliseconds
                ));
                if self.last_stats.gpu_frame_milliseconds > 0.0 {
                    ui.monospace(format!(
                        "gpu frame {:>5.2} ms",
                        self.last_stats.gpu_frame_milliseconds
                    ));
                }
                if self.last_stats.gpu_world_milliseconds > 0.0 {
                    ui.monospace(format!(
                        "gpu spans i {:>4.2}  s {:>4.2}  w {:>4.2}  c {:>4.2} ms",
                        self.last_stats.gpu_interaction_milliseconds,
                        self.last_stats.gpu_shadow_milliseconds,
                        self.last_stats.gpu_world_milliseconds,
                        self.last_stats.gpu_composite_milliseconds,
                    ))
                    .on_hover_text(
                        "Pass timestamp spans can overlap; GPU frame measures the total directly.",
                    );
                }
                ui.monospace(format!("patches   {:>7}", self.last_stats.visible_patches));
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
                    ui.monospace(format!(
                        "gpu       {:?} · {:?} · timestamps {}",
                        capabilities.backend,
                        capabilities.tier,
                        if capabilities.timestamp_queries {
                            "on"
                        } else {
                            "off"
                        }
                    ));
                    ui.monospace(format!("adapter   {}", capabilities.adapter_name));
                }
                ui.monospace(format!("seed      {}", self.run.planet.world_seed));
                ui.monospace(format!(
                    "hash      {:016X}",
                    self.run.planet.deterministic_hash
                ));
            });
    }

    fn process_commands(&mut self, commands: Vec<UiCommand>, event_loop: &ActiveEventLoop) {
        for command in commands {
            match command {
                UiCommand::OpenEditor => {
                    self.state = GameState::WorldEditor;
                    self.preview_attempt = None;
                    self.status_message = None;
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
                        self.state = GameState::Title;
                    }
                    if self.state == GameState::Paused {
                        self.run.paused = true;
                    }
                    self.save_profile();
                }
                UiCommand::Start(seed) => self.start_generation(seed),
                UiCommand::Random => self.start_generation(Self::random_seed()),
                UiCommand::Resume => {
                    self.state = GameState::Playing;
                    self.run.paused = false;
                    self.clock = FixedStepClock::default();
                    self.input.clear_transient();
                    self.confirmation = None;
                    self.last_frame = Instant::now();
                }
                UiCommand::Restart => {
                    self.run.restart(&self.profile.settings.accessibility);
                    self.enter_sandbox();
                }
                UiCommand::ReturnToEditor => {
                    self.state = GameState::WorldEditor;
                    self.run.paused = true;
                    self.preview_attempt = None;
                    self.status_message = None;
                }
                UiCommand::Submit => self.finish_run(),
                UiCommand::Retry => {
                    self.start_generation(self.run.planet.world_seed);
                }
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
                                .then(|| Fullscreen::Borderless(None)),
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
            renderer.set_visual_options(
                self.profile.settings.quality,
                self.profile.settings.accessibility.high_contrast_grass,
                self.profile.settings.accessibility.reduced_particles,
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
        egui_state.handle_platform_output(&window, full_output.platform_output);
        self.egui_state = Some(egui_state);
        self.process_commands(ui_commands, event_loop);
        self.update_editor_preview();
        let pixels_per_point = context.pixels_per_point();
        let paint_jobs = context.tessellate(full_output.shapes, pixels_per_point);

        let Some(renderer) = &mut self.renderer else {
            return;
        };
        renderer.set_scene_left_inset(match self.state {
            GameState::WorldEditor => EDITOR_SCENE_INSET / context.content_rect().width(),
            GameState::Title => TITLE_SCENE_INSET / context.content_rect().width(),
            _ => 0.0,
        });
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
        let rebinding = self.input.rebind_action.is_some();
        let consumed = !(rebinding
            && matches!(&event, WindowEvent::KeyboardInput { event, .. }
            if event.state == winit::event::ElementState::Pressed))
            && self
                .egui_state
                .as_mut()
                .is_some_and(|state| state.on_window_event(window, &event).consumed);
        let release_or_focus = matches!(&event,
            WindowEvent::KeyboardInput { event, .. } if event.state == winit::event::ElementState::Released
        ) || matches!(
            &event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                ..
            } | WindowEvent::Focused(_)
        );
        if !consumed || rebinding || release_or_focus {
            self.input
                .handle_window_event(&event, &mut self.profile.settings.controls);
        }
        match event {
            WindowEvent::CloseRequested => {
                self.save_profile();
                event_loop.exit();
            }
            WindowEvent::Focused(false) => self.pause(),
            WindowEvent::Occluded(occluded) => {
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
            WindowEvent::RedrawRequested => self.redraw(event_loop),
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
        self.input
            .poll_gamepads(&mut self.profile.settings.controls);
        event_loop.set_control_flow(ControlFlow::Poll);
        window.request_redraw();
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.pause();
        self.save_profile();
    }
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
    let mut visuals = egui::Visuals::light();
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
    });
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
    fn editor_layout_keeps_controls_and_play_action_beside_the_planet() {
        fn play_text_rect(shape: &egui::epaint::Shape) -> Option<egui::Rect> {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    (text.galley.text() == "Start Mowing").then_some(text.visual_bounding_rect())
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
    fn editor_slider_endpoints_generate_valid_worlds() {
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
        // Exercise actual root allocation at shipping density, including the
        // largest fully grassy world, rather than only metadata generation.
        for rockiness in [0.0, 24.0] {
            let settings = WorldEditorSettings {
                planet_radius: 22.0,
                rock_coverage_percent: rockiness,
                peak_clusters: 9,
                peak_height: 7.0,
                rolling_amplitude: 1.2,
            };
            let shipping = GameConfig::shipping().unwrap().generator;
            let planet = PlanetGenerator::new(
                CURRENT_GENERATOR_VERSION,
                settings.generator_config(&shipping),
            )
            .generate(WorldSeed(42))
            .unwrap();
            assert!(!planet.grass_roots.is_empty());
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
