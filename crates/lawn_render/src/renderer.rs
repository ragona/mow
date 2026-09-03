use std::{sync::Arc, time::Instant};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use lawn_core::{cube_map::CubeFace, profile::QualityPreset, run::RunState};
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

use crate::{
    gpu_profiler::GpuProfiler,
    interaction::{GrassInteraction, INTERACTION_RESOLUTION},
    mesh::{self, MeshVertex, TuftVertex},
    particles::ClippingParticles,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const WORLD_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const SHADOW_SIZE: u32 = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderTier {
    Baseline,
    EnhancedAvailable,
}

#[derive(Clone, Debug)]
pub struct RenderCapabilities {
    pub adapter_name: String,
    pub backend: wgpu::Backend,
    pub tier: RenderTier,
    pub timestamp_queries: bool,
    pub maximum_texture_dimension: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    pub visible_patches: u32,
    pub visible_tufts: u32,
    pub generated_triangles: u32,
    pub mowing_tile_uploads: u32,
    pub clipping_particles: u32,
    /// Known frame-local heap vector allocations in renderer-owned code.
    pub transient_allocations: u32,
    pub cpu_encode_milliseconds: f32,
    pub gpu_interaction_milliseconds: f32,
    pub gpu_shadow_milliseconds: f32,
    pub gpu_world_milliseconds: f32,
    pub gpu_composite_milliseconds: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameAcquireError {
    #[error("surface acquisition timed out")]
    Timeout,
    #[error("window is currently occluded")]
    Occluded,
    #[error("surface is outdated and must be reconfigured")]
    Outdated,
    #[error("surface was lost and must be reconfigured")]
    Lost,
    #[error("surface acquisition failed validation")]
    Validation,
}

pub struct RenderFrame {
    pub output: wgpu::SurfaceTexture,
    pub view: wgpu::TextureView,
    pub encoder: wgpu::CommandEncoder,
    pub stats: FrameStats,
}

impl std::fmt::Debug for RenderFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RenderFrame")
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
struct FrameUniformGpu {
    view_proj: [[f32; 4]; 4],
    light_view_proj: [[f32; 4]; 4],
    camera_time: [f32; 4],
    light_epoch: [f32; 4],
    options: [f32; 4],
    locator: [f32; 4],
}

#[derive(Debug)]
struct PlanetResources {
    terrain_vertices: wgpu::Buffer,
    terrain_indices: wgpu::Buffer,
    terrain_index_count: u32,
    grass_roots: wgpu::Buffer,
    mowing_texture: wgpu::Texture,
    mowing_view: wgpu::TextureView,
}

#[derive(Debug)]
struct RenderTargets {
    _world_texture: wgpu::Texture,
    world_view: wgpu::TextureView,
    _multisample_texture: Option<wgpu::Texture>,
    multisample_view: Option<wgpu::TextureView>,
    _depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
}

#[derive(Debug)]
struct ShadowTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

#[derive(Debug)]
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    msaa_samples: u32,
    capabilities: RenderCapabilities,
    frame_uniform: wgpu::Buffer,
    frame_bind_group: wgpu::BindGroup,
    shadow_bind_group: wgpu::BindGroup,
    grass_layout: wgpu::BindGroupLayout,
    grass_bind_groups: [wgpu::BindGroup; 2],
    terrain_pipeline: wgpu::RenderPipeline,
    grass_pipeline: wgpu::RenderPipeline,
    particle_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,
    composite_sampler: wgpu::Sampler,
    composite_bind_group: wgpu::BindGroup,
    targets: RenderTargets,
    shadow: ShadowTarget,
    planet: PlanetResources,
    interaction: GrassInteraction,
    particles: ClippingParticles,
    tuft_vertices: wgpu::Buffer,
    tuft_indices: wgpu::Buffer,
    tuft_index_count: u32,
    vehicle_vertices: wgpu::Buffer,
    vehicle_indices: wgpu::Buffer,
    vehicle_index_count: u32,
    started: Instant,
    quality: QualityPreset,
    high_contrast: bool,
    reduced_particles: bool,
    render_scale: f32,
    last_visual_frame: Instant,
    gpu_profiler: Option<GpuProfiler>,
}

impl Renderer {
    /// Creates the window surface, conservative baseline device, pipelines, and
    /// immutable resources for the active planet.
    ///
    /// # Errors
    ///
    /// Returns an error when no compatible adapter, device, or surface format
    /// can be created.
    pub async fn new(
        window: Arc<Window>,
        run: &RunState,
        msaa_samples: u32,
        quality: QualityPreset,
        render_scale: f32,
        high_contrast: bool,
        reduced_particles: bool,
    ) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window)
            .context("failed to create the window surface")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .context("no compatible graphics adapter was found")?;
        let adapter_info = adapter.get_info();
        let adapter_features = adapter.features();
        let enhanced = adapter_features.contains(
            wgpu::Features::MULTI_DRAW_INDIRECT_COUNT | wgpu::Features::INDIRECT_FIRST_INSTANCE,
        );
        // Rendering never depends on optional features. Timestamp queries are
        // enabled only as a diagnostics side channel when an adapter exposes them.
        let diagnostic_features = adapter_features & wgpu::Features::TIMESTAMP_QUERY;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Lawn Orbit device"),
                required_features: diagnostic_features,
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .context("failed to create the graphics device")?;
        let surface_capabilities = surface.get_capabilities(&adapter);
        let format = surface_capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .unwrap_or(surface_capabilities.formats[0]);
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: surface_capabilities.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);
        let msaa_samples = match msaa_samples {
            1 | 2 | 4 => msaa_samples,
            _ => 4,
        };
        let render_scale = render_scale.clamp(0.5, 1.0);
        let targets = create_targets(&device, &surface_config, msaa_samples, render_scale);
        let shadow = create_shadow(&device);
        let frame_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame uniform"),
            size: std::mem::size_of::<FrameUniformGpu>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_layout = create_frame_layout(&device);
        let frame_bind_group =
            create_frame_bind_group(&device, &frame_layout, &frame_uniform, &shadow);
        let shadow_layout = create_shadow_frame_layout(&device);
        let shadow_bind_group =
            create_shadow_frame_bind_group(&device, &shadow_layout, &frame_uniform);
        let grass_layout = create_grass_layout(&device);
        let interaction = GrassInteraction::new(&device, run.planet.config.base_radius);
        let particles = ClippingParticles::new(&device);
        let planet = create_planet_resources(&device, &queue, run);
        let grass_bind_groups = create_grass_bind_groups(
            &device,
            &grass_layout,
            &planet.mowing_view,
            interaction.displacement_buffers(),
        );
        let (terrain_pipeline, grass_pipeline, particle_pipeline, shadow_pipeline) =
            create_pipelines(
                &device,
                WORLD_FORMAT,
                msaa_samples,
                &frame_layout,
                &grass_layout,
                &shadow_layout,
            );
        let (composite_pipeline, composite_layout, composite_sampler, composite_bind_group) =
            create_composite_resources(&device, format, &targets.world_view);
        let (tuft_vertices_data, tuft_indices_data) = mesh::build_tuft();
        let tuft_vertices = create_init_buffer(
            &device,
            "tuft vertices",
            bytemuck::cast_slice(&tuft_vertices_data),
            wgpu::BufferUsages::VERTEX,
        );
        let tuft_indices = create_init_buffer(
            &device,
            "tuft indices",
            bytemuck::cast_slice(&tuft_indices_data),
            wgpu::BufferUsages::INDEX,
        );
        let vehicle_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dynamic vehicle vertices"),
            size: 16 * 1024,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let vehicle_indices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dynamic vehicle indices"),
            size: 8 * 1024,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Report limits of the device we actually requested, not the adapter's
        // potentially larger native limits. Baseline device creation deliberately
        // uses conservative defaults.
        let limits = device.limits();
        let capabilities = RenderCapabilities {
            adapter_name: adapter_info.name,
            backend: adapter_info.backend,
            tier: if enhanced {
                RenderTier::EnhancedAvailable
            } else {
                RenderTier::Baseline
            },
            timestamp_queries: diagnostic_features.contains(wgpu::Features::TIMESTAMP_QUERY),
            maximum_texture_dimension: limits.max_texture_dimension_2d,
        };
        let gpu_profiler = capabilities
            .timestamp_queries
            .then(|| GpuProfiler::new(&device, queue.get_timestamp_period()));
        Ok(Self {
            surface,
            device,
            queue,
            surface_config,
            size,
            msaa_samples,
            capabilities,
            frame_uniform,
            frame_bind_group,
            shadow_bind_group,
            grass_layout,
            grass_bind_groups,
            terrain_pipeline,
            grass_pipeline,
            particle_pipeline,
            shadow_pipeline,
            composite_pipeline,
            composite_layout,
            composite_sampler,
            composite_bind_group,
            targets,
            shadow,
            planet,
            interaction,
            particles,
            tuft_vertices,
            tuft_indices,
            tuft_index_count: tuft_indices_data.len() as u32,
            vehicle_vertices,
            vehicle_indices,
            vehicle_index_count: 0,
            started: Instant::now(),
            quality,
            high_contrast,
            reduced_particles,
            render_scale,
            last_visual_frame: Instant::now(),
            gpu_profiler,
        })
    }

    #[must_use]
    pub const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    #[must_use]
    pub const fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    #[must_use]
    pub const fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_config.format
    }

    #[must_use]
    pub const fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    #[must_use]
    pub const fn capabilities(&self) -> &RenderCapabilities {
        &self.capabilities
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.size = size;
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.surface.configure(&self.device, &self.surface_config);
        self.targets = create_targets(
            &self.device,
            &self.surface_config,
            self.msaa_samples,
            self.render_scale,
        );
        self.composite_bind_group = create_composite_bind_group(
            &self.device,
            &self.composite_layout,
            &self.composite_sampler,
            &self.targets.world_view,
        );
    }

    pub fn set_visual_options(
        &mut self,
        quality: QualityPreset,
        high_contrast: bool,
        reduced_particles: bool,
    ) {
        self.quality = quality;
        self.high_contrast = high_contrast;
        self.reduced_particles = reduced_particles;
    }

    pub fn upload_planet(&mut self, run: &RunState) {
        self.planet = create_planet_resources(&self.device, &self.queue, run);
        self.interaction = GrassInteraction::new(&self.device, run.planet.config.base_radius);
        self.particles.clear();
        self.grass_bind_groups = create_grass_bind_groups(
            &self.device,
            &self.grass_layout,
            &self.planet.mowing_view,
            self.interaction.displacement_buffers(),
        );
    }

    /// Acquires the surface image and encodes the complete explicit frame.
    ///
    /// # Errors
    ///
    /// Returns [`FrameAcquireError`] for recoverable surface states or an
    /// out-of-memory condition reported by the graphics backend.
    pub fn begin_frame(
        &mut self,
        run: &mut RunState,
        interpolation_alpha: f32,
    ) -> Result<RenderFrame, FrameAcquireError> {
        let _span = tracing::debug_span!("render_encode").entered();
        let encode_started = Instant::now();
        let visual_dt = self
            .last_visual_frame
            .elapsed()
            .as_secs_f32()
            .clamp(1.0 / 240.0, 1.0 / 30.0);
        self.last_visual_frame = Instant::now();
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Timeout => return Err(FrameAcquireError::Timeout),
            wgpu::CurrentSurfaceTexture::Occluded => return Err(FrameAcquireError::Occluded),
            wgpu::CurrentSurfaceTexture::Outdated => return Err(FrameAcquireError::Outdated),
            wgpu::CurrentSurfaceTexture::Lost => return Err(FrameAcquireError::Lost),
            wgpu::CurrentSurfaceTexture::Validation => return Err(FrameAcquireError::Validation),
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let transform = run.vehicle.interpolated_transform(interpolation_alpha);
        let reversing = run
            .vehicle
            .state
            .linear_velocity
            .dot(run.vehicle.state.transform.forward)
            < -0.15;
        let (vehicle_vertices, vehicle_indices) = mesh::build_vehicle(
            transform,
            run.vehicle.state.mower_enabled,
            reversing,
            run.simulation_seconds,
        );
        self.queue.write_buffer(
            &self.vehicle_vertices,
            0,
            bytemuck::cast_slice(&vehicle_vertices),
        );
        self.queue.write_buffer(
            &self.vehicle_indices,
            0,
            bytemuck::cast_slice(&vehicle_indices),
        );
        self.vehicle_index_count = vehicle_indices.len() as u32;
        self.particles
            .update(&self.queue, run, self.reduced_particles);
        let dirty_tiles = run.mowing.take_dirty_tiles();
        // Vehicle mesh (2), interaction sources (1), optional particle staging
        // (1), plus dirty-key/result vectors and one cell vector per dirty tile.
        let transient_allocations = 3
            + u32::from(self.particles.len() > 0)
            + if dirty_tiles.is_empty() {
                0
            } else {
                2 + dirty_tiles.len() as u32
            };
        for tile in &dirty_tiles {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.planet.mowing_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: tile.origin_x,
                        y: tile.origin_y,
                        z: tile.face.index() as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&tile.cells),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tile.width * 4),
                    rows_per_image: Some(tile.height),
                },
                wgpu::Extent3d {
                    width: tile.width,
                    height: tile.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let frame_uniform = make_frame_uniform(self, run);
        self.queue
            .write_buffer(&self.frame_uniform, 0, bytemuck::bytes_of(&frame_uniform));
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Lawn Orbit frame"),
            });
        let gpu_profile_slot = self
            .gpu_profiler
            .as_mut()
            .and_then(|profiler| profiler.begin_frame(&self.device));
        let query_set =
            gpu_profile_slot.and_then(|_| self.gpu_profiler.as_ref().map(GpuProfiler::query_set));
        self.interaction.encode(
            &self.queue,
            &mut encoder,
            &run.planet,
            &run.vehicle.state,
            run.vehicle_tuning.mower_width,
            run.simulation_seconds,
            visual_dt,
            query_set.map(|query_set| wgpu::ComputePassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: Some(1),
            }),
        );
        self.encode_shadow(&mut encoder, query_set);
        let (visible_patches, visible_tufts) = self.encode_world(&mut encoder, run, query_set);
        self.encode_composite(&mut encoder, &view, query_set);
        if let (Some(slot), Some(profiler)) = (gpu_profile_slot, self.gpu_profiler.as_mut()) {
            profiler.finish_encoding(&mut encoder, slot);
        }
        let gpu_times = self
            .gpu_profiler
            .as_ref()
            .map(GpuProfiler::latest)
            .unwrap_or_default();
        let generated_triangles = self.planet.terrain_index_count / 3
            + self.vehicle_index_count / 3
            + visible_tufts * (self.tuft_index_count / 3)
            + self.particles.len() * 2;
        Ok(RenderFrame {
            output,
            view,
            encoder,
            stats: FrameStats {
                visible_patches,
                visible_tufts,
                generated_triangles,
                mowing_tile_uploads: dirty_tiles.len() as u32,
                clipping_particles: self.particles.len(),
                transient_allocations,
                cpu_encode_milliseconds: encode_started.elapsed().as_secs_f32() * 1000.0,
                gpu_interaction_milliseconds: gpu_times.interaction,
                gpu_shadow_milliseconds: gpu_times.shadow,
                gpu_world_milliseconds: gpu_times.world,
                gpu_composite_milliseconds: gpu_times.composite,
            },
        })
    }

    pub fn finish_frame(&self, frame: RenderFrame) {
        self.queue.submit(Some(frame.encoder.finish()));
        frame.output.present();
    }

    fn encode_shadow(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        query_set: Option<&wgpu::QuerySet>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("directional shadow"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.shadow.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: query_set.map(|query_set| wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(2),
                end_of_pass_write_index: Some(3),
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shadow_pipeline);
        pass.set_bind_group(0, &self.shadow_bind_group, &[]);
        pass.set_vertex_buffer(0, self.planet.terrain_vertices.slice(..));
        pass.set_index_buffer(
            self.planet.terrain_indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        pass.draw_indexed(0..self.planet.terrain_index_count, 0, 0..1);
        pass.set_vertex_buffer(0, self.vehicle_vertices.slice(..));
        pass.set_index_buffer(self.vehicle_indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.vehicle_index_count, 0, 0..1);
    }

    fn encode_world(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        run: &RunState,
        query_set: Option<&wgpu::QuerySet>,
    ) -> (u32, u32) {
        let (color_view, resolve_target) = self
            .targets
            .multisample_view
            .as_ref()
            .map_or((&self.targets.world_view, None), |view| {
                (view, Some(&self.targets.world_view))
            });
        let color_attachment = Some(wgpu::RenderPassColorAttachment {
            view: color_view,
            depth_slice: None,
            resolve_target,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("opaque world and geometric grass"),
            color_attachments: &[color_attachment],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.targets.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: query_set.map(|query_set| wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(4),
                end_of_pass_write_index: Some(5),
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.terrain_pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_vertex_buffer(0, self.planet.terrain_vertices.slice(..));
        pass.set_index_buffer(
            self.planet.terrain_indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        pass.draw_indexed(0..self.planet.terrain_index_count, 0, 0..1);
        pass.set_vertex_buffer(0, self.vehicle_vertices.slice(..));
        pass.set_index_buffer(self.vehicle_indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.vehicle_index_count, 0, 0..1);

        pass.set_pipeline(&self.grass_pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        let interaction_index = usize::from(!std::ptr::eq(
            self.interaction.current_displacement(),
            self.interaction.displacement_buffers()[0],
        ));
        pass.set_bind_group(1, &self.grass_bind_groups[interaction_index], &[]);
        pass.set_vertex_buffer(0, self.tuft_vertices.slice(..));
        pass.set_vertex_buffer(1, self.planet.grass_roots.slice(..));
        pass.set_index_buffer(self.tuft_indices.slice(..), wgpu::IndexFormat::Uint16);
        let camera_position = run.camera.state.position;
        let camera_direction = camera_position.normalize();
        let camera_radius = camera_position.length();
        let horizon_angle = (run.planet.config.base_radius / camera_radius)
            .clamp(0.0, 1.0)
            .acos();
        let camera_forward = (run.camera.state.target - camera_position).normalize();
        let base_density = quality_density(self.quality);
        let mut visible_patches = 0;
        let mut visible_tufts = 0;
        for patch in &run.planet.grass_patches {
            if patch.roots.is_empty() {
                continue;
            }
            let angle = camera_direction.dot(patch.center).clamp(-1.0, 1.0).acos();
            if angle > horizon_angle + patch.angular_radius + 0.08 {
                continue;
            }
            let patch_position = patch.center * run.planet.config.base_radius;
            let toward_patch = (patch_position - camera_position).normalize();
            if camera_forward.dot(toward_patch) < -0.32 {
                continue;
            }
            let total = patch.roots.end - patch.roots.start;
            let distance = camera_position.distance(patch_position);
            let high_camera = (camera_radius - run.planet.config.base_radius).max(0.0);
            let distance_lod = 1.0 - smoothstep(13.0, 48.0, distance) * 0.38;
            let toy_camera_lod = 1.0 - smoothstep(11.0, 28.0, high_camera) * 0.48;
            let density = base_density * distance_lod * toy_camera_lod;
            let draw_count = ((total as f32 * density).ceil() as u32).min(total);
            if draw_count == 0 {
                continue;
            }
            visible_patches += 1;
            visible_tufts += draw_count;
            pass.draw_indexed(
                0..self.tuft_index_count,
                0,
                patch.roots.start..patch.roots.start + draw_count,
            );
        }
        pass.set_pipeline(&self.particle_pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        self.particles.draw(&mut pass);
        (visible_patches, visible_tufts)
    }

    fn encode_composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        surface_view: &wgpu::TextureView,
        query_set: Option<&wgpu::QuerySet>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tone mapping and world upscale"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: surface_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: query_set.map(|query_set| wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(6),
                end_of_pass_write_index: Some(7),
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, &self.composite_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn make_frame_uniform(renderer: &Renderer, run: &RunState) -> FrameUniformGpu {
    let camera = run.camera.state;
    let aspect = renderer.surface_config.width as f32 / renderer.surface_config.height as f32;
    let view = Mat4::look_at_rh(camera.position, camera.target, camera.up);
    let projection = Mat4::perspective_rh(
        camera.field_of_view_degrees.to_radians(),
        aspect,
        0.08,
        180.0,
    );
    let light_direction = Vec3::new(-0.42, -0.81, -0.38).normalize();
    let light_position = -light_direction * 75.0;
    let light_view = Mat4::look_at_rh(light_position, Vec3::ZERO, Vec3::Y);
    let light_projection = Mat4::orthographic_rh(-42.0, 42.0, -42.0, 42.0, 1.0, 150.0);
    let locator = run.locator_direction().unwrap_or(Vec3::ZERO);
    FrameUniformGpu {
        view_proj: (projection * view).to_cols_array_2d(),
        light_view_proj: (light_projection * light_view).to_cols_array_2d(),
        camera_time: [
            camera.position.x,
            camera.position.y,
            camera.position.z,
            renderer.started.elapsed().as_secs_f32(),
        ],
        light_epoch: [
            light_direction.x,
            light_direction.y,
            light_direction.z,
            (run.metrics.elapsed_seconds * 30.0) % 256.0,
        ],
        options: [
            quality_density(renderer.quality),
            INTERACTION_RESOLUTION as f32,
            if renderer.high_contrast { 1.0 } else { 0.0 },
            0.0,
        ],
        locator: [
            locator.x,
            locator.y,
            locator.z,
            if locator == Vec3::ZERO { 0.0 } else { 1.0 },
        ],
    }
}

fn quality_density(quality: QualityPreset) -> f32 {
    match quality {
        QualityPreset::Low => 0.5,
        QualityPreset::Standard | QualityPreset::High => 1.0,
    }
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn create_planet_resources(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    run: &RunState,
) -> PlanetResources {
    let (terrain_vertices, terrain_indices) = mesh::build_terrain(&run.planet);
    let terrain_vertex_buffer = create_init_buffer(
        device,
        "terrain vertices",
        bytemuck::cast_slice(&terrain_vertices),
        wgpu::BufferUsages::VERTEX,
    );
    let terrain_index_buffer = create_init_buffer(
        device,
        "terrain indices",
        bytemuck::cast_slice(&terrain_indices),
        wgpu::BufferUsages::INDEX,
    );
    let roots: &[u8] = if run.planet.grass_roots.is_empty() {
        &[0; 16]
    } else {
        bytemuck::cast_slice(&run.planet.grass_roots)
    };
    let grass_roots = create_init_buffer(device, "grass roots", roots, wgpu::BufferUsages::VERTEX);
    let resolution = run.mowing.resolution();
    let mowing_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("authoritative mowing mirror"),
        size: wgpu::Extent3d {
            width: resolution,
            height: resolution,
            depth_or_array_layers: 6,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let face_area = (resolution * resolution) as usize;
    let packed = run.mowing.packed_cells();
    for face in CubeFace::ALL {
        let start = face.index() * face_area;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &mowing_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: face.index() as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&packed[start..start + face_area]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(resolution * 4),
                rows_per_image: Some(resolution),
            },
            wgpu::Extent3d {
                width: resolution,
                height: resolution,
                depth_or_array_layers: 1,
            },
        );
    }
    let mowing_view = mowing_texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mowing 2d-array view"),
        format: None,
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
        aspect: wgpu::TextureAspect::All,
        base_mip_level: 0,
        mip_level_count: Some(1),
        base_array_layer: 0,
        array_layer_count: Some(6),
    });
    PlanetResources {
        terrain_vertices: terrain_vertex_buffer,
        terrain_indices: terrain_index_buffer,
        terrain_index_count: terrain_indices.len() as u32,
        grass_roots,
        mowing_texture,
        mowing_view,
    }
}

fn create_frame_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("frame layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ],
    })
}

fn create_frame_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    shadow: &ShadowTarget,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("frame bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&shadow.view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&shadow.sampler),
            },
        ],
    })
}

fn create_shadow_frame_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("shadow frame layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

fn create_shadow_frame_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("shadow frame bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        }],
    })
}

fn create_grass_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("grass resources layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn create_grass_bind_groups(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    mowing_view: &wgpu::TextureView,
    displacement: [&wgpu::Buffer; 2],
) -> [wgpu::BindGroup; 2] {
    displacement.map(|buffer| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grass resources"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(mowing_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        })
    })
}

fn create_pipelines(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    samples: u32,
    frame_layout: &wgpu::BindGroupLayout,
    grass_layout: &wgpu::BindGroupLayout,
    shadow_layout: &wgpu::BindGroupLayout,
) -> (
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
) {
    let terrain_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("terrain shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/terrain.wgsl").into()),
    });
    let grass_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("grass shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/grass.wgsl").into()),
    });
    let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("shadow shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shadow.wgsl").into()),
    });
    let particle_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("clipping particle shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/particles.wgsl").into()),
    });
    let frame_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("terrain pipeline layout"),
        bind_group_layouts: &[Some(frame_layout)],
        immediate_size: 0,
    });
    let grass_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("grass pipeline layout"),
        bind_group_layouts: &[Some(frame_layout), Some(grass_layout)],
        immediate_size: 0,
    });
    let shadow_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("shadow pipeline layout"),
        bind_group_layouts: &[Some(shadow_layout)],
        immediate_size: 0,
    });
    let targets = [Some(wgpu::ColorTargetState {
        format: color_format,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    })];
    let depth = Some(wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    });
    let terrain_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("terrain and vehicle pipeline"),
        layout: Some(&frame_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &terrain_shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[MeshVertex::layout()],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: depth.clone(),
        multisample: wgpu::MultisampleState {
            count: samples,
            ..wgpu::MultisampleState::default()
        },
        fragment: Some(wgpu::FragmentState {
            module: &terrain_shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    });
    let grass_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("opaque geometric grass pipeline"),
        layout: Some(&grass_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &grass_shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[TuftVertex::layout(), mesh::root_layout()],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: depth,
        multisample: wgpu::MultisampleState {
            count: samples,
            alpha_to_coverage_enabled: true,
            ..wgpu::MultisampleState::default()
        },
        fragment: Some(wgpu::FragmentState {
            module: &grass_shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    });
    let particle_targets = [Some(wgpu::ColorTargetState {
        format: color_format,
        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    })];
    let particle_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("clipping particle pipeline"),
        layout: Some(&frame_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &particle_shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &ClippingParticles::vertex_layouts(),
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: samples,
            ..wgpu::MultisampleState::default()
        },
        fragment: Some(wgpu::FragmentState {
            module: &particle_shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &particle_targets,
        }),
        multiview_mask: None,
        cache: None,
    });
    let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("directional shadow pipeline"),
        layout: Some(&shadow_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shadow_shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[MeshVertex::layout()],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState {
                constant: 2,
                slope_scale: 2.0,
                clamp: 0.0,
            },
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: None,
        multiview_mask: None,
        cache: None,
    });
    (
        terrain_pipeline,
        grass_pipeline,
        particle_pipeline,
        shadow_pipeline,
    )
}

fn create_targets(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    samples: u32,
    render_scale: f32,
) -> RenderTargets {
    let extent = wgpu::Extent3d {
        width: (config.width as f32 * render_scale).round().max(1.0) as u32,
        height: (config.height as f32 * render_scale).round().max(1.0) as u32,
        depth_or_array_layers: 1,
    };
    let world_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("resolved HDR world color"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: WORLD_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let world_view = world_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let (multisample_texture, multisample_view) = if samples > 1 {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("multisampled HDR world color"),
            size: extent,
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: WORLD_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (Some(texture), Some(view))
    } else {
        (None, None)
    };
    let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("world depth"),
        size: extent,
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());
    RenderTargets {
        _world_texture: world_texture,
        world_view,
        _multisample_texture: multisample_texture,
        multisample_view,
        _depth_texture: depth_texture,
        depth_view,
    }
}

fn create_composite_resources(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    world_view: &wgpu::TextureView,
) -> (
    wgpu::RenderPipeline,
    wgpu::BindGroupLayout,
    wgpu::Sampler,
    wgpu::BindGroup,
) {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("world composite layout"),
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
        label: Some("world upscale sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..wgpu::SamplerDescriptor::default()
    });
    let bind_group = create_composite_bind_group(device, &layout, &sampler, world_view);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("tone mapping and upscale shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/composite.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("world composite pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("tone mapping and world upscale pipeline"),
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
            entry_point: Some(if surface_format.is_srgb() {
                "fs_main_linear_framebuffer"
            } else {
                "fs_main_gamma_framebuffer"
            }),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, layout, sampler, bind_group)
}

fn create_composite_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    world_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("world composite bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(world_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn create_shadow(device: &wgpu::Device) -> ShadowTarget {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("directional shadow map"),
        size: wgpu::Extent3d {
            width: SHADOW_SIZE,
            height: SHADOW_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("shadow comparison sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        compare: Some(wgpu::CompareFunction::LessEqual),
        ..wgpu::SamplerDescriptor::default()
    });
    ShadowTarget {
        _texture: texture,
        view,
        sampler,
    }
}

fn create_init_buffer(
    device: &wgpu::Device,
    label: &'static str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage,
    })
}
