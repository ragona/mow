//! Small, persistent quarter-resolution HDR bloom. No frame-local allocations.

use crate::renderer::WORLD_FORMAT;

#[derive(Debug)]
pub(crate) struct Bloom {
    targets: BloomTargets,
    pipelines: [wgpu::RenderPipeline; 3],
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Bloom {
    pub fn new(device: &wgpu::Device, world: &wgpu::TextureView, width: u32, height: u32) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bloom source layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bloom linear clamp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("soft highlight bloom"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bloom.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bloom pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipelines = ["prefilter", "horizontal", "vertical"].map(|entry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: WORLD_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        Self {
            targets: BloomTargets::new(device, &layout, &sampler, world, width, height),
            pipelines,
            layout,
            sampler,
        }
    }

    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        world: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        self.targets = BloomTargets::new(device, &self.layout, &self.sampler, world, width, height);
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.targets.views[0]
    }

    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, queries: Option<&wgpu::QuerySet>) {
        for index in 0..3 {
            let attachment = Some(wgpu::RenderPassColorAttachment {
                view: &self.targets.views[index % 2],
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("quarter-resolution bloom"),
                color_attachments: &[attachment],
                timestamp_writes: if index == 0 {
                    queries.map(|query_set| wgpu::RenderPassTimestampWrites {
                        query_set,
                        beginning_of_pass_write_index: Some(6),
                        end_of_pass_write_index: None,
                    })
                } else {
                    None
                },
                ..Default::default()
            });
            pass.set_pipeline(&self.pipelines[index]);
            pass.set_bind_group(0, &self.targets.sources[index], &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// Only size-dependent resources are recreated during a window resize.
#[derive(Debug)]
struct BloomTargets {
    // Texture views retain their parent textures in wgpu.
    views: [wgpu::TextureView; 2],
    sources: [wgpu::BindGroup; 3],
}

impl BloomTargets {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        world: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Self {
        let textures: [wgpu::Texture; 2] = std::array::from_fn(|_| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("quarter-resolution bloom"),
                size: wgpu::Extent3d {
                    width: width.div_ceil(4).max(1),
                    height: height.div_ceil(4).max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: WORLD_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | if cfg!(test) {
                        wgpu::TextureUsages::COPY_SRC
                    } else {
                        wgpu::TextureUsages::empty()
                    },
                view_formats: &[],
            })
        });
        let views = textures
            .each_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let sources = [world, &views[0], &views[1]].map(|view| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bloom source"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        });
        Self { views, sources }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // IEEE binary16 encodings; nonnegative finite values sort in the
    // same order as their bits, avoiding a half-float test dependency.
    const HALF: u16 = 0x3800;
    const ONE: u16 = 0x3c00;
    const TWO: u16 = 0x4000;
    const THREE: u16 = 0x4200;
    const FOUR: u16 = 0x4400;

    fn source_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bloom test source"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORLD_FORMAT,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    fn render_and_read(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bloom: &Bloom,
        source: &wgpu::Texture,
        pixels: &[[u16; 4]],
    ) -> Vec<[u16; 4]> {
        let bytes: Vec<_> = pixels
            .iter()
            .flatten()
            .flat_map(|channel| channel.to_le_bytes())
            .collect();
        queue.write_texture(
            source.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(source.width() * 8),
                rows_per_image: Some(source.height()),
            },
            source.size(),
        );
        let output = bloom.view().texture();
        let row_bytes = (output.width() * 8).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bloom test readback"),
            size: u64::from(row_bytes) * u64::from(output.height()),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        bloom.encode(&mut encoder, None);
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(output.height()),
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
        let mapped = readback.slice(..).get_mapped_range();
        let result = mapped
            .chunks_exact(row_bytes as usize)
            .flat_map(|row| row[..output.width() as usize * 8].chunks_exact(8))
            .map(|pixel| {
                std::array::from_fn(|channel| {
                    u16::from_le_bytes([pixel[channel * 2], pixel[channel * 2 + 1]])
                })
            })
            .collect();
        drop(mapped);
        readback.unmap();
        result
    }

    /// Read back the real HDR result to check thresholding, two-axis spreading,
    /// target replacement, and clamped sampling of tiny/odd-sized textures.
    #[test]
    #[ignore = "requires a working wgpu graphics adapter"]
    fn gpu_bloom_threshold_spread_and_resizing() {
        pollster::block_on(async {
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .unwrap();
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .unwrap();
            println!("Bloom readback adapter: {}", adapter.get_info().name);
            let initial = source_texture(&device, 1, 1);
            let mut bloom = Bloom::new(
                &device,
                &initial.create_view(&wgpu::TextureViewDescriptor::default()),
                1,
                1,
            );
            let pipelines = bloom.pipelines.clone();
            let layout = bloom.layout.clone();
            let sampler = bloom.sampler.clone();
            for (width, height) in [(1, 1), (17, 9), (64, 64), (3, 1)] {
                let source = source_texture(&device, width, height);
                bloom.resize(
                    &device,
                    &source.create_view(&wgpu::TextureViewDescriptor::default()),
                    width,
                    height,
                );
                assert_eq!(bloom.pipelines, pipelines);
                assert_eq!(bloom.layout, layout);
                assert_eq!(bloom.sampler, sampler);
                assert_eq!(bloom.view().texture().width(), width.div_ceil(4));
                assert_eq!(bloom.view().texture().height(), height.div_ceil(4));
                // Drawing the dim field after the bright one also catches
                // stale ping-pong contents and incomplete fullscreen coverage.
                for intensity in [FOUR, HALF] {
                    let pixels =
                        vec![[intensity, intensity, intensity, ONE]; (width * height) as usize];
                    let result = render_and_read(&device, &queue, &bloom, &source, &pixels);
                    assert!(result.iter().all(|pixel| pixel[3] == ONE));
                    if intensity == FOUR {
                        assert!(
                            result.iter().all(|pixel| {
                                pixel[..3].iter().all(|value| (TWO..THREE).contains(value))
                            }),
                            "uniform HDR highlights must survive threshold and both blur passes"
                        );
                    } else {
                        assert!(
                            result.iter().all(|pixel| pixel[..3] == [0, 0, 0]),
                            "dim scenery must produce no bloom"
                        );
                    }
                }
            }
            let source = source_texture(&device, 64, 64);
            bloom.resize(
                &device,
                &source.create_view(&wgpu::TextureViewDescriptor::default()),
                64,
                64,
            );
            let mut pixels = vec![[0, 0, 0, ONE]; 64 * 64];
            for y in 28..36 {
                for x in 28..36 {
                    pixels[y * 64 + x] = [FOUR, ONE, 0, ONE];
                }
            }
            let result = render_and_read(&device, &queue, &bloom, &source, &pixels);
            let center = result[8 * 16 + 8];
            for halo in [result[8 * 16 + 11], result[11 * 16 + 8]] {
                assert!(
                    center[0] > halo[0] && halo[0] > halo[1] && halo[1] > 0,
                    "HDR light must spread beyond the source on both axes, preserving its hue"
                );
            }
            assert_eq!(&result[0][..3], &[0, 0, 0], "distant pixels remain dark");
            assert!(
                result
                    .iter()
                    .flatten()
                    .all(|value| value & 0xfc00 != 0x7c00 && value & 0x8000 == 0),
                "bloom values must stay finite and nonnegative"
            );
        });
    }
}
