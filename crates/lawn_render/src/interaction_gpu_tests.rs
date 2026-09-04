//! Readback regressions for the actual interaction compute shader.

use super::*;
use lawn_core::cube_map::{CubeFace, face_uv_to_direction};

// An odd grid has an exact +Y sample. That catches the otherwise easy-to-miss
// direction singularity when a force source crosses a field-cell center.
const TEST_RESOLUTION: u32 = 3;
const FIELD_COUNT: usize = (6 * TEST_RESOLUTION * TEST_RESOLUTION) as usize;
const CENTER_INDEX: usize = (2 * TEST_RESOLUTION * TEST_RESOLUTION
    + TEST_RESOLUTION * (TEST_RESOLUTION / 2)
    + TEST_RESOLUTION / 2) as usize;
const VECTOR_BYTES: u64 = 16;
const PLANET_RADIUS: f32 = 20.0;
const MAX_DISPLACEMENT: f32 = 1.45;

fn params(dt: f32, source_count: u32) -> InteractionParamsGpu {
    InteractionParamsGpu {
        dt,
        time: 0.7,
        source_count,
        resolution: TEST_RESOLUTION,
        stiffness: INTERACTION_STIFFNESS,
        damping: INTERACTION_DAMPING,
        planet_radius: PLANET_RADIUS,
        maximum_displacement: MAX_DISPLACEMENT,
    }
}

#[allow(clippy::used_underscore_binding)]
fn seed_field(queue: &wgpu::Queue, interaction: &mut GrassInteraction, center: Vec3) {
    let zeroes = [[0.0_f32; 4]; FIELD_COUNT];
    let mut displacement = zeroes;
    displacement[CENTER_INDEX] = center.extend(0.0).to_array();
    for field in &interaction.displacement {
        queue.write_buffer(field, 0, bytemuck::cast_slice(&displacement));
    }
    // The shipping field retains velocity only through bind groups; regression
    // setup resets it explicitly so each experiment has known initial state.
    for field in &interaction._velocity {
        queue.write_buffer(field, 0, bytemuck::cast_slice(&zeroes));
    }
    interaction.parity = 0;
}

fn dispatch(interaction: &mut GrassInteraction, encoder: &mut wgpu::CommandEncoder) {
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("interaction regression step"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&interaction.pipeline);
        pass.set_bind_group(0, &interaction.bind_groups[interaction.parity], &[]);
        pass.dispatch_workgroups((FIELD_COUNT as u32).div_ceil(64), 1, 1);
    }
    interaction.parity ^= 1;
}

fn readback_buffer(device: &wgpu::Device, count: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("interaction regression readback"),
        size: count as u64 * VECTOR_BYTES,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

fn submit_and_read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: wgpu::CommandEncoder,
    readback: &wgpu::Buffer,
) -> Vec<Vec3> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    encoder.map_buffer_on_submit(readback, wgpu::MapMode::Read, .., move |result| {
        sender.send(result).unwrap();
    });
    queue.submit([encoder.finish()]);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receiver.recv().unwrap().unwrap();
    let mapped = readback.slice(..).get_mapped_range();
    let vectors = mapped
        .chunks_exact(VECTOR_BYTES as usize)
        .map(|bytes| {
            let channels: [f32; 4] = std::array::from_fn(|channel| {
                let start = channel * 4;
                f32::from_le_bytes(bytes[start..start + 4].try_into().unwrap())
            });
            assert_eq!(channels[3], 0.0, "the field's padding must remain clear");
            Vec3::new(channels[0], channels[1], channels[2])
        })
        .collect();
    drop(mapped);
    readback.unmap();
    vectors
}

fn assert_valid_field(field: &[Vec3]) {
    assert_eq!(field.len(), FIELD_COUNT);
    let face_area = TEST_RESOLUTION * TEST_RESOLUTION;
    for (index, displacement) in field.iter().enumerate() {
        let index = index as u32;
        let local = index % face_area;
        let uv = (glam::Vec2::new(
            (local % TEST_RESOLUTION) as f32 + 0.5,
            (local / TEST_RESOLUTION) as f32 + 0.5,
        ) / TEST_RESOLUTION as f32)
            * 2.0
            - glam::Vec2::ONE;
        let normal = face_uv_to_direction(CubeFace::ALL[(index / face_area) as usize], uv);
        assert!(
            displacement.is_finite(),
            "nonfinite displacement at {index}"
        );
        assert!(displacement.length() <= MAX_DISPLACEMENT + 1.0e-5);
        assert!(
            displacement.dot(normal).abs() < 1.0e-5,
            "displacement must stay tangent to cell {index}"
        );
    }
}

fn source_response(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    interaction: &mut GrassInteraction,
    offset: f32,
    strength: f32,
) -> Vec<Vec3> {
    seed_field(queue, interaction, Vec3::ZERO);
    let position = (Vec3::Y * PLANET_RADIUS + Vec3::X * offset).normalize() * PLANET_RADIUS;
    let source = source(position, 2.4, Vec3::ZERO, strength);
    queue.write_buffer(&interaction.source_buffer, 0, bytemuck::bytes_of(&source));
    queue.write_buffer(
        &interaction.params_buffer,
        0,
        bytemuck::bytes_of(&params(1.0 / 30.0, 1)),
    );
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    dispatch(interaction, &mut encoder);
    let readback = readback_buffer(device, FIELD_COUNT);
    encoder.copy_buffer_to_buffer(
        interaction.current_displacement(),
        0,
        &readback,
        0,
        FIELD_COUNT as u64 * VECTOR_BYTES,
    );
    let field = submit_and_read(device, queue, encoder, &readback);
    assert_valid_field(&field);
    field
}

fn release_trajectory(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    interaction: &mut GrassInteraction,
    frequency: u32,
) -> Vec<Vec3> {
    let initial = Vec3::X * 0.6;
    seed_field(queue, interaction, initial);
    queue.write_buffer(
        &interaction.params_buffer,
        0,
        bytemuck::bytes_of(&params(1.0 / frequency as f32, 0)),
    );
    let steps = frequency as usize * 2;
    let readback = readback_buffer(device, steps + FIELD_COUNT);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    for step in 0..steps {
        dispatch(interaction, &mut encoder);
        encoder.copy_buffer_to_buffer(
            interaction.current_displacement(),
            CENTER_INDEX as u64 * VECTOR_BYTES,
            &readback,
            step as u64 * VECTOR_BYTES,
            VECTOR_BYTES,
        );
    }
    encoder.copy_buffer_to_buffer(
        interaction.current_displacement(),
        0,
        &readback,
        steps as u64 * VECTOR_BYTES,
        FIELD_COUNT as u64 * VECTOR_BYTES,
    );
    let result = submit_and_read(device, queue, encoder, &readback);
    assert_valid_field(&result[steps..]);
    let mut previous = initial.x;
    for (step, displacement) in result[..steps].iter().enumerate() {
        assert!(displacement.is_finite());
        assert!(
            displacement.x >= -1.0e-6 && displacement.x <= previous + 1.0e-6,
            "released grass reversed or grew at {frequency} Hz, step {step}: {displacement:?} after {previous}"
        );
        assert!(displacement.y.abs() < 1.0e-6 && displacement.z.abs() < 1.0e-6);
        previous = displacement.x;
    }
    assert!(previous < initial.x * 0.01, "grass must settle promptly");
    result[..steps].to_vec()
}

#[test]
#[ignore = "requires a working wgpu graphics adapter"]
fn gpu_hover_wash_has_a_smooth_core_and_settles_without_reversing() {
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
        println!("Hover wash readback adapter: {}", adapter.get_info().name);
        let mut interaction = GrassInteraction::new(&device, PLANET_RADIUS);

        let centered = source_response(&device, &queue, &mut interaction, 0.0, 20.0);
        assert!(
            centered[CENTER_INDEX].length() < 1.0e-6,
            "a centered source must not kick in an arbitrary direction"
        );
        let mut responses = Vec::new();
        for offset in [0.1, -0.1, 0.001, -0.001] {
            let response =
                source_response(&device, &queue, &mut interaction, offset, 20.0)[CENTER_INDEX];
            assert!(
                response.x * offset < 0.0,
                "wash must point away from its source"
            );
            responses.push(response);
        }
        for pair in responses.chunks_exact(2) {
            assert!((pair[0] + pair[1]).length() < pair[0].length() * 0.05 + 1.0e-7);
        }
        assert!(
            responses[2].length() < responses[0].length() * 0.05,
            "the force must fade continuously as a source approaches the cell center"
        );
        assert!(responses[2].length() > responses[0].length() * 0.002);
        // A strong impulse exercises the displacement limiter through the real
        // shader, independently of the more gentle production source tuning.
        source_response(&device, &queue, &mut interaction, 0.1, 1.0e6);

        let trajectories = [30, 60, 120]
            .map(|frequency| release_trajectory(&device, &queue, &mut interaction, frequency));
        // Compare equal elapsed times, not equal frame indices.
        for step in 0..60 {
            let at_30 = trajectories[0][step];
            assert!(at_30.distance(trajectories[1][step * 2 + 1]) < 1.0e-4);
            assert!(at_30.distance(trajectories[2][step * 4 + 3]) < 1.0e-4);
        }
    });
}
