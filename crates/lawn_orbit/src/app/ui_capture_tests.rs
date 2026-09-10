//! Optional real-egui UI references, rendered offscreen without a native window.
//! Run with:
//! ```text
//! LAWN_UI_CAPTURE_DIR=/tmp/lawn-ui-polish cargo test -p lawn_orbit gpu_capture_garden_ui_references -- --ignored --nocapture
//! ```

use std::{io::Write, path::Path};

use super::*;

#[test]
#[ignore = "optional visual references require a wgpu adapter and LAWN_UI_CAPTURE_DIR"]
fn gpu_capture_garden_ui_references() {
    let Some(directory) = std::env::var_os("LAWN_UI_CAPTURE_DIR") else {
        println!("Set LAWN_UI_CAPTURE_DIR to write UI reference images.");
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .expect("a graphics adapter is required for UI capture");
        println!("UI reference adapter: {}", adapter.get_info().name);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap();
        for size in [[960, 540], [1280, 720]] {
            for scene in [
                "title",
                "editor",
                "gameplay",
                "pause",
                "settings",
                "settings-bottom",
                "results",
                "race",
                "race-pause",
                "race-victory",
                "race-victory-lap",
                "race-victory-pause",
                "race-result-loss",
                "race-result-draw",
            ] {
                capture_scene(&device, &queue, &directory, size, scene);
            }
        }
    });
}

fn reference_app(scene: &str) -> LawnOrbitApp {
    let mut app = LawnOrbitApp::with_config(
        ProfileStore::temporary("ui-capture-unused"),
        Profile::default(),
        GameConfig {
            generator: GeneratorConfig::test_quality(),
            ..GameConfig::default()
        },
    )
    .unwrap();
    app.profile_write_enabled = false;
    app.input.gilrs = None;
    for seed in [21, 42, 100, 2026] {
        app.profile.record_seed(
            lawn_core::planet::CURRENT_GENERATOR_VERSION,
            WorldSeed(seed),
        );
    }
    let mut mowing = app.run.mowing.snapshot();
    for (index, cell) in mowing.cells.iter_mut().enumerate() {
        if index % 10 < 4 {
            *cell = lawn_core::mowing::PackedMowingCell(255);
        }
    }
    app.run.mowing.restore(&mowing).unwrap();
    app.run.vehicle.state.boost_charge = app.run.vehicle_tuning.boost_capacity_seconds * 0.65;
    app.run.vehicle.state.linear_velocity = app.run.vehicle.state.transform.forward * 8.5;
    app.run.metrics.elapsed_seconds = 92.4;
    app.run.tutorial_enabled = true;
    if scene.starts_with("race") {
        app.run.start_race(&app.profile.settings.accessibility);
        app.selected_mode = GameMode::TurfRace;
        app.run.simulation_seconds = 72.4;
        app.run.tutorial_enabled = false;
        app.run.vehicle.state.boost_charge = app.run.vehicle_tuning.boost_capacity_seconds * 0.65;
        let race = app.run.race.as_mut().unwrap();
        race.bumps = 3;
        (race.player_coverage, race.rival_coverage, race.outcome) = match scene {
            "race-victory" | "race-victory-lap" | "race-victory-pause" => {
                (0.511, 0.413, Some(RaceOutcome::PlayerWon))
            }
            "race-result-loss" => (0.427, 0.501, Some(RaceOutcome::RivalWon)),
            "race-result-draw" => (0.5, 0.5, Some(RaceOutcome::Draw)),
            _ => (0.342, 0.299, None),
        };
        race.finished_seconds = race.outcome.map(|_| 72.4);
        app.run.active = race.outcome.is_none() || race.outcome == Some(RaceOutcome::PlayerWon);
        if app.run.is_victory_lap() {
            app.run.rival = None;
            app.run.simulation_seconds = 93.0;
            app.victory_card_open = scene != "race-victory-lap";
            let mut mowing = app.run.mowing.snapshot();
            for (index, cell) in mowing.cells.iter_mut().enumerate() {
                if index % 100 < 94 {
                    *cell = lawn_core::mowing::PackedMowingCell(255);
                }
            }
            app.run.mowing.restore(&mowing).unwrap();
        }
        if scene == "race" {
            app.run.rival.as_mut().unwrap().state.transform.position =
                -app.run.vehicle.state.transform.position;
        }
    }
    app.state = match scene {
        "title" => GameState::Title,
        "editor" => GameState::WorldEditor,
        "gameplay" | "race" | "race-victory" | "race-victory-lap" => GameState::Playing,
        "pause" | "race-pause" | "race-victory-pause" => GameState::Paused,
        "race-result-loss" | "race-result-draw" => GameState::Results,
        "settings" | "settings-bottom" => {
            app.settings_open = true;
            GameState::Title
        }
        "results" => {
            let results = Results::evaluate(
                app.run.planet.generator_version,
                app.run.planet.world_seed,
                lawn_core::score::RunMetrics {
                    elapsed_seconds: 92.4,
                    coverage: 0.997,
                    distance_traveled: 1224.0,
                    estimated_ideal_distance: 1140.0,
                    ..Default::default()
                },
                &app.game_config.job,
            );
            app.profile.record_result(&results);
            app.results = Some(results);
            GameState::Results
        }
        _ => unreachable!("unknown capture scene"),
    };
    app.run.paused = app.state != GameState::Playing;
    app
}

fn capture_scene(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    directory: &Path,
    [width, height]: [u32; 2],
    scene: &str,
) {
    let mut app = reference_app(scene);
    let context = egui::Context::default();
    configure_egui_style(&context);
    let screen_rect =
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width as f32, height as f32));
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [width, height],
        pixels_per_point: 1.0,
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer =
        egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default());
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen UI reference"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = output.create_view(&wgpu::TextureViewDescriptor::default());
    let bytes_per_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("offscreen UI reference readback"),
        size: u64::from(bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    // Allow automatic window sizing and scroll animation to settle exactly as
    // they do in the actual app. Font texture deltas are applied on every frame.
    let frame_count = 24;
    for frame in 0..frame_count {
        // The capture represents an already generated preview. Slider widgets
        // can clamp test-quality defaults on their first layout pass.
        if scene == "editor" {
            app.preview_recipe = app.editor_recipe();
        }
        let mut events = Vec::new();
        if scene == "settings-bottom" && frame == 4 {
            events.push(egui::Event::PointerMoved(screen_rect.center()));
            events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -10_000.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
        }
        if frame == 5 {
            events.push(egui::Event::PointerGone);
        }
        let full_output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                time: Some(f64::from(frame) / 60.0),
                events,
                ..Default::default()
            },
            |root_ui| {
                let context = root_ui.ctx().clone();
                let mut commands = Vec::new();
                if app.settings_open {
                    // Present ordinary settings, not the profile-load warning.
                    // We only draw: commands are never processed or persisted.
                    app.profile_write_enabled = true;
                    app.draw_settings(&context, &mut commands);
                    app.profile_write_enabled = false;
                } else {
                    commands.extend(app.draw_ui(&context));
                }
                assert!(commands.is_empty(), "capture must never activate an action");
            },
        );
        for (id, delta) in &full_output.textures_delta.set {
            renderer.update_texture(device, queue, *id, delta);
        }
        let last_frame = frame + 1 == frame_count;
        let required_labels: &[&str] = match scene {
            "title" => &["Lawn Orbit", "Create a Planet"],
            "editor" => &["Shape a tiny planet", "Start Race", "Inspect"],
            "gameplay" => &["A little tidier."],
            "pause" => &["The lawn can wait.", "Resume"],
            "settings" => &["Make yourself at home.", "Camera & controls"],
            "settings-bottom" => &["Make yourself at home.", "Done"],
            "results" => &["A lovely day's work.", "Retry seed", "World editor"],
            "race" => &["YOU  34.2%", "29.9%  RIVAL", "Rival · far side"],
            "race-pause" => &["The race can wait.", "Resume", "Restart race"],
            "race-victory" => &[
                "The lawn is yours!",
                "Keep mowing",
                "Rematch",
                "World editor",
                "01:12.4",
            ],
            "race-victory-lap" => &["Race won · View result", "A little tidier."],
            "race-victory-pause" => &[
                "The lawn can wait.",
                "Keep mowing",
                "View race result",
                "Rematch",
            ],
            "race-result-loss" => &["A rematch, perhaps?", "Rematch", "World editor"],
            "race-result-draw" => &["An evenly shared lawn.", "Rematch", "World editor"],
            _ => unreachable!(),
        };
        let missing_labels: Vec<_> = if last_frame {
            required_labels
                .iter()
                .filter(|label| {
                    !full_output.shapes.iter().any(|shape| {
                        label_is_visible(&shape.shape, shape.clip_rect, screen_rect, label)
                    })
                })
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let jobs = context.tessellate(full_output.shapes, screen.pixels_per_point);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("UI reference encoder"),
        });
        let extra = renderer.update_buffers(device, queue, &mut encoder, &jobs, &screen);
        queue.submit(extra);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("actual app UI on quiet dark background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.011,
                            g: 0.016,
                            b: 0.029,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        if last_frame {
            encoder.copy_texture_to_buffer(
                output.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            encoder.map_buffer_on_submit(&readback, wgpu::MapMode::Read, .., move |result| {
                sender.send(result).unwrap();
            });
        }
        queue.submit([encoder.finish()]);
        for id in &full_output.textures_delta.free {
            renderer.free_texture(id);
        }
        if last_frame {
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            receiver.recv().unwrap().unwrap();
            let pixels = readback.slice(..).get_mapped_range();
            let path = directory.join(format!("{scene}-{width}x{height}.ppm"));
            let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
            write!(file, "P6\n{width} {height}\n255\n").unwrap();
            for row in pixels.chunks_exact(bytes_per_row as usize) {
                for pixel in row[..width as usize * 4].chunks_exact(4) {
                    assert_eq!(pixel[3], 255);
                    file.write_all(&pixel[..3]).unwrap();
                }
            }
            file.flush().unwrap();
            drop(pixels);
            readback.unmap();
            println!("Saved {}", path.display());
            assert!(
                missing_labels.is_empty(),
                "{scene} {width}×{height} has clipped or missing labels: {missing_labels:?}"
            );
        }
    }
    assert!(
        !app.profile_store.path().exists(),
        "capture wrote a profile"
    );
}

fn label_is_visible(
    shape: &egui::epaint::Shape,
    clip: egui::Rect,
    screen: egui::Rect,
    label: &str,
) -> bool {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
            let bounds = text.visual_bounding_rect();
            clip.intersect(screen).expand(1.0).contains_rect(bounds)
        }
        egui::epaint::Shape::Vec(shapes) => shapes
            .iter()
            .any(|shape| label_is_visible(shape, clip, screen, label)),
        _ => false,
    }
}
