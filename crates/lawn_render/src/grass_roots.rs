//! Upload-only grass data. The immutable generator records stay compact and
//! deterministic; the renderer caches fringe, garden clusters and cavity shade.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use lawn_core::planet::GrassRootGpu;

use crate::surface::TerrainSurface;

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub(crate) struct RenderGrassRoot {
    position: [f32; 3],
    packed_normal_seed: u32,
    turf_weight: f32,
    garden_data: u32,
}

const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
    1 => Float32x3,
    2 => Uint32,
    3 => Float32,
    4 => Uint32
];

pub(crate) const fn layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<RenderGrassRoot>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRIBUTES,
    }
}

fn turf_weight(rock_coverage: f32) -> f32 {
    let t = ((rock_coverage - 0.20) / 0.30).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

pub(crate) fn prepare(roots: &[GrassRootGpu], surface: &TerrainSurface) -> Vec<RenderGrassRoot> {
    // Preserve every index so existing patch ranges and stable LOD prefixes
    // remain valid. Zero-weight roots become degenerate geometry in the shader.
    roots
        .iter()
        .map(|root| RenderGrassRoot {
            position: root.position,
            packed_normal_seed: root.packed_normal_seed,
            turf_weight: turf_weight(surface.rock_coverage(Vec3::from_array(root.position))),
            garden_data: pack_garden(surface.garden(Vec3::from_array(root.position)))
                | (individual_variation(root.packed_normal_seed) << 24),
        })
        .collect()
}

fn pack_garden(values: [f32; 3]) -> u32 {
    let [tone, shape, cavity] = values.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u32);
    tone | (shape << 8) | (cavity << 16)
}

fn individual_variation(packed_normal_seed: u32) -> u32 {
    // This stable variation is shared by all 24 vertices. Prepare it in the
    // unused byte instead of repeating the hash in every vertex invocation.
    let mut value = (packed_normal_seed >> 20).wrapping_mul(1_597_334_677);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    (value as f32 / u32::MAX as f32 * 255.0).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use lawn_core::{
        GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION,
    };

    #[test]
    fn fringe_preserves_root_identity_and_matches_the_rock_contour() {
        let planet = PlanetGenerator::new(
            CURRENT_GENERATOR_VERSION,
            GeneratorConfig {
                grass_roots_per_square_meter: 8.0,
                ..GeneratorConfig::test_quality()
            },
        )
        .generate(WorldSeed(21))
        .unwrap();
        let original = planet.grass_roots.clone();
        let surface = TerrainSurface::new(&planet);
        let roots = prepare(&planet.grass_roots, &surface);
        assert_eq!(roots.len(), original.len());
        let mut fringe_count = 0;
        for (core, rendered) in original.iter().zip(&roots) {
            assert_eq!(core.position, rendered.position);
            assert_eq!(core.packed_normal_seed, rendered.packed_normal_seed);
            assert!((0.0..=1.0).contains(&rendered.turf_weight));
            for (channel, value) in surface
                .garden(Vec3::from_array(core.position))
                .into_iter()
                .enumerate()
            {
                let cached = ((rendered.garden_data >> (channel * 8)) & 255) as f32 / 255.0;
                assert!((cached - value).abs() <= 0.5 / 255.0 + f32::EPSILON);
            }
            let coverage = surface.rock_coverage(Vec3::from_array(core.position));
            if coverage >= 0.5 {
                assert_eq!(
                    rendered.turf_weight, 0.0,
                    "rock cannot grow a furry overlay"
                );
            }
            if coverage <= 0.20 {
                assert_eq!(
                    rendered.turf_weight, 1.0,
                    "interior lawn retains its height"
                );
            }
            if rendered.turf_weight > 0.0 && rendered.turf_weight < 1.0 {
                fringe_count += 1;
            }
        }
        assert!(
            fringe_count > 0,
            "the rocky test world must exercise the fringe"
        );
        assert_eq!(planet.grass_roots, original);
        assert_eq!(std::mem::size_of::<GrassRootGpu>(), 16);
        assert_eq!(std::mem::size_of::<RenderGrassRoot>(), 24);
    }
}
