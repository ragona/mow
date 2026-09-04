use bytemuck::{Pod, Zeroable};
use glam::{Vec2, Vec3};
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
pub const VEHICLE_VERTEX_BUFFER_SIZE: u64 = 32 * 1024;
pub const VEHICLE_INDEX_BUFFER_SIZE: u64 = 16 * 1024;

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
    chassis_transform: VehicleTransform,
    deck_transform: VehicleTransform,
    mower_enabled: bool,
    elapsed_seconds: f32,
) -> (Vec<MeshVertex>, Vec<u32>) {
    const HOVER_PADS: [(f32, f32); 4] =
        [(-0.72, -0.72), (0.72, -0.72), (-0.72, 0.72), (0.72, 0.72)];

    let mut vertices = Vec::with_capacity(720);
    let mut indices = Vec::with_capacity(1_800);

    // A dark lower skirt makes the orange shell read as a separate, floating
    // chassis instead of one monolithic cube.
    add_chamfered_frustum(
        &mut vertices,
        &mut indices,
        chassis_transform,
        Vec3::new(0.0, -0.08, 0.0),
        Vec2::splat(0.82),
        Vec2::splat(0.74),
        0.2,
        0.2,
        4,
    );
    add_chamfered_frustum(
        &mut vertices,
        &mut indices,
        chassis_transform,
        Vec3::new(0.0, 0.18, 0.0),
        Vec2::splat(0.79),
        Vec2::splat(0.65),
        0.27,
        0.19,
        2,
    );
    add_chamfered_frustum(
        &mut vertices,
        &mut indices,
        chassis_transform,
        Vec3::new(0.0, 0.5, 0.0),
        Vec2::splat(0.57),
        Vec2::splat(0.42),
        0.16,
        0.14,
        3,
    );
    add_cylinder(
        &mut vertices,
        &mut indices,
        chassis_transform,
        Vec3::new(0.0, 0.685, 0.0),
        0.29,
        0.035,
        12,
        2,
    );

    // The omnidirectional mower deck sits directly beneath the chassis.
    add_cylinder(
        &mut vertices,
        &mut indices,
        deck_transform,
        Vec3::new(
            0.0,
            -0.5 + if mower_enabled {
                (elapsed_seconds * 57.0).sin() * 0.018
            } else {
                0.0
            },
            0.0,
        ),
        1.04,
        0.055,
        14,
        if mower_enabled { 7 } else { 4 },
    );

    for (index, (side, longitudinal)) in HOVER_PADS.into_iter().enumerate() {
        // Short outriggers visually connect each independently readable pad to
        // the central chassis without privileging a front direction.
        add_box(
            &mut vertices,
            &mut indices,
            chassis_transform,
            Vec3::new(side * 0.73, -0.13, longitudinal),
            Vec3::new(0.24, 0.065, 0.09),
            4,
        );
        add_cylinder(
            &mut vertices,
            &mut indices,
            chassis_transform,
            Vec3::new(side, -0.22, longitudinal),
            0.31,
            0.115,
            10,
            4,
        );
        let pulse = 1.0 + (elapsed_seconds * 5.0 + index as f32 * 1.7).sin() * 0.025;
        add_cylinder(
            &mut vertices,
            &mut indices,
            chassis_transform,
            Vec3::new(side, -0.355, longitudinal),
            0.25 * pulse,
            0.055,
            10,
            5,
        );
    }
    (vertices, indices)
}

#[allow(clippy::too_many_arguments)]
fn add_chamfered_frustum(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    transform: VehicleTransform,
    center: Vec3,
    bottom_half: Vec2,
    top_half: Vec2,
    half_height: f32,
    chamfer: f32,
    material: u32,
) {
    let bottom = chamfered_ring(bottom_half, chamfer);
    let top = chamfered_ring(top_half, chamfer.min(top_half.min_element() * 0.8));
    for index in 0..8 {
        let next = (index + 1) % 8;
        let bottom_left = center + Vec3::new(bottom[index].x, -half_height, bottom[index].y);
        let bottom_right = center + Vec3::new(bottom[next].x, -half_height, bottom[next].y);
        let top_left = center + Vec3::new(top[index].x, half_height, top[index].y);
        let top_right = center + Vec3::new(top[next].x, half_height, top[next].y);
        let normal = (top_left - bottom_left)
            .cross(bottom_right - bottom_left)
            .normalize();
        let start = vertices.len() as u32;
        for local in [bottom_left, bottom_right, top_left, top_right] {
            push_model_vertex(vertices, transform, local, normal, material);
        }
        indices.extend_from_slice(&[start, start + 1, start + 2, start + 1, start + 3, start + 2]);
    }
    add_ring_cap(
        vertices,
        indices,
        transform,
        center + Vec3::Y * half_height,
        &top,
        Vec3::Y,
        material,
    );
    add_ring_cap(
        vertices,
        indices,
        transform,
        center - Vec3::Y * half_height,
        &bottom,
        Vec3::NEG_Y,
        material,
    );
}

#[allow(clippy::too_many_arguments)]
fn add_cylinder(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    transform: VehicleTransform,
    center: Vec3,
    radius: f32,
    half_height: f32,
    segments: usize,
    material: u32,
) {
    debug_assert!(segments >= 3);
    let side_start = vertices.len() as u32;
    let mut ring = Vec::with_capacity(segments);
    for index in 0..segments {
        let angle = index as f32 * std::f32::consts::TAU / segments as f32;
        let radial = Vec3::new(angle.cos(), 0.0, angle.sin());
        let offset = radial * radius;
        ring.push(Vec2::new(offset.x, offset.z));
        push_model_vertex(
            vertices,
            transform,
            center + offset - Vec3::Y * half_height,
            radial,
            material,
        );
        push_model_vertex(
            vertices,
            transform,
            center + offset + Vec3::Y * half_height,
            radial,
            material,
        );
    }
    for index in 0..segments {
        let next = (index + 1) % segments;
        let bottom = side_start + (index * 2) as u32;
        let top = bottom + 1;
        let next_bottom = side_start + (next * 2) as u32;
        let next_top = next_bottom + 1;
        indices.extend_from_slice(&[bottom, next_bottom, top, next_bottom, next_top, top]);
    }
    add_ring_cap(
        vertices,
        indices,
        transform,
        center + Vec3::Y * half_height,
        &ring,
        Vec3::Y,
        material,
    );
    add_ring_cap(
        vertices,
        indices,
        transform,
        center - Vec3::Y * half_height,
        &ring,
        Vec3::NEG_Y,
        material,
    );
}

fn chamfered_ring(half: Vec2, chamfer: f32) -> [Vec2; 8] {
    let chamfer = chamfer.clamp(0.0, half.min_element());
    [
        Vec2::new(-half.x + chamfer, -half.y),
        Vec2::new(half.x - chamfer, -half.y),
        Vec2::new(half.x, -half.y + chamfer),
        Vec2::new(half.x, half.y - chamfer),
        Vec2::new(half.x - chamfer, half.y),
        Vec2::new(-half.x + chamfer, half.y),
        Vec2::new(-half.x, half.y - chamfer),
        Vec2::new(-half.x, -half.y + chamfer),
    ]
}

fn add_ring_cap(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    transform: VehicleTransform,
    center: Vec3,
    ring: &[Vec2],
    normal: Vec3,
    material: u32,
) {
    let start = vertices.len() as u32;
    push_model_vertex(vertices, transform, center, normal, material);
    for point in ring {
        push_model_vertex(
            vertices,
            transform,
            center + Vec3::new(point.x, 0.0, point.y),
            normal,
            material,
        );
    }
    for index in 0..ring.len() {
        let current = start + 1 + index as u32;
        let next = start + 1 + ((index + 1) % ring.len()) as u32;
        if normal.y > 0.0 {
            indices.extend_from_slice(&[start, current, next]);
        } else {
            indices.extend_from_slice(&[start, next, current]);
        }
    }
}

fn push_model_vertex(
    vertices: &mut Vec<MeshVertex>,
    transform: VehicleTransform,
    local: Vec3,
    normal: Vec3,
    material: u32,
) {
    let world = transform.position + transform.rotation.mul_vec3(local);
    let world_normal = transform.rotation.mul_vec3(normal).normalize();
    vertices.push(MeshVertex {
        position: world.to_array(),
        normal: world_normal.to_array(),
        material,
        variation: 0.5,
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    fn identity_transform() -> VehicleTransform {
        VehicleTransform {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            forward: Vec3::NEG_Z,
            up: Vec3::Y,
        }
    }

    #[test]
    fn vehicle_mesh_exposes_four_glowing_pads_within_gpu_budget() {
        let transform = identity_transform();
        let (vertices, indices) = build_vehicle(transform, transform, true, 0.0);
        assert!(vertices.len() > 600, "vehicle mesh is unexpectedly simple");
        assert!(
            vertices.len() * std::mem::size_of::<MeshVertex>()
                <= VEHICLE_VERTEX_BUFFER_SIZE as usize
        );
        assert!(indices.len() * std::mem::size_of::<u32>() <= VEHICLE_INDEX_BUFFER_SIZE as usize);
        assert!(vertices.iter().all(|vertex| {
            let normal = Vec3::from_array(vertex.normal);
            normal.is_finite() && (normal.length() - 1.0).abs() < 1.0e-4
        }));

        let glowing_pad_quadrants =
            vertices
                .iter()
                .filter(|vertex| vertex.material == 5)
                .fold(0_u8, |mask, vertex| {
                    let x = usize::from(vertex.position[0] >= 0.0);
                    let z = usize::from(vertex.position[2] >= 0.0);
                    mask | (1 << (x * 2 + z))
                });
        assert_eq!(glowing_pad_quadrants, 0b1111);
        assert!(vertices.iter().any(|vertex| vertex.material == 7));
    }
}
