use super::*;

fn direction_frame(direction: Vec3, camera: Vec3) -> CompositeUniformGpu {
    let view = Mat4::look_at_rh(
        camera,
        camera + direction,
        direction.any_orthonormal_vector(),
    );
    let projection = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.08, 180.0);
    CompositeUniformGpu {
        inverse_view_proj: (projection * view).inverse().to_cols_array_2d(),
        // A negative test radius moves the entire atmosphere beyond its falloff,
        // leaving an unobscured panorama for exact direction/color comparisons.
        camera_radius: camera.extend(-100.0).to_array(),
        sun_time: [0.42, 0.81, 0.38, 0.0],
        display: [1.0, 0.0, 0.65, 0.0],
    }
}

fn expected_display_channel(linear: f32) -> u8 {
    let mapped = ((linear * (2.51 * linear + 0.03)) / (linear * (2.43 * linear + 0.59) + 0.14))
        .clamp(0.0, 1.0);
    let srgb = if mapped <= 0.003_130_8 {
        mapped * 12.92
    } else {
        1.055 * mapped.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round() as u8
}

fn render_direction_probes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    sky: &wgpu::TextureView,
    probes: &[(Vec3, Vec3)],
) -> Vec<[u8; 4]> {
    let transparent = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("transparent world for panorama probes"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: WORLD_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        transparent.as_image_copy(),
        &[0; 8],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        transparent.size(),
    );
    let world = transparent.create_view(&wgpu::TextureViewDescriptor::default());
    let uniforms: Vec<_> = probes
        .iter()
        .map(|&(direction, camera)| {
            create_init_buffer(
                device,
                "world-direction panorama probe",
                bytemuck::bytes_of(&direction_frame(direction, camera)),
                wgpu::BufferUsages::UNIFORM,
            )
        })
        .collect();
    let (pipeline, layout, sampler, _) =
        create_composite_resources(device, format, &world, &world, &uniforms[0], sky);
    let groups: Vec<_> = uniforms
        .iter()
        .map(|uniform| {
            create_composite_bind_group(device, &layout, &sampler, &world, &world, uniform, sky)
        })
        .collect();
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("panorama direction and output-transfer probes"),
        size: wgpu::Extent3d {
            width: probes.len() as u32,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
    let row_bytes = (probes.len() * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("panorama probe readback"),
        size: row_bytes as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("real composite shader panorama probes"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        for (index, group) in groups.iter().enumerate() {
            pass.set_viewport(index as f32, 0.0, 1.0, 1.0, 0.0, 1.0);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
    encoder.copy_texture_to_buffer(
        output.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes as u32),
                rows_per_image: Some(1),
            },
        },
        output.size(),
    );
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    encoder.map_buffer_on_submit(&readback, wgpu::MapMode::Read, .., move |result| {
        sender.send(result).unwrap();
    });
    queue.submit([encoder.finish()]);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receiver.recv().unwrap().unwrap();
    let bytes = readback.slice(..).get_mapped_range();
    let pixels = bytes[..probes.len() * 4]
        .chunks_exact(4)
        .map(|pixel| pixel.try_into().unwrap())
        .collect();
    drop(bytes);
    readback.unmap();
    pixels
}

#[test]
#[ignore = "requires a working wgpu graphics adapter"]
fn gpu_sky_cube_orientation_seams_translation_and_output_transfer() {
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
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let resolution = 64;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("known world-direction colors using shipping cube basis"),
            size: wgpu::Extent3d {
                width: resolution,
                height: resolution,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &crate::sky::direction_color_cube(resolution),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(resolution * 4),
                rows_per_image: Some(resolution),
            },
            texture.size(),
        );
        let sky = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let mut directions = Vec::new();
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let raw = Vec3::new(x as f32, y as f32, z as f32);
                    if raw == Vec3::ZERO {
                        continue;
                    }
                    directions.push(raw.normalize());
                    for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                        for offset in [-0.001, 0.001] {
                            directions.push((raw + axis * offset).normalize());
                        }
                    }
                }
            }
        }
        let probes: Vec<_> = directions
            .iter()
            .flat_map(|&direction| {
                [Vec3::ZERO, Vec3::new(31.0, -13.0, 27.0)].map(|camera| (direction, camera))
            })
            .collect();
        let gamma = render_direction_probes(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &sky,
            &probes,
        );
        let srgb = render_direction_probes(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            &sky,
            &probes,
        );
        for (index, &(direction, _)) in probes.iter().enumerate() {
            let expected = (direction * 0.5 + Vec3::splat(0.5))
                .to_array()
                .map(expected_display_channel);
            for channel in 0..3 {
                assert!(
                    gamma[index][channel].abs_diff(expected[channel]) <= 4,
                    "cube orientation mismatch at {direction:?}: {:?}, expected {expected:?}",
                    gamma[index]
                );
                assert!(gamma[index][channel].abs_diff(srgb[index][channel]) <= 2);
                assert!(gamma[index][channel].abs_diff(gamma[index ^ 1][channel]) <= 1);
            }
            assert_eq!(gamma[index][3], 255);
            assert_eq!(srgb[index][3], 255);
        }
        // Each group starts at an exact face center/edge/corner and probes tiny
        // offsets on both sides of every axis, so a face convention mismatch
        // cannot hide behind a self-consistent CPU projection roundtrip.
        for neighborhood in gamma.chunks_exact(14) {
            for pixel in neighborhood {
                for channel in 0..3 {
                    assert!(pixel[channel].abs_diff(neighborhood[0][channel]) <= 3);
                }
            }
        }
        assert!(error_scope.pop().await.is_none());
        println!(
            "{} sky directions checked across two camera origins and both framebuffer transfers",
            directions.len()
        );
    });
}
