//! Deterministic, three-dimensional seeded planet generation and validation.

use std::{collections::VecDeque, fmt, ops::Range, str::FromStr};

use bytemuck::{Pod, Zeroable};
use glam::{Vec2, Vec3};
use rand::{Rng, SeedableRng, seq::SliceRandom};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    config::GeneratorConfig,
    cube_map::{
        CubeCell, CubeFace, CubeGrid, cell_center_direction, cell_solid_angle, direction_to_cell,
        face_uv_to_direction, offset_cell, tangent_frame,
    },
};

pub const CURRENT_GENERATOR_VERSION: GeneratorVersion = GeneratorVersion(2);
pub const TUTORIAL_SEED: WorldSeed = WorldSeed(0x4c41_574e_4f52_4249);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GeneratorVersion(pub u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorldSeed(pub u64);

impl fmt::Display for WorldSeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:016X}", self.0)
    }
}

impl FromStr for WorldSeed {
    type Err = SeedParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(SeedParseError);
        }
        if let Some(without_prefix) = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
        {
            return u64::from_str_radix(without_prefix, 16)
                .map(Self)
                .map_err(|_| SeedParseError);
        }
        if let Ok(number) = trimmed.parse::<u64>() {
            return Ok(Self(number));
        }
        // Human-readable seed phrases are stable across platforms and releases.
        Ok(Self(fnv1a(trimmed.as_bytes())))
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("a seed cannot be empty")]
pub struct SeedParseError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SurfaceMaterial {
    #[default]
    Grass = 0,
    Rock = 1,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TerrainCell {
    pub radius: f32,
    pub normal: Vec3,
    pub area: f32,
    pub slope_radians: f32,
    pub mountain_influence: f32,
    pub material: SurfaceMaterial,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mountain {
    pub center: Vec3,
    pub angular_radius: f32,
    pub height: f32,
    pub cragginess: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnPoint {
    pub position: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    pub clearance: f32,
}

/// Compact immutable root record consumed directly by the renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct GrassRootGpu {
    pub position: [f32; 3],
    /// Two 10-bit octahedral normal components and a 12-bit stable variation seed.
    pub packed_normal_seed: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrassPatch {
    pub face: CubeFace,
    pub tile_x: u32,
    pub tile_y: u32,
    pub roots: Range<u32>,
    pub center: Vec3,
    pub angular_radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub mowable_ratio: f64,
    pub reachable_ratio: f64,
    pub mowable_area: f64,
    pub largest_region_area: f64,
    pub spawn_clearance: f32,
    pub spawn_slope_degrees: f32,
    pub estimated_ideal_seconds: f32,
    pub grass_regions: u32,
    pub corridor_core_regions: u32,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Planet {
    pub generator_version: GeneratorVersion,
    pub world_seed: WorldSeed,
    pub generation_attempt: u32,
    pub config: GeneratorConfig,
    pub terrain: CubeGrid<TerrainCell>,
    pub mountains: Vec<Mountain>,
    pub grass_roots: Vec<GrassRootGpu>,
    pub grass_patches: Vec<GrassPatch>,
    pub spawn: SpawnPoint,
    pub validation: ValidationReport,
    pub deterministic_hash: u64,
}

impl Planet {
    #[must_use]
    pub fn terrain_cell(&self, direction: Vec3) -> &TerrainCell {
        self.terrain
            .get(direction_to_cell(direction, self.terrain.resolution()))
    }

    #[must_use]
    pub fn surface_radius(&self, direction: Vec3) -> f32 {
        sample_radius(
            direction.normalize(),
            self.config.base_radius,
            self.config.rolling_amplitude,
            self.generator_version,
            self.world_seed,
            self.generation_attempt,
            &self.mountains,
        )
    }

    #[must_use]
    pub fn surface_point(&self, direction: Vec3) -> Vec3 {
        let d = direction.normalize();
        d * self.surface_radius(d)
    }

    #[must_use]
    pub fn is_mowable(&self, direction: Vec3) -> bool {
        self.terrain_cell(direction).material == SurfaceMaterial::Grass
    }

    /// Stable safe point nearest to a direction. The connectivity/spawn grid is
    /// also used by manual and automatic vehicle recovery.
    #[must_use]
    pub fn nearest_safe_point(&self, direction: Vec3) -> SpawnPoint {
        let resolution = self.terrain.resolution();
        let start = direction_to_cell(direction, resolution);
        let mut queue = VecDeque::from([start]);
        let mut visited = vec![false; self.terrain.len()];
        visited[self.terrain.flat_index(start)] = true;
        while let Some(cell) = queue.pop_front() {
            let sample = self.terrain.get(cell);
            if sample.material == SurfaceMaterial::Grass
                && sample.slope_radians.to_degrees() <= self.config.spawn_max_slope_degrees * 1.6
            {
                let d = cell_center_direction(cell, resolution);
                let up = sample.normal;
                let previous_forward = self.spawn.forward;
                let forward = (previous_forward - up * previous_forward.dot(up))
                    .try_normalize()
                    .unwrap_or_else(|| tangent_frame(up).0);
                return SpawnPoint {
                    position: d * sample.radius + up * 0.8,
                    up,
                    forward,
                    clearance: 0.0,
                };
            }
            for neighbor in neighbors4(cell, resolution) {
                let index = self.terrain.flat_index(neighbor);
                if !visited[index] {
                    visited[index] = true;
                    queue.push_back(neighbor);
                }
            }
        }
        self.spawn
    }
}

#[derive(Clone, Debug)]
pub struct PlanetGenerator {
    pub version: GeneratorVersion,
    pub config: GeneratorConfig,
}

impl Default for PlanetGenerator {
    fn default() -> Self {
        Self {
            version: CURRENT_GENERATOR_VERSION,
            config: GeneratorConfig::default(),
        }
    }
}

impl PlanetGenerator {
    #[must_use]
    pub const fn new(version: GeneratorVersion, config: GeneratorConfig) -> Self {
        Self { version, config }
    }

    /// Generate a shipping-quality planet including deterministic grass roots.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationError`] when configuration is invalid or every
    /// deterministic generation/repair attempt fails validation.
    pub fn generate(&self, seed: WorldSeed) -> Result<Planet, GenerationError> {
        self.generate_with_roots(seed, true)
    }

    /// Generate all gameplay data while optionally skipping the large cosmetic
    /// root buffer. Validation/fuzz tools use `include_roots = false`.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationError`] when configuration is invalid or every
    /// deterministic generation/repair attempt fails validation.
    pub fn generate_with_roots(
        &self,
        seed: WorldSeed,
        include_roots: bool,
    ) -> Result<Planet, GenerationError> {
        if self.config.terrain_resolution < 8 || self.config.mowing_resolution < 8 {
            return Err(GenerationError::InvalidConfig(
                "terrain and mowing resolutions must be at least 8".into(),
            ));
        }
        let mut last_report = ValidationReport::default();
        for attempt in 0..self.config.maximum_generation_attempts {
            let planet = self.generate_attempt(seed, attempt, include_roots);
            last_report = planet.validation.clone();
            if planet.validation.valid {
                return Ok(planet);
            }
        }
        Err(GenerationError::AttemptsExhausted {
            seed,
            attempts: self.config.maximum_generation_attempts,
            report: Box::new(last_report),
        })
    }

    fn generate_attempt(&self, seed: WorldSeed, attempt: u32, include_roots: bool) -> Planet {
        let mut mountain_rng = stage_rng(self.version, seed, attempt, 1);
        let mountain_count = mountain_rng
            .random_range(self.config.mountain_count_min..=self.config.mountain_count_max);
        let mountains = generate_mountains(&self.config, &mut mountain_rng, mountain_count);
        let resolution = self.config.terrain_resolution;
        let mut terrain = CubeGrid::new(resolution, TerrainCell::default());

        for cell in terrain.cells().collect::<Vec<_>>() {
            let direction = cell_center_direction(cell, resolution);
            let (radius, influence) = sample_radius_and_influence(
                direction,
                self.config.base_radius,
                self.config.rolling_amplitude,
                self.version,
                seed,
                attempt,
                &mountains,
            );
            let normal = sample_normal(
                direction,
                self.config.base_radius,
                self.config.rolling_amplitude,
                self.version,
                seed,
                attempt,
                &mountains,
                resolution,
            );
            let radial_alignment = normal.dot(direction).clamp(0.35, 1.0);
            let area = (cell_solid_angle(cell.x, cell.y, resolution) * f64::from(radius * radius)
                / f64::from(radial_alignment)) as f32;
            *terrain.get_mut(cell) = TerrainCell {
                radius,
                normal,
                area,
                slope_radians: radial_alignment.acos(),
                mountain_influence: influence,
                material: SurfaceMaterial::Grass,
            };
        }

        let mut class_rng = stage_rng(self.version, seed, attempt, 2);
        let rock_min = 1.0 - self.config.mowable_ratio_max;
        let rock_max = 1.0 - self.config.mowable_ratio_min;
        // Retain the existing seeded distribution for normal worlds. Narrow
        // ranges (including exactly zero rock) must not produce an empty RNG range.
        let target_rock_ratio = if rock_max - rock_min > 0.02 {
            class_rng.random_range((rock_min + 0.01)..(rock_max - 0.01))
        } else {
            (rock_min + rock_max) * 0.5
        };
        classify_rock(&mut terrain, target_rock_ratio);
        remove_tiny_grass_islands(&mut terrain, self.config.required_reachable_ratio);

        let clearance = distance_from_rock(&terrain);
        let spawn = select_spawn(&self.config, &terrain, &clearance);
        let validation = validate_planet(&self.config, &terrain, &mountains, spawn);

        let (grass_roots, grass_patches) =
            if include_roots && self.config.grass_roots_per_square_meter > 0.0 {
                generate_grass_roots(
                    &self.config,
                    self.version,
                    seed,
                    attempt,
                    &terrain,
                    &mountains,
                )
            } else {
                (Vec::new(), Vec::new())
            };

        let deterministic_hash = hash_planet(
            self.version,
            seed,
            attempt,
            &terrain,
            &mountains,
            spawn,
            &grass_roots,
        );

        Planet {
            generator_version: self.version,
            world_seed: seed,
            generation_attempt: attempt,
            config: self.config.clone(),
            terrain,
            mountains,
            grass_roots,
            grass_patches,
            spawn,
            validation,
            deterministic_hash,
        }
    }
}

#[derive(Debug, Error)]
pub enum GenerationError {
    #[error("invalid generator configuration: {0}")]
    InvalidConfig(String),
    #[error("seed {seed} remained invalid after {attempts} deterministic attempts: {report:?}")]
    AttemptsExhausted {
        seed: WorldSeed,
        attempts: u32,
        report: Box<ValidationReport>,
    },
}

fn generate_mountains(config: &GeneratorConfig, rng: &mut ChaCha8Rng, count: u8) -> Vec<Mountain> {
    let mut centers: Vec<Vec3> = Vec::with_capacity(count as usize);
    let minimum_dot = config.mountain_separation_radians.cos();
    let mut attempts = 0;
    while centers.len() < count as usize && attempts < 20_000 {
        attempts += 1;
        let candidate = random_unit_vector(rng);
        if centers
            .iter()
            .all(|other| candidate.dot(*other) < minimum_dot)
        {
            centers.push(candidate);
        }
    }
    // The configured separation/count ranges always fit. This fallback keeps
    // generation total even for aggressively edited development settings.
    while centers.len() < count as usize {
        centers.push(random_unit_vector(rng));
    }
    centers
        .into_iter()
        .map(|center| Mountain {
            center,
            angular_radius: rng.random_range(0.34..0.52),
            height: rng.random_range(config.mountain_height_min..=config.mountain_height_max),
            cragginess: rng.random_range(0.12..0.3),
        })
        .collect()
}

fn random_unit_vector(rng: &mut ChaCha8Rng) -> Vec3 {
    let z = rng.random_range(-1.0_f32..=1.0);
    let phi = rng.random_range(0.0..std::f32::consts::TAU);
    let radial = (1.0 - z * z).sqrt();
    Vec3::new(radial * phi.cos(), z, radial * phi.sin())
}

fn classify_rock(terrain: &mut CubeGrid<TerrainCell>, target_ratio: f32) {
    let total_area: f64 = terrain.iter().map(|cell| f64::from(cell.area)).sum();
    let target_area = total_area * f64::from(target_ratio);
    let mut ranked: Vec<(usize, f32)> = terrain
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let steepness = (cell.slope_radians / 0.75).clamp(0.0, 1.0);
            (
                index,
                cell.mountain_influence.mul_add(0.78, steepness * 0.22),
            )
        })
        .collect();
    ranked.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut rock_area = 0.0;
    for (index, _) in ranked {
        if rock_area >= target_area {
            break;
        }
        let cell = &mut terrain.as_mut_slice()[index];
        cell.material = SurfaceMaterial::Rock;
        rock_area += f64::from(cell.area);
    }
}

fn remove_tiny_grass_islands(terrain: &mut CubeGrid<TerrainCell>, required_ratio: f32) {
    let components = grass_components(terrain);
    let Some(largest_index) = components
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
    else {
        return;
    };
    let total: f64 = components.iter().map(|(_, area)| *area).sum();
    for (component_index, (indices, area)) in components.iter().enumerate() {
        if component_index != largest_index && *area / total < 1.0 - f64::from(required_ratio) {
            for &index in indices {
                terrain.as_mut_slice()[index].material = SurfaceMaterial::Rock;
            }
        }
    }
}

fn grass_components(terrain: &CubeGrid<TerrainCell>) -> Vec<(Vec<usize>, f64)> {
    let mut visited = vec![false; terrain.len()];
    let mut result = Vec::new();
    for index in 0..terrain.len() {
        if visited[index] || terrain.as_slice()[index].material != SurfaceMaterial::Grass {
            continue;
        }
        let mut queue = VecDeque::from([index]);
        let mut indices = Vec::new();
        let mut area = 0.0;
        visited[index] = true;
        while let Some(current) = queue.pop_front() {
            indices.push(current);
            area += f64::from(terrain.as_slice()[current].area);
            let cell = terrain.cell_from_index(current);
            for neighbor in neighbors4(cell, terrain.resolution()) {
                let next = terrain.flat_index(neighbor);
                if !visited[next] && terrain.as_slice()[next].material == SurfaceMaterial::Grass {
                    visited[next] = true;
                    queue.push_back(next);
                }
            }
        }
        result.push((indices, area));
    }
    result
}

fn distance_from_rock(terrain: &CubeGrid<TerrainCell>) -> Vec<u16> {
    let mut distances = vec![u16::MAX; terrain.len()];
    let mut queue = VecDeque::new();
    for (index, cell) in terrain.iter().enumerate() {
        if cell.material == SurfaceMaterial::Rock {
            distances[index] = 0;
            queue.push_back(index);
        }
    }
    while let Some(index) = queue.pop_front() {
        let distance = distances[index];
        let cell = terrain.cell_from_index(index);
        for neighbor in neighbors4(cell, terrain.resolution()) {
            let next = terrain.flat_index(neighbor);
            if distances[next] > distance.saturating_add(1) {
                distances[next] = distance.saturating_add(1);
                queue.push_back(next);
            }
        }
    }
    distances
}

fn select_spawn(
    config: &GeneratorConfig,
    terrain: &CubeGrid<TerrainCell>,
    clearance_cells: &[u16],
) -> SpawnPoint {
    let approximate_cell_width = config.base_radius * 2.0 / terrain.resolution() as f32;
    let mut best: Option<(usize, f32)> = None;
    for (index, sample) in terrain.iter().enumerate() {
        if sample.material != SurfaceMaterial::Grass {
            continue;
        }
        let slope_degrees = sample.slope_radians.to_degrees();
        let clearance = f32::from(clearance_cells[index]) * approximate_cell_width;
        let flatness = (config.spawn_max_slope_degrees - slope_degrees).max(0.0);
        let score = clearance + flatness * 2.0;
        if slope_degrees <= config.spawn_max_slope_degrees
            && best.is_none_or(|(_, best_score)| score > best_score)
        {
            best = Some((index, score));
        }
    }
    let index = best.map_or_else(
        || {
            terrain
                .iter()
                .enumerate()
                .filter(|(_, cell)| cell.material == SurfaceMaterial::Grass)
                .min_by(|a, b| a.1.slope_radians.total_cmp(&b.1.slope_radians))
                .map_or(0, |pair| pair.0)
        },
        |pair| pair.0,
    );
    let cell = terrain.cell_from_index(index);
    let sample = terrain.as_slice()[index];
    let direction = cell_center_direction(cell, terrain.resolution());
    let (forward, _) = tangent_frame(sample.normal);
    SpawnPoint {
        position: direction * sample.radius + sample.normal * 0.8,
        up: sample.normal,
        forward,
        clearance: f32::from(clearance_cells[index]) * approximate_cell_width,
    }
}

fn validate_planet(
    config: &GeneratorConfig,
    terrain: &CubeGrid<TerrainCell>,
    mountains: &[Mountain],
    spawn: SpawnPoint,
) -> ValidationReport {
    let total_area: f64 = terrain.iter().map(|cell| f64::from(cell.area)).sum();
    let mowable_area: f64 = terrain
        .iter()
        .filter(|cell| cell.material == SurfaceMaterial::Grass)
        .map(|cell| f64::from(cell.area))
        .sum();
    let components = grass_components(terrain);
    let clearance = distance_from_rock(terrain);
    let approximate_cell_width = config.base_radius * 2.0 / terrain.resolution() as f32;
    let corridor_clearance_cells = ((config.pass_clearance * 0.5) / approximate_cell_width)
        .ceil()
        .max(1.0) as u16;
    let corridor_components =
        grass_clearance_components(terrain, &clearance, corridor_clearance_cells);
    let corridor_core_area: f64 = corridor_components.iter().map(|(_, area)| *area).sum();
    let corridor_core_regions = corridor_components
        .iter()
        .filter(|(_, area)| *area / corridor_core_area.max(f64::EPSILON) >= 0.02)
        .count() as u32;
    let largest_region_area = components
        .iter()
        .map(|(_, area)| *area)
        .fold(0.0_f64, f64::max);
    let mowable_ratio = mowable_area / total_area;
    let reachable_ratio = largest_region_area / mowable_area.max(f64::EPSILON);
    let spawn_direction = spawn.position.normalize();
    let spawn_sample = terrain.get(direction_to_cell(spawn_direction, terrain.resolution()));
    let spawn_slope_degrees = spawn_sample.slope_radians.to_degrees();
    let ideal_distance = mowable_area as f32 / 2.2 * 1.18;
    let estimated_ideal_seconds = ideal_distance / 8.0;
    let mut errors = Vec::new();

    if !(f64::from(config.mowable_ratio_min)..=f64::from(config.mowable_ratio_max))
        .contains(&mowable_ratio)
    {
        errors.push(format!(
            "mowable ratio {mowable_ratio:.4} outside configured range"
        ));
    }
    if reachable_ratio + 1.0e-6 < f64::from(config.required_reachable_ratio) {
        errors.push(format!(
            "only {:.2}% of grass is mutually reachable",
            reachable_ratio * 100.0
        ));
    }
    if !(config.mountain_count_min as usize..=config.mountain_count_max as usize)
        .contains(&mountains.len())
    {
        errors.push(format!(
            "mountain count {} outside configured range",
            mountains.len()
        ));
    }
    if spawn.clearance < config.spawn_clearance {
        errors.push(format!(
            "spawn clearance {:.2} m is too small",
            spawn.clearance
        ));
    }
    if spawn_slope_degrees > config.spawn_max_slope_degrees {
        errors.push(format!(
            "spawn slope {spawn_slope_degrees:.2} degrees is too steep"
        ));
    }
    if !(config.ideal_time_min_seconds..=config.ideal_time_max_seconds)
        .contains(&estimated_ideal_seconds)
    {
        errors.push(format!(
            "estimated ideal time {estimated_ideal_seconds:.1}s is implausible"
        ));
    }
    if corridor_core_area <= f64::EPSILON {
        errors.push(format!(
            "no grass route retains {:.2} m of intended pass clearance",
            config.pass_clearance
        ));
    } else if corridor_core_regions > 1 {
        errors.push(format!(
            "{corridor_core_regions} significant grass regions remain after pass-clearance erosion"
        ));
    }

    ValidationReport {
        valid: errors.is_empty(),
        mowable_ratio,
        reachable_ratio,
        mowable_area,
        largest_region_area,
        spawn_clearance: spawn.clearance,
        spawn_slope_degrees,
        estimated_ideal_seconds,
        grass_regions: components.len() as u32,
        corridor_core_regions,
        errors,
    }
}

fn grass_clearance_components(
    terrain: &CubeGrid<TerrainCell>,
    clearance: &[u16],
    minimum_clearance: u16,
) -> Vec<(Vec<usize>, f64)> {
    let mut visited = vec![false; terrain.len()];
    let mut result = Vec::new();
    for index in 0..terrain.len() {
        if visited[index]
            || terrain.as_slice()[index].material != SurfaceMaterial::Grass
            || clearance[index] < minimum_clearance
        {
            continue;
        }
        let mut queue = VecDeque::from([index]);
        let mut indices = Vec::new();
        let mut area = 0.0;
        visited[index] = true;
        while let Some(current) = queue.pop_front() {
            indices.push(current);
            area += f64::from(terrain.as_slice()[current].area);
            for neighbor in neighbors4(terrain.cell_from_index(current), terrain.resolution()) {
                let next = terrain.flat_index(neighbor);
                if !visited[next]
                    && terrain.as_slice()[next].material == SurfaceMaterial::Grass
                    && clearance[next] >= minimum_clearance
                {
                    visited[next] = true;
                    queue.push_back(next);
                }
            }
        }
        result.push((indices, area));
    }
    result
}

fn generate_grass_roots(
    config: &GeneratorConfig,
    version: GeneratorVersion,
    seed: WorldSeed,
    attempt: u32,
    terrain: &CubeGrid<TerrainCell>,
    mountains: &[Mountain],
) -> (Vec<GrassRootGpu>, Vec<GrassPatch>) {
    let resolution = terrain.resolution();
    let patch_cells = config.patch_cells.max(1);
    let patches_per_face = resolution.div_ceil(patch_cells);
    let estimated_roots = terrain
        .iter()
        .filter(|cell| cell.material == SurfaceMaterial::Grass)
        .map(|cell| cell.area * config.grass_roots_per_square_meter)
        .sum::<f32>() as usize;
    let mut roots = Vec::with_capacity(estimated_roots);
    let mut patches = Vec::with_capacity((6 * patches_per_face * patches_per_face) as usize);
    let mut rng = stage_rng(version, seed, attempt, 3);

    for face in CubeFace::ALL {
        for tile_y in 0..patches_per_face {
            for tile_x in 0..patches_per_face {
                let start = roots.len() as u32;
                let x_start = tile_x * patch_cells;
                let y_start = tile_y * patch_cells;
                let x_end = (x_start + patch_cells).min(resolution);
                let y_end = (y_start + patch_cells).min(resolution);
                for y in y_start..y_end {
                    for x in x_start..x_end {
                        let cell = CubeCell { face, x, y };
                        let sample = terrain.get(cell);
                        if sample.material != SurfaceMaterial::Grass {
                            continue;
                        }
                        let expected = sample.area * config.grass_roots_per_square_meter;
                        let mut count = expected.floor() as u32;
                        if rng.random::<f32>() < expected.fract() {
                            count += 1;
                        }
                        // A randomized R2 sequence is deterministic and much more
                        // even than independent samples while avoiding a visible
                        // Cartesian sub-grid. The per-cell offset prevents phase
                        // alignment at terrain-cell boundaries.
                        let sequence_offset = Vec2::new(rng.random::<f32>(), rng.random::<f32>());
                        for root_index in 0..count {
                            let index = root_index as f32;
                            let jitter = Vec2::new(
                                (sequence_offset.x + index * 0.754_877_7).fract(),
                                (sequence_offset.y + index * 0.569_840_3).fract(),
                            );
                            let uv = Vec2::new(
                                (x as f32 + jitter.x).mul_add(2.0 / resolution as f32, -1.0),
                                (y as f32 + jitter.y).mul_add(2.0 / resolution as f32, -1.0),
                            );
                            let direction = face_uv_to_direction(face, uv);
                            let radius = sample_radius(
                                direction,
                                config.base_radius,
                                config.rolling_amplitude,
                                version,
                                seed,
                                attempt,
                                mountains,
                            );
                            let variation = rng.random::<u32>() & 0x0fff;
                            roots.push(GrassRootGpu {
                                position: (direction * radius).to_array(),
                                packed_normal_seed: pack_normal_seed(sample.normal, variation),
                            });
                        }
                    }
                }
                let end = roots.len() as u32;
                // Renderer LOD retains a stable prefix. Shuffle within each patch
                // so every prefix remains spatially representative instead of
                // emptying whole terrain cells from one edge of the patch.
                roots[start as usize..end as usize].shuffle(&mut rng);
                let center_uv = Vec2::new(
                    ((x_start + x_end) as f32 * 0.5).mul_add(2.0 / resolution as f32, -1.0),
                    ((y_start + y_end) as f32 * 0.5).mul_add(2.0 / resolution as f32, -1.0),
                );
                let center = face_uv_to_direction(face, center_uv);
                let corner_uv = Vec2::new(
                    x_start as f32 * 2.0 / resolution as f32 - 1.0,
                    y_start as f32 * 2.0 / resolution as f32 - 1.0,
                );
                let angular_radius = center
                    .dot(face_uv_to_direction(face, corner_uv))
                    .clamp(-1.0, 1.0)
                    .acos();
                patches.push(GrassPatch {
                    face,
                    tile_x,
                    tile_y,
                    roots: start..end,
                    center,
                    angular_radius,
                });
            }
        }
    }
    (roots, patches)
}

fn pack_normal_seed(normal: Vec3, seed: u32) -> u32 {
    let mut oct = normal / (normal.x.abs() + normal.y.abs() + normal.z.abs());
    if oct.z < 0.0 {
        let old_x = oct.x;
        oct.x = (1.0 - oct.y.abs()) * old_x.signum();
        oct.y = (1.0 - old_x.abs()) * oct.y.signum();
    }
    let x = ((oct.x * 0.5 + 0.5) * 1023.0).round() as u32 & 0x3ff;
    let y = ((oct.y * 0.5 + 0.5) * 1023.0).round() as u32 & 0x3ff;
    x | (y << 10) | ((seed & 0x0fff) << 20)
}

fn sample_normal(
    direction: Vec3,
    base_radius: f32,
    rolling_amplitude: f32,
    version: GeneratorVersion,
    seed: WorldSeed,
    attempt: u32,
    mountains: &[Mountain],
    resolution: u32,
) -> Vec3 {
    let (tangent, bitangent) = tangent_frame(direction);
    let epsilon = 0.75 / resolution as f32;
    let du0 = (direction - tangent * epsilon).normalize();
    let du1 = (direction + tangent * epsilon).normalize();
    let dv0 = (direction - bitangent * epsilon).normalize();
    let dv1 = (direction + bitangent * epsilon).normalize();
    let pu0 = du0
        * sample_radius(
            du0,
            base_radius,
            rolling_amplitude,
            version,
            seed,
            attempt,
            mountains,
        );
    let pu1 = du1
        * sample_radius(
            du1,
            base_radius,
            rolling_amplitude,
            version,
            seed,
            attempt,
            mountains,
        );
    let pv0 = dv0
        * sample_radius(
            dv0,
            base_radius,
            rolling_amplitude,
            version,
            seed,
            attempt,
            mountains,
        );
    let pv1 = dv1
        * sample_radius(
            dv1,
            base_radius,
            rolling_amplitude,
            version,
            seed,
            attempt,
            mountains,
        );
    let mut normal = (pu1 - pu0).cross(pv1 - pv0).normalize();
    if normal.dot(direction) < 0.0 {
        normal = -normal;
    }
    normal
}

fn sample_radius(
    direction: Vec3,
    base_radius: f32,
    rolling_amplitude: f32,
    version: GeneratorVersion,
    seed: WorldSeed,
    attempt: u32,
    mountains: &[Mountain],
) -> f32 {
    sample_radius_and_influence(
        direction,
        base_radius,
        rolling_amplitude,
        version,
        seed,
        attempt,
        mountains,
    )
    .0
}

fn sample_radius_and_influence(
    direction: Vec3,
    base_radius: f32,
    rolling_amplitude: f32,
    version: GeneratorVersion,
    seed: WorldSeed,
    attempt: u32,
    mountains: &[Mountain],
) -> (f32, f32) {
    let noise_seed = stream_key(version, seed, attempt, 10);
    let rolling = fbm(direction * 1.65, noise_seed, 4) * rolling_amplitude;
    let detail_seed = stream_key(version, seed, attempt, 11);
    let mut mountain_height = 0.0;
    let mut influence = 0.0_f32;
    for (index, mountain) in mountains.iter().enumerate() {
        let angle = direction.dot(mountain.center).clamp(-1.0, 1.0).acos();
        let x = (1.0 - angle / mountain.angular_radius).clamp(0.0, 1.0);
        let smooth_mass = x * x * (3.0 - 2.0 * x);
        let ridged = 1.0
            - value_noise(
                direction * 7.5 + Vec3::splat(index as f32 * 1.731),
                detail_seed ^ index as u64,
            )
            .abs();
        let profile = smooth_mass * (1.0 - mountain.cragginess + mountain.cragginess * ridged);
        mountain_height += mountain.height * profile;
        influence = influence.max(smooth_mass);
    }
    (base_radius + rolling + mountain_height, influence)
}

fn fbm(mut point: Vec3, mut seed: u64, octaves: u32) -> f32 {
    let mut total = 0.0;
    let mut amplitude = 0.58;
    for _ in 0..octaves {
        total += value_noise(point, seed) * amplitude;
        point *= 2.03;
        amplitude *= 0.48;
        seed = splitmix64(seed);
    }
    total
}

fn value_noise(point: Vec3, seed: u64) -> f32 {
    let base = point.floor();
    let fraction = point - base;
    let smooth = fraction * fraction * (Vec3::splat(3.0) - 2.0 * fraction);
    let bx = base.x as i32;
    let by = base.y as i32;
    let bz = base.z as i32;
    let mut values = [0.0; 8];
    let mut index = 0;
    for z in 0..=1 {
        for y in 0..=1 {
            for x in 0..=1 {
                values[index] = lattice_value(bx + x, by + y, bz + z, seed);
                index += 1;
            }
        }
    }
    let x00 = values[0] + (values[1] - values[0]) * smooth.x;
    let x10 = values[2] + (values[3] - values[2]) * smooth.x;
    let x01 = values[4] + (values[5] - values[4]) * smooth.x;
    let x11 = values[6] + (values[7] - values[6]) * smooth.x;
    let y0 = x00 + (x10 - x00) * smooth.y;
    let y1 = x01 + (x11 - x01) * smooth.y;
    y0 + (y1 - y0) * smooth.z
}

fn lattice_value(x: i32, y: i32, z: i32, seed: u64) -> f32 {
    let mut hash = seed;
    hash ^= (i64::from(x) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    hash = splitmix64(hash ^ (i64::from(y) as u64).rotate_left(21));
    hash = splitmix64(hash ^ (i64::from(z) as u64).rotate_left(42));
    ((hash >> 40) as f32 / ((1_u32 << 24) - 1) as f32).mul_add(2.0, -1.0)
}

fn neighbors4(cell: CubeCell, resolution: u32) -> [CubeCell; 4] {
    [
        offset_cell(cell, -1, 0, resolution),
        offset_cell(cell, 1, 0, resolution),
        offset_cell(cell, 0, -1, resolution),
        offset_cell(cell, 0, 1, resolution),
    ]
}

fn stage_rng(version: GeneratorVersion, seed: WorldSeed, attempt: u32, stage: u64) -> ChaCha8Rng {
    let key = stream_key(version, seed, attempt, stage);
    let mut bytes = [0_u8; 32];
    let mut value = key;
    for chunk in bytes.chunks_exact_mut(8) {
        value = splitmix64(value);
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    ChaCha8Rng::from_seed(bytes)
}

fn stream_key(version: GeneratorVersion, seed: WorldSeed, attempt: u32, stage: u64) -> u64 {
    splitmix64(
        seed.0
            ^ u64::from(version.0).rotate_left(17)
            ^ u64::from(attempt).rotate_left(33)
            ^ stage.wrapping_mul(0xD6E8_FEB8_6659_FD93),
    )
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

fn hash_planet(
    version: GeneratorVersion,
    seed: WorldSeed,
    attempt: u32,
    terrain: &CubeGrid<TerrainCell>,
    mountains: &[Mountain],
    spawn: SpawnPoint,
    roots: &[GrassRootGpu],
) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut add = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    add(&version.0.to_le_bytes());
    add(&seed.0.to_le_bytes());
    add(&attempt.to_le_bytes());
    for cell in terrain.iter() {
        add(&cell.radius.to_bits().to_le_bytes());
        for value in cell.normal.to_array() {
            add(&value.to_bits().to_le_bytes());
        }
        add(&[cell.material as u8]);
    }
    for mountain in mountains {
        for value in mountain.center.to_array() {
            add(&value.to_bits().to_le_bytes());
        }
        add(&mountain.angular_radius.to_bits().to_le_bytes());
        add(&mountain.height.to_bits().to_le_bytes());
    }
    for value in spawn.position.to_array() {
        add(&value.to_bits().to_le_bytes());
    }
    for root in roots {
        for value in root.position {
            add(&value.to_bits().to_le_bytes());
        }
        add(&root.packed_normal_seed.to_le_bytes());
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generator() -> PlanetGenerator {
        PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
    }

    #[test]
    fn same_version_and_seed_are_identical() {
        let first = generator()
            .generate_with_roots(WorldSeed(42), false)
            .unwrap();
        let second = generator()
            .generate_with_roots(WorldSeed(42), false)
            .unwrap();
        assert_eq!(first.deterministic_hash, second.deterministic_hash);
        assert_eq!(first.spawn, second.spawn);
        assert_eq!(first.mountains, second.mountains);
        assert_eq!(first.terrain, second.terrain);
    }

    #[test]
    fn grass_roots_are_compact_and_deterministic() {
        assert_eq!(std::mem::size_of::<GrassRootGpu>(), 16);

        let mut config = GeneratorConfig::test_quality();
        // Keep the regression quick while exercising fractional root counts,
        // low-discrepancy placement, patch shuffling, and packed variation.
        config.grass_roots_per_square_meter = 3.25;
        let generator = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config);
        let first = generator
            .generate_with_roots(WorldSeed(0xC0FF_EE42), true)
            .unwrap();
        let second = generator
            .generate_with_roots(WorldSeed(0xC0FF_EE42), true)
            .unwrap();

        assert!(!first.grass_roots.is_empty());
        assert_eq!(first.grass_roots, second.grass_roots);
        assert_eq!(first.grass_patches, second.grass_patches);
        assert_eq!(first.deterministic_hash, second.deterministic_hash);
    }

    #[test]
    fn different_seeds_change_the_planet() {
        let first = generator()
            .generate_with_roots(WorldSeed(1), false)
            .unwrap();
        let second = generator()
            .generate_with_roots(WorldSeed(2), false)
            .unwrap();
        assert_ne!(first.deterministic_hash, second.deterministic_hash);
    }

    #[test]
    fn generator_meets_core_invariants() {
        for seed in 0..25 {
            let planet = generator()
                .generate_with_roots(WorldSeed(seed), false)
                .unwrap();
            assert!(planet.validation.valid, "{:?}", planet.validation.errors);
            assert!((0.85..=0.95).contains(&planet.validation.mowable_ratio));
            assert!(planet.validation.reachable_ratio >= 0.98);
            assert_eq!(planet.validation.corridor_core_regions, 1);
            assert!((3..=7).contains(&planet.mountains.len()));
        }
    }

    #[test]
    fn one_thousand_arbitrary_seeds_pass_playability_validation() {
        let generator = generator();
        let mut value = 0xA17C_9E37_5EED_1234_u64;
        for index in 0..1_000 {
            value = splitmix64(value ^ index);
            let planet = generator
                .generate_with_roots(WorldSeed(value), false)
                .unwrap_or_else(|error| panic!("seed {value:016X} failed: {error}"));
            assert!(
                planet.validation.valid,
                "seed {value:016X}: {:?}",
                planet.validation.errors
            );
            assert!((0.85..=0.95).contains(&planet.validation.mowable_ratio));
            assert!(planet.validation.reachable_ratio >= 0.98);
            assert_eq!(planet.validation.corridor_core_regions, 1);
        }
    }

    #[test]
    fn phrase_seeds_are_stable_and_nonempty() {
        assert_eq!(
            "cozy planet".parse::<WorldSeed>().unwrap(),
            "cozy planet".parse().unwrap()
        );
        assert!("".parse::<WorldSeed>().is_err());
    }

    #[test]
    fn decimal_and_prefixed_hex_seeds_are_unambiguous_and_round_trip() {
        assert_eq!("10".parse::<WorldSeed>().unwrap(), WorldSeed(10));
        assert_eq!("0x10".parse::<WorldSeed>().unwrap(), WorldSeed(16));
        let seed = WorldSeed(0x1234_5678_90AB_CDEF);
        assert_eq!(seed.to_string().parse::<WorldSeed>().unwrap(), seed);
    }
}
