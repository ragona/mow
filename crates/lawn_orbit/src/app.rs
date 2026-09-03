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
    flow::GameState,
    input::{Action, InputSnapshot},
    planet::{Planet, TUTORIAL_SEED},
    profile::{Profile, QualityPreset, RecordKey},
    run::{GameMode, RunEvent, RunState, TutorialStage},
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

use crate::{audio::AudioDirector, input_adapter::InputAdapter, profile_store::ProfileStore};

#[derive(Clone, Copy, Debug)]
enum ConfirmAction {
    Restart,
    ReturnToModes,
    RandomFreeMow,
}

#[derive(Clone, Debug)]
enum UiCommand {
    OpenModes,
    OpenSettings,
    Back,
    Start(GameMode, WorldSeed),
    Random(GameMode),
    Resume,
    Restart,
    ReturnToModes,
    Submit,
    Retry,
    FreeMow,
    Quit,
    ToggleFullscreen,
    ToggleFavorite(WorldSeed),
}

struct PendingGeneration {
    receiver: mpsc::Receiver<Result<Planet, String>>,
    mode: GameMode,
}

pub struct LawnOrbitApp {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    run: RunState,
    game_config: GameConfig,
    state: GameState,
    settings_return_state: GameState,
    settings_open: bool,
    clock: FixedStepClock,
    last_frame: Instant,
    input: InputAdapter,
    pending_input: InputSnapshot,
    profile: Profile,
    profile_store: ProfileStore,
    audio: Option<AudioDirector>,
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
    sound_caption: Option<(&'static str, Instant)>,
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
            GameMode::Standard,
            game_config.vehicle.clone(),
            game_config.job.clone(),
            &profile.settings.accessibility,
            !profile.tutorial_completed,
        );
        let audio = AudioDirector::new()
            .map_err(|error| {
                tracing::warn!(%error, "audio device unavailable; continuing silently");
                error
            })
            .ok();
        Ok(Self {
            window: None,
            renderer: None,
            egui_state: None,
            egui_renderer: None,
            run,
            game_config,
            state: GameState::Title,
            settings_return_state: GameState::Title,
            settings_open: false,
            clock: FixedStepClock::default(),
            last_frame: Instant::now(),
            input: InputAdapter::default(),
            pending_input: InputSnapshot::default(),
            profile,
            profile_store,
            audio,
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
            sound_caption: None,
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
        if submit && self.run.completion_available {
            self.finish_run();
        }
        if let Some(audio) = &mut self.audio {
            audio.update(&self.run, &self.profile.settings.audio);
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
            if let Some(audio) = &mut self.audio {
                audio.play_completion(&self.profile.settings.audio);
            }
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
        let mode = pending.mode;
        self.pending_generation = None;
        match result {
            Ok(planet) => {
                self.profile
                    .record_seed(planet.generator_version, planet.world_seed);
                self.seed_text = planet.world_seed.to_string();
                self.run = RunState::new(
                    planet,
                    mode,
                    self.game_config.vehicle.clone(),
                    self.game_config.job.clone(),
                    &self.profile.settings.accessibility,
                    !self.profile.tutorial_completed && mode == GameMode::Standard,
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
                self.state = GameState::ModeSelect;
            }
        }
    }

    fn start_generation(&mut self, mode: GameMode, seed: WorldSeed) {
        let generator = PlanetGenerator::new(
            lawn_core::planet::CURRENT_GENERATOR_VERSION,
            self.game_config.generator.clone(),
        );
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("planet-generator".into())
            .spawn(move || {
                let _span = tracing::info_span!("planet_generation", seed = %seed).entered();
                let result = generator.generate(seed).map_err(|error| error.to_string());
                let _ = sender.send(result);
            })
            .expect("planet generation worker should start");
        self.pending_generation = Some(PendingGeneration { receiver, mode });
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
        let radius = 52.0;
        self.run.camera.state.position =
            glam::Vec3::new(angle.cos() * radius, 22.0, angle.sin() * radius);
        self.run.camera.state.target = glam::Vec3::ZERO;
        self.run.camera.state.up = glam::Vec3::Y;
    }

    fn draw_ui(&mut self, context: &egui::Context) -> Vec<UiCommand> {
        let mut commands = Vec::new();
        match self.state {
            GameState::Title => Self::draw_title(context, &mut commands),
            GameState::ModeSelect => self.draw_mode_select(context, &mut commands),
            GameState::Loading => Self::draw_loading(context),
            GameState::Playing => self.draw_hud(context, &mut commands),
            GameState::Paused => self.draw_pause(context, &mut commands),
            GameState::Results => self.draw_results(context, &mut commands),
            GameState::Boot => {}
        }
        if self.state == GameState::Title || self.state == GameState::Results {
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
                        .add_sized([250.0, 42.0], egui::Button::new("Mow a Planet"))
                        .clicked()
                    {
                        commands.push(UiCommand::OpenModes);
                    }
                    if ui.button("Settings & Accessibility").clicked() {
                        commands.push(UiCommand::OpenSettings);
                    }
                    if ui.button("Quit").clicked() {
                        commands.push(UiCommand::Quit);
                    }
                    ui.add_space(8.0);
                    ui.small(format!("Tutorial seed {TUTORIAL_SEED}"));
                });
            });
    }

    fn draw_mode_select(&mut self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        egui::Window::new("Choose a job")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.set_min_width(460.0);
                ui.heading("Planet seed");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.seed_text);
                    if ui.button("Tutorial seed").clicked() {
                        self.seed_text = TUTORIAL_SEED.to_string();
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
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            parsed.is_ok(),
                            egui::Button::new("Standard Job\n98% target · rated"),
                        )
                        .clicked()
                    {
                        commands.push(UiCommand::Start(GameMode::Standard, parsed.unwrap()));
                    }
                    if ui
                        .add_enabled(
                            parsed.is_ok(),
                            egui::Button::new("Free Mow\nNo timer · no penalties"),
                        )
                        .clicked()
                    {
                        commands.push(UiCommand::Start(GameMode::FreeMow, parsed.unwrap()));
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Random standard job").clicked() {
                        commands.push(UiCommand::Random(GameMode::Standard));
                    }
                    if ui.button("Random Free Mow").clicked() {
                        commands.push(UiCommand::Random(GameMode::FreeMow));
                    }
                });
                ui.add_space(8.0);
                if ui.button("Back").clicked() {
                    commands.push(UiCommand::Back);
                }
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
                        if self.run.mode == GameMode::Standard {
                            ui.label(format_time(self.run.metrics.elapsed_seconds));
                        } else {
                            ui.label("FREE MOW");
                        }
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
        if self.run.completion_available {
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
        if let Some(caption) = self
            .run
            .events()
            .iter()
            .rev()
            .find_map(|event| match event {
                RunEvent::GrassCut { .. } => Some("✦ clippings rustle"),
                RunEvent::RockScrape => Some("⚠ deck scraping rock"),
                RunEvent::SubstantialCollision { .. } => Some("◆ rock impact"),
                RunEvent::Recovered => Some("↻ mower recovered"),
                RunEvent::CompletionAvailable => Some("★ job target reached"),
                _ => None,
            })
        {
            self.sound_caption = Some((caption, Instant::now()));
        }
        if self
            .sound_caption
            .is_some_and(|(_, started)| started.elapsed() > Duration::from_secs_f32(1.4))
        {
            self.sound_caption = None;
        }
        if let Some((caption, _)) = self.sound_caption {
            egui::Area::new("sound-caption".into())
                .anchor(Align2::RIGHT_BOTTOM, [-18.0, -18.0])
                .show(context, |ui| {
                    ui.label(RichText::new(caption).color(Color32::WHITE));
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
                if ui.button("Restart same seed").clicked() {
                    self.confirmation = Some(ConfirmAction::Restart);
                }
                if self.run.mode == GameMode::FreeMow && ui.button("New random planet").clicked() {
                    self.confirmation = Some(ConfirmAction::RandomFreeMow);
                }
                if ui.button("Return to mode select").clicked() {
                    self.confirmation = Some(ConfirmAction::ReturnToModes);
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
                    if ui.button("Free Mow").clicked() {
                        commands.push(UiCommand::FreeMow);
                    }
                    if ui.button("Mode select").clicked() {
                        commands.push(UiCommand::ReturnToModes);
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
                    ui.heading("Audio");
                    ui.add(egui::Slider::new(&mut self.profile.settings.audio.master, 0.0..=1.0).text("Master"));
                    ui.add(egui::Slider::new(&mut self.profile.settings.audio.music, 0.0..=1.0).text("Music"));
                    ui.add(egui::Slider::new(&mut self.profile.settings.audio.effects, 0.0..=1.0).text("Effects"));
                    ui.add(egui::Slider::new(&mut self.profile.settings.audio.ambience, 0.0..=1.0).text("Ambience"));
                    ui.separator();
                    ui.heading("Camera & controls");
                    let a = &mut self.profile.settings.accessibility;
                    ui.add(egui::Slider::new(&mut a.camera_shake, 0.0..=1.0).text("Camera shake"));
                    ui.add(egui::Slider::new(&mut a.field_of_view_degrees, 50.0..=95.0).text("Field of view"));
                    ui.add(egui::Slider::new(&mut a.camera_follow_stiffness, 1.0..=20.0).text("Follow stiffness"));
                    ui.add(egui::Slider::new(&mut a.steering_sensitivity, 0.25..=2.0).text("Strafe sensitivity"));
                    ui.checkbox(&mut a.invert_steering, "Invert strafe");
                    ui.checkbox(&mut a.invert_camera_y, "Invert camera Y");
                    ui.checkbox(&mut a.fixed_horizon, "Fixed-horizon comfort mode");
                    ui.checkbox(&mut a.boost_enabled, "Enable boost");
                    ui.separator();
                    ui.heading("Visual accessibility");
                    ui.checkbox(&mut a.high_contrast_grass, "High-contrast cut grass");
                    ui.checkbox(&mut a.reduced_particles, "Reduced particles");
                    ui.checkbox(&mut a.enlarged_locator, "Enlarged uncut-grass locator");
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
                                Action::SteerLeft => "StrafeLeft".to_owned(),
                                Action::SteerRight => "StrafeRight".to_owned(),
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
        egui::Window::new("Discard this run?")
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
                            ConfirmAction::ReturnToModes => UiCommand::ReturnToModes,
                            ConfirmAction::RandomFreeMow => UiCommand::Random(GameMode::FreeMow),
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
                UiCommand::OpenModes => self.state = GameState::ModeSelect,
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
                UiCommand::Start(mode, seed) => self.start_generation(mode, seed),
                UiCommand::Random(mode) => self.start_generation(mode, Self::random_seed()),
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
                UiCommand::ReturnToModes => {
                    self.state = GameState::ModeSelect;
                    self.run.paused = true;
                }
                UiCommand::Submit => self.finish_run(),
                UiCommand::Retry => {
                    self.start_generation(GameMode::Standard, self.run.planet.world_seed);
                }
                UiCommand::FreeMow => {
                    self.start_generation(GameMode::FreeMow, self.run.planet.world_seed);
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
                        GameState::ModeSelect => Some(UiCommand::Back),
                        GameState::Results => Some(UiCommand::ReturnToModes),
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
