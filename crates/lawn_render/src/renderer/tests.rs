use super::*;

mod sky;
mod visibility;
use lawn_core::{
    GameMode, GeneratorConfig, JobConfig, PlanetGenerator, VehicleTuning, WorldSeed,
    input::InputSnapshot, planet::CURRENT_GENERATOR_VERSION, profile::AccessibilitySettings,
};

/// Art references use the real authoritative sweep API, including its comb
/// encoding and seam handling. Never paint synthetic stripes in the shader.
fn prepare_mowing_reference(run: &mut RunState, view: &str) -> (Vec3, Vec3) {
    use lawn_core::mowing::MowingStamp;
    let normal =
        Vec3::new(0.42, 0.81, 0.38).normalize() * if view.ends_with("night") { -1.0 } else { 1.0 };
    let right = normal.cross(Vec3::Y).normalize();
    let up = right.cross(normal).normalize();
    let radius = run.planet.config.base_radius;
    let surface = |x: f32, y: f32| {
        run.planet
            .surface_point((normal * radius + right * x + up * y).normalize())
    };
    let mut paths: Vec<Vec<Vec3>> = Vec::new();
    if view.starts_with("curve") {
        for radius in [3.6_f32, 7.2] {
            paths.push(
                (0..=80)
                    .map(|step| {
                        let angle = step as f32 / 80.0 * std::f32::consts::TAU * 0.85;
                        surface(angle.cos() * radius, angle.sin() * radius)
                    })
                    .collect(),
            );
        }
    } else {
        for lane in -3..=3 {
            let x = lane as f32 * 2.25;
            let direction = if lane % 2 == 0 { 1.0 } else { -1.0 };
            paths.push(
                (0..=40)
                    .map(|step| surface(x, (step as f32 * 0.4 - 8.0) * direction))
                    .collect(),
            );
        }
        if view.starts_with("crosscut") {
            for y in [-4.0, 1.0, 6.0] {
                paths.push(
                    (0..=40)
                        .map(|step| surface(step as f32 * 0.45 - 9.0, y))
                        .collect(),
                );
            }
        }
    }
    for path in paths {
        for pair in path.windows(2) {
            run.mowing.stamp(MowingStamp {
                from: pair[0],
                to: pair[1],
                comb_direction: (pair[1] - pair[0]).normalize(),
                deck_width: run.vehicle_tuning.mower_width,
                cut_delta: 1.0,
                recent_epoch: 0,
            });
        }
    }
    (normal * 40.0, up)
}

/// Keep the offscreen race reference deterministic while using real mower
/// geometry and authoritative owned stamps for both trails.
fn prepare_race_reference(run: &mut RunState, bumper: bool) {
    use lawn_core::{mowing::MowingStamp, vehicle::VehicleTransform};
    let normal = Vec3::new(0.42, 0.81, 0.38).normalize();
    let forward = (Vec3::Z - normal * normal.z).normalize();
    let right = forward.cross(normal).normalize();
    let radius = run.planet.config.base_radius;
    let separation = if bumper { 1.10 } else { 2.35 };
    let mut poses = Vec::new();
    for (x, owner) in [(-separation, 1), (separation, 2)] {
        let point = |distance: f32| {
            run.planet
                .surface_point((normal * radius + right * x + forward * distance).normalize())
        };
        for step in 0..40 {
            let from = point(-6.0 + step as f32 * 0.15);
            let to = point(-6.0 + (step + 1) as f32 * 0.15);
            run.mowing.stamp_owned(
                MowingStamp {
                    from,
                    to,
                    comb_direction: (to - from).normalize(),
                    deck_width: run.vehicle_tuning.mower_width,
                    cut_delta: 1.0,
                    recent_epoch: 0,
                },
                owner,
            );
        }
        let surface = point(0.0);
        let up = surface.normalize();
        let forward = (forward - up * forward.dot(up)).normalize();
        poses.push(VehicleTransform {
            position: surface + up * run.vehicle_tuning.hover_height,
            rotation: glam::Quat::from_mat3(&glam::Mat3::from_cols(
                forward.cross(up),
                up,
                -forward,
            )),
            forward,
            up,
        });
    }
    run.vehicle.state.transform = poses[0];
    run.vehicle.state.grounded = true;
    let rival = run.rival.as_mut().expect("race must create a rival");
    rival.state.transform = poses[1];
    rival.state.grounded = true;
}

#[test]
fn ownership_mirror_preserves_cut_and_comb_and_reuses_tile_storage() {
    use lawn_core::mowing::PackedMowingCell;
    let cells = [
        PackedMowingCell(0x91_34_56_fa),
        PackedMowingCell(0xb3_cd_ef_ff),
    ];
    let mut staging = Vec::with_capacity(256);
    let address = staging.as_ptr();
    pack_owned_cells(&mut staging, &cells, &[1, 2]);
    assert_eq!(staging, [0x01_34_56_fa, 0x02_cd_ef_ff]);
    pack_owned_cells(&mut staging, &cells, &[]);
    assert_eq!(staging, [0x00_34_56_fa, 0x00_cd_ef_ff]);
    assert_eq!(staging.as_ptr(), address);
}

#[test]
fn msaa_selection_requires_color_depth_and_resolve_support() {
    use wgpu::TextureFormatFeatureFlags as Flags;
    let baseline = Flags::MULTISAMPLE_X4 | Flags::MULTISAMPLE_RESOLVE;
    assert_eq!(supported_msaa_samples(2, baseline, baseline), 1);
    assert_eq!(supported_msaa_samples(4, baseline, baseline), 4);
    assert_eq!(supported_msaa_samples(4, baseline, Flags::empty()), 1);
    assert_eq!(
        supported_msaa_samples(4, Flags::MULTISAMPLE_X4, baseline),
        1
    );
    let extended = baseline | Flags::MULTISAMPLE_X2;
    assert_eq!(supported_msaa_samples(2, extended, extended), 2);
    assert_eq!(supported_msaa_samples(1, extended, extended), 1);
}

#[test]
fn shadow_projection_contains_small_and_large_worlds() {
    for radius in [8.0, 20.0, 85.0, 150.0] {
        let projection = shadow_view_projection(radius);
        for direction in [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::Z,
            Vec3::NEG_Z,
        ] {
            let projected = projection.project_point3(direction * radius);
            assert!(projected.x.abs() <= 1.0 && projected.y.abs() <= 1.0);
            assert!((0.0..=1.0).contains(&projected.z));
        }
    }
}

#[test]
fn grass_horizon_retains_elevated_roots_and_blades() {
    // The far tangent point of an elevated shell is visible across both
    // camera-to-occluder and occluder-to-blade tangent angles.
    let expected = (14.0_f32 / 20.0).acos() + (14.0_f32 / 19.0).acos();
    assert!((grass_horizon_angle(14.0, 19.0, 20.0) - expected).abs() < 1.0e-6);
    assert!(grass_horizon_angle(14.0, 19.0, 20.0) > (15.0_f32 / 20.0).acos() + 0.14);
    assert_eq!(grass_horizon_angle(14.0, 19.0, 13.0), std::f32::consts::PI);
}

/// Runs the real compute, shadow, world, particle and composite pipelines,
/// including GPU readback, without depending on a window server.
#[test]
#[ignore = "requires a working wgpu graphics adapter"]
fn gpu_smoke_renders_all_passes_at_supported_sample_counts() {
    run_gpu_smoke(false);
}

#[test]
#[ignore = "requires a working wgpu graphics adapter"]
fn gpu_smoke_race_renders_both_mowers_at_supported_sample_counts() {
    run_gpu_smoke(true);
}

fn run_gpu_smoke(force_race: bool) {
    pollster::block_on(async {
        // Optional art references use the same real render passes as the smoke
        // check, with shipping density and a repeatable camera/planet recipe.
        let capture_dir = std::env::var_os("LAWN_CAPTURE_DIR").map(std::path::PathBuf::from);
        let capture_view = std::env::var("LAWN_CAPTURE_VIEW").unwrap_or_else(|_| "day".into());
        let capture = capture_dir.is_some();
        let race = force_race || matches!(capture_view.as_str(), "race" | "bumper");
        let mowing_reference = capture
            && ["stripes", "crosscut", "curve"]
                .iter()
                .any(|prefix| capture_view.starts_with(prefix));
        let benchmark = std::env::var_os("LAWN_BENCH").is_some();
        let scene = std::env::var("LAWN_BENCH_SCENE").unwrap_or_else(|_| "craggy".into());
        let parameter = |name, default| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(default)
        };
        let quality = if std::env::var("LAWN_BENCH_QUALITY").is_ok_and(|value| value == "low") {
            QualityPreset::Low
        } else {
            QualityPreset::High
        };
        let (width, height) = if benchmark {
            (
                parameter("LAWN_BENCH_WIDTH", 1280),
                parameter("LAWN_BENCH_HEIGHT", 720),
            )
        } else if capture {
            (1024, 768)
        } else {
            (64, 64)
        };
        assert!(
            width > 0 && height > 0 && width % 64 == 0,
            "readback rows must be aligned"
        );
        let measured_frames = parameter("LAWN_BENCH_FRAMES", 120).max(1);
        let warmup_frames = 60;
        let frame_count = if benchmark {
            warmup_frames + measured_frames
        } else {
            1
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        let features = adapter.features()
            & (wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
                | wgpu::Features::TIMESTAMP_QUERY);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: features,
                ..Default::default()
            })
            .await
            .unwrap();
        let flags = |format: wgpu::TextureFormat| {
            if features.contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES) {
                adapter.get_texture_format_features(format).flags
            } else {
                format.guaranteed_format_features(features).flags
            }
        };
        let mut counts = [1, 2, 4]
            .map(|requested| {
                supported_msaa_samples(requested, flags(WORLD_FORMAT), flags(DEPTH_FORMAT))
            })
            .to_vec();
        counts.dedup();
        if benchmark {
            assert!(
                features.contains(wgpu::Features::TIMESTAMP_QUERY),
                "benchmark requires GPU timestamps"
            );
            counts = vec![supported_msaa_samples(
                parameter(
                    "LAWN_BENCH_MSAA",
                    if quality == QualityPreset::Low { 2 } else { 4 },
                ),
                flags(WORLD_FORMAT),
                flags(DEPTH_FORMAT),
            )];
        }
        println!(
            "GPU smoke adapter: {}; sample counts: {counts:?}",
            adapter.get_info().name
        );
        let mut profiler = features
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| GpuProfiler::new(&device, queue.get_timestamp_period()));
        let accessibility = AccessibilitySettings::default();
        let mut planet_config = if capture || benchmark {
            GeneratorConfig {
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
                ..GeneratorConfig::default()
            }
        } else {
            GeneratorConfig {
                grass_roots_per_square_meter: 2.0,
                ..GeneratorConfig::test_quality()
            }
        };
        if (benchmark && scene == "meadow") || mowing_reference || (capture && race) {
            planet_config = GeneratorConfig {
                mountain_count_min: 0,
                mountain_count_max: 0,
                mowable_ratio_min: 1.0,
                mowable_ratio_max: 1.0,
                rolling_amplitude: 0.3,
                ..GeneratorConfig::default()
            };
        }
        let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, planet_config)
            .generate(WorldSeed(21))
            .unwrap();
        assert!(
            !planet.grass_roots.is_empty(),
            "smoke test must exercise grass vertex data"
        );
        println!("GPU smoke grass roots: {}", planet.grass_roots.len());
        let mut run = RunState::new(
            planet,
            if race {
                GameMode::TurfRace
            } else {
                GameMode::FreeMow
            },
            VehicleTuning::default(),
            JobConfig::default(),
            &accessibility,
            false,
        );
        for _ in 0..120 {
            run.tick(InputSnapshot::default(), &accessibility);
            if run
                .events()
                .iter()
                .any(|event| matches!(event, lawn_core::run::RunEvent::GrassCut { .. }))
            {
                break;
            }
        }
        if race {
            prepare_race_reference(&mut run, capture_view == "bumper");
        }
        let mowing_camera =
            mowing_reference.then(|| prepare_mowing_reference(&mut run, &capture_view));
        if benchmark && scene == "mown" {
            let mut snapshot = run.mowing.snapshot();
            snapshot
                .cells
                .fill(lawn_core::mowing::PackedMowingCell(255));
            run.mowing.restore(&snapshot).unwrap();
        }
        let resources = create_planet_resources(&device, &queue, &run);
        let mut interaction = GrassInteraction::new(&device, run.planet.config.base_radius);
        let mut particles = ClippingParticles::new(&device);
        particles.update(&queue, &run, false);
        if capture {
            use lawn_core::run::RunEvent;
            let event = match capture_view.as_str() {
                "bumper" => Some(RunEvent::MowerBump {
                    impulse: 8.0,
                    position: (run.vehicle.state.transform.position
                        + run.rival.as_ref().unwrap().state.transform.position)
                        * 0.5,
                }),
                "recovery" => Some(RunEvent::Recovered),
                "impact" => Some(RunEvent::SubstantialCollision { impulse: 8.0 }),
                "milestone" => Some(RunEvent::CoverageMilestone(25)),
                _ => None,
            };
            if let Some(event) = event {
                particles.preview_event(&queue, &run, event);
            }
        }
        assert!(
            particles.len() > 0,
            "smoke test must exercise a particle draw"
        );
        let mut vehicle_vertices = Vec::new();
        let mut vehicle_indices = Vec::new();
        mesh::build_vehicle(
            &mut vehicle_vertices,
            &mut vehicle_indices,
            run.vehicle.state.transform,
            run.vehicle.state.transform,
            true,
            0.0,
        );
        let vehicle_vertices = create_init_buffer(
            &device,
            "smoke vehicle vertices",
            bytemuck::cast_slice(&vehicle_vertices),
            wgpu::BufferUsages::VERTEX,
        );
        let rival_vertices = run.rival.as_ref().map(|rival| {
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            mesh::build_rival_vehicle(
                &mut vertices,
                &mut indices,
                rival.state.transform,
                rival.state.transform,
                true,
                0.0,
            );
            assert_eq!(indices, vehicle_indices, "rival must share player topology");
            create_init_buffer(
                &device,
                "smoke rival vertices",
                bytemuck::cast_slice(&vertices),
                wgpu::BufferUsages::VERTEX,
            )
        });
        let vehicle_index_count = vehicle_indices.len() as u32;
        let vehicle_indices = create_init_buffer(
            &device,
            "smoke vehicle indices",
            bytemuck::cast_slice(&vehicle_indices),
            wgpu::BufferUsages::INDEX,
        );
        let (tuft_vertices, tuft_indices) = mesh::build_tuft();
        let tuft_index_count = tuft_indices.len() as u32;
        let tuft_vertices = create_init_buffer(
            &device,
            "smoke tuft vertices",
            bytemuck::cast_slice(&tuft_vertices),
            wgpu::BufferUsages::VERTEX,
        );
        let tuft_indices = create_init_buffer(
            &device,
            "smoke tuft indices",
            bytemuck::cast_slice(&tuft_indices),
            wgpu::BufferUsages::INDEX,
        );
        let (camera, look_at) = if race {
            let player = run.vehicle.state.transform;
            let rival = run.rival.as_ref().unwrap().state.transform;
            let center = (player.position + rival.position) * 0.5;
            let up = center.normalize();
            (
                center + up * 9.0 - player.forward * 5.5,
                center - player.forward * 1.3,
            )
        } else if let Some((camera, _)) = mowing_camera {
            (camera, Vec3::ZERO)
        } else if benchmark {
            (run.camera.state.position, run.camera.state.target)
        } else if capture {
            match capture_view.as_str() {
                "vehicle" | "recovery" | "impact" | "milestone" => {
                    let pose = run.vehicle.state.transform;
                    (
                        pose.position + pose.up * 4.8 - pose.forward * 2.2,
                        pose.position,
                    )
                }
                "night" => (Vec3::new(-30.0, -24.0, -34.0), Vec3::ZERO),
                "moon" => {
                    let moon = Vec3::new(-0.76, 0.15, -0.63).normalize();
                    (
                        (-moon + moon.cross(Vec3::Y) * 0.68).normalize() * 50.0,
                        Vec3::ZERO,
                    )
                }
                "detail" => {
                    let mountain = run
                        .planet
                        .mountains
                        .iter()
                        .max_by(|a, b| {
                            a.center
                                .dot(Vec3::new(0.42, 0.81, 0.38))
                                .total_cmp(&b.center.dot(Vec3::new(0.42, 0.81, 0.38)))
                        })
                        .unwrap();
                    let target = run.planet.surface_point(mountain.center);
                    (target + mountain.center * 11.0 + Vec3::Y * 2.0, target)
                }
                _ => (Vec3::new(0.0, 29.0, 38.0), Vec3::ZERO),
            }
        } else {
            (Vec3::new(0.0, 25.0, 32.0), Vec3::ZERO)
        };
        let camera_up = if race {
            run.vehicle.state.transform.forward
        } else if let Some((_, up)) = mowing_camera {
            up
        } else if ["vehicle", "recovery", "impact", "milestone"].contains(&capture_view.as_str()) {
            run.vehicle.state.transform.forward
        } else if benchmark {
            run.camera.state.up
        } else {
            Vec3::Y
        };
        let camera_fov = if benchmark {
            run.camera.state.field_of_view_degrees
        } else {
            60.0_f32
        };
        let uniform = FrameUniformGpu {
            view_proj: (Mat4::perspective_rh(
                camera_fov.to_radians(),
                width as f32 / height as f32,
                0.08,
                180.0,
            ) * Mat4::look_at_rh(camera, look_at, camera_up))
            .to_cols_array_2d(),
            light_view_proj: resources.light_view_proj.to_cols_array_2d(),
            camera_time: [camera.x, camera.y, camera.z, 1.0],
            light_epoch: [-0.42, -0.81, -0.38, 0.0],
            options: [
                quality_density(quality) * if race { -1.0 } else { 1.0 },
                INTERACTION_RESOLUTION as f32,
                0.0,
                if capture || benchmark {
                    run.planet.config.grass_height_scale
                } else {
                    1.0
                },
            ],
            locator: [0.0; 4],
            mower_position: run.vehicle.state.transform.position.extend(1.0).to_array(),
            mower_forward: run.vehicle.state.transform.forward.extend(0.0).to_array(),
            rival_position: run.rival.as_ref().map_or([0.0; 4], |rival| {
                rival.state.transform.position.extend(1.0).to_array()
            }),
            rival_forward: run.rival.as_ref().map_or([0.0; 4], |rival| {
                rival.state.transform.forward.extend(0.0).to_array()
            }),
        };
        let camera_projection = Mat4::from_cols_array_2d(&uniform.view_proj);
        let mut visibility = GrassVisibility::new(
            &run,
            CameraState {
                position: camera,
                target: look_at,
                up: camera_up,
                field_of_view_degrees: camera_fov,
            },
            quality,
            &resources,
            Mat4::from_cols_array_2d(&uniform.view_proj),
            uniform.options[3],
        );
        // Reference mode keeps the same density and root prefixes but bypasses
        // visibility rejection for pixel comparisons against the optimized path.
        if benchmark && std::env::var_os("LAWN_BENCH_NO_CULL").is_some() {
            visibility.planes = [Vec4::ZERO; 6];
            visibility.inner_radius = 0.0;
        }
        let visible_roots: u32 = run
            .planet
            .grass_patches
            .iter()
            .zip(&resources.grass_bounds)
            .map(|(patch, bounds)| visibility.draw_count(patch, bounds))
            .sum();
        let composite_uniform = create_init_buffer(
            &device,
            "smoke composite uniform",
            bytemuck::bytes_of(&CompositeUniformGpu {
                inverse_view_proj: Mat4::from_cols_array_2d(&uniform.view_proj)
                    .inverse()
                    .to_cols_array_2d(),
                camera_radius: camera
                    .extend(run.planet.config.base_radius + 0.6)
                    .to_array(),
                sun_time: [0.42, 0.81, 0.38, 1.0],
                display: [
                    width as f32 / height as f32,
                    0.0,
                    0.65,
                    if quality == QualityPreset::Low {
                        0.0
                    } else {
                        0.20
                    },
                ],
            }),
            wgpu::BufferUsages::UNIFORM,
        );
        let uniform = create_init_buffer(
            &device,
            "smoke frame uniform",
            bytemuck::bytes_of(&uniform),
            wgpu::BufferUsages::UNIFORM,
        );
        let shadow = create_shadow(&device);
        let frame_layout = create_frame_layout(&device);
        let frame_group = create_frame_bind_group(&device, &frame_layout, &uniform, &shadow);
        let shadow_layout = create_shadow_frame_layout(&device);
        let shadow_group = create_shadow_frame_bind_group(&device, &shadow_layout, &uniform);
        let grass_layout = create_grass_layout(&device);
        let grass_groups = create_grass_bind_groups(
            &device,
            &grass_layout,
            &resources.mowing_view,
            interaction.displacement_buffers(),
        );
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Rgba8Unorm,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };
        let sky = SkyMap::new(&device, &queue);
        for samples in counts {
            let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let targets = create_targets(&device, &config, samples, 1.0);
            let (terrain_pipeline, grass_pipeline, particle_pipeline, shadow_pipeline) =
                create_pipelines(
                    &device,
                    WORLD_FORMAT,
                    samples,
                    &frame_layout,
                    &grass_layout,
                    &shadow_layout,
                );
            let bloom = Bloom::new(&device, &targets.world_view, config.width, config.height);
            let (composite, _, _, composite_group) = create_composite_resources(
                &device,
                config.format,
                &targets.world_view,
                bloom.view(),
                &composite_uniform,
                sky.view(),
            );
            let output = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("smoke output"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("smoke readback"),
                size: u64::from(width) * u64::from(height) * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut measurements = Vec::with_capacity(measured_frames as usize);
            for frame_index in 0..frame_count {
                let encode_started = Instant::now();
                let mut encoder =
                    device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                let slot = profiler
                    .as_mut()
                    .and_then(|profiler| profiler.begin_frame(&device));
                let query_set = slot.and_then(|_| profiler.as_ref().map(GpuProfiler::query_set));
                interaction.encode(
                    &queue,
                    &mut encoder,
                    &run.planet,
                    &run.vehicle.state,
                    run.rival.as_ref().map(|rival| &rival.state),
                    2.2,
                    frame_index as f32 / 60.0,
                    1.0 / 60.0,
                    query_set.map(|query_set| wgpu::ComputePassTimestampWrites {
                        query_set,
                        beginning_of_pass_write_index: Some(0),
                        end_of_pass_write_index: Some(1),
                    }),
                );
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        timestamp_writes: query_set.map(|query_set| {
                            wgpu::RenderPassTimestampWrites {
                                query_set,
                                beginning_of_pass_write_index: Some(2),
                                end_of_pass_write_index: Some(3),
                            }
                        }),
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &shadow.view,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        }),
                        ..Default::default()
                    });
                    pass.set_pipeline(&shadow_pipeline);
                    pass.set_bind_group(0, &shadow_group, &[]);
                    pass.set_vertex_buffer(0, resources.terrain_vertices.slice(..));
                    pass.set_index_buffer(
                        resources.terrain_indices.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(0..resources.terrain_index_count, 0, 0..1);
                    pass.set_vertex_buffer(0, vehicle_vertices.slice(..));
                    pass.set_index_buffer(vehicle_indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..vehicle_index_count, 0, 0..1);
                    if let Some(rival_vertices) = &rival_vertices {
                        pass.set_vertex_buffer(0, rival_vertices.slice(..));
                        pass.draw_indexed(0..vehicle_index_count, 0, 0..1);
                    }
                }
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        timestamp_writes: query_set.map(|query_set| {
                            wgpu::RenderPassTimestampWrites {
                                query_set,
                                beginning_of_pass_write_index: Some(4),
                                end_of_pass_write_index: Some(5),
                            }
                        }),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: targets
                                .multisample_view
                                .as_ref()
                                .unwrap_or(&targets.world_view),
                            depth_slice: None,
                            resolve_target: targets
                                .multisample_view
                                .as_ref()
                                .map(|_| &targets.world_view),
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: if targets.multisample_view.is_some() {
                                    wgpu::StoreOp::Discard
                                } else {
                                    wgpu::StoreOp::Store
                                },
                            },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &targets.depth_view,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                store: wgpu::StoreOp::Discard,
                            }),
                            stencil_ops: None,
                        }),
                        ..Default::default()
                    });
                    pass.set_pipeline(&terrain_pipeline);
                    pass.set_bind_group(0, &frame_group, &[]);
                    pass.set_vertex_buffer(0, resources.terrain_vertices.slice(..));
                    pass.set_index_buffer(
                        resources.terrain_indices.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(0..resources.terrain_index_count, 0, 0..1);
                    pass.set_vertex_buffer(0, vehicle_vertices.slice(..));
                    pass.set_index_buffer(vehicle_indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..vehicle_index_count, 0, 0..1);
                    if let Some(rival_vertices) = &rival_vertices {
                        pass.set_vertex_buffer(0, rival_vertices.slice(..));
                        pass.draw_indexed(0..vehicle_index_count, 0, 0..1);
                    }
                    pass.set_pipeline(&grass_pipeline);
                    let parity = usize::from(!std::ptr::eq(
                        interaction.current_displacement(),
                        interaction.displacement_buffers()[0],
                    ));
                    pass.set_bind_group(1, &grass_groups[parity], &[]);
                    pass.set_vertex_buffer(0, tuft_vertices.slice(..));
                    pass.set_vertex_buffer(1, resources.grass_roots.slice(..));
                    pass.set_index_buffer(tuft_indices.slice(..), wgpu::IndexFormat::Uint16);
                    if benchmark {
                        for (patch, bounds) in
                            run.planet.grass_patches.iter().zip(&resources.grass_bounds)
                        {
                            let count = visibility.draw_count(patch, bounds);
                            if count > 0 {
                                pass.draw_indexed(
                                    0..tuft_index_count,
                                    0,
                                    patch.roots.start..patch.roots.start + count,
                                );
                            }
                        }
                    } else {
                        pass.draw_indexed(
                            0..tuft_index_count,
                            0,
                            0..run.planet.grass_roots.len() as u32,
                        );
                    }
                    pass.set_pipeline(&particle_pipeline);
                    particles.draw(&mut pass);
                }
                if quality != QualityPreset::Low {
                    bloom.encode(&mut encoder, query_set);
                }
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        timestamp_writes: query_set.map(|query_set| {
                            wgpu::RenderPassTimestampWrites {
                                query_set,
                                beginning_of_pass_write_index: (quality == QualityPreset::Low)
                                    .then_some(6),
                                end_of_pass_write_index: Some(7),
                            }
                        }),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &output_view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    pass.set_pipeline(&composite);
                    pass.set_bind_group(0, &composite_group, &[]);
                    pass.draw(0..3, 0..1);
                }
                let final_frame = frame_index + 1 == frame_count;
                if final_frame {
                    encoder.copy_texture_to_buffer(
                        output.as_image_copy(),
                        wgpu::TexelCopyBufferInfo {
                            buffer: &readback,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(width * 4),
                                rows_per_image: Some(height),
                            },
                        },
                        wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                if final_frame {
                    encoder.map_buffer_on_submit(
                        &readback,
                        wgpu::MapMode::Read,
                        ..,
                        move |result| {
                            sender.send(result).unwrap();
                        },
                    );
                }
                if let (Some(slot), Some(profiler)) = (slot, profiler.as_mut()) {
                    profiler.finish_encoding(&mut encoder, slot);
                }
                queue.submit([encoder.finish()]);
                let encode_ms = encode_started.elapsed().as_secs_f32() * 1000.0;
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                if final_frame {
                    receiver.recv().unwrap().unwrap();
                    let pixels = readback.slice(..).get_mapped_range();
                    if !capture && !benchmark {
                        let center =
                            &pixels[((height / 2 * width + width / 2) * 4) as usize..][..4];
                        assert!(
                            center[0] > center[2] || center[1] > center[2],
                            "world center must show terrain, not blue sky: {center:?}"
                        );
                    }
                    assert!(pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
                    if race {
                        for (position, blue) in [
                            (run.vehicle.state.transform.position, false),
                            (run.rival.as_ref().unwrap().state.transform.position, true),
                        ] {
                            let ndc = camera_projection.project_point3(position);
                            let center_x = (ndc.x * 0.5 + 0.5) * width as f32;
                            let center_y = (-ndc.y * 0.5 + 0.5) * height as f32;
                            let radius = height as f32 * 0.13;
                            let matching = pixels
                                .chunks_exact(4)
                                .enumerate()
                                .filter(|(index, pixel)| {
                                    let x = (*index % width as usize) as f32;
                                    let y = (*index / width as usize) as f32;
                                    let color = if blue {
                                        f32::from(pixel[2]) > f32::from(pixel[0]) * 1.25
                                            && f32::from(pixel[2]) > f32::from(pixel[1]) * 1.10
                                    } else {
                                        f32::from(pixel[0]) > f32::from(pixel[2]) * 1.25
                                            && f32::from(pixel[0]) > f32::from(pixel[1]) * 1.25
                                    };
                                    (x - center_x).abs() < radius
                                        && (y - center_y).abs() < radius
                                        && color
                                })
                                .count();
                            assert!(
                                matching >= 2,
                                "both mower palettes must reach the GPU image; blue={blue}, pixels={matching}"
                            );
                        }
                    }
                    if let Some(directory) = &capture_dir {
                        use std::io::Write;
                        std::fs::create_dir_all(directory).unwrap();
                        let path =
                            directory.join(format!("asteroid-{capture_view}-{samples}x.ppm"));
                        let mut output =
                            std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
                        write!(output, "P6\n{width} {height}\n255\n").unwrap();
                        for pixel in pixels.chunks_exact(4) {
                            output.write_all(&pixel[..3]).unwrap();
                        }
                        output.flush().unwrap();
                        println!("Saved {}", path.display());
                    }
                    drop(pixels);
                    readback.unmap();
                }
                if let Some(profiler) = profiler.as_mut() {
                    profiler.begin_frame(&device);
                    let times = profiler.latest();
                    if benchmark && frame_index >= warmup_frames {
                        measurements.push([
                            encode_ms,
                            times.interaction,
                            times.shadow,
                            times.world,
                            times.composite,
                            times.frame,
                        ]);
                    }
                    if capture && final_frame {
                        println!(
                            "Reference {capture_view}, {samples}x MSAA GPU milliseconds: {times:?}"
                        );
                    }
                    assert!(
                        [
                            times.interaction,
                            times.shadow,
                            times.world,
                            times.composite
                        ]
                        .into_iter()
                        .all(|time| time.is_finite() && time >= 0.0)
                    );
                }
            }
            if benchmark {
                println!(
                    "BENCH scene={scene} size={width}x{height} msaa={samples} quality={quality:?} roots={visible_roots} frames={measured_frames}"
                );
                for (index, label) in [
                    "cpu_encode",
                    "interaction",
                    "shadow",
                    "world",
                    "bloom_composite",
                    "gpu_total",
                ]
                .into_iter()
                .enumerate()
                {
                    let mut values: Vec<_> =
                        measurements.iter().map(|sample| sample[index]).collect();
                    values.sort_by(f32::total_cmp);
                    println!(
                        "BENCH {label} median_ms={:.6} p95_ms={:.6}",
                        values[values.len() / 2],
                        values[values.len() * 95 / 100]
                    );
                }
            }
            assert!(
                error_scope.pop().await.is_none(),
                "GPU validation failed for {samples}x MSAA"
            );
        }
    });
}
