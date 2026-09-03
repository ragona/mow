use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use lawn_core::{
    cube_map::{CubeFace, direction_to_cell, face_uv_to_direction},
    planet::{Planet, SurfaceMaterial},
    vehicle::VehicleTransform,
};

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub material: u32,
    pub variation: f32,
}

impl MeshVertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Uint32,
        3 => Float32
    ];

    #[must_use]
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
pub struct TuftVertex {
    pub local_position: [f32; 3],
}

impl TuftVertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];

    #[must_use]
    pub const fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

pub const ROOT_ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
    1 => Float32x3,
    2 => Uint32
];

#[must_use]
pub const fn root_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: 16,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ROOT_ATTRIBUTES,
    }
}

pub fn build_terrain(planet: &Planet) -> (Vec<MeshVertex>, Vec<u32>) {
    let resolution = planet.terrain.resolution();
    let vertices_per_face = (resolution + 1) * (resolution + 1);
    let mut vertices = Vec::with_capacity((vertices_per_face * 6) as usize);
    let mut indices = Vec::with_capacity((resolution * resolution * 6 * 6) as usize);
    for face in CubeFace::ALL {
        let face_start = vertices.len() as u32;
        for y in 0..=resolution {
            for x in 0..=resolution {
                let uv = glam::Vec2::new(
                    x as f32 * 2.0 / resolution as f32 - 1.0,
                    y as f32 * 2.0 / resolution as f32 - 1.0,
                );
                let direction = face_uv_to_direction(face, uv);
                let position = planet.surface_point(direction);
                let sample = planet.terrain_cell(direction);
                let cell = direction_to_cell(direction, planet.terrain.resolution());
                let variation = hash01((face.index() as u32) << 28 | cell.y << 14 | cell.x);
                vertices.push(MeshVertex {
                    position: position.to_array(),
                    normal: sample.normal.to_array(),
                    material: match sample.material {
                        SurfaceMaterial::Grass => 0,
                        SurfaceMaterial::Rock => 1,
                    },
                    variation,
                });
            }
        }
        let stride = resolution + 1;
        for y in 0..resolution {
            for x in 0..resolution {
                let i0 = face_start + y * stride + x;
                let i1 = i0 + 1;
                let i2 = i0 + stride;
                let i3 = i2 + 1;
                indices.extend_from_slice(&[i0, i2, i1, i1, i2, i3]);
            }
        }
    }
    (vertices, indices)
}

pub fn build_tuft() -> (Vec<TuftVertex>, Vec<u16>) {
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(54);
    for blade in 0..3_u16 {
        let angle = f32::from(blade) * std::f32::consts::PI / 3.0;
        let width_axis = Vec3::new(angle.cos(), 0.0, angle.sin());
        let bend_axis = Vec3::new(-angle.sin(), 0.0, angle.cos());
        let base = vertices.len() as u16;
        for segment in 0..4 {
            let height = segment as f32 / 3.0;
            let half_width = 0.045 * (1.0 - height * 0.86);
            let bend = bend_axis * (height * height * 0.07);
            for side in [-1.0_f32, 1.0] {
                let local = width_axis * half_width * side + bend + Vec3::Y * height;
                vertices.push(TuftVertex {
                    local_position: local.to_array(),
                });
            }
        }
        for segment in 0..3_u16 {
            let row = base + segment * 2;
            indices.extend_from_slice(&[row, row + 2, row + 1, row + 1, row + 2, row + 3]);
        }
    }
    (vertices, indices)
}

pub fn build_vehicle(
    transform: VehicleTransform,
    mower_enabled: bool,
    reversing: bool,
    elapsed_seconds: f32,
) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(80);
    let mut indices = Vec::with_capacity(120);
    add_box(
        &mut vertices,
        &mut indices,
        transform,
        Vec3::new(0.0, 0.05, 0.0),
        Vec3::new(1.05, 0.36, 1.2),
        2,
    );
    add_box(
        &mut vertices,
        &mut indices,
        transform,
        Vec3::new(0.0, 0.42, 0.18),
        Vec3::new(0.72, 0.27, 0.68),
        3,
    );
    // Front-mounted deck makes the exact cutting width readable.
    add_box(
        &mut vertices,
        &mut indices,
        transform,
        Vec3::new(
            0.0,
            -0.22
                + if mower_enabled {
                    (elapsed_seconds * 57.0).sin() * 0.018
                } else {
                    0.0
                },
            -1.18,
        ),
        Vec3::new(1.1, 0.11, 0.38),
        if mower_enabled { 7 } else { 4 },
    );
    for side in [-0.72_f32, 0.72] {
        for longitudinal in [-0.68_f32, 0.68] {
            add_box(
                &mut vertices,
                &mut indices,
                transform,
                Vec3::new(side, -0.31, longitudinal),
                Vec3::new(0.22, 0.06, 0.24),
                5,
            );
        }
    }
    if reversing {
        for side in [-0.58_f32, 0.58] {
            add_box(
                &mut vertices,
                &mut indices,
                transform,
                Vec3::new(side, 0.08, 1.205),
                Vec3::new(0.11, 0.08, 0.025),
                6,
            );
        }
    }
    (vertices, indices)
}

fn add_box(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    transform: VehicleTransform,
    center: Vec3,
    half: Vec3,
    material: u32,
) {
    const FACES: [(Vec3, [Vec3; 4]); 6] = [
        (
            Vec3::X,
            [
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::NEG_X,
            [
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(-1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::Y,
            [
                Vec3::new(-1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(-1.0, 1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::NEG_Y,
            [
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, -1.0),
            ],
        ),
        (
            Vec3::Z,
            [
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::NEG_Z,
            [
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
            ],
        ),
    ];
    for (normal, corners) in FACES {
        let start = vertices.len() as u32;
        let world_normal = transform.rotation.mul_vec3(normal);
        for corner in corners {
            let local = center + corner * half;
            let world = transform.position + transform.rotation.mul_vec3(local);
            vertices.push(MeshVertex {
                position: world.to_array(),
                normal: world_normal.to_array(),
                material,
                variation: 0.5,
            });
        }
        indices.extend_from_slice(&[start, start + 1, start + 2, start + 1, start + 3, start + 2]);
    }
}

fn hash01(mut value: u32) -> f32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value as f32 / u32::MAX as f32
}
