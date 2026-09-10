//! Bounded lawn clippings, stone dust, and quiet occasion accents.
//! All effects share one draw and consume simulation events at most once.

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
    // kind, stable variation, opacity, reserved
    appearance: [f32; 4],
}

impl ParticleGpu {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        1 => Float32x4,
        2 => Float32x4,
        3 => Float32x4
    ];

    const fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
enum ParticleKind {
    Clipping,
    Dust,
    Recovery,
    Milestone,
}

#[derive(Clone, Copy, Debug)]
struct Particle {
    position: Vec3,
    velocity: Vec3,
    size: f32,
    age: f32,
    lifetime: f32,
    kind: ParticleKind,
    variation: f32,
    opacity: f32,
}

impl Particle {
    fn advance(&mut self, dt: f32) {
        self.age += dt;
        let radial = self.position.normalize_or(Vec3::Y);
        let (gravity, drag) = match self.kind {
            ParticleKind::Clipping => (3.2, 1.8),
            ParticleKind::Dust => (0.45, 3.4),
            ParticleKind::Recovery | ParticleKind::Milestone => (-0.15, 2.0),
        };
        self.velocity -= radial * (gravity * dt);
        self.velocity *= (-drag * dt).exp();
        self.position += self.velocity * dt;
    }
}

#[derive(Debug, Default, PartialEq)]
struct EmissionBatch {
    clippings: usize,
    cut_richness: f32,
    dust: usize,
    impact: usize,
    recovery: usize,
    milestone: usize,
}

#[derive(Debug, Default)]
struct ParticleEmission {
    last_seconds: Option<f32>,
    fractional_clippings: f32,
    fractional_dust: f32,
}

impl ParticleEmission {
    fn sample(
        &mut self,
        seconds: f32,
        events: &[RunEvent],
        active: bool,
        reduced: bool,
        speed: f32,
        deck_width: f32,
    ) -> EmissionBatch {
        let previous = self.last_seconds.replace(seconds);
        if !active || previous.is_some_and(|last| seconds < last) {
            self.fractional_clippings = 0.0;
            self.fractional_dust = 0.0;
            return EmissionBatch::default();
        }
        // Event buffers can survive another render or a pause. Advancing
        // simulation time, rather than the display clock, authorizes a batch.
        if previous == Some(seconds) {
            return EmissionBatch::default();
        }
        let dt = previous.map_or(1.0 / 60.0, |last| (seconds - last).clamp(0.0, 0.05));
        let mut batch = EmissionBatch::default();
        let mut cut_area = 0.0;
        let mut scraping = false;
        for event in events {
            match *event {
                RunEvent::GrassCut { weight } if weight.is_finite() && weight > 0.0 => {
                    cut_area = (cut_area + weight).min(8.0);
                }
                RunEvent::RockScrape => scraping = true,
                RunEvent::SubstantialCollision { impulse } => {
                    batch.impact = batch.impact.max(if reduced {
                        3
                    } else {
                        (6.0 + impulse.clamp(0.0, 12.0) * 0.7) as usize
                    });
                }
                RunEvent::Recovered => batch.recovery = if reduced { 8 } else { 24 },
                RunEvent::CoverageMilestone(_) => {
                    batch.milestone = if reduced { 4 } else { 10 };
                }
                _ => {}
            }
        }
        if cut_area > 0.0 {
            // GrassCut weight is square metres of newly covered lawn. Tying
            // quantity to that area makes an edge shave lighter than a full
            // pass and naturally produces more clippings at higher speed.
            let density = if reduced { 3.0 } else { 12.0 };
            let requested = self.fractional_clippings + (cut_area * density) as f32;
            let whole = requested.floor();
            batch.clippings = (whole as usize).min(if reduced { 16 } else { 64 });
            // Discard overflow: a hitch must not queue a later particle flood.
            self.fractional_clippings = requested - whole;
            let swept_area = dt * deck_width * speed.max(2.0);
            batch.cut_richness = (cut_area as f32 / swept_area.max(0.01)).clamp(0.0, 1.0);
        }
        if scraping && speed > 1.0 && batch.impact == 0 {
            let rate = speed.min(16.0) * if reduced { 0.35 } else { 1.4 };
            let requested = self.fractional_dust + dt * rate;
            batch.dust = requested as usize;
            self.fractional_dust = requested - batch.dust as f32;
        } else {
            self.fractional_dust = 0.0;
        }
        // A recovery is its own clean gesture; catch-up events from before
        // teleporting should not deposit grass or rock dust at the new spawn.
        if batch.recovery > 0 {
            batch.clippings = 0;
            batch.dust = 0;
            batch.impact = 0;
            batch.milestone = 0;
        }
        batch
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
    emission: ParticleEmission,
    last_update: Instant,
}

impl ClippingParticles {
    pub fn new(device: &wgpu::Device) -> Self {
        let vertices = [
            ParticleVertex { local: [-0.5, 0.0] },
            ParticleVertex { local: [0.5, 0.0] },
            ParticleVertex { local: [-0.5, 1.0] },
            ParticleVertex { local: [0.5, 1.0] },
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
            emission: ParticleEmission::default(),
            last_update: Instant::now(),
        }
    }

    pub fn update(&mut self, queue: &wgpu::Queue, run: &RunState, reduced: bool) {
        let dt = self.last_update.elapsed().as_secs_f32().min(1.0 / 20.0);
        self.last_update = Instant::now();
        let active = run.active && !run.paused;
        let batch = self.emission.sample(
            run.simulation_seconds,
            run.events(),
            active,
            reduced,
            run.vehicle.state.speed(),
            run.vehicle_tuning.mower_width,
        );
        if !active {
            return;
        }
        for particle in &mut self.particles {
            particle.advance(dt);
        }
        self.particles
            .retain(|particle| particle.age < particle.lifetime);

        if batch.recovery > 0 {
            self.particles.clear();
        }
        self.spawn(&run.vehicle.state, &batch, reduced);

        self.upload(queue);
    }

    fn upload(&mut self, queue: &wgpu::Queue) {
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
                appearance: [
                    particle.kind as u8 as f32,
                    particle.variation,
                    particle.opacity,
                    0.0,
                ],
            }));
        queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instances),
        );
    }

    /// Deterministic effect reference for the headless renderer capture test.
    #[cfg(test)]
    pub(super) fn preview_event(&mut self, queue: &wgpu::Queue, run: &RunState, event: RunEvent) {
        self.clear();
        let batch = self.emission.sample(
            run.simulation_seconds,
            &[event],
            true,
            false,
            run.vehicle.state.speed().max(12.0),
            run.vehicle_tuning.mower_width,
        );
        self.spawn(&run.vehicle.state, &batch, false);
        for particle in &mut self.particles {
            particle.advance(0.12);
        }
        self.upload(queue);
    }

    pub fn clear(&mut self) {
        self.particles.clear();
        self.emission = ParticleEmission::default();
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

    fn spawn(&mut self, vehicle: &VehicleState, batch: &EmissionBatch, reduced: bool) {
        let groups = [
            (ParticleKind::Clipping, batch.clippings),
            (ParticleKind::Dust, batch.dust + batch.impact),
            (ParticleKind::Recovery, batch.recovery),
            (ParticleKind::Milestone, batch.milestone),
        ];
        let count = groups.iter().map(|(_, count)| count).sum::<usize>();
        let overflow = (self.particles.len() + count).saturating_sub(MAX_PARTICLES);
        self.particles.drain(..overflow.min(self.particles.len()));
        for (kind, count) in groups {
            for index in 0..count {
                self.spawn_counter = self.spawn_counter.wrapping_add(1);
                self.particles.push(make_particle(
                    vehicle,
                    kind,
                    self.spawn_counter,
                    index as f32 / count as f32,
                    batch.cut_richness,
                    batch.impact > 0,
                    reduced,
                ));
            }
        }
    }
}

fn make_particle(
    vehicle: &VehicleState,
    kind: ParticleKind,
    seed: u32,
    ring_fraction: f32,
    cut_richness: f32,
    impact: bool,
    reduced: bool,
) -> Particle {
    let transform = vehicle.transform;
    let up = transform.up;
    let deck = transform.position - up * 0.48;
    let travel = (vehicle.linear_velocity - up * vehicle.linear_velocity.dot(up))
        .normalize_or(transform.forward);
    let right = travel.cross(up).normalize_or(Vec3::X);
    let a = hash01(seed.wrapping_mul(0x9E37_79B9));
    let b = hash01(seed.wrapping_mul(0x85EB_CA6B));
    let c = hash01(seed.wrapping_mul(0xC2B2_AE35));
    let side = if a < 0.5 { -1.0 } else { 1.0 };
    let outward = (right * side - travel * (0.25 + a * 0.65)).normalize();
    let mut particle = match kind {
        ParticleKind::Clipping => {
            let boost_lift = if vehicle.boost_active { 0.4 } else { 0.0 };
            let fullness = 0.68 + cut_richness * 0.32;
            Particle {
                position: deck + outward * (0.72 + b * 0.27),
                velocity: vehicle.linear_velocity * 0.30
                    + up * (1.7 + c * 1.6 + boost_lift) * fullness
                    + outward * (1.8 + b * 1.2) * fullness
                    - travel * 0.6,
                size: (0.065 + b * 0.075) * fullness,
                age: 0.0,
                lifetime: 0.4 + c * 0.35,
                kind,
                variation: a,
                opacity: 0.95,
            }
        }
        ParticleKind::Dust => Particle {
            position: deck + outward * (0.8 + b * 0.3) - up * 0.1,
            velocity: vehicle.linear_velocity * 0.10
                + outward * (0.5 + b * if impact { 1.3 } else { 0.6 })
                + up * (0.35 + c * 0.5),
            size: 0.14 + b * if impact { 0.15 } else { 0.09 },
            age: 0.0,
            lifetime: 0.35 + c * 0.22,
            kind,
            variation: a,
            opacity: if impact { 0.42 } else { 0.28 },
        },
        ParticleKind::Recovery | ParticleKind::Milestone => {
            // Evenly spaced motes make a readable arrival gesture without a
            // bright flash or a screen-filling confetti burst.
            let angle = ring_fraction * std::f32::consts::TAU;
            let direction = right * angle.cos() + travel * angle.sin();
            let recovering = kind == ParticleKind::Recovery;
            Particle {
                position: deck
                    + direction * if recovering { 1.25 } else { 1.0 }
                    + up * if recovering { 0.65 } else { 1.1 },
                velocity: direction * if recovering { 0.7 } else { 0.45 }
                    + up * if recovering { 0.22 } else { 0.65 + c * 0.25 },
                size: if recovering {
                    0.13 + b * 0.045
                } else {
                    0.09 + b * 0.04
                },
                age: 0.0,
                lifetime: if recovering {
                    0.8 + c * 0.16
                } else {
                    0.7 + c * 0.2
                },
                kind,
                variation: a,
                opacity: 0.72,
            }
        }
    };
    if reduced {
        particle.velocity *= 0.45;
        particle.opacity *= 0.85;
    }
    particle
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

    fn sample_area(frames: u32, area: f64, reduced: bool) -> usize {
        let mut emission = ParticleEmission::default();
        emission.sample(0.0, &[], true, reduced, 12.0, 2.2);
        (1..=frames)
            .map(|frame| {
                emission
                    .sample(
                        frame as f32 / frames as f32,
                        &[RunEvent::GrassCut {
                            weight: area / f64::from(frames),
                        }],
                        true,
                        reduced,
                        12.0,
                        2.2,
                    )
                    .clippings
            })
            .sum()
    }

    #[test]
    fn clipping_density_tracks_cut_area_independent_of_display_rate() {
        for reduced in [false, true] {
            assert!(sample_area(60, 20.0, reduced).abs_diff(sample_area(240, 20.0, reduced)) <= 1);
        }
        let full_pass = sample_area(120, 20.0, false);
        let edge_shave = sample_area(120, 5.0, false);
        assert!(full_pass.abs_diff(edge_shave * 4) <= 4);
        assert!((239..=240).contains(&full_pass));
        assert!((59..=60).contains(&sample_area(120, 20.0, true)));
    }

    #[test]
    fn event_batches_are_consumed_once_and_do_not_replay_after_pause_or_rewind() {
        let events = [
            RunEvent::GrassCut { weight: 1.0 },
            RunEvent::SubstantialCollision { impulse: 5.0 },
            RunEvent::CoverageMilestone(25),
        ];
        let mut emission = ParticleEmission::default();
        let first = emission.sample(1.0, &events, true, false, 12.0, 2.2);
        assert!(first.clippings > 0 && first.impact > 0 && first.milestone > 0);
        assert_eq!(
            emission.sample(1.0, &events, true, false, 12.0, 2.2),
            EmissionBatch::default()
        );
        assert_eq!(
            emission.sample(1.1, &events, false, false, 12.0, 2.2),
            EmissionBatch::default()
        );
        assert_eq!(
            emission.sample(1.1, &events, true, false, 12.0, 2.2),
            EmissionBatch::default()
        );
        assert_eq!(
            emission.sample(0.5, &events, true, false, 12.0, 2.2),
            EmissionBatch::default()
        );
        assert_eq!(
            emission.sample(0.6, &[], true, false, 12.0, 2.2),
            EmissionBatch::default()
        );
        assert!(emission.sample(0.7, &events, true, false, 12.0, 2.2).impact > 0);
    }

    #[test]
    fn catch_up_emission_is_bounded_and_never_deferred() {
        let mut emission = ParticleEmission::default();
        let events = vec![RunEvent::GrassCut { weight: 100_000.0 }; 8];
        let batch = emission.sample(10.0, &events, true, false, 12.0, 2.2);
        assert_eq!(batch.clippings, 64);
        assert_eq!(
            emission.sample(10.1, &[], true, false, 12.0, 2.2),
            EmissionBatch::default()
        );
        assert_eq!(
            emission
                .sample(10.2, &events, true, true, 12.0, 2.2)
                .clippings,
            16
        );
    }

    #[test]
    fn scrape_requires_motion_and_recovery_suppresses_old_location_effects() {
        let mut emission = ParticleEmission::default();
        assert_eq!(
            emission
                .sample(1.0, &[RunEvent::RockScrape], true, false, 0.0, 2.2)
                .dust,
            0
        );
        let dust = (1..=10)
            .map(|frame| {
                emission
                    .sample(
                        1.0 + frame as f32 / 60.0,
                        &[RunEvent::RockScrape],
                        true,
                        false,
                        12.0,
                        2.2,
                    )
                    .dust
            })
            .sum::<usize>();
        assert!(dust > 0);
        let events = [
            RunEvent::GrassCut { weight: 1.0 },
            RunEvent::RockScrape,
            RunEvent::SubstantialCollision { impulse: 9.0 },
            RunEvent::CoverageMilestone(25),
            RunEvent::Recovered,
        ];
        let batch = emission.sample(1.2, &events, true, false, 12.0, 2.2);
        assert_eq!(
            (batch.clippings, batch.dust, batch.impact, batch.milestone),
            (0, 0, 0, 0)
        );
        assert_eq!(batch.recovery, 24);
        assert_eq!(
            emission
                .sample(1.3, &events, true, true, 12.0, 2.2)
                .recovery,
            8
        );
    }

    #[test]
    fn reduced_effects_have_less_motion_and_accents_remain_finite() {
        use lawn_core::{config::VehicleTuning, planet::SpawnPoint};
        let vehicle = VehicleState::at_spawn(
            SpawnPoint {
                position: Vec3::Y * 16.0,
                forward: Vec3::NEG_Z,
                up: Vec3::Y,
                clearance: 3.0,
            },
            &VehicleTuning::default(),
        );
        for kind in [
            ParticleKind::Clipping,
            ParticleKind::Dust,
            ParticleKind::Recovery,
            ParticleKind::Milestone,
        ] {
            let mut full = make_particle(&vehicle, kind, 3, 0.2, 1.0, true, false);
            let reduced = make_particle(&vehicle, kind, 3, 0.2, 1.0, true, true);
            assert!(reduced.velocity.length() < full.velocity.length());
            assert!(reduced.opacity < full.opacity);
            let still = full;
            full.advance(0.0);
            assert_eq!(full.position, still.position);
            assert_eq!(full.age, still.age);
            for _ in 0..60 {
                full.advance(1.0 / 60.0);
                assert!(full.position.is_finite() && full.velocity.is_finite());
            }
            assert!(full.age > full.lifetime);
        }
    }
}
