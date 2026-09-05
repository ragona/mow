use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Quat, Vec2, Vec3};
use lawn_core::{cube_map::CubeFace, planet::Planet, vehicle::VehicleTransform};

use crate::surface::TerrainSurface;

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub material: u32,
    pub variation: f32,
    pub detail: [f32; 4],
}

impl MeshVertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Uint32,
        3 => Float32,
        4 => Float32x4
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

pub const VEHICLE_VERTEX_BUFFER_SIZE: u64 = 48 * 1024;
pub const VEHICLE_INDEX_BUFFER_SIZE: u64 = 16 * 1024;

pub fn build_terrain(planet: &Planet, surface: &TerrainSurface) -> (Vec<MeshVertex>, Vec<u32>) {
    let resolution = terrain_render_resolution(planet.terrain.resolution());
    let stride = resolution + 1;
    // Weld all twelve cube edges and eight corners. Integer lattice keys avoid
    // rounding differences between reversed face coordinates, including odd
    // resolutions. Only the boundary needs hashing; the interior stays linear.
    let mut boundary_vertices = HashMap::with_capacity((12 * resolution) as usize);
    let mut face_vertices = vec![0; (stride * stride) as usize];
    let mut vertices = Vec::with_capacity((6 * resolution * resolution + 2) as usize);
    let mut indices = Vec::with_capacity((resolution * resolution * 6 * 6) as usize);
    for face in CubeFace::ALL {
        for y in 0..=resolution {
            for x in 0..=resolution {
                let lattice = terrain_lattice(face, x, y, resolution);
                let boundary = x == 0 || y == 0 || x == resolution || y == resolution;
                let existing = if boundary {
                    boundary_vertices.get(&lattice).copied()
                } else {
                    None
                };
                let index = existing.unwrap_or_else(|| {
                    let direction = Vec3::from_array(lattice.map(|value| value as f32)).normalize();
                    let index = vertices.len() as u32;
                    let coverage = surface.rock_coverage(direction);
                    let (deformation, detail) = surface.meteor_detail(direction, coverage);
                    let undeformed = planet.surface_point(direction);
                    // Valid custom rolling terrain can approach zero radius.
                    // Keep a positive shell even in an unusually deep valley.
                    let deformation = deformation.max(-undeformed.length() * 0.20);
                    vertices.push(MeshVertex {
                        position: (undeformed + direction * deformation).to_array(),
                        normal: [0.0; 3],
                        material: 0,
                        variation: coverage,
                        detail,
                    });
                    if boundary {
                        boundary_vertices.insert(lattice, index);
                    }
                    index
                });
                face_vertices[(y * stride + x) as usize] = index;
            }
        }
        for y in 0..resolution {
            for x in 0..resolution {
                let offset = (y * stride + x) as usize;
                let i0 = face_vertices[offset];
                let i1 = face_vertices[offset + 1];
                let i2 = face_vertices[offset + stride as usize];
                let i3 = face_vertices[offset + stride as usize + 1];
                indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
            }
        }
    }

    // Area-weighted normals describe the actual rendered triangles, rather
    // than the nearest simulation cell. Shared edge vertices receive both
    // faces' contributions without any additional analytic terrain samples.
    for triangle in indices.chunks_exact(3) {
        let a = Vec3::from_array(vertices[triangle[0] as usize].position);
        let b = Vec3::from_array(vertices[triangle[1] as usize].position);
        let c = Vec3::from_array(vertices[triangle[2] as usize].position);
        let weighted_normal = (b - a).cross(c - a);
        for &index in triangle {
            let vertex = &mut vertices[index as usize];
            vertex.normal = (Vec3::from_array(vertex.normal) + weighted_normal).to_array();
        }
    }
    for vertex in &mut vertices {
        let radial = Vec3::from_array(vertex.position).normalize();
        let mut normal = Vec3::from_array(vertex.normal)
            .try_normalize()
            .unwrap_or(radial);
        if normal.dot(radial) < 0.0 {
            normal = -normal;
        }
        vertex.normal = normal.to_array();
    }
    (vertices, indices)
}

fn terrain_render_resolution(simulation_resolution: u32) -> u32 {
    // Refine shipping terrain while leaving already-dense editor worlds at
    // their original resolution instead of multiplying their upload budget.
    simulation_resolution.max(simulation_resolution.saturating_mul(2).min(128))
}

fn terrain_lattice(face: CubeFace, x: u32, y: u32, resolution: u32) -> [i64; 3] {
    let extent = i64::from(resolution);
    let u = i64::from(x) * 2 - extent;
    let v = i64::from(y) * 2 - extent;
    match face {
        CubeFace::PositiveX => [extent, v, -u],
        CubeFace::NegativeX => [-extent, v, u],
        CubeFace::PositiveY => [u, extent, -v],
        CubeFace::NegativeY => [u, -extent, v],
        CubeFace::PositiveZ => [u, v, extent],
        CubeFace::NegativeZ => [-u, v, -extent],
    }
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
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    chassis_transform: VehicleTransform,
    deck_transform: VehicleTransform,
    mower_enabled: bool,
    elapsed_seconds: f32,
) {
    const HOVER_PADS: [(f32, f32); 4] =
        [(-0.72, -0.72), (0.72, -0.72), (-0.72, 0.72), (0.72, 0.72)];

    vertices.clear();
    indices.clear();

    // A dark rubber skirt, a broad enamel shoulder, and a rounded cream canopy
    // give the little mower distinct, tactile layers even at gameplay distance.
    add_chamfered_frustum(
        vertices,
        indices,
        chassis_transform,
        Vec3::new(0.0, -0.08, 0.0),
        Vec2::splat(0.82),
        Vec2::splat(0.74),
        0.2,
        0.2,
        4,
    );
    add_chamfered_frustum(
        vertices,
        indices,
        chassis_transform,
        Vec3::new(0.0, 0.12, 0.0),
        Vec2::splat(0.78),
        Vec2::splat(0.83),
        0.17,
        0.28,
        2,
    );
    add_chamfered_frustum(
        vertices,
        indices,
        chassis_transform,
        Vec3::new(0.0, 0.345, 0.0),
        Vec2::splat(0.83),
        Vec2::splat(0.65),
        0.055,
        0.28,
        2,
    );
    add_ellipsoid(
        vertices,
        indices,
        chassis_transform,
        Vec3::new(0.0, 0.53, 0.0),
        Vec3::new(0.55, 0.30, 0.55),
        3,
    );
    add_cylinder(
        vertices,
        indices,
        chassis_transform,
        Vec3::new(0.0, 0.835, 0.0),
        0.19,
        0.025,
        10,
        2,
    );

    // The antenna bends against the springy chassis lean. Its small idle sway
    // uses simulation time, so the whole silhouette holds still when paused.
    let local_deck_up = chassis_transform.rotation.inverse() * deck_transform.up;
    let antenna_sway = Vec3::new(local_deck_up.x, 0.0, local_deck_up.z) * 0.42
        + Vec3::new(
            (elapsed_seconds * 3.6).sin(),
            0.0,
            (elapsed_seconds * 2.9).cos(),
        ) * 0.013;
    let antenna_base = Vec3::new(0.36, 0.64, 0.22);
    let antenna_mid = antenna_base + Vec3::Y * 0.24 + antenna_sway * 0.3;
    let antenna_tip = antenna_base + Vec3::Y * 0.49 + antenna_sway;
    for (from, to) in [(antenna_base, antenna_mid), (antenna_mid, antenna_tip)] {
        let stem_rotation = Quat::from_rotation_arc(Vec3::Y, (to - from).normalize());
        let stem_transform = VehicleTransform {
            position: chassis_transform.position + chassis_transform.rotation * (from + to) * 0.5,
            rotation: chassis_transform.rotation * stem_rotation,
            ..chassis_transform
        };
        add_cylinder(
            vertices,
            indices,
            stem_transform,
            Vec3::ZERO,
            0.026,
            from.distance(to) * 0.5,
            6,
            4,
        );
    }
    add_ellipsoid(
        vertices,
        indices,
        chassis_transform,
        antenna_tip,
        Vec3::splat(0.095),
        3,
    );

    // The omnidirectional mower deck sits directly beneath the chassis.
    add_cylinder(
        vertices,
        indices,
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
            vertices,
            indices,
            chassis_transform,
            Vec3::new(side * 0.73, -0.13, longitudinal),
            Vec3::new(0.24, 0.065, 0.09),
            4,
        );
        add_cylinder(
            vertices,
            indices,
            chassis_transform,
            Vec3::new(side, -0.22, longitudinal),
            0.31,
            0.115,
            10,
            4,
        );
        let pulse = 1.0 + (elapsed_seconds * 5.0 + index as f32 * 1.7).sin() * 0.025;
        add_cylinder(
            vertices,
            indices,
            chassis_transform,
            Vec3::new(side, -0.355, longitudinal),
            0.25 * pulse,
            0.055,
            10,
            5,
        );
    }
}

/// A small smooth shape with shared ring vertices and separate pole fans. The
/// poles avoid zero-area triangles, which also keeps the shadow pass clean.
fn add_ellipsoid(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    transform: VehicleTransform,
    center: Vec3,
    radii: Vec3,
    material: u32,
) {
    const SEGMENTS: u32 = 12;
    const RINGS: u32 = 5;
    let start = vertices.len() as u32;
    push_model_vertex(
        vertices,
        transform,
        center + Vec3::Y * radii.y,
        Vec3::Y,
        material,
    );
    for ring in 1..=RINGS {
        let latitude = std::f32::consts::PI * ring as f32 / (RINGS + 1) as f32;
        let (latitude_sin, latitude_cos) = latitude.sin_cos();
        for segment in 0..SEGMENTS {
            let angle = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
            let (sine, cosine) = angle.sin_cos();
            let sphere = Vec3::new(latitude_sin * cosine, latitude_cos, latitude_sin * sine);
            push_model_vertex(
                vertices,
                transform,
                center + sphere * radii,
                (sphere / radii).normalize(),
                material,
            );
        }
    }
    let bottom = vertices.len() as u32;
    push_model_vertex(
        vertices,
        transform,
        center - Vec3::Y * radii.y,
        Vec3::NEG_Y,
        material,
    );
    for segment in 0..SEGMENTS {
        let next = (segment + 1) % SEGMENTS;
        indices.extend_from_slice(&[start, start + 1 + next, start + 1 + segment]);
        for ring in 0..RINGS - 1 {
            let row = start + 1 + ring * SEGMENTS;
            let upper = row + segment;
            let upper_next = row + next;
            let lower = upper + SEGMENTS;
            let lower_next = upper_next + SEGMENTS;
            indices.extend_from_slice(&[upper, upper_next, lower, upper_next, lower_next, lower]);
        }
        let last_row = start + 1 + (RINGS - 1) * SEGMENTS;
        indices.extend_from_slice(&[bottom, last_row + segment, last_row + next]);
    }
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
        indices.extend_from_slice(&[start, start + 2, start + 1, start + 1, start + 2, start + 3]);
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
    // All shipping cylinders have at most fourteen sides. Stack storage avoids
    // ten short-lived ring allocations on every rendered frame.
    let mut ring = [Vec2::ZERO; 14];
    let ring = &mut ring[..segments];
    for (index, point) in ring.iter_mut().enumerate() {
        let angle = index as f32 * std::f32::consts::TAU / segments as f32;
        let radial = Vec3::new(angle.cos(), 0.0, angle.sin());
        let offset = radial * radius;
        *point = Vec2::new(offset.x, offset.z);
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
        indices.extend_from_slice(&[bottom, top, next_bottom, next_bottom, top, next_top]);
    }
    add_ring_cap(
        vertices,
        indices,
        transform,
        center + Vec3::Y * half_height,
        ring,
        Vec3::Y,
        material,
    );
    add_ring_cap(
        vertices,
        indices,
        transform,
        center - Vec3::Y * half_height,
        ring,
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
            indices.extend_from_slice(&[start, next, current]);
        } else {
            indices.extend_from_slice(&[start, current, next]);
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
        detail: [0.0; 4],
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
                detail: [0.0; 4],
            });
        }
        indices.extend_from_slice(&[start, start + 2, start + 1, start + 1, start + 2, start + 3]);
    }
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
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        build_vehicle(&mut vertices, &mut indices, transform, transform, true, 0.0);
        assert!(vertices.len() > 600, "vehicle mesh is unexpectedly simple");
        assert!(
            vertices.len() * std::mem::size_of::<MeshVertex>()
                <= VEHICLE_VERTEX_BUFFER_SIZE as usize
        );
        assert!(indices.len() * std::mem::size_of::<u32>() <= VEHICLE_INDEX_BUFFER_SIZE as usize);
        assert!(vertices.iter().all(|vertex| {
            let normal = Vec3::from_array(vertex.normal);
            normal.is_finite()
                && (normal.length() - 1.0).abs() < 1.0e-4
                && vertex.detail == [0.0; 4]
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

    fn assert_outward_triangles(vertices: &[MeshVertex], indices: &[u32]) {
        for triangle in indices.chunks_exact(3) {
            let [a, b, c] = triangle.try_into().unwrap();
            let a = vertices[a as usize];
            let b = vertices[b as usize];
            let c = vertices[c as usize];
            let geometric = (Vec3::from_array(b.position) - Vec3::from_array(a.position))
                .cross(Vec3::from_array(c.position) - Vec3::from_array(a.position));
            let normal = Vec3::from_array(a.normal)
                + Vec3::from_array(b.normal)
                + Vec3::from_array(c.normal);
            assert!(
                geometric.dot(normal) > 0.0,
                "inward or degenerate triangle: {triangle:?}"
            );
        }
    }

    #[test]
    fn solid_meshes_face_outward_for_backface_culling_and_shadows() {
        let planet = lawn_core::PlanetGenerator::new(
            lawn_core::planet::CURRENT_GENERATOR_VERSION,
            lawn_core::GeneratorConfig::test_quality(),
        )
        .generate_with_roots(lawn_core::WorldSeed(21), false)
        .unwrap();
        let (terrain, terrain_indices) = build_terrain(&planet, &TerrainSurface::new(&planet));
        assert_outward_triangles(&terrain, &terrain_indices);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let transform = identity_transform();
        build_vehicle(&mut vertices, &mut indices, transform, transform, true, 0.0);
        assert_outward_triangles(&vertices, &indices);
    }

    #[test]
    fn terrain_is_a_closed_surface_with_smooth_shared_seams() {
        let mut config = lawn_core::GeneratorConfig::test_quality();
        config.grass_roots_per_square_meter = 1.0;
        let planet =
            lawn_core::PlanetGenerator::new(lawn_core::planet::CURRENT_GENERATOR_VERSION, config)
                .generate(lawn_core::WorldSeed(21))
                .unwrap();
        let before = planet.clone();
        let surface = TerrainSurface::new(&planet);
        let (vertices, indices) = build_terrain(&planet, &surface);
        let resolution = terrain_render_resolution(planet.terrain.resolution()) as usize;
        assert_eq!(vertices.len(), 6 * resolution * resolution + 2);
        assert_eq!(indices.len(), 36 * resolution * resolution);
        assert_eq!(std::mem::size_of::<MeshVertex>(), 48);

        let mut positions = std::collections::HashSet::with_capacity(vertices.len());
        for vertex in &vertices {
            let position = Vec3::from_array(vertex.position);
            let direction = position.normalize();
            let normal = Vec3::from_array(vertex.normal);
            assert!(position.is_finite());
            assert!(normal.is_finite() && (normal.length() - 1.0).abs() < 1.0e-5);
            assert!(normal.dot(direction) > 0.0);
            assert_eq!(vertex.material, 0);
            assert!((0.0..=1.0).contains(&vertex.variation));
            assert!((vertex.variation - surface.rock_coverage(direction)).abs() < 1.0e-4);
            let displacement = position.length() - planet.surface_radius(direction);
            assert!((-0.3601..=0.0701).contains(&displacement));
            assert!(
                vertex
                    .detail
                    .into_iter()
                    .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
            );
            if vertex.variation <= 0.75 {
                // Reconstructing the direction from rounded positions adds a
                // little error to the procedural height resample.
                assert!(
                    displacement.abs() < 1.0e-4,
                    "craters moved the grass surface: {displacement}"
                );
                assert_eq!(vertex.detail, [0.0; 4]);
            }
            assert!(
                positions.insert(vertex.position.map(f32::to_bits)),
                "unwelded vertex"
            );
        }

        // Every edge must have exactly two oppositely directed neighbors,
        // including all twelve cube seams and the eight three-face corners.
        let mut edges = HashMap::<(u32, u32), (u32, i32)>::new();
        for triangle in indices.chunks_exact(3) {
            let a = Vec3::from_array(vertices[triangle[0] as usize].position);
            let b = Vec3::from_array(vertices[triangle[1] as usize].position);
            let c = Vec3::from_array(vertices[triangle[2] as usize].position);
            let geometric = (b - a).cross(c - a);
            assert!(geometric.dot(a + b + c) > 0.0);
            for corner in 0..3 {
                let from = triangle[corner];
                let to = triangle[(corner + 1) % 3];
                let edge = edges.entry((from.min(to), from.max(to))).or_default();
                edge.0 += 1;
                edge.1 += if from < to { 1 } else { -1 };
            }
        }
        assert!(
            edges
                .values()
                .all(|&(count, orientation)| count == 2 && orientation == 0)
        );

        // Rendering consumes generated data without changing the simulation,
        // deterministic seed results, root distribution, or saved-world data.
        assert!(!planet.grass_roots.is_empty());
        assert_eq!(planet.terrain, before.terrain);
        assert_eq!(planet.grass_roots, before.grass_roots);
        assert_eq!(planet.grass_patches, before.grass_patches);
        assert_eq!(planet.config, before.config);
        assert_eq!(planet.mountains, before.mountains);
        assert_eq!(planet.spawn, before.spawn);
        assert_eq!(planet.validation, before.validation);
        assert_eq!(planet.deterministic_hash, before.deterministic_hash);

        // Nearest-cell normals are a gameplay approximation and must no
        // longer introduce shading steps into the refined render geometry.
        let mut different_cell_normals = planet;
        for cell in different_cell_normals.terrain.iter_mut() {
            cell.normal = Vec3::NEG_Y;
        }
        let (resampled, _) = build_terrain(&different_cell_normals, &surface);
        for (vertex, resampled_vertex) in vertices.iter().zip(resampled) {
            assert_eq!(vertex.normal, resampled_vertex.normal);
        }
    }

    #[test]
    fn cube_lattice_welds_reversed_edges_at_odd_and_even_resolutions() {
        for resolution in [17, 48, 128, 129] {
            let mut boundaries = HashMap::<[i64; 3], u32>::new();
            for face in CubeFace::ALL {
                for y in 0..=resolution {
                    for x in 0..=resolution {
                        if x == 0 || y == 0 || x == resolution || y == resolution {
                            *boundaries
                                .entry(terrain_lattice(face, x, y, resolution))
                                .or_default() += 1;
                        }
                    }
                }
            }
            assert_eq!(boundaries.len(), (12 * (resolution - 1) + 8) as usize);
            assert_eq!(boundaries.values().filter(|&&count| count == 3).count(), 8);
            assert!(boundaries.values().all(|&count| count == 2 || count == 3));
        }
    }

    #[test]
    fn terrain_refinement_has_bounded_upload_cost_for_dense_editor_worlds() {
        for (simulation, render) in [
            (8, 16),
            (24, 48),
            (64, 128),
            (96, 128),
            (129, 129),
            (512, 512),
        ] {
            assert_eq!(terrain_render_resolution(simulation), render);
        }
        let shipping_resolution =
            terrain_render_resolution(lawn_core::GeneratorConfig::default().terrain_resolution)
                as usize;
        let vertex_bytes =
            (6 * shipping_resolution * shipping_resolution + 2) * std::mem::size_of::<MeshVertex>();
        let index_bytes =
            36 * shipping_resolution * shipping_resolution * std::mem::size_of::<u32>();
        assert!(vertex_bytes + index_bytes < 7 * 1024 * 1024);
    }

    #[test]
    fn vehicle_animation_reuses_storage_and_preserves_topology() {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let initial = identity_transform();
        build_vehicle(&mut vertices, &mut indices, initial, initial, true, 0.0);
        let topology = indices.clone();
        let pointers = (vertices.as_ptr(), indices.as_ptr());
        let mut animated = initial;
        animated.position = Vec3::new(4.0, 7.0, -2.0);
        animated.rotation = Quat::from_rotation_x(0.2);
        for enabled in [true, false] {
            build_vehicle(&mut vertices, &mut indices, animated, initial, enabled, 4.5);
            assert_eq!(indices, topology);
            assert_eq!((vertices.as_ptr(), indices.as_ptr()), pointers);
            assert_outward_triangles(&vertices, &indices);
        }
    }
}
