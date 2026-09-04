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

#[derive(Debug, Default)]
struct ClippingEmission {
    last_seconds: Option<f32>,
    fractional_count: f32,
}

impl ClippingEmission {
    fn count(&mut self, seconds: f32, emitting: bool, reduced: bool, boost: bool) -> usize {
        let previous = self.last_seconds.replace(seconds);
        if !emitting {
            self.fractional_count = 0.0;
            return 0;
        }
        // Limit spawn work after a long frame and tie emission to simulation
        // time, so a fast display does not produce a much denser spray.
        let dt = previous.map_or(1.0 / 60.0, |last| (seconds - last).clamp(0.0, 0.05));
        let rate = if reduced {
            75.0
        } else if boost {
            375.0
        } else {
            300.0
        };
        let requested = self.fractional_count + dt * rate;
        let count = requested as usize;
        self.fractional_count = requested - count as f32;
        count
    }
}

#[derive(Debug)]
pub struct ClippingParticles {
    particles: Vec<Particle>,
    instances: Vec<ParticleGpu>,
    instance_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    spawn_counter: u32,
    emission: ClippingEmission,
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
            instances: Vec::with_capacity(MAX_PARTICLES),
            instance_buffer,
            vertex_buffer,
            index_buffer,
            spawn_counter: 0,
            emission: ClippingEmission::default(),
            last_update: Instant::now(),
        }
    }

    pub fn update(&mut self, queue: &wgpu::Queue, run: &RunState, reduced: bool) {
        let dt = self.last_update.elapsed().as_secs_f32().min(1.0 / 20.0);
        self.last_update = Instant::now();
        let active = run.active && !run.paused;
        let newly_cut = run
            .events()
            .iter()
            .any(|event| matches!(event, RunEvent::GrassCut { .. }));
        let count = self.emission.count(
            run.simulation_seconds,
            newly_cut && active,
            reduced,
            run.vehicle.state.boost_active,
        );
        if !active {
            return;
        }
        for particle in &mut self.particles {
            particle.age += dt;
            let radial = particle.position.normalize_or(Vec3::Y);
            particle.velocity -= radial * (3.2 * dt);
            particle.velocity *= (-1.8 * dt).exp();
            particle.position += particle.velocity * dt;
        }
        self.particles
            .retain(|particle| particle.age < particle.lifetime);

        if count > 0 {
            self.spawn(&run.vehicle.state, count);
        }

        if self.particles.is_empty() {
            return;
        }
        self.instances.clear();
        self.instances
            .extend(self.particles.iter().map(|particle| ParticleGpu {
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
            }));
        queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instances),
        );
    }

    pub fn clear(&mut self) {
        self.particles.clear();
        self.emission = ClippingEmission::default();
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
        let deck = transform.position - transform.up * 0.48;
        let travel = (vehicle.linear_velocity
            - transform.up * vehicle.linear_velocity.dot(transform.up))
        .normalize_or(transform.forward);
        let right = travel.cross(transform.up).normalize();
        let boost_lift = if vehicle.boost_active { 0.4 } else { 0.0 };
        let count = count.min(MAX_PARTICLES);
        let overflow = (self.particles.len() + count).saturating_sub(MAX_PARTICLES);
        self.particles.drain(..overflow);
        for _ in 0..count {
            self.spawn_counter = self.spawn_counter.wrapping_add(1);
            let a = hash01(self.spawn_counter.wrapping_mul(0x9E37_79B9));
            let b = hash01(self.spawn_counter.wrapping_mul(0x85EB_CA6B));
            let c = hash01(self.spawn_counter.wrapping_mul(0xC2B2_AE35));
            let side = if a < 0.5 { -1.0 } else { 1.0 };
            let outward = (right * side - travel * (0.25 + a * 0.65)).normalize();
            let position = deck + outward * (0.72 + b * 0.27);
            let velocity = vehicle.linear_velocity * 0.30
                + transform.up * (1.7 + c * 1.6 + boost_lift)
                + outward * (1.8 + b * 1.2)
                - travel * 0.6;
            self.particles.push(Particle {
                position,
                velocity,
                size: 0.075 + b * 0.075,
                age: 0.0,
                lifetime: 0.4 + c * 0.35,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_emission_is_bounded_and_independent_of_display_rate() {
        let sample = |frames: u32, reduced: bool| {
            let mut emission = ClippingEmission::default();
            emission.count(0.0, false, reduced, false);
            (1..=frames)
                .map(|frame| emission.count(frame as f32 / frames as f32, true, reduced, false))
                .sum::<usize>()
        };
        assert!(sample(60, false).abs_diff(sample(240, false)) <= 1);
        assert!((299..=300).contains(&sample(120, false)));
        assert!((74..=75).contains(&sample(120, true)));

        let mut emission = ClippingEmission::default();
        emission.count(0.0, false, false, false);
        assert!(emission.count(10.0, true, false, true) <= 19);
    }

    #[test]
    fn clipping_emission_requires_fresh_cutting_and_stays_paused() {
        let mut emission = ClippingEmission::default();
        assert!(emission.count(1.0, true, false, false) > 0);
        assert_eq!(emission.count(1.0, true, false, false), 0);
        assert_eq!(emission.count(1.01, false, false, false), 0);
        assert_eq!(emission.count(1.01, true, false, false), 0);
        assert_eq!(emission.count(1.02, false, false, false), 0);
        assert!(emission.count(1.03, true, false, false) > 0);
    }
}
