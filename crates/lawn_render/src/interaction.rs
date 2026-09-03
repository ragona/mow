use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use lawn_core::{FIXED_DT, planet::Planet, vehicle::VehicleState};
use wgpu::util::DeviceExt;

pub const INTERACTION_RESOLUTION: u32 = 128;
const MAX_FORCE_SOURCES: usize = 16;

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
struct ForceSourceGpu {
    position_radius: [f32; 4],
    direction_strength: [f32; 4],
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
struct InteractionParamsGpu {
    dt: f32,
    time: f32,
    source_count: u32,
    resolution: u32,
    stiffness: f32,
    damping: f32,
    planet_radius: f32,
    maximum_displacement: f32,
}

#[derive(Debug)]
pub struct GrassInteraction {
    displacement: [wgpu::Buffer; 2],
    _velocity: [wgpu::Buffer; 2],
    source_buffer: wgpu::Buffer,
    params_buffer: wgpu::Buffer,
    bind_groups: [wgpu::BindGroup; 2],
    pipeline: wgpu::ComputePipeline,
    parity: usize,
    planet_radius: f32,
}

impl GrassInteraction {
    #[must_use]
    pub fn new(device: &wgpu::Device, planet_radius: f32) -> Self {
        let vector_count = (6 * INTERACTION_RESOLUTION * INTERACTION_RESOLUTION) as usize;
        let zeroes = vec![[0.0_f32; 4]; vector_count];
        let make_field = |label: &'static str| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&zeroes),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            })
        };
        let displacement = [
            make_field("interaction displacement a"),
            make_field("interaction displacement b"),
        ];
        let velocity = [
            make_field("interaction velocity a"),
            make_field("interaction velocity b"),
        ];
        let source_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grass force sources"),
            size: (MAX_FORCE_SOURCES * std::mem::size_of::<ForceSourceGpu>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grass interaction params"),
            size: std::mem::size_of::<InteractionParamsGpu>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grass interaction layout"),
            entries: &[
                storage_entry(0, true),
                storage_entry(1, true),
                storage_entry(2, false),
                storage_entry(3, false),
                storage_entry(4, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind_groups = [
            create_bind_group(
                device,
                &layout,
                &displacement[0],
                &velocity[0],
                &displacement[1],
                &velocity[1],
                &source_buffer,
                &params_buffer,
                "grass interaction a to b",
            ),
            create_bind_group(
                device,
                &layout,
                &displacement[1],
                &velocity[1],
                &displacement[0],
                &velocity[0],
                &source_buffer,
                &params_buffer,
                "grass interaction b to a",
            ),
        ];
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grass interaction shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/interaction.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grass interaction pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("grass interaction pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            displacement,
            _velocity: velocity,
            source_buffer,
            params_buffer,
            bind_groups,
            pipeline,
            parity: 0,
            planet_radius,
        }
    }

    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        planet: &Planet,
        vehicle: &VehicleState,
        mower_width: f32,
        elapsed_seconds: f32,
        frame_dt: f32,
        timestamp_writes: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let sources = force_sources(planet, vehicle, mower_width);
        queue.write_buffer(&self.source_buffer, 0, bytemuck::cast_slice(&sources));
        let params = InteractionParamsGpu {
            dt: frame_dt.clamp(FIXED_DT * 0.5, 1.0 / 30.0),
            time: elapsed_seconds,
            source_count: sources.len() as u32,
            resolution: INTERACTION_RESOLUTION,
            stiffness: 24.0,
            damping: 8.5,
            planet_radius: self.planet_radius,
            maximum_displacement: 0.42,
        };
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(&params));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("transient grass interaction"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_groups[self.parity], &[]);
            let count = 6 * INTERACTION_RESOLUTION * INTERACTION_RESOLUTION;
            pass.dispatch_workgroups(count.div_ceil(64), 1, 1);
        }
        self.parity ^= 1;
    }

    #[must_use]
    pub fn current_displacement(&self) -> &wgpu::Buffer {
        &self.displacement[self.parity]
    }

    #[must_use]
    pub fn displacement_buffers(&self) -> [&wgpu::Buffer; 2] {
        [&self.displacement[0], &self.displacement[1]]
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    displacement_in: &wgpu::Buffer,
    velocity_in: &wgpu::Buffer,
    displacement_out: &wgpu::Buffer,
    velocity_out: &wgpu::Buffer,
    sources: &wgpu::Buffer,
    params: &wgpu::Buffer,
    label: &'static str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            buffer_entry(0, displacement_in),
            buffer_entry(1, velocity_in),
            buffer_entry(2, displacement_out),
            buffer_entry(3, velocity_out),
            buffer_entry(4, sources),
            buffer_entry(5, params),
        ],
    })
}

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn force_sources(planet: &Planet, vehicle: &VehicleState, mower_width: f32) -> Vec<ForceSourceGpu> {
    let transform = vehicle.transform;
    let right = transform.forward.cross(transform.up).normalize();
    let velocity_direction = vehicle
        .linear_velocity
        .try_normalize()
        .unwrap_or(-transform.forward);
    let mut result = Vec::with_capacity(7);
    for forward_offset in [-0.78_f32, 0.78] {
        for side_offset in [-0.72_f32, 0.72] {
            let position =
                transform.position + transform.forward * forward_offset + right * side_offset
                    - transform.up * 0.45;
            result.push(source(position, 2.0, -velocity_direction * 0.2, 4.0));
        }
    }
    result.push(source(
        transform.position - transform.up * 0.35,
        3.1,
        -velocity_direction,
        3.0 + vehicle.speed() * 0.18,
    ));
    let deck = transform.position + transform.forward * 0.86 - transform.up * 0.58;
    result.push(source(
        deck,
        mower_width * 0.75,
        -transform.forward * 0.35,
        5.5,
    ));
    let wake_position = transform.position - transform.forward * 2.2;
    result.push(source(
        wake_position,
        4.2,
        -velocity_direction * 1.8,
        vehicle.speed() * 0.22,
    ));
    for source in &mut result {
        let direction =
            Vec3::from_array(source.position_radius[..3].try_into().unwrap()).normalize();
        let radius = planet.surface_radius(direction);
        let point = direction * radius;
        source.position_radius[0] = point.x;
        source.position_radius[1] = point.y;
        source.position_radius[2] = point.z;
    }
    result
}

fn source(position: Vec3, radius: f32, direction: Vec3, strength: f32) -> ForceSourceGpu {
    ForceSourceGpu {
        position_radius: [position.x, position.y, position.z, radius],
        direction_strength: [direction.x, direction.y, direction.z, strength],
    }
}
