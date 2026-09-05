//! Continuous visual materials derived from the unchanged gameplay cell mask.

use glam::{Vec2, Vec3};
use lawn_core::{
    cube_map::{CubeFace, direction_to_face_uv},
    planet::{Planet, SurfaceMaterial},
};

/// A cached, padded mask shared by terrain shading and the rendered grass edge.
/// A small local filter rounds the coarse contour without moving gameplay data.
#[derive(Debug)]
pub(crate) struct TerrainSurface {
    resolution: u32,
    stride: usize,
    coverage: Vec<f32>,
}

impl TerrainSurface {
    pub(crate) fn new(planet: &Planet) -> Self {
        let mask: Vec<_> = planet
            .terrain
            .iter()
            .map(|cell| f32::from(cell.material == SurfaceMaterial::Rock))
            .collect();
        Self::from_mask(planet.terrain.resolution(), &mask)
    }

    fn from_mask(resolution: u32, mask: &[f32]) -> Self {
        // Interpolation alone still preserves the staircase in a binary mask.
        // Round its contour with a one-cell-radius tent before interpolation;
        // the fragment shader still uses a sharp 0.5 threshold, so this does
        // not blur the material itself. Constant grass/rock stays exactly 0/1.
        let mut rounded_mask = Vec::with_capacity(mask.len());
        for face in CubeFace::ALL {
            for y in 0..i64::from(resolution) {
                for x in 0..i64::from(resolution) {
                    let mut value = 0.0;
                    for (dy, wy) in [(-1, 1.0), (0, 2.0), (1, 1.0)] {
                        for (dx, wx) in [(-1, 1.0), (0, 2.0), (1, 1.0)] {
                            value += mask_tap(mask, resolution, face, x + dx, y + dy) * wx * wy;
                        }
                    }
                    rounded_mask.push(value / 16.0);
                }
            }
        }
        let stride = resolution as usize + 2;
        let mut coverage = Vec::with_capacity(6 * stride * stride);
        for face in CubeFace::ALL {
            for y in -1..=i64::from(resolution) {
                for x in -1..=i64::from(resolution) {
                    coverage.push(mask_tap(&rounded_mask, resolution, face, x, y));
                }
            }
        }
        Self {
            resolution,
            stride,
            coverage,
        }
    }

    /// Samples four cached values, with a smooth transition about cell centers.
    /// Interior values remain exactly zero or one; 0.5 is the shared rock edge.
    pub(crate) fn rock_coverage(&self, direction: Vec3) -> f32 {
        let address = direction_to_face_uv(direction);
        let coordinate =
            (address.uv * 0.5 + Vec2::splat(0.5)) * self.resolution as f32 + Vec2::splat(0.5);
        let base = coordinate.floor();
        let fraction = coordinate - base;
        let weight = fraction * fraction * (Vec2::splat(3.0) - fraction * 2.0);
        let index = address.face.index() * self.stride * self.stride
            + base.y as usize * self.stride
            + base.x as usize;
        let top = lerp(self.coverage[index], self.coverage[index + 1], weight.x);
        let bottom = lerp(
            self.coverage[index + self.stride],
            self.coverage[index + self.stride + 1],
            weight.x,
        );
        lerp(top, bottom, weight.y).clamp(0.0, 1.0)
    }
}

fn lerp(a: f32, b: f32, weight: f32) -> f32 {
    (b - a).mul_add(weight, a)
}

fn mask_cell(mask: &[f32], resolution: u32, face: CubeFace, x: i64, y: i64) -> f32 {
    mask[(face.index() * resolution as usize + y as usize) * resolution as usize + x as usize]
}

fn mask_tap(mask: &[f32], resolution: u32, face: CubeFace, x: i64, y: i64) -> f32 {
    let last = i64::from(resolution) - 1;
    let outside_x = !(0..=last).contains(&x);
    let outside_y = !(0..=last).contains(&y);
    if !outside_x && !outside_y {
        return mask_cell(mask, resolution, face, x, y);
    }
    if outside_x && outside_y {
        // Three faces meet at a cube corner. A shared average supplies the
        // missing fourth tap, giving each incident face the same corner limit.
        let corner_x = x.clamp(0, last);
        let corner_y = y.clamp(0, last);
        return (mask_cell(mask, resolution, face, corner_x, corner_y)
            + edge_cell(mask, resolution, face, x, corner_y)
            + edge_cell(mask, resolution, face, corner_x, y))
            / 3.0;
    }
    edge_cell(mask, resolution, face, x, y)
}

fn edge_cell(mask: &[f32], resolution: u32, face: CubeFace, x: i64, y: i64) -> f32 {
    let uv =
        (Vec2::new(x as f32, y as f32) + Vec2::splat(0.5)) * (2.0 / resolution as f32) - Vec2::ONE;
    let direction = face_vector(face, uv);
    let neighbor = direction_to_face_uv(direction);
    // Unfold the neighboring face. Perspective division would otherwise
    // shrink the coordinate along an edge and select a different pair of taps.
    let edge_scale = direction.abs().max_element();
    let coordinate = ((neighbor.uv * edge_scale * 0.5 + Vec2::splat(0.5)) * resolution as f32)
        .floor()
        .clamp(Vec2::ZERO, Vec2::splat((resolution - 1) as f32));
    mask_cell(
        mask,
        resolution,
        neighbor.face,
        coordinate.x as i64,
        coordinate.y as i64,
    )
}

fn face_vector(face: CubeFace, uv: Vec2) -> Vec3 {
    match face {
        CubeFace::PositiveX => Vec3::new(1.0, uv.y, -uv.x),
        CubeFace::NegativeX => Vec3::new(-1.0, uv.y, uv.x),
        CubeFace::PositiveY => Vec3::new(uv.x, 1.0, -uv.y),
        CubeFace::NegativeY => Vec3::new(uv.x, -1.0, uv.y),
        CubeFace::PositiveZ => Vec3::new(uv.x, uv.y, 1.0),
        CubeFace::NegativeZ => Vec3::new(-uv.x, uv.y, -1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lawn_core::{
        GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION,
    };

    const RESOLUTION: u32 = 8;

    fn irregular_surface() -> TerrainSurface {
        let mask: Vec<_> = (0..6 * RESOLUTION * RESOLUTION)
            .map(|index| f32::from(index.wrapping_mul(747_796_405).count_ones() % 2 == 0))
            .collect();
        TerrainSurface::from_mask(RESOLUTION, &mask)
    }

    #[test]
    fn visual_coverage_preserves_pure_regions_and_stays_bounded() {
        for material in [0.0, 1.0] {
            let surface = TerrainSurface::from_mask(
                RESOLUTION,
                &vec![material; (6 * RESOLUTION * RESOLUTION) as usize],
            );
            for face in CubeFace::ALL {
                for y in -12..=12 {
                    for x in -12..=12 {
                        let direction = face_vector(face, Vec2::new(x as f32, y as f32) / 12.0);
                        assert_eq!(surface.rock_coverage(direction), material);
                    }
                }
            }
        }
        let surface = irregular_surface();
        for face in CubeFace::ALL {
            for y in -30..=30 {
                for x in -30..=30 {
                    let direction = face_vector(face, Vec2::new(x as f32, y as f32) / 30.0);
                    assert!((0.0..=1.0).contains(&surface.rock_coverage(direction)));
                }
            }
        }
    }

    #[test]
    fn material_transition_is_continuous_between_cell_centers() {
        let mask: Vec<_> = (0..6 * RESOLUTION * RESOLUTION)
            .map(|index| f32::from(index % RESOLUTION < RESOLUTION / 2))
            .collect();
        let surface = TerrainSurface::from_mask(RESOLUTION, &mask);
        let mut previous = 1.0;
        for step in 0..=300 {
            let x = -0.375 + step as f32 * 0.0025;
            let coverage =
                surface.rock_coverage(face_vector(CubeFace::PositiveX, Vec2::new(x, 0.2)));
            assert!(coverage <= previous + 1.0e-6);
            assert!((previous - coverage).abs() < 0.02);
            previous = coverage;
        }
        assert!(previous < 1.0e-6);
        assert!((surface.rock_coverage(Vec3::X) - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn material_coverage_matches_across_cube_edges_and_corners() {
        let surface = irregular_surface();
        let epsilon = 1.0e-5;
        for face in CubeFace::ALL {
            for side in [-1.0, 1.0] {
                for along in [-1.0, -0.81, -0.37, 0.0, 0.12, 0.55, 1.0] {
                    for transpose in [false, true] {
                        let sample = |offset: f32| {
                            let uv = if transpose {
                                Vec2::new(along, side + offset)
                            } else {
                                Vec2::new(side + offset, along)
                            };
                            surface.rock_coverage(face_vector(face, uv))
                        };
                        assert!((sample(-epsilon) - sample(epsilon)).abs() < 0.001);
                    }
                }
            }
        }
        for x in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                for z in [-1.0, 1.0] {
                    let corner = Vec3::new(x, y, z);
                    let coverage = surface.rock_coverage(corner);
                    for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                        let incident = corner + corner * axis * epsilon;
                        assert!((surface.rock_coverage(incident) - coverage).abs() < 0.001);
                    }
                }
            }
        }
    }

    #[test]
    fn building_visual_surface_does_not_change_generated_world_data() {
        let mut config = GeneratorConfig::test_quality();
        config.grass_roots_per_square_meter = 1.0;
        let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config)
            .generate(WorldSeed(21))
            .unwrap();
        let original = planet.clone();
        let surface = TerrainSurface::new(&planet);
        for root in &planet.grass_roots {
            assert!((0.0..=1.0).contains(&surface.rock_coverage(Vec3::from_array(root.position))));
        }
        assert_eq!(planet.deterministic_hash, original.deterministic_hash);
        assert_eq!(planet.terrain, original.terrain);
        assert_eq!(planet.grass_roots, original.grass_roots);
        assert_eq!(planet.grass_patches, original.grass_patches);
        assert_eq!(planet.spawn, original.spawn);
    }
}
