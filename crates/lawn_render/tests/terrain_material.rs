//! The terrain edge must be evaluated within a triangle, not selected from one
//! provoking vertex. Exercise the shipping fragment shader with a coverage ramp.

use wgpu::util::DeviceExt;

#[test]
#[ignore = "requires a working wgpu graphics adapter"]
fn gpu_terrain_material_has_a_crisp_interpolated_boundary() {
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
        println!("Terrain material adapter: {}", adapter.get_info().name);
        let source = format!(
            "{}\n{}",
            include_str!("../src/shaders/terrain.wgsl"),
            r"
@vertex
fn probe_vertex(@builtin(vertex_index) index: u32) -> VertexOutput {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var output: VertexOutput;
    output.clip_position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    output.world_position = vec3<f32>(uv.x * 2.0, 15.0, uv.y * 2.0);
    output.normal = vec3<f32>(0.0, 1.0, 0.0);
    output.material = 0u;
    output.variation = uv.x;
    output.shadow_position = vec4<f32>(0.0, 0.0, 2.0, 1.0);
    return output;
}

"
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain coverage regression"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain coverage probe"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("probe_vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let mut frame = [0.0_f32; 56];
        frame[32..36].copy_from_slice(&[0.0, 24.0, 20.0, 1.0]);
        frame[36..40].copy_from_slice(&[-0.42, -0.81, -0.38, 0.0]);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("terrain coverage frame"),
            contents: bytemuck::cast_slice(&frame),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let shadow = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terrain coverage shadow"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain coverage bindings"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terrain coverage output"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terrain coverage readback"),
            size: 64 * 64 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(64),
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
        let pixels = readback.slice(..).get_mapped_range();
        for row in pixels.chunks_exact(256) {
            for (x, color) in row.chunks_exact(4).enumerate() {
                assert_eq!(color[3], 255);
                if x <= 30 {
                    assert!(
                        color[1] > color[0] + 15,
                        "grass must stay green up to the narrow contour: x={x}, {color:?}"
                    );
                } else if x >= 33 {
                    assert!(
                        color[0] > color[1],
                        "the same triangle must become stone beyond the contour: x={x}, {color:?}"
                    );
                }
            }
        }
        drop(pixels);
        readback.unmap();
    });
}
