//! Immutable bounds for grass visibility. Root data and patch ordering remain
//! untouched; dynamic blade growth and bending only add scalar padding.

use glam::Vec3;
use lawn_core::{Planet, planet::GrassRootGpu};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GrassPatchBounds {
    pub center: Vec3,
    pub half_extents: Vec3,
    /// Root sphere around `center`, before adding `blade_extent`.
    pub radius: f32,
    /// Maximum root distance from the planet origin, before blade padding.
    pub max_radius: f32,
}

impl GrassPatchBounds {
    fn from_roots(roots: &[GrassRootGpu]) -> Self {
        let Some(first) = roots.first() else {
            return Self::default();
        };
        let first = Vec3::from_array(first.position);
        let mut minimum = first;
        let mut maximum = first;
        let mut maximum_radius_squared = first.length_squared();
        for root in &roots[1..] {
            let position = Vec3::from_array(root.position);
            minimum = minimum.min(position);
            maximum = maximum.max(position);
            let radius_squared = position.length_squared();
            maximum_radius_squared = maximum_radius_squared.max(radius_squared);
        }
        let center = minimum + (maximum - minimum) * 0.5;
        let maximum_radius = maximum_radius_squared.sqrt();
        // Cover float rounding when centers, lengths and GPU vertex positions
        // are evaluated in a different order, including unusually small worlds.
        let padding = 8.0 * f32::EPSILON * maximum_radius.max(1.0);
        let half_extents = (maximum - center).max(center - minimum) + Vec3::splat(padding);
        let radius_squared = roots
            .iter()
            .map(|root| Vec3::from_array(root.position).distance_squared(center))
            .fold(0.0_f32, f32::max);
        Self {
            center,
            half_extents,
            radius: radius_squared.sqrt() + padding,
            max_radius: maximum_radius + padding,
        }
    }
}

/// One bound per original patch, including an empty bound for an empty patch.
pub(crate) fn prepare(planet: &Planet) -> Vec<GrassPatchBounds> {
    planet
        .grass_patches
        .iter()
        .map(|patch| {
            GrassPatchBounds::from_roots(
                &planet.grass_roots[patch.roots.start as usize..patch.roots.end as usize],
            )
        })
        .collect()
}

/// Maximum distance from a root to any tuft vertex in `grass.wgsl`.
/// Bending preserves the blade's length; the mesh's local horizontal offset
/// adds width and curvature independently of wind, combing or rotor wash.
pub(crate) fn blade_extent(height_scale: f32) -> f32 {
    let height_scale = height_scale.max(0.1);
    let maximum_height = 0.63 * height_scale;
    let maximum_horizontal = 0.045_f32.hypot(0.07) * 1.42 * (0.45 + 0.55 * height_scale.sqrt());
    maximum_height + maximum_horizontal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::build_tuft;
    use lawn_core::{
        GeneratorConfig, PlanetGenerator, WorldSeed, cube_map::tangent_frame,
        planet::CURRENT_GENERATOR_VERSION,
    };

    fn planet(seed: u64) -> Planet {
        PlanetGenerator::new(
            CURRENT_GENERATOR_VERSION,
            GeneratorConfig {
                grass_roots_per_square_meter: 1.5,
                ..GeneratorConfig::test_quality()
            },
        )
        .generate(WorldSeed(seed))
        .unwrap()
    }

    #[test]
    fn cached_bounds_enclose_every_generated_root_without_mutating_data() {
        for seed in [21, 42] {
            let planet = planet(seed);
            let original_roots = planet.grass_roots.clone();
            let original_patches = planet.grass_patches.clone();
            let bounds = prepare(&planet);
            assert_eq!(bounds.len(), planet.grass_patches.len());
            let mut checked_roots = 0;
            for (patch, bound) in planet.grass_patches.iter().zip(bounds) {
                assert!(bound.center.is_finite());
                assert!(bound.half_extents.is_finite());
                assert!(bound.radius.is_finite());
                assert!(bound.max_radius >= 0.0);
                for root in
                    &planet.grass_roots[patch.roots.start as usize..patch.roots.end as usize]
                {
                    let position = Vec3::from_array(root.position);
                    assert!(
                        (position - bound.center)
                            .abs()
                            .cmple(bound.half_extents)
                            .all()
                    );
                    assert!(position.distance(bound.center) <= bound.radius);
                    assert!(position.length() <= bound.max_radius);
                    checked_roots += 1;
                }
            }
            assert_eq!(checked_roots, planet.grass_roots.len());
            assert!(checked_roots > 1000);
            assert_eq!(planet.grass_roots, original_roots);
            assert_eq!(planet.grass_patches, original_patches);
        }
    }

    #[test]
    fn empty_and_single_root_bounds_remain_finite() {
        let empty = GrassPatchBounds::from_roots(&[]);
        assert_eq!(empty.center, Vec3::ZERO);
        assert_eq!(empty.half_extents, Vec3::ZERO);
        assert_eq!(empty.radius, 0.0);
        assert_eq!(empty.max_radius, 0.0);
        for scale in [0.001, 1.0, 1000.0] {
            let position = Vec3::new(-0.01, 0.006, 0.003) * scale;
            let bound = GrassPatchBounds::from_roots(&[GrassRootGpu {
                position: position.to_array(),
                packed_normal_seed: 0,
            }]);
            assert_eq!(bound.center, position);
            assert!(bound.half_extents.cmpgt(Vec3::ZERO).all());
            assert!(bound.radius > 0.0);
            assert!(position.length() <= bound.max_radius);
        }
    }

    #[test]
    fn actual_tuft_vertices_stay_inside_padded_bounds_under_extreme_bending() {
        let planet = planet(21);
        let bounds = prepare(&planet);
        let (vertices, _) = build_tuft();
        for (patch, bound) in planet.grass_patches.iter().zip(bounds) {
            for root in &planet.grass_roots[patch.roots.start as usize..patch.roots.end as usize] {
                let position = Vec3::from_array(root.position);
                let normal = decode_normal(root.packed_normal_seed);
                let (tangent, bitangent) = tangent_frame(normal);
                for height_scale in [0.1_f32, 2.25, 3.6] {
                    let extent = blade_extent(height_scale);
                    let height = 0.63 * height_scale;
                    let width = 1.42 * (0.45 + 0.55 * height_scale.sqrt());
                    // Both signs, both tangent axes, and saturation far beyond
                    // the interaction limit exercise the shader's length cap.
                    for requested_bend in [
                        Vec3::ZERO,
                        tangent * 0.01,
                        -bitangent * 0.5,
                        (tangent + bitangent) * 1000.0,
                        (bitangent - tangent) * 1000.0,
                    ] {
                        let bend_limit = height * 0.94;
                        let bounded_bend = requested_bend * bend_limit
                            / (bend_limit * bend_limit + requested_bend.length_squared()).sqrt();
                        for vertex in &vertices {
                            let local = Vec3::from_array(vertex.local_position);
                            let horizontal = (tangent * local.x + bitangent * local.z) * width;
                            let bend = bounded_bend * local.y * local.y;
                            let upright = local.y * height;
                            let bent_height =
                                (upright * upright - bend.length_squared()).max(0.0).sqrt();
                            let offset = normal * bent_height + horizontal + bend;
                            let vertex = position + offset;
                            assert!(offset.length() <= extent);
                            assert!(vertex.distance(bound.center) <= bound.radius + extent);
                            assert!(
                                (vertex - bound.center)
                                    .abs()
                                    .cmple(bound.half_extents + Vec3::splat(extent))
                                    .all()
                            );
                            assert!(vertex.length() <= bound.max_radius + extent);
                        }
                    }
                }
            }
        }
    }

    fn decode_normal(packed: u32) -> Vec3 {
        let x = (packed & 0x3ff) as f32 / 1023.0 * 2.0 - 1.0;
        let y = ((packed >> 10) & 0x3ff) as f32 / 1023.0 * 2.0 - 1.0;
        let mut normal = Vec3::new(x, y, 1.0 - x.abs() - y.abs());
        if normal.z < 0.0 {
            normal.x = (1.0 - y.abs()) * if x >= 0.0 { 1.0 } else { -1.0 };
            normal.y = (1.0 - x.abs()) * if y >= 0.0 { 1.0 } else { -1.0 };
        }
        normal.normalize()
    }
}
