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
    input::{Action, InputSnapshot},
    planet::{Planet, TUTORIAL_SEED},
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
    event_loop::ActiveEventLoop,
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

struct PendingGeneration {
    receiver: mpsc::Receiver<Result<Planet, String>>,
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

        let rock_coverage = (self.rock_coverage_percent / 100.0).clamp(0.08, 0.24);
        config.mowable_ratio_min = (1.0 - rock_coverage - 0.05).clamp(0.68, 0.9);
        config.mowable_ratio_max = (1.0 - rock_coverage + 0.05).clamp(0.76, 0.97);

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
            rock_coverage_percent: 8.0,
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
    pending_input: InputSnapshot,
    profile: Profile,
    profile_store: ProfileStore,
    results: Option<Results>,
    pending_generation: Option<PendingGeneration>,
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
        let profile_store = ProfileStore::discover();
        let profile = profile_store.load().unwrap_or_else(|error| {
            tracing::warn!(%error, "profile could not be loaded; defaults will be used");
            Profile::default()
        });
        let game_config = GameConfig::shipping().context("shipping gameplay config is invalid")?;
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
            pending_input: InputSnapshot::default(),
            profile,
            profile_store,
            results: None,
            pending_generation: None,
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
        if self
            .survey_started
            .is_some_and(|started| started.elapsed() < Duration::from_secs(4))
        {
            self.animate_planet_camera(0.12);
            return;
        }
        self.survey_started = None;
        self.run.clear_frame_events();
        let current = self.input.snapshot(
            &self.profile.settings.controls,
            &self.profile.settings.accessibility,
        );
        self.pending_input.steer = current.steer;
        self.pending_input.accelerate = current.accelerate;
        self.pending_input.brake_reverse = current.brake_reverse;
        self.pending_input.camera_orbit = current.camera_orbit;
        self.pending_input.boost_held = current.boost_held;
        self.pending_input.look_behind = current.look_behind;
        self.pending_input.recover_held = current.recover_held;
        self.pending_input.recenter_pressed |= current.recenter_pressed;
        self.pending_input.pause_pressed |= current.pause_pressed;
        self.pending_input.submit_pressed |= current.submit_pressed;
        if self.pending_input.pause_pressed {
            self.pending_input.pause_pressed = false;
            self.state = GameState::Paused;
            self.run.paused = true;
            return;
        }
        let input = &mut self.pending_input;
        let run = &mut self.run;
        let accessibility = &self.profile.settings.accessibility;
        let mut submit = false;
        let steps = self.clock.advance(elapsed, || {
            let snapshot = *input;
            run.tick(snapshot, accessibility);
            submit |= snapshot.submit_pressed;
            input.recenter_pressed = false;
            input.submit_pressed = false;
        });
        if steps > 0 {
            input.camera_orbit = [0.0; 2];
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
        let Ok(result) = pending.receiver.try_recv() else {
            return;
        };
        self.pending_generation = None;
        match result {
            Ok(planet) => {
                self.profile
                    .record_seed(planet.generator_version, planet.world_seed);
                self.seed_text = planet.world_seed.to_string();
                self.run = RunState::new(
                    planet,
                    GameMode::FreeMow,
                    self.game_config.vehicle.clone(),
                    self.game_config.job.clone(),
                    &self.profile.settings.accessibility,
                    !self.profile.tutorial_completed,
                );
                if let Some(renderer) = &mut self.renderer {
                    renderer.upload_planet(&self.run);
                }
                self.results = None;
                self.state = GameState::Playing;
                self.survey_started = Some(Instant::now());
                self.clock = FixedStepClock::default();
                self.last_frame = Instant::now();
                self.save_profile();
            }
            Err(error) => {
                self.status_message = Some(format!("Planet generation failed: {error}"));
                self.state = GameState::WorldEditor;
            }
        }
    }

    fn start_generation(&mut self, seed: WorldSeed) {
        let config = self
            .world_editor
            .generator_config(&self.game_config.generator);
        let generator = PlanetGenerator::new(lawn_core::planet::CURRENT_GENERATOR_VERSION, config);
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("planet-generator".into())
            .spawn(move || {
                let _span = tracing::info_span!("planet_generation", seed = %seed).entered();
                let result = generator.generate(seed).map_err(|error| error.to_string());
                let _ = sender.send(result);
            })
            .expect("planet generation worker should start");
        self.pending_generation = Some(PendingGeneration { receiver });
        self.state = GameState::Loading;
        self.status_message = None;
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
        if matches!(
            self.state,
            GameState::Title | GameState::WorldEditor | GameState::Results
        ) {
            self.animate_planet_camera(0.08);
        }
        if self.state == GameState::Loading {
            context.request_repaint_after(Duration::from_millis(16));
        }
        if let Some(message) = &self.status_message {
            egui::Area::new("status".into())
                .anchor(Align2::LEFT_BOTTOM, [16.0, -16.0])
                .show(context, |ui| {
                    egui::Frame::window(&context.style_of(egui::Theme::Dark)).show(ui, |ui| {
                        ui.colored_label(Color32::LIGHT_RED, message);
                    });
                });
        }
        self.draw_confirmation(context, &mut commands);
        commands
    }

    fn draw_title(context: &egui::Context, commands: &mut Vec<UiCommand>) {
        egui::Window::new("Lawn Orbit")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(
                egui::Frame::window(&context.style_of(egui::Theme::Dark))
                    .fill(Color32::from_black_alpha(205)),
            )
            .show(context, |ui| {
                ui.set_min_width(390.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("LAWN ORBIT")
                            .size(42.0)
                            .strong()
                            .color(Color32::from_rgb(188, 239, 125)),
                    );
                    ui.label("A fuzzy whole world under your wheels");
                    ui.add_space(24.0);
                    if ui
                        .add_sized([250.0, 42.0], egui::Button::new("Create a Planet"))
                        .clicked()
                    {
                        commands.push(UiCommand::OpenEditor);
                    }
                    if ui.button("Settings & Accessibility").clicked() {
                        commands.push(UiCommand::OpenSettings);
                    }
                    if ui.button("Quit").clicked() {
                        commands.push(UiCommand::Quit);
                    }
                });
            });
    }

    fn draw_world_editor(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let classic = WorldEditorSettings::from_generator(&self.game_config.generator);
        egui::Window::new("World Editor")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.set_min_width(610.0);
                egui::ScrollArea::vertical()
                    .max_height(470.0)
                    .show(ui, |ui| {
                ui.heading("Shape a tiny planet");
                ui.label("Tune the world, choose a seed, then drop straight into the sandbox.");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Presets").strong());
                    if ui.button("Meadow").clicked() {
                        self.world_editor = WorldEditorSettings::meadow();
                    }
                    if ui.button("Classic").clicked() {
                        self.world_editor = classic;
                    }
                    if ui.button("Craggy").clicked() {
                        self.world_editor = WorldEditorSettings::craggy();
                    }
                });
                ui.separator();
                egui::Grid::new("world-editor-terrain")
                    .num_columns(3)
                    .spacing([14.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("Planet size");
                        ui.add(
                            egui::Slider::new(&mut self.world_editor.planet_radius, 12.0..=22.0)
                                .suffix(" m"),
                        );
                        ui.small("Radius");
                        ui.end_row();

                        ui.label("Rockiness");
                        ui.add(
                            egui::Slider::new(
                                &mut self.world_editor.rock_coverage_percent,
                                8.0..=24.0,
                            )
                            .suffix("%"),
                        );
                        ui.small("Approx. coverage");
                        ui.end_row();

                        ui.label("Peak clusters");
                        ui.add(egui::Slider::new(
                            &mut self.world_editor.peak_clusters,
                            2..=9,
                        ));
                        ui.small("Landmark groups");
                        ui.end_row();

                        ui.label("Peak size");
                        ui.add(
                            egui::Slider::new(&mut self.world_editor.peak_height, 2.0..=7.0)
                                .suffix(" m"),
                        );
                        ui.small("Average height");
                        ui.end_row();

                        ui.label("Rolling terrain");
                        ui.add(
                            egui::Slider::new(
                                &mut self.world_editor.rolling_amplitude,
                                0.0..=1.2,
                            )
                            .suffix(" m"),
                        );
                        ui.small("Lawn undulation");
                        ui.end_row();
                    });
                let circumference = std::f32::consts::TAU * self.world_editor.planet_radius;
                let grassy_surface = 4.0
                    * std::f32::consts::PI
                    * self.world_editor.planet_radius.powi(2)
                    * (1.0 - self.world_editor.rock_coverage_percent / 100.0);
                ui.label(format!(
                    "≈ {circumference:.0} m around · {grassy_surface:.0} m² of grass · unscored sandbox"
                ));
                ui.small("World settings are applied when the planet is grown; gameplay handling stays unchanged.");
                ui.separator();
                ui.heading("Planet seed");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.seed_text);
                    if ui.button("New seed").clicked() {
                        self.seed_text = Self::random_seed().to_string();
                    }
                    if ui.button("Copy").clicked() {
                        context.copy_text(self.seed_text.clone());
                    }
                });
                ui.label("Enter hexadecimal, decimal, or any memorable phrase.");
                let parsed = self.seed_text.parse::<WorldSeed>();
                if let Ok(seed) = parsed {
                    let favorite = self
                        .profile
                        .favorite_seeds
                        .contains(&(lawn_core::planet::CURRENT_GENERATOR_VERSION.0, seed.0));
                    if ui
                        .button(if favorite {
                            "★ Remove favorite"
                        } else {
                            "☆ Favorite seed"
                        })
                        .clicked()
                    {
                        commands.push(UiCommand::ToggleFavorite(seed));
                    }
                }
                let recent: Vec<_> = self.profile.recent_seeds.iter().take(4).copied().collect();
                if !recent.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Recent:");
                        for key in recent {
                            if ui.small_button(key.seed.to_string()).clicked() {
                                self.seed_text = key.seed.to_string();
                            }
                        }
                    });
                }
                if parsed.is_err() {
                    ui.colored_label(Color32::LIGHT_RED, "Enter a seed before growing the planet.");
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            parsed.is_ok(),
                            egui::Button::new(RichText::new("Grow Planet").strong()),
                        )
                        .clicked()
                    {
                        commands.push(UiCommand::Start(parsed.unwrap()));
                    }
                    if ui.button("Back").clicked() {
                        commands.push(UiCommand::Back);
                    }
                });
                    });
            });
    }

    fn draw_loading(context: &egui::Context) {
        egui::Area::new("loading".into())
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                egui::Frame::window(&context.style_of(egui::Theme::Dark))
                    .fill(Color32::from_black_alpha(210))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.heading("Growing a tiny planet…");
                        });
                        ui.label("Building terrain, routes, grass roots, and collision metadata");
                    });
            });
    }

    fn draw_hud(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        egui::Area::new("hud".into())
            .fixed_pos([18.0, 18.0])
            .show(context, |ui| {
                egui::Frame::window(&context.style_of(egui::Theme::Dark))
                    .fill(Color32::from_black_alpha(145))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{:.1}%",
                                self.run.mowing.display_coverage_percent()
                            ))
                            .size(29.0)
                            .strong(),
                        );
                        ui.label("PLANET SANDBOX");
                        ui.horizontal(|ui| {
                            ui.label("◉ ALWAYS MOWING");
                            ui.label(format!(
                                "Impacts {}",
                                self.run.metrics.substantial_collision_count()
                            ));
                        });
                        let boost = self.run.vehicle.state.boost_charge
                            / self.run.vehicle_tuning.boost_capacity_seconds;
                        ui.add(
                            egui::ProgressBar::new(boost)
                                .desired_width(180.0)
                                .text("Boost"),
                        );
                    });
            });
        egui::Area::new("telemetry".into())
            .anchor(Align2::RIGHT_TOP, [-18.0, 18.0])
            .show(context, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · {:.0} km/h",
                        self.input.last_device_label,
                        self.run.vehicle.state.speed() * 3.6
                    ))
                    .color(Color32::WHITE),
                );
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
                    ui.label(
                        RichText::new(format!("{arrow} UNCUT GRASS"))
                            .size(size)
                            .strong()
                            .color(Color32::from_rgb(220, 255, 126)),
                    );
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
                    egui::Frame::window(&context.style_of(egui::Theme::Dark))
                        .fill(Color32::from_black_alpha(190))
                        .show(ui, |ui| {
                            ui.label(RichText::new(prompt).size(18.0));
                        });
                });
        }
        if self.survey_started.is_some() {
            egui::Area::new("survey".into())
                .anchor(Align2::CENTER_TOP, [0.0, 70.0])
                .show(context, |ui| {
                    ui.label(RichText::new("Surveying mountain routes…").size(20.0));
                });
        }
        if self.run.mode == GameMode::Standard && self.run.completion_available {
            egui::Area::new("submit".into())
                .anchor(Align2::CENTER_BOTTOM, [0.0, -90.0])
                .show(context, |ui| {
                    if ui
                        .add_sized([260.0, 40.0], egui::Button::new("Submit Job (Enter)"))
                        .clicked()
                    {
                        commands.push(UiCommand::Submit);
                    }
                    ui.label("or continue mowing to 100%");
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
                if ui.button("Resume").clicked() {
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
        egui::Window::new("Discard current mowing?")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
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
                    "encode    {:>5.2} ms",
                    self.last_stats.cpu_encode_milliseconds
                ));
                if self.last_stats.gpu_world_milliseconds > 0.0 {
                    ui.monospace(format!(
                        "gpu       i {:>4.2}  s {:>4.2}  w {:>4.2}  c {:>4.2} ms",
                        self.last_stats.gpu_interaction_milliseconds,
                        self.last_stats.gpu_shadow_milliseconds,
                        self.last_stats.gpu_world_milliseconds,
                        self.last_stats.gpu_composite_milliseconds,
                    ));
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
                UiCommand::OpenEditor => self.state = GameState::WorldEditor,
                UiCommand::OpenSettings => {
                    self.settings_return_state = self.state;
                    self.settings_open = true;
                }
                UiCommand::Back => {
                    if self.settings_open {
                        self.settings_open = false;
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
                    self.last_frame = Instant::now();
                }
                UiCommand::Restart => {
                    self.run.restart(&self.profile.settings.accessibility);
                    self.state = GameState::Playing;
                    self.survey_started = Some(Instant::now());
                }
                UiCommand::ReturnToEditor => {
                    self.state = GameState::WorldEditor;
                    self.run.paused = true;
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
        self.poll_generation();
        if self.state == GameState::Playing {
            // East is recovery during play, not a menu-cancel action.
            let _ = self.input.take_menu_cancel();
        } else {
            let pause_or_back = self.input.take_pause_pressed();
            let cancel = self.input.take_menu_cancel();
            if pause_or_back || cancel {
                let command = if self.settings_open {
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
        self.input.append_egui_gamepad_events(&mut raw_input);
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
        let pixels_per_point = context.pixels_per_point();
        let paint_jobs = context.tessellate(full_output.shapes, pixels_per_point);

        let Some(renderer) = &mut self.renderer else {
            return;
        };
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
        let mut frame = match renderer.begin_frame(&mut self.run, self.clock.interpolation_alpha())
        {
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
        self.profile.sanitize();
        if let Err(error) = self.profile_store.save(&self.profile) {
            tracing::warn!(%error, path = %self.profile_store.path().display(), "profile save failed");
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
        let consumed = self
            .egui_state
            .as_mut()
            .is_some_and(|state| state.on_window_event(window, &event).consumed);
        if !consumed || self.input.rebind_action.is_some() {
            self.input
                .handle_window_event(&event, &mut self.profile.settings.controls);
        }
        match event {
            WindowEvent::CloseRequested => {
                self.save_profile();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
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
                if event.state == winit::event::ElementState::Pressed
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F3) =>
            {
                self.show_diagnostics = !self.show_diagnostics;
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.input
            .poll_gamepads(&mut self.profile.settings.controls);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.run.paused = true;
        if self.state == GameState::Playing {
            self.state = GameState::Paused;
        }
        self.save_profile();
    }
}

fn configure_egui_style(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = Color32::from_rgb(23, 31, 39);
    visuals.panel_fill = Color32::from_rgb(20, 28, 36);
    visuals.selection.bg_fill = Color32::from_rgb(79, 128, 68);
    visuals.widgets.active.bg_fill = Color32::from_rgb(95, 148, 77);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(67, 102, 61);
    context.set_visuals_of(egui::Theme::Dark, visuals.clone());
    context.set_visuals_of(egui::Theme::Light, visuals);
    context.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 9.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
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
    use lawn_core::planet::CURRENT_GENERATOR_VERSION;

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
    fn world_editor_presets_generate_playable_planets() {
        let baseline = GeneratorConfig::test_quality();
        let presets = [
            WorldEditorSettings::meadow(),
            WorldEditorSettings::from_generator(&baseline),
            WorldEditorSettings::craggy(),
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
