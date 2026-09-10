use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use lawn_core::{planet::Planet, vehicle::VehicleState};
use wgpu::util::DeviceExt;

pub const INTERACTION_RESOLUTION: u32 = 128;
const MAX_FORCE_SOURCES: usize = 16;
const INTERACTION_STIFFNESS: f32 = 16.0;
const INTERACTION_DAMPING: f32 = 8.0;
// Below a slow walking pace, travel bias fades into an outward stationary wash.
// Normalizing residual hover velocity gives numerical jitter a full-strength wake.
const WASH_DIRECTION_SPEED: f32 = 2.0;

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
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | if cfg!(test) {
                        wgpu::BufferUsages::COPY_SRC
                    } else {
                        wgpu::BufferUsages::empty()
                    },
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
        rival: Option<&VehicleState>,
        mower_width: f32,
        elapsed_seconds: f32,
        frame_dt: f32,
        timestamp_writes: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let (sources, source_count) = paired_force_sources(planet, vehicle, rival, mower_width);
        queue.write_buffer(
            &self.source_buffer,
            0,
            bytemuck::cast_slice(&sources[..source_count]),
        );
        let params = InteractionParamsGpu {
            dt: frame_dt.clamp(0.0, 1.0 / 30.0),
            time: elapsed_seconds,
            source_count: source_count as u32,
            resolution: INTERACTION_RESOLUTION,
            // Critical damping lets the rotor pressure roll through the grass
            // and settle without springing backwards as the mower stops.
            stiffness: INTERACTION_STIFFNESS,
            damping: INTERACTION_DAMPING,
            planet_radius: self.planet_radius,
            maximum_displacement: 1.45,
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

fn force_sources(planet: &Planet, vehicle: &VehicleState, mower_width: f32) -> [ForceSourceGpu; 7] {
    let transform = vehicle.transform;
    let right = transform.forward.cross(transform.up).normalize();
    let tangent_velocity =
        vehicle.linear_velocity - transform.up * vehicle.linear_velocity.dot(transform.up);
    let speed = tangent_velocity.length();
    let travel_bias = tangent_velocity / speed.max(WASH_DIRECTION_SPEED);
    let mut result = [ForceSourceGpu::zeroed(); 7];
    let mut wheel_index = 0;
    for forward_offset in [
        -crate::mesh::HOVER_PAD_OFFSET,
        crate::mesh::HOVER_PAD_OFFSET,
    ] {
        for side_offset in [
            -crate::mesh::HOVER_PAD_OFFSET,
            crate::mesh::HOVER_PAD_OFFSET,
        ] {
            let position =
                transform.position + transform.forward * forward_offset + right * side_offset
                    - transform.up * 0.45;
            result[wheel_index] = source(position, 2.8, -travel_bias * 0.35, 12.0);
            wheel_index += 1;
        }
    }
    result[4] = source(
        transform.position - transform.up * 0.35,
        4.8,
        -travel_bias * 1.15,
        14.0 + speed * 0.45,
    );
    let deck = transform.position - transform.up * 0.58;
    result[5] = source(deck, mower_width * 1.25, -travel_bias * 0.45, 20.0);
    let wake_position = transform.position - travel_bias * 2.2;
    result[6] = source(wake_position, 6.0, -travel_bias * 2.0, speed * 0.65);
    for source in &mut result {
        let direction =
            Vec3::from_array(source.position_radius[..3].try_into().unwrap()).normalize();
        // The compute field lives on the base sphere. Comparing it with
        // terrain-elevated sources incorrectly weakens wash on hills.
        let point = direction * planet.config.base_radius;
        source.position_radius[0] = point.x;
        source.position_radius[1] = point.y;
        source.position_radius[2] = point.z;
    }
    result
}

fn paired_force_sources(
    planet: &Planet,
    vehicle: &VehicleState,
    rival: Option<&VehicleState>,
    mower_width: f32,
) -> ([ForceSourceGpu; MAX_FORCE_SOURCES], usize) {
    let mut sources = [ForceSourceGpu::zeroed(); MAX_FORCE_SOURCES];
    sources[..7].copy_from_slice(&force_sources(planet, vehicle, mower_width));
    let count = if let Some(rival) = rival {
        sources[7..14].copy_from_slice(&force_sources(planet, rival, mower_width));
        14
    } else {
        7
    };
    (sources, count)
}

fn source(position: Vec3, radius: f32, direction: Vec3, strength: f32) -> ForceSourceGpu {
    ForceSourceGpu {
        position_radius: [position.x, position.y, position.z, radius],
        direction_strength: [direction.x, direction.y, direction.z, strength],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lawn_core::{
        GeneratorConfig, PlanetGenerator, VehicleTuning, WorldSeed,
        planet::CURRENT_GENERATOR_VERSION, vehicle::VehicleState,
    };

    #[test]
    fn centered_mower_emits_the_strongest_stationary_wash() {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(21), false)
                .unwrap();
        let vehicle = VehicleState::at_spawn(planet.spawn, &VehicleTuning::default());
        let sources = force_sources(&planet, &vehicle, 2.2);

        assert_eq!(sources.len(), 7);
        assert!(sources[..4].iter().all(|source| {
            (source.direction_strength[3] - 12.0).abs() < f32::EPSILON
                && (source.position_radius[3] - 2.8).abs() < f32::EPSILON
        }));
        let central = sources[4];
        let mower = sources[5];
        let wake = sources[6];
        assert_eq!(central.direction_strength[3], 14.0);
        assert_eq!(mower.direction_strength[3], 20.0);
        assert!((mower.position_radius[3] - 2.75).abs() < 1.0e-6);
        assert_eq!(wake.direction_strength[3], 0.0);

        let mower_direction =
            Vec3::from_array(mower.position_radius[..3].try_into().unwrap()).normalize();
        assert!(mower_direction.dot(vehicle.transform.position.normalize()) > 0.999_99);
    }

    #[test]
    fn elevated_sources_match_the_compute_fields_base_sphere() {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(21), false)
                .unwrap();
        let mut vehicle = VehicleState::at_spawn(planet.spawn, &VehicleTuning::default());
        vehicle.transform.position += vehicle.transform.up * 6.0;
        for source in force_sources(&planet, &vehicle, 2.2) {
            let point = Vec3::from_array(source.position_radius[..3].try_into().unwrap());
            assert!((point.length() - planet.config.base_radius).abs() < 1.0e-4);
        }
    }

    #[test]
    fn stopping_wash_fades_continuously_through_velocity_reversals() {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(21), false)
                .unwrap();
        let mut vehicle = VehicleState::at_spawn(planet.spawn, &VehicleTuning::default());
        let stationary = force_sources(&planet, &vehicle, 2.2);
        for speed in [-0.5_f32, -0.05, -1.0e-6, 0.0, 1.0e-6, 0.05, 0.5, 2.0, 12.0] {
            vehicle.linear_velocity = vehicle.transform.forward * speed;
            let moving = force_sources(&planet, &vehicle, 2.2);
            let bias = Vec3::from_slice(&moving[4].direction_strength[..3]);
            // The body wash must become negligible near zero, not jump to a
            // full unit vector when the hover suspension changes velocity sign.
            assert!(
                bias.length() <= (speed.abs() / WASH_DIRECTION_SPEED).min(1.0) * 1.151 + 1.0e-6
            );
            assert!(bias.dot(vehicle.linear_velocity) <= 1.0e-6);
            if speed.abs() < 1.0e-5 {
                let wake = Vec3::from_slice(&moving[6].position_radius[..3]);
                let resting_wake = Vec3::from_slice(&stationary[6].position_radius[..3]);
                assert!(wake.distance(resting_wake) < 1.0e-4);
                assert!(moving[6].direction_strength[3] < 1.0e-5);
            }
            if speed.abs() >= WASH_DIRECTION_SPEED {
                assert!(bias.length() > 1.1, "cruising must retain a strong wake");
            }
        }
    }

    #[test]
    fn radial_hover_motion_does_not_redirect_the_wash() {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(21), false)
                .unwrap();
        let mut vehicle = VehicleState::at_spawn(planet.spawn, &VehicleTuning::default());
        for speed in [0.0, 3.0] {
            vehicle.linear_velocity = vehicle.transform.forward * speed;
            let level = force_sources(&planet, &vehicle, 2.2);
            vehicle.linear_velocity += vehicle.transform.up * 4.0;
            let bouncing = force_sources(&planet, &vehicle, 2.2);
            for (a, b) in level.iter().zip(bouncing.iter()) {
                for (a, b) in a.direction_strength.iter().zip(b.direction_strength.iter()) {
                    assert!((a - b).abs() < 1.0e-5);
                }
            }
        }
    }

    #[test]
    fn rival_adds_independent_sources_within_existing_compute_capacity() {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(21), false)
                .unwrap();
        let player = VehicleState::at_spawn(planet.spawn, &VehicleTuning::default());
        let mut rival = player.clone();
        rival.transform.position = -player.transform.position;
        rival.transform.up = -player.transform.up;
        let (single, single_count) = paired_force_sources(&planet, &player, None, 2.2);
        let (paired, paired_count) = paired_force_sources(&planet, &player, Some(&rival), 2.2);
        assert_eq!(single_count, 7);
        assert_eq!(paired_count, 14);
        assert!(paired_count <= MAX_FORCE_SOURCES);
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(&single[..7]),
            bytemuck::cast_slice::<_, u8>(&paired[..7])
        );
        for source in &paired[7..14] {
            let point = Vec3::from_slice(&source.position_radius[..3]);
            assert!(point.dot(rival.transform.up) > 0.0);
            assert!(point.dot(player.transform.up) < 0.0);
        }
    }
}

#[cfg(test)]
#[path = "interaction_gpu_tests.rs"]
mod gpu_tests;
