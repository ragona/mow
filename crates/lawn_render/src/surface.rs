//! Continuous visual materials derived from the unchanged gameplay cell mask.

use glam::{Vec2, Vec3};
use lawn_core::{
    cube_map::{
        CubeFace, cell_center_direction, direction_to_cell, direction_to_face_uv, tangent_frame,
    },
    planet::{Planet, SurfaceMaterial},
};

const CRATER_BIN_RESOLUTION: u32 = 16;
const GRASS_PROTECTION_COVERAGE: f32 = 0.75;

#[derive(Clone, Debug, PartialEq)]
struct Crater {
    center: Vec3,
    tangent: Vec3,
    bitangent: Vec3,
    radius: f32,
    surface_radius: f32,
    depth: f32,
    rim_height: f32,
    phase: f32,
    tint: f32,
    support_cosine: f32,
}

/// A cached, padded mask shared by terrain shading and the rendered grass edge.
/// A small local filter rounds the coarse contour without moving gameplay data.
#[derive(Debug)]
pub(crate) struct TerrainSurface {
    resolution: u32,
    coverage: Vec<f32>,
    garden_phase: Vec3,
    cavity_resolution: u32,
    cavity: Vec<f32>,
    craters: Vec<Crater>,
    crater_bins: Vec<Vec<usize>>,
    base_radius: f32,
}

impl TerrainSurface {
    pub(crate) fn new(planet: &Planet) -> Self {
        let mask: Vec<_> = planet
            .terrain
            .iter()
            .map(|cell| f32::from(cell.material == SurfaceMaterial::Rock))
            .collect();
        let mut surface = Self::from_mask(planet.terrain.resolution(), &mask);
        surface.prepare_garden(planet);
        surface.prepare_craters(planet);
        surface
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
            coverage,
            garden_phase: Vec3::ZERO,
            cavity_resolution: 0,
            cavity: Vec::new(),
            craters: Vec::new(),
            crater_bins: Vec::new(),
            base_radius: 0.0,
        }
    }

    /// Samples four cached values, with a smooth transition about cell centers.
    /// Interior values remain exactly zero or one; 0.5 is the shared rock edge.
    pub(crate) fn rock_coverage(&self, direction: Vec3) -> f32 {
        sample_padded(&self.coverage, self.resolution, direction)
    }

    /// Seeded, broad garden clusters shared by terrain and blade preparation.
    /// Evaluated only on upload: the grass shader reads one packed attribute.
    pub(crate) fn garden(&self, direction: Vec3) -> [f32; 3] {
        let direction = direction.normalize();
        let broad = (direction.dot(Vec3::new(4.7, 6.1, 2.9)) + self.garden_phase.x).sin();
        let middle = (direction.dot(Vec3::new(12.2, -7.8, 5.4)) + self.garden_phase.y).sin();
        let fine = (direction.dot(Vec3::new(-16.3, 14.1, 10.5)) + self.garden_phase.z).sin();
        let tone = smoothstep(0.12, 0.88, 0.5 + broad * 0.27 + middle * 0.15 + fine * 0.08);
        let shape = smoothstep(0.10, 0.90, 0.5 + middle * 0.30 + fine * 0.20);
        let cavity = if self.cavity_resolution == 0 {
            0.0
        } else {
            sample_padded(&self.cavity, self.cavity_resolution, direction)
        };
        [tone, shape, cavity]
    }

    fn prepare_garden(&mut self, planet: &Planet) {
        let seed = planet.world_seed.0 ^ 0x4741_5244_454E;
        self.garden_phase = Vec3::new(
            random01(hash64(seed)),
            random01(hash64(seed ^ 0x544F_4E45)),
            random01(hash64(seed ^ 0x0053_4841_5045)),
        ) * std::f32::consts::TAU;

        // A small static horizon field captures sheltered rock feet and valleys.
        // Its resolution is bounded independently of large editor worlds. Only
        // these centers sample analytic heights; the horizon uses cached taps.
        let resolution = self.resolution.min(64);
        let radii: Vec<_> = CubeFace::ALL
            .into_iter()
            .flat_map(|face| {
                (0..resolution).flat_map(move |y| {
                    (0..resolution).map(move |x| {
                        let uv = (Vec2::new(x as f32, y as f32) + Vec2::splat(0.5))
                            * (2.0 / resolution as f32)
                            - Vec2::ONE;
                        planet.surface_radius(face_vector(face, uv))
                    })
                })
            })
            .collect();
        let mut cavities = Vec::with_capacity(radii.len());
        for face in CubeFace::ALL {
            for y in 0..i64::from(resolution) {
                for x in 0..i64::from(resolution) {
                    let point = |dx: i64, dy: i64| {
                        let uv = (Vec2::new((x + dx) as f32, (y + dy) as f32) + Vec2::splat(0.5))
                            * (2.0 / resolution as f32)
                            - Vec2::ONE;
                        face_vector(face, uv).normalize()
                            * mask_tap(&radii, resolution, face, x + dx, y + dy)
                    };
                    let center = point(0, 0);
                    let radial = center.normalize();
                    let mut normal = (point(1, 0) - point(-1, 0))
                        .cross(point(0, 1) - point(0, -1))
                        .try_normalize()
                        .unwrap_or(radial);
                    if normal.dot(radial) < 0.0 {
                        normal = -normal;
                    }
                    let mut occlusion = 0.0;
                    for (dx, dy) in [
                        (-1, 0),
                        (1, 0),
                        (0, -1),
                        (0, 1),
                        (-3, 0),
                        (3, 0),
                        (0, -3),
                        (0, 3),
                        (-2, -2),
                        (-2, 2),
                        (2, -2),
                        (2, 2),
                    ] {
                        let delta = point(dx, dy) - center;
                        let horizon = delta.dot(normal) / delta.length().max(0.00001);
                        occlusion += (horizon - 0.025).max(0.0);
                    }
                    cavities.push((occlusion * (3.0 / 12.0)).clamp(0.0, 1.0));
                }
            }
        }
        // Reuse the seam-safe padded tent filter. Values remain continuous as
        // terrain vertices and roots cross cube boundaries and corners.
        let filtered = Self::from_mask(resolution, &cavities);
        self.cavity_resolution = resolution;
        self.cavity = filtered.coverage;
    }

    fn surface_detail(&self, direction: Vec3, bowl: f32, rim: f32, tint: f32) -> [f32; 4] {
        let [tone, _, cavity] = self.garden(direction);
        [
            bowl,
            rim,
            lerp(tone, tint, bowl.max(rim)),
            (cavity + bowl * 0.24).min(1.0),
        ]
    }

    /// Cosmetic radial offset and bowl/rim/tone/cavity data, evaluated only
    /// while building the immutable render mesh. Gameplay heights stay intact.
    pub(crate) fn meteor_detail(&self, direction: Vec3, coverage: f32) -> (f32, [f32; 4]) {
        if coverage <= GRASS_PROTECTION_COVERAGE || self.craters.is_empty() {
            return (0.0, self.surface_detail(direction, 0.0, 0.0, 0.0));
        }
        let cell = direction_to_cell(direction, CRATER_BIN_RESOLUTION);
        let index = (cell.face.index() * CRATER_BIN_RESOLUTION as usize + cell.y as usize)
            * CRATER_BIN_RESOLUTION as usize
            + cell.x as usize;
        self.sample_craters(direction, coverage, self.crater_bins[index].iter().copied())
    }

    fn sample_craters(
        &self,
        direction: Vec3,
        coverage: f32,
        candidates: impl Iterator<Item = usize>,
    ) -> (f32, [f32; 4]) {
        let protection = smoothstep(GRASS_PROTECTION_COVERAGE, 1.0, coverage);
        let mut displacement = 0.0;
        let mut bowl_mask: f32 = 0.0;
        let mut rim_mask: f32 = 0.0;
        let mut tint = 0.0;
        let mut strongest = 0.0;
        for index in candidates {
            let crater = &self.craters[index];
            let alignment = direction.dot(crater.center);
            if alignment < crater.support_cosine {
                continue;
            }
            let delta = (direction - crater.center) * crater.surface_radius;
            let distance = delta.length();
            let angle = delta.dot(crater.bitangent).atan2(delta.dot(crater.tangent));
            // Broad asymmetric lobes and missing rim sections read as chipped
            // impact stone rather than perfect circular holes punched in a ball.
            let edge = 1.0
                + 0.065 * (angle * 3.0 + crater.phase).sin()
                + 0.035 * (angle * 7.0 - crater.phase).sin();
            let normalized = distance / (crater.radius * edge);
            let bowl = 1.0 - smoothstep(0.30, 1.0, normalized);
            let chip = smoothstep(0.25, 0.90, (angle * 5.0 + crater.phase).sin());
            let rim =
                (1.0 - smoothstep(0.0, 0.26, (normalized - 1.04).abs())) * (1.0 - chip * 0.72);
            displacement += -crater.depth * bowl + crater.rim_height * rim;
            bowl_mask = bowl_mask.max(bowl);
            rim_mask = rim_mask.max(rim);
            let influence = bowl.max(rim);
            if influence > strongest {
                strongest = influence;
                tint = crater.tint;
            }
        }
        // Overlapping old impacts cannot compound into collision-sized holes.
        // The entire effect vanishes well before any visible grass fringe.
        let safety_scale = (self.base_radius / 15.0).min(1.0);
        (
            displacement.clamp(-0.36 * safety_scale, 0.07 * safety_scale) * protection,
            self.surface_detail(
                direction,
                bowl_mask * protection,
                rim_mask * protection,
                tint,
            ),
        )
    }

    fn prepare_craters(&mut self, planet: &Planet) {
        self.base_radius = planet.config.base_radius;
        let seed = planet.world_seed.0
            ^ (u64::from(planet.generation_attempt) << 32)
            ^ u64::from(planet.generator_version.0)
            ^ 0x4D45_5445_4F52_4954;
        let mut candidates = Vec::new();
        for (index, cell) in planet.terrain.iter().enumerate() {
            if cell.material != SurfaceMaterial::Rock {
                continue;
            }
            let direction = cell_center_direction(
                planet.terrain.cell_from_index(index),
                planet.terrain.resolution(),
            );
            if self.rock_coverage(direction) >= 0.92 {
                candidates.push((hash64(seed ^ index as u64), direction));
            }
        }
        if candidates.is_empty() {
            return;
        }
        candidates.sort_unstable_by_key(|&(rank, _)| rank);
        let target_count = 64 + (hash64(seed) % 33) as usize;
        let size_scale = self.base_radius / 15.0;
        let depth_scale = size_scale.min(1.25);
        self.craters.reserve(target_count);
        for (rank, center) in candidates {
            let variation = random01(hash64(rank ^ 0x5349_5A45));
            let radius = (0.80 + 1.15 * variation) * size_scale;
            if self.craters.iter().any(|other| {
                let separation = (other.radius + radius) * 0.58;
                center.distance_squared(other.center) * self.base_radius * self.base_radius
                    < separation * separation
            }) {
                continue;
            }
            let (tangent, bitangent) = tangent_frame(center);
            let surface_radius = planet.surface_radius(center);
            let support = radius * 1.5 / surface_radius;
            self.craters.push(Crater {
                center,
                tangent,
                bitangent,
                radius,
                surface_radius,
                depth: (0.180 + 0.160 * random01(hash64(rank))) * depth_scale,
                rim_height: (0.027 + 0.033 * random01(hash64(rank ^ 0x0052_494D))) * depth_scale,
                phase: random01(hash64(rank ^ 0x0050_4841_5345)) * std::f32::consts::TAU,
                tint: random01(hash64(rank ^ 0x5449_4E54)),
                support_cosine: 1.0 - support * support * 0.5,
            });
            if self.craters.len() == target_count {
                break;
            }
        }
        self.prepare_crater_bins();
    }

    fn prepare_crater_bins(&mut self) {
        // Conservative spherical bins reduce a mesh vertex's candidate set
        // from dozens of impacts to its nearby few. The broad cell bound also
        // covers cube corners, so pruning never creates a seam or clips a rim.
        let count = (6 * CRATER_BIN_RESOLUTION * CRATER_BIN_RESOLUTION) as usize;
        self.crater_bins = Vec::with_capacity(count);
        for index in 0..count {
            let face_area = (CRATER_BIN_RESOLUTION * CRATER_BIN_RESOLUTION) as usize;
            let cell = lawn_core::cube_map::CubeCell {
                face: CubeFace::ALL[index / face_area],
                x: (index % CRATER_BIN_RESOLUTION as usize) as u32,
                y: (index % face_area / CRATER_BIN_RESOLUTION as usize) as u32,
            };
            let center = cell_center_direction(cell, CRATER_BIN_RESOLUTION);
            let nearby = self
                .craters
                .iter()
                .enumerate()
                .filter_map(|(index, crater)| {
                    // Deep custom valleys can put an impact's local surface
                    // radius below its support radius. Such a cap covers the
                    // sphere; it must not produce NaN and disappear from bins.
                    let half_chord = (crater.radius * 0.75 / crater.surface_radius).min(1.0);
                    let angular_bound = (3.0 / CRATER_BIN_RESOLUTION as f32
                        + half_chord.asin() * 2.0)
                        .min(std::f32::consts::PI);
                    (angular_bound >= std::f32::consts::PI
                        || center.dot(crater.center) >= angular_bound.cos())
                    .then_some(index)
                })
                .collect();
            self.crater_bins.push(nearby);
        }
    }
}

fn smoothstep(start: f32, end: f32, value: f32) -> f32 {
    let value = ((value - start) / (end - start)).clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn hash64(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn random01(value: u64) -> f32 {
    (value >> 40) as f32 / 16_777_215.0
}

fn lerp(a: f32, b: f32, weight: f32) -> f32 {
    (b - a).mul_add(weight, a)
}

fn sample_padded(values: &[f32], resolution: u32, direction: Vec3) -> f32 {
    let address = direction_to_face_uv(direction);
    let coordinate = (address.uv * 0.5 + Vec2::splat(0.5)) * resolution as f32 + Vec2::splat(0.5);
    let base = coordinate.floor();
    let fraction = coordinate - base;
    let weight = fraction * fraction * (Vec2::splat(3.0) - fraction * 2.0);
    let stride = resolution as usize + 2;
    let index = address.face.index() * stride * stride + base.y as usize * stride + base.x as usize;
    let top = lerp(values[index], values[index + 1], weight.x);
    let bottom = lerp(values[index + stride], values[index + stride + 1], weight.x);
    lerp(top, bottom, weight.y).clamp(0.0, 1.0)
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

    fn crater_planet() -> Planet {
        PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::default())
            .generate_with_roots(WorldSeed(21), false)
            .unwrap()
    }

    #[test]
    fn garden_clusters_and_cavity_are_bounded_continuous_and_seeded() {
        let planet = crater_planet();
        let first = TerrainSurface::new(&planet);
        let second = TerrainSurface::new(&planet);
        assert_eq!(first.garden_phase, second.garden_phase);
        assert_eq!(first.cavity, second.cavity);
        assert_eq!(first.cavity_resolution, planet.terrain.resolution().min(64));
        assert!(first.cavity.len() <= 6 * 66 * 66);
        let mut maximum_cavity: f32 = 0.0;
        let mut tone_range = (1.0_f32, 0.0_f32);
        for face in CubeFace::ALL {
            for y in -20..=20 {
                for x in -20..=20 {
                    let direction = face_vector(face, Vec2::new(x as f32, y as f32) / 20.0);
                    let garden = first.garden(direction);
                    assert!(garden.into_iter().all(|value| (0.0..=1.0).contains(&value)));
                    maximum_cavity = maximum_cavity.max(garden[2]);
                    tone_range.0 = tone_range.0.min(garden[0]);
                    tone_range.1 = tone_range.1.max(garden[0]);
                }
            }
            for side in [-1.0, 1.0] {
                for along in [-1.0, -0.7, -0.2, 0.0, 0.35, 0.8, 1.0] {
                    let first_side =
                        first.garden(face_vector(face, Vec2::new(side - 1.0e-5, along)));
                    let other_side =
                        first.garden(face_vector(face, Vec2::new(side + 1.0e-5, along)));
                    for channel in 0..3 {
                        assert!((first_side[channel] - other_side[channel]).abs() < 0.001);
                    }
                }
            }
        }
        assert!(
            maximum_cavity > 0.02,
            "rocky terrain should contain sheltered ground"
        );
        assert!(tone_range.1 - tone_range.0 > 0.65);
        let mut different_seed = planet;
        different_seed.world_seed = WorldSeed(22);
        assert_ne!(
            first.garden_phase,
            TerrainSurface::new(&different_seed).garden_phase
        );
    }

    #[test]
    fn a_smooth_sphere_has_no_false_static_contact_shade() {
        let mut planet = crater_planet();
        planet.mountains.clear();
        planet.config.rolling_amplitude = 0.0;
        let surface = TerrainSurface::new(&planet);
        assert!(surface.cavity.iter().all(|&value| value < 1.0e-5));
    }

    #[test]
    fn meteor_impacts_are_deterministic_varied_and_protect_grass() {
        let planet = crater_planet();
        let first = TerrainSurface::new(&planet);
        let second = TerrainSurface::new(&planet);
        assert_eq!(first.craters, second.craters);
        assert_eq!(first.crater_bins, second.crater_bins);
        assert!(
            (48..=96).contains(&first.craters.len()),
            "crater count: {}",
            first.craters.len()
        );
        let smallest = first
            .craters
            .iter()
            .map(|crater| crater.radius)
            .fold(f32::INFINITY, f32::min);
        let largest = first
            .craters
            .iter()
            .map(|crater| crater.radius)
            .fold(0.0, f32::max);
        assert!(smallest >= 0.7 && largest <= 1.95);
        assert!(
            largest - smallest > 0.7,
            "impacts need visibly different sizes"
        );
        let mut visible_bowls = 0;
        for crater in &first.craters {
            assert!((0.18..=0.34).contains(&crater.depth));
            let coverage = first.rock_coverage(crater.center);
            assert!(coverage >= 0.92);
            let (offset, detail) = first.meteor_detail(crater.center, coverage);
            visible_bowls += usize::from(offset < -0.02 && detail[0] > 0.7);
            for grass in [0.0, 0.2, 0.5, GRASS_PROTECTION_COVERAGE] {
                let (offset, detail) = first.meteor_detail(crater.center, grass);
                assert_eq!(offset, 0.0);
                assert_eq!(&detail[..2], &[0.0, 0.0]);
                assert_eq!(detail[2], first.garden(crater.center)[0]);
            }
        }
        assert!(visible_bowls >= first.craters.len() * 3 / 4);
        for face in CubeFace::ALL {
            for y in -24..=24 {
                for x in -24..=24 {
                    let direction =
                        face_vector(face, Vec2::new(x as f32, y as f32) / 24.0).normalize();
                    let coverage = first.rock_coverage(direction);
                    let (offset, detail) = first.meteor_detail(direction, coverage);
                    assert!(offset.is_finite() && (-0.36001..=0.07001).contains(&offset));
                    assert!(
                        detail
                            .into_iter()
                            .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
                    );
                }
            }
        }
        let mut other_seed = planet.clone();
        other_seed.world_seed = WorldSeed(22);
        assert_ne!(first.craters, TerrainSurface::new(&other_seed).craters);
    }

    #[test]
    fn crater_geometry_has_real_bowls_and_chipped_raised_rims() {
        let planet = crater_planet();
        let source = TerrainSurface::new(&planet);
        let mut surface = TerrainSurface::from_mask(8, &vec![1.0; 6 * 8 * 8]);
        surface.base_radius = planet.config.base_radius;
        let mut crater = source.craters[0].clone();
        crater.center = Vec3::ONE.normalize();
        (crater.tangent, crater.bitangent) = tangent_frame(crater.center);
        surface.craters.push(crater.clone());
        surface.prepare_crater_bins();
        let (center_depth, center_detail) = surface.meteor_detail(crater.center, 1.0);
        assert!(center_depth < -0.17 && center_detail[0] > 0.99);
        let mut strongest_rim: f32 = 0.0;
        let mut weakest_rim = f32::INFINITY;
        for index in 0..64 {
            let angle = index as f32 / 64.0 * std::f32::consts::TAU;
            let tangent = crater.tangent * angle.cos() + crater.bitangent * angle.sin();
            let edge = 1.0
                + 0.065 * (angle * 3.0 + crater.phase).sin()
                + 0.035 * (angle * 7.0 - crater.phase).sin();
            let direction = (crater.center * crater.surface_radius
                + tangent * crater.radius * edge * 1.04)
                .normalize();
            let (offset, detail) = surface.meteor_detail(direction, 1.0);
            assert!(offset > 0.0 && detail[1] > 0.15);
            strongest_rim = strongest_rim.max(offset);
            weakest_rim = weakest_rim.min(offset);
        }
        assert!(strongest_rim > 0.02 && strongest_rim - weakest_rim > 0.01);
        // Probe both sides of every cube seam and the three incident faces at
        // this crater's corner. The lookup must exactly match unpruned geometry.
        for face in CubeFace::ALL {
            for edge in [-1.0, 1.0] {
                for along in -32..=32 {
                    for epsilon in [-0.00001, 0.0, 0.00001] {
                        let direction =
                            face_vector(face, Vec2::new(edge + epsilon, along as f32 / 32.0))
                                .normalize();
                        let pruned = surface.meteor_detail(direction, 1.0);
                        let full = surface.sample_craters(direction, 1.0, 0..surface.craters.len());
                        assert_eq!(
                            pruned, full,
                            "spatial pruning clipped a crater across a cube seam"
                        );
                    }
                }
            }
        }
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            let direction = (Vec3::ONE + axis * 0.00001).normalize();
            assert!((surface.meteor_detail(direction, 1.0).0 - center_depth).abs() < 0.0001);
        }
    }

    #[test]
    fn crater_relief_scales_safely_on_tiny_valid_worlds() {
        for radius in [0.01, 0.1] {
            let config = GeneratorConfig {
                base_radius: radius,
                rolling_amplitude: 0.0,
                mountain_height_min: radius * 0.20,
                mountain_height_max: radius * 0.36,
                pass_clearance: radius * 0.1,
                spawn_clearance: radius * 0.1,
                ideal_time_min_seconds: 0.0,
                ..GeneratorConfig::test_quality()
            };
            let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config)
                .generate_with_roots(WorldSeed(21), false)
                .unwrap();
            let surface = TerrainSurface::new(&planet);
            assert!(!surface.craters.is_empty());
            for crater in &surface.craters {
                assert!(crater.depth > 0.0 && crater.depth < radius * 0.03);
                let (offset, _) = surface.meteor_detail(crater.center, 1.0);
                assert!(offset >= -radius * 0.02401);
            }
            let (vertices, _) = crate::mesh::build_terrain(&planet, &surface);
            for vertex in vertices {
                let position = Vec3::from_array(vertex.position);
                let normal = Vec3::from_array(vertex.normal);
                assert!(position.is_finite() && position.length() > radius * 0.97);
                assert!(normal.is_finite() && normal.dot(position) > 0.0);
            }
        }
    }

    #[test]
    fn crater_bins_remain_conservative_when_support_covers_a_small_radial_valley() {
        let mut surface = TerrainSurface::new(&crater_planet());
        surface.craters.truncate(1);
        let crater = &mut surface.craters[0];
        crater.surface_radius = crater.radius * 0.25;
        let support = crater.radius * 1.5 / crater.surface_radius;
        crater.support_cosine = 1.0 - support * support * 0.5;
        surface.prepare_crater_bins();
        assert!(surface.crater_bins.iter().all(|bin| bin == &[0]));
        for direction in [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::Z,
            Vec3::NEG_Z,
        ] {
            assert_eq!(
                surface.meteor_detail(direction, 1.0),
                surface.sample_craters(direction, 1.0, 0..1)
            );
        }
    }
}
