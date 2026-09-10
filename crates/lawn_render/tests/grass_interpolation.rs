//! Exercise the shipping grass sampler against an analytic continuous field.

use glam::{Vec2, Vec3};
use lawn_core::cube_map::{CubeCell, CubeFace, cell_center_direction, face_uv_to_direction};
use wgpu::util::DeviceExt;

#[test]
#[ignore = "requires a working wgpu graphics adapter"]
fn gpu_garden_breeze_is_tangent_continuous_and_changes_smoothly() {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap();
        // Paired probes cross every face edge, include poles/corners, then move
        // forward one frame. The actual shader must remain spatially coherent
        // and tangent even where a fixed tangent-frame basis would switch axes.
        let mut queries = Vec::<[f32; 4]>::new();
        for time in [0.0, 1.5, 11.0, 40.0] {
            for face in CubeFace::ALL {
                for side in [-1.0, 1.0] {
                    for along in [-1.0, -0.5, 0.0, 0.5, 1.0] {
                        for offset in [-0.00001, 0.00001] {
                            let direction =
                                face_uv_to_direction(face, Vec2::new(side + offset, along));
                            queries.push(direction.extend(time).to_array());
                            queries.push(direction.extend(time + 1.0 / 60.0).to_array());
                        }
                    }
                }
            }
        }
        let source = format!(
            "{}\n{}",
            include_str!("../src/shaders/grass.wgsl"),
            r"
@group(0) @binding(3) var<storage, read> wind_probes: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> wind_results: array<vec4<f32>>;
@compute @workgroup_size(64)
fn test_breeze(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < arrayLength(&wind_probes)) {
        let probe = wind_probes[id.x];
        let direction = normalize(probe.xyz);
        wind_results[id.x] = vec4<f32>(garden_breeze(direction, direction, probe.w), 0.0);
    }
}
"
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shipping coherent garden breeze"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("garden breeze continuity probe"),
            layout: None,
            module: &shader,
            entry_point: Some("test_breeze"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let probes = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("garden breeze directions and times"),
            contents: bytemuck::cast_slice(&queries),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let size = (queries.len() * 16) as u64;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("garden breeze results"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("garden breeze readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("garden breeze probe bindings"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: probes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups((queries.len() as u32).div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        encoder.map_buffer_on_submit(&readback, wgpu::MapMode::Read, .., move |result| {
            sender.send(result).unwrap();
        });
        queue.submit([encoder.finish()]);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let mapped = readback.slice(..).get_mapped_range();
        let values: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
        for (probe, result) in queries.iter().zip(values) {
            let breeze = Vec3::from_slice(result);
            assert!(breeze.is_finite() && breeze.length() < 0.10);
            assert!(breeze.dot(Vec3::from_slice(probe)).abs() < 1.0e-6);
        }
        for pair in values.chunks_exact(4) {
            assert!(Vec3::from_slice(&pair[0]).distance(Vec3::from_slice(&pair[2])) < 0.00002);
            assert!(Vec3::from_slice(&pair[1]).distance(Vec3::from_slice(&pair[3])) < 0.00002);
            assert!(Vec3::from_slice(&pair[0]).distance(Vec3::from_slice(&pair[1])) < 0.0015);
        }
        drop(mapped);
        readback.unmap();
    });
}

#[test]
#[ignore = "requires a working wgpu graphics adapter"]
fn gpu_grass_interpolation_is_smooth_across_cells_edges_and_corners() {
    const RESOLUTION: u32 = 32;
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap();
        println!("Grass interpolation adapter: {}", adapter.get_info().name);
        let mut field = Vec::new();
        for face in CubeFace::ALL {
            for y in 0..RESOLUTION {
                for x in 0..RESOLUTION {
                    field.push(
                        cell_center_direction(CubeCell { face, x, y }, RESOLUTION)
                            .extend(0.0)
                            .to_array(),
                    );
                }
            }
        }
        let mut queries = Vec::<Vec3>::new();
        let mut continuous_groups = Vec::new();
        // Adjacent probes straddle every oriented cube edge at five positions.
        for a in 0..3 {
            for b in (a + 1)..3 {
                let other = 3 - a - b;
                for sign_a in [-1.0, 1.0] {
                    for sign_b in [-1.0, 1.0] {
                        for along in [-0.75, -0.2, 0.0, 0.3, 0.75] {
                            let start = queries.len();
                            for offset in [-0.00001, 0.00001] {
                                let mut direction = Vec3::ZERO;
                                direction[a] = sign_a * (1.0 + offset);
                                direction[b] = sign_b;
                                direction[other] = along;
                                queries.push(direction.normalize());
                            }
                            continuous_groups.push(start..queries.len());
                        }
                    }
                }
            }
        }
        // Approach each corner through all three incident faces.
        for x in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                for z in [-1.0, 1.0] {
                    let start = queries.len();
                    for axis in 0..3 {
                        let mut direction = Vec3::new(x, y, z);
                        direction[axis] *= 1.00001;
                        queries.push(direction.normalize());
                    }
                    continuous_groups.push(start..queries.len());
                }
            }
        }
        // Sample between cell centers throughout each face. Nearest-cell
        // sampling has a much larger error against this analytic field.
        let interior_start = queries.len();
        for face in CubeFace::ALL {
            for y in 3..RESOLUTION - 3 {
                for x in 3..RESOLUTION - 3 {
                    let uv = Vec2::new(x as f32 + 0.1, y as f32 + 0.8) * (2.0 / RESOLUTION as f32)
                        - Vec2::ONE;
                    queries.push(face_uv_to_direction(face, uv));
                }
            }
        }
        let packed_queries: Vec<_> = queries
            .iter()
            .map(|direction| direction.extend(0.0).to_array())
            .collect();
        let source = format!(
            "{}\n{}",
            include_str!("../src/shaders/grass.wgsl"),
            r"
@group(2) @binding(0) var<storage, read> probes: array<vec4<f32>>;
@group(2) @binding(1) var<storage, read_write> results: array<vec4<f32>>;
@compute @workgroup_size(64)
fn test_sample(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < arrayLength(&probes)) {
        results[id.x] = vec4<f32>(sample_interaction(direction_to_face_uv(probes[id.x].xyz)), 0.0);
    }
}
"
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shipping grass interpolation test"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("grass interpolation probes"),
            layout: None,
            module: &module,
            entry_point: Some("test_sample"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        // FrameUniform has two matrices followed by eight vec4s; options.y is
        // the interaction resolution. The sampler uses no other frame fields.
        let mut frame = [0.0_f32; 64];
        frame[41] = RESOLUTION as f32;
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grass interpolation frame"),
            contents: bytemuck::cast_slice(&frame),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let interaction = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("continuous direction field"),
            contents: bytemuck::cast_slice(&field),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let probes = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grass interpolation probes"),
            contents: bytemuck::cast_slice(&packed_queries),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let output_size = (queries.len() * 16) as u64;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grass interpolation results"),
            size: output_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grass interpolation readback"),
            size: output_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bindings = [
            vec![(0, &uniform)],
            vec![(1, &interaction)],
            vec![(0, &probes), (1, &output)],
        ];
        let groups: Vec<_> = bindings
            .iter()
            .enumerate()
            .map(|(index, bindings)| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("grass interpolation bindings"),
                    layout: &pipeline.get_bind_group_layout(index as u32),
                    entries: &bindings
                        .iter()
                        .map(|(binding, buffer)| wgpu::BindGroupEntry {
                            binding: *binding,
                            resource: buffer.as_entire_binding(),
                        })
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            for (index, group) in groups.iter().enumerate() {
                pass.set_bind_group(index as u32, group, &[]);
            }
            pass.dispatch_workgroups((queries.len() as u32).div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, output_size);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        encoder.map_buffer_on_submit(&readback, wgpu::MapMode::Read, .., move |result| {
            sender.send(result).unwrap();
        });
        queue.submit([encoder.finish()]);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let mapped = readback.slice(..).get_mapped_range();
        let values: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
        for (index, (&expected, value)) in queries.iter().zip(values).enumerate() {
            let actual = Vec3::from_slice(value);
            assert!(actual.is_finite());
            // Cube-edge taps lie on two different planes; the unfolded
            // interpolation is continuous but has a larger geometric error.
            let tolerance = if index < interior_start { 0.012 } else { 0.002 };
            assert!(
                actual.distance(expected) < tolerance,
                "probe {index}: interpolation must follow the smooth field: {actual:?} vs {expected:?}"
            );
        }
        for group in continuous_groups {
            let first = Vec3::from_slice(&values[group.start]);
            for index in group {
                let next = Vec3::from_slice(&values[index]);
                assert!(
                    first.distance(next) < 0.0005,
                    "cube edge/corner must have no visible jump: {first:?} vs {next:?}"
                );
            }
        }
        drop(mapped);
        readback.unmap();
    });
}
