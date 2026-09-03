//! Bounded clipping particles stored as one GPU-oriented instance array.

use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use lawn_core::{
    run::{RunEvent, RunState},
    vehicle::VehicleState,
};
use wgpu::util::DeviceExt;

const MAX_PARTICLES: usize = 2_048;

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct ParticleVertex {
    pub local: [f32; 2],
}

impl ParticleVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x2];

    pub const fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
struct ParticleGpu {
    position_size: [f32; 4],
    velocity_life: [f32; 4],
}

impl ParticleGpu {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
        1 => Float32x4,
        2 => Float32x4
    ];

    const fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Particle {
    position: Vec3,
    velocity: Vec3,
    size: f32,
    age: f32,
    lifetime: f32,
}

#[derive(Debug)]
pub struct ClippingParticles {
    particles: Vec<Particle>,
    instance_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    spawn_counter: u32,
    last_spawn_epoch: u32,
    last_update: Instant,
}

impl ClippingParticles {
    pub fn new(device: &wgpu::Device) -> Self {
        let vertices = [
            ParticleVertex { local: [-0.5, 0.0] },
            ParticleVertex { local: [0.5, 0.0] },
            ParticleVertex {
                local: [-0.25, 1.0],
            },
            ParticleVertex { local: [0.25, 1.0] },
        ];
        let indices = [0_u16, 1, 2, 1, 3, 2];
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("clipping particle vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("clipping particle indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clipping particle instances"),
            size: (MAX_PARTICLES * std::mem::size_of::<ParticleGpu>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            particles: Vec::with_capacity(MAX_PARTICLES),
            instance_buffer,
            vertex_buffer,
            index_buffer,
            spawn_counter: 0,
            last_spawn_epoch: u32::MAX,
            last_update: Instant::now(),
        }
    }

    pub fn update(&mut self, queue: &wgpu::Queue, run: &RunState, reduced: bool) {
        let dt = self.last_update.elapsed().as_secs_f32().min(1.0 / 20.0);
        self.last_update = Instant::now();
        for particle in &mut self.particles {
            particle.age += dt;
            let radial = particle.position.normalize_or(Vec3::Y);
            particle.velocity -= radial * (3.2 * dt);
            particle.velocity *= (-1.8 * dt).exp();
            particle.position += particle.velocity * dt;
        }
        self.particles
            .retain(|particle| particle.age < particle.lifetime);

        let newly_cut = run
            .events()
            .iter()
            .any(|event| matches!(event, RunEvent::GrassCut { .. }));
        let spawn_epoch = run.simulation_seconds.to_bits();
        if newly_cut && spawn_epoch != self.last_spawn_epoch {
            let count = if reduced { 3 } else { 11 };
            self.spawn(&run.vehicle.state, count);
            self.last_spawn_epoch = spawn_epoch;
        }

        if self.particles.is_empty() {
            return;
        }
        let instances: Vec<_> = self
            .particles
            .iter()
            .map(|particle| ParticleGpu {
                position_size: [
                    particle.position.x,
                    particle.position.y,
                    particle.position.z,
                    particle.size,
                ],
                velocity_life: [
                    particle.velocity.x,
                    particle.velocity.y,
                    particle.velocity.z,
                    1.0 - particle.age / particle.lifetime,
                ],
            })
            .collect();
        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&instances));
    }

    pub fn clear(&mut self) {
        self.particles.clear();
        self.last_spawn_epoch = u32::MAX;
        self.last_update = Instant::now();
    }

    #[must_use]
    pub fn len(&self) -> u32 {
        self.particles.len() as u32
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.particles.is_empty() {
            return;
        }
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..6, 0, 0..self.particles.len() as u32);
    }

    pub const fn vertex_layouts() -> [wgpu::VertexBufferLayout<'static>; 2] {
        [ParticleVertex::layout(), ParticleGpu::layout()]
    }

    fn spawn(&mut self, vehicle: &VehicleState, count: usize) {
        let transform = vehicle.transform;
        let right = transform.forward.cross(transform.up).normalize();
        let deck = transform.position + transform.forward * 0.9 - transform.up * 0.48;
        for _ in 0..count {
            self.spawn_counter = self.spawn_counter.wrapping_add(1);
            let a = hash01(self.spawn_counter.wrapping_mul(0x9E37_79B9));
            let b = hash01(self.spawn_counter.wrapping_mul(0x85EB_CA6B));
            let c = hash01(self.spawn_counter.wrapping_mul(0xC2B2_AE35));
            let position =
                deck + right * ((a - 0.5) * 1.9) + transform.forward * ((b - 0.5) * 0.55);
            let velocity = vehicle.linear_velocity * 0.22
                + transform.up * (1.2 + c * 2.0)
                + right * ((a - 0.5) * 2.4)
                - transform.forward * (0.4 + b);
            if self.particles.len() == MAX_PARTICLES {
                self.particles.remove(0);
            }
            self.particles.push(Particle {
                position,
                velocity,
                size: 0.045 + b * 0.075,
                age: 0.0,
                lifetime: 0.38 + c * 0.52,
            });
        }
    }
}

fn hash01(mut value: u32) -> f32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^= value >> 16;
    value as f32 / u32::MAX as f32
}
