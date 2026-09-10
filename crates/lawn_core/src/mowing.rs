//! CPU-authoritative six-face mowing field.

use std::{collections::VecDeque, sync::OnceLock};

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::{
    cube_map::{CubeCell, CubeFace, cell_center_direction, face_solid_angles, offset_cell},
    planet::{Planet, SurfaceMaterial},
};

pub const CUT_COVERAGE_THRESHOLD: u8 = 230;
pub const DEFAULT_DIRTY_TILE_SIZE: u32 = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PackedMowingCell(pub u32);

impl PackedMowingCell {
    #[must_use]
    pub const fn cut_amount(self) -> u8 {
        self.0 as u8
    }

    #[must_use]
    pub const fn comb_x(self) -> u8 {
        (self.0 >> 8) as u8
    }

    #[must_use]
    pub const fn comb_y(self) -> u8 {
        (self.0 >> 16) as u8
    }

    #[must_use]
    pub const fn recent_epoch(self) -> u8 {
        (self.0 >> 24) as u8
    }

    fn packed(cut: u8, comb: (u8, u8), recent_epoch: u8) -> Self {
        Self(
            u32::from(cut)
                | (u32::from(comb.0) << 8)
                | (u32::from(comb.1) << 16)
                | (u32::from(recent_epoch) << 24),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MowingStamp {
    pub from: Vec3,
    pub to: Vec3,
    pub comb_direction: Vec3,
    pub deck_width: f32,
    pub cut_delta: f32,
    pub recent_epoch: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StampResult {
    pub touched_grass_cells: u32,
    pub newly_covered_cells: u32,
    pub newly_cut_weight: f64,
    pub touched_rock: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirtyTileUpdate {
    pub face: CubeFace,
    pub origin_x: u32,
    pub origin_y: u32,
    pub width: u32,
    pub height: u32,
    pub cells: Vec<u32>,
}

/// A compact tile whose packed cells are valid for the duration of an upload
/// callback. Upload consumers can copy directly from this shared staging slice.
#[derive(Clone, Copy, Debug)]
pub struct DirtyTileView<'a> {
    pub face: CubeFace,
    pub origin_x: u32,
    pub origin_y: u32,
    pub width: u32,
    pub height: u32,
    pub cells: &'a [PackedMowingCell],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MowingFieldSnapshot {
    pub resolution: u32,
    pub cells: Vec<PackedMowingCell>,
    /// Sub-byte cut progress; absent in snapshots from older builds.
    #[serde(default)]
    pub cut_residuals: Vec<f32>,
    pub cut_weight: f64,
}

/// All scoring state is maintained incrementally on the CPU. `mowable` and
/// `weights` never need to be mirrored back from the GPU.
#[derive(Clone, Debug)]
pub struct MowingField {
    resolution: u32,
    cells: Vec<PackedMowingCell>,
    // One extra float per cell (6 MiB at 512² per face) avoids discarding
    // sub-byte progress from slow cuts and time-divided sweep samples.
    cut_residuals: Vec<f32>,
    mowable: Vec<bool>,
    weights: Vec<f32>,
    total_mowable_weight: f64,
    cut_weight: f64,
    dirty_tile_size: u32,
    dirty_tiles: Vec<u32>,
    dirty_flags: Vec<bool>,
    dirty_staging: Vec<PackedMowingCell>,
    locator_cache: OnceLock<Option<Vec3>>,
    nominal_radius: f32,
}

impl MowingField {
    #[must_use]
    pub fn from_planet(planet: &Planet) -> Self {
        let resolution = planet.config.mowing_resolution;
        let len = 6 * resolution as usize * resolution as usize;
        let mut mowable = vec![false; len];
        let mut weights = vec![0.0; len];
        let mut total_mowable_weight = 0.0;
        let solid_angles = face_solid_angles(resolution);
        for face in CubeFace::ALL {
            for y in 0..resolution {
                for x in 0..resolution {
                    let cell = CubeCell { face, x, y };
                    let index = flat_index(cell, resolution);
                    let direction = cell_center_direction(cell, resolution);
                    let terrain = planet.terrain_cell(direction);
                    if terrain.material == SurfaceMaterial::Grass {
                        let radial_alignment = terrain.normal.dot(direction).clamp(0.35, 1.0);
                        let weight = (solid_angles[(y * resolution + x) as usize]
                            * f64::from(terrain.radius * terrain.radius)
                            / f64::from(radial_alignment))
                            as f32;
                        mowable[index] = true;
                        weights[index] = weight;
                        total_mowable_weight += f64::from(weight);
                    }
                }
            }
        }
        Self {
            resolution,
            cells: vec![PackedMowingCell::default(); len],
            cut_residuals: vec![0.0; len],
            mowable,
            weights,
            total_mowable_weight,
            cut_weight: 0.0,
            dirty_tile_size: DEFAULT_DIRTY_TILE_SIZE,
            dirty_tiles: Vec::new(),
            dirty_flags: vec![
                false;
                6 * resolution.div_ceil(DEFAULT_DIRTY_TILE_SIZE).pow(2) as usize
            ],
            dirty_staging: Vec::with_capacity(DEFAULT_DIRTY_TILE_SIZE.pow(2) as usize),
            locator_cache: OnceLock::new(),
            nominal_radius: planet.config.base_radius,
        }
    }

    #[must_use]
    pub const fn resolution(&self) -> u32 {
        self.resolution
    }

    #[must_use]
    pub const fn total_mowable_weight(&self) -> f64 {
        self.total_mowable_weight
    }

    #[must_use]
    pub const fn cut_weight(&self) -> f64 {
        self.cut_weight
    }

    #[must_use]
    pub fn coverage(&self) -> f64 {
        if self.total_mowable_weight <= f64::EPSILON {
            0.0
        } else {
            (self.cut_weight / self.total_mowable_weight).clamp(0.0, 1.0)
        }
    }

    /// HUD display value, rounded down so it cannot announce completion early.
    #[must_use]
    pub fn display_coverage_percent(&self) -> f64 {
        (self.coverage() * 1000.0).floor() / 10.0
    }

    #[must_use]
    pub fn cell(&self, cell: CubeCell) -> PackedMowingCell {
        self.cells[flat_index(cell, self.resolution)]
    }

    #[must_use]
    pub fn is_mowable(&self, cell: CubeCell) -> bool {
        self.mowable[flat_index(cell, self.resolution)]
    }

    #[must_use]
    pub fn packed_cells(&self) -> &[PackedMowingCell] {
        &self.cells
    }

    #[must_use]
    pub fn snapshot(&self) -> MowingFieldSnapshot {
        MowingFieldSnapshot {
            resolution: self.resolution,
            cells: self.cells.clone(),
            cut_residuals: self.cut_residuals.clone(),
            cut_weight: self.cut_weight,
        }
    }

    /// Restores an authoritative field snapshot and recomputes its coverage.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::ResolutionMismatch`] if the snapshot dimensions
    /// do not exactly match this field, or [`SnapshotError::InvalidCutResiduals`]
    /// if optional fractional progress has the wrong shape or numeric domain.
    pub fn restore(&mut self, snapshot: &MowingFieldSnapshot) -> Result<(), SnapshotError> {
        if snapshot.resolution != self.resolution || snapshot.cells.len() != self.cells.len() {
            return Err(SnapshotError::ResolutionMismatch);
        }
        if !snapshot.cut_residuals.is_empty()
            && (snapshot.cut_residuals.len() != self.cells.len()
                || snapshot
                    .cut_residuals
                    .iter()
                    .any(|value| !value.is_finite() || !(0.0..1.0).contains(value)))
        {
            return Err(SnapshotError::InvalidCutResiduals);
        }
        self.cells.clone_from(&snapshot.cells);
        if snapshot.cut_residuals.is_empty() {
            self.cut_residuals.fill(0.0);
        } else {
            self.cut_residuals.clone_from(&snapshot.cut_residuals);
        }
        self.cut_weight = self
            .cells
            .iter()
            .enumerate()
            .filter(|(index, cell)| {
                self.mowable[*index] && cell.cut_amount() >= CUT_COVERAGE_THRESHOLD
            })
            .map(|(index, _)| f64::from(self.weights[index]))
            .sum();
        // Never trust a serialized cached aggregate over authoritative cells.
        self.dirty_tiles.clear();
        self.dirty_flags.fill(false);
        self.locator_cache.take();
        let tiles_per_face = self.resolution.div_ceil(self.dirty_tile_size);
        for face in CubeFace::ALL {
            for y in 0..tiles_per_face {
                for x in 0..tiles_per_face {
                    self.mark_dirty(CubeCell {
                        face,
                        x: x * self.dirty_tile_size,
                        y: y * self.dirty_tile_size,
                    });
                }
            }
        }
        Ok(())
    }

    /// Apply a swept circular deck footprint. Subdivide at quarter deck width,
    /// with a half-texel lower bound, and share elapsed cut time among samples.
    pub fn stamp(&mut self, stamp: MowingStamp) -> StampResult {
        if !stamp.deck_width.is_finite()
            || !stamp.cut_delta.is_finite()
            || !stamp.from.is_finite()
            || !stamp.to.is_finite()
            || !stamp.comb_direction.is_finite()
            || stamp.deck_width <= 0.0
            || stamp.cut_delta <= 0.0
        {
            return StampResult::default();
        }
        let from_direction = stamp.from.normalize_or_zero();
        let to_direction = stamp.to.normalize_or_zero();
        if from_direction == Vec3::ZERO || to_direction == Vec3::ZERO {
            return StampResult::default();
        }
        let angle = from_direction.dot(to_direction).clamp(-1.0, 1.0).acos();
        let distance = angle * self.nominal_radius;
        // Going below half a texel adds work without improving the footprint,
        // whose conservative boundary already extends by 1.5 texels.
        let max_interval =
            (stamp.deck_width * 0.25).max(self.nominal_radius / self.resolution as f32 * 0.5);
        let samples = (distance / max_interval).ceil().max(1.0) as u32;
        let cut_delta = stamp.cut_delta / samples as f32;
        let mut result = StampResult::default();
        for sample_index in 0..samples {
            let t = (sample_index as f32 + 0.5) / samples as f32;
            let direction = slerp_direction(from_direction, to_direction, t);
            self.stamp_disc(
                direction,
                stamp.comb_direction,
                stamp.deck_width * 0.5,
                cut_delta,
                stamp.recent_epoch,
                &mut result,
            );
        }
        result
    }

    fn stamp_disc(
        &mut self,
        center_direction: Vec3,
        comb_direction: Vec3,
        radius: f32,
        cut_delta: f32,
        recent_epoch: u8,
        result: &mut StampResult,
    ) {
        let angular_radius =
            (radius / self.nominal_radius + 1.5 / self.resolution as f32).min(std::f32::consts::PI);
        let minimum_dot = angular_radius.cos();
        let comb = encode_octahedral(
            (comb_direction - center_direction * comb_direction.dot(center_direction))
                .try_normalize()
                .unwrap_or(Vec3::ZERO),
        );
        let fractional_increment = cut_delta.clamp(0.0, 1.0) * 255.0;
        for face in CubeFace::ALL {
            let Some((min_x, max_x, min_y, max_y)) =
                cap_cell_bounds(face, center_direction, angular_radius, self.resolution)
            else {
                continue;
            };
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    let cell = CubeCell { face, x, y };
                    let index = flat_index(cell, self.resolution);
                    let direction = cell_center_direction(cell, self.resolution);
                    if center_direction.dot(direction) < minimum_dot {
                        continue;
                    }
                    if !self.mowable[index] {
                        result.touched_rock = true;
                        continue;
                    }
                    result.touched_grass_cells += 1;
                    let previous = self.cells[index];
                    let was_covered = previous.cut_amount() >= CUT_COVERAGE_THRESHOLD;
                    let cut = if previous.cut_amount() < u8::MAX {
                        let progress = self.cut_residuals[index] + fractional_increment;
                        let increment = progress.floor() as u8;
                        let cut = previous.cut_amount().saturating_add(increment);
                        self.cut_residuals[index] = if cut == u8::MAX {
                            0.0
                        } else {
                            progress - f32::from(increment)
                        };
                        cut
                    } else {
                        u8::MAX
                    };
                    let updated = PackedMowingCell::packed(cut, comb, recent_epoch);
                    if updated != previous {
                        self.cells[index] = updated;
                        self.mark_dirty(cell);
                    }
                    if !was_covered && cut >= CUT_COVERAGE_THRESHOLD {
                        result.newly_covered_cells += 1;
                        let weight = f64::from(self.weights[index]);
                        result.newly_cut_weight += weight;
                        self.cut_weight += weight;
                        self.locator_cache.take();
                    }
                }
            }
        }
    }

    fn mark_dirty(&mut self, cell: CubeCell) {
        let tiles_per_face = self.resolution.div_ceil(self.dirty_tile_size);
        let x = cell.x / self.dirty_tile_size;
        let y = cell.y / self.dirty_tile_size;
        let index =
            (cell.face as u32 * tiles_per_face * tiles_per_face + y * tiles_per_face + x) as usize;
        if !self.dirty_flags[index] {
            self.dirty_flags[index] = true;
            self.dirty_tiles.push(tile_key(cell.face, x, y));
        }
    }

    /// Drain compact tile uploads for the GPU mirror. Renderers should prefer
    /// `visit_dirty_tiles` to reuse staging memory instead of owning each tile.
    pub fn take_dirty_tiles(&mut self) -> Vec<DirtyTileUpdate> {
        let mut updates = Vec::with_capacity(self.dirty_tiles.len());
        self.visit_dirty_tiles(|tile| {
            updates.push(DirtyTileUpdate {
                face: tile.face,
                origin_x: tile.origin_x,
                origin_y: tile.origin_y,
                width: tile.width,
                height: tile.height,
                cells: tile.cells.iter().map(|cell| cell.0).collect(),
            });
        });
        updates
    }

    /// Drain sorted dirty tiles through a reusable staging slice. Calls after
    /// construction allocate no tile storage, including for partial edge tiles.
    pub fn visit_dirty_tiles(&mut self, mut visit: impl FnMut(DirtyTileView<'_>)) {
        if self.dirty_tiles.is_empty() {
            return;
        }
        self.dirty_flags.fill(false);
        self.dirty_tiles.sort_unstable();
        for &key in &self.dirty_tiles {
            let (face, tile_x, tile_y) = decode_tile_key(key);
            let origin_x = tile_x * self.dirty_tile_size;
            let origin_y = tile_y * self.dirty_tile_size;
            let width = self.dirty_tile_size.min(self.resolution - origin_x);
            let height = self.dirty_tile_size.min(self.resolution - origin_y);
            self.dirty_staging.clear();
            for y in origin_y..origin_y + height {
                let start = flat_index(
                    CubeCell {
                        face,
                        x: origin_x,
                        y,
                    },
                    self.resolution,
                );
                self.dirty_staging
                    .extend_from_slice(&self.cells[start..start + width as usize]);
            }
            visit(DirtyTileView {
                face,
                origin_x,
                origin_y,
                width,
                height,
                cells: &self.dirty_staging,
            });
        }
        self.dirty_tiles.clear();
    }

    /// Weighted centroid of the largest connected uncut region, used by the 95%
    /// locator assist. Returns `None` only when no required grass remains.
    #[must_use]
    pub fn largest_uncut_direction(&self) -> Option<Vec3> {
        *self
            .locator_cache
            .get_or_init(|| self.compute_largest_uncut_direction())
    }

    fn compute_largest_uncut_direction(&self) -> Option<Vec3> {
        let mut visited = vec![false; self.cells.len()];
        let mut best_weight = 0.0;
        let mut best_centroid = Vec3::ZERO;
        let mut best_fallback = Vec3::Y;
        for start in 0..self.cells.len() {
            if visited[start]
                || !self.mowable[start]
                || self.cells[start].cut_amount() >= CUT_COVERAGE_THRESHOLD
            {
                continue;
            }
            let mut queue = VecDeque::from([start]);
            visited[start] = true;
            let mut weight_sum = 0.0_f64;
            let mut centroid = Vec3::ZERO;
            while let Some(index) = queue.pop_front() {
                let weight = self.weights[index];
                weight_sum += f64::from(weight);
                centroid +=
                    cell_center_direction(cell_from_index(index, self.resolution), self.resolution)
                        * weight;
                let cell = cell_from_index(index, self.resolution);
                for neighbor in [
                    offset_cell(cell, -1, 0, self.resolution),
                    offset_cell(cell, 1, 0, self.resolution),
                    offset_cell(cell, 0, -1, self.resolution),
                    offset_cell(cell, 0, 1, self.resolution),
                ] {
                    let next = flat_index(neighbor, self.resolution);
                    if !visited[next]
                        && self.mowable[next]
                        && self.cells[next].cut_amount() < CUT_COVERAGE_THRESHOLD
                    {
                        visited[next] = true;
                        queue.push_back(next);
                    }
                }
            }
            if weight_sum > best_weight {
                best_weight = weight_sum;
                best_centroid = centroid;
                best_fallback =
                    cell_center_direction(cell_from_index(start, self.resolution), self.resolution);
            }
        }
        (best_weight > 0.0).then(|| best_centroid.normalize_or(best_fallback))
    }
}

/// Project the spherical cap onto each cube face independently. Remapping a
/// square of offsets from just its center face can skip texels near cube corners.
fn cap_cell_bounds(
    face: CubeFace,
    center: Vec3,
    angle: f32,
    resolution: u32,
) -> Option<(u32, u32, u32, u32)> {
    let (normal, u, v) = match face {
        CubeFace::PositiveX => (center.x, -center.z, center.y),
        CubeFace::NegativeX => (-center.x, center.z, center.y),
        CubeFace::PositiveY => (center.y, center.x, -center.z),
        CubeFace::NegativeY => (-center.y, center.x, center.z),
        CubeFace::PositiveZ => (center.z, center.x, center.y),
        CubeFace::NegativeZ => (-center.z, -center.x, center.y),
    };
    if angle >= std::f32::consts::FRAC_PI_2 {
        return Some((0, resolution - 1, 0, resolution - 1));
    }
    let (sine, cosine) = angle.sin_cos();
    let maximum_normal = if normal >= cosine {
        1.0
    } else {
        normal * cosine + (1.0 - normal * normal).max(0.0).sqrt() * sine
    };
    // Every direction assigned to this face has normal component >= 1/sqrt(3).
    if maximum_normal + 1.0e-6 < 1.0 / 3.0_f32.sqrt() {
        return None;
    }
    if normal <= sine {
        return Some((0, resolution - 1, 0, resolution - 1));
    }
    let projection_bounds = |axis: f32| {
        let denominator = normal * normal - sine * sine;
        let extent = sine
            * (normal * normal + axis * axis - sine * sine)
                .max(0.0)
                .sqrt();
        let minimum = (normal * axis - extent) / denominator;
        let maximum = (normal * axis + extent) / denominator;
        if minimum > 1.0 || maximum < -1.0 {
            return None;
        }
        // One guard texel protects the analytic bound from floating-point rounding.
        let first = ((minimum.clamp(-1.0, 1.0) + 1.0) * 0.5 * resolution as f32).floor() as u32;
        let last = ((maximum.clamp(-1.0, 1.0) + 1.0) * 0.5 * resolution as f32).ceil() as u32;
        Some((first.saturating_sub(1), last.min(resolution - 1)))
    };
    let (min_x, max_x) = projection_bounds(u)?;
    let (min_y, max_y) = projection_bounds(v)?;
    Some((min_x, max_x, min_y, max_y))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("mowing snapshot resolution does not match this planet")]
    ResolutionMismatch,
    #[error(
        "mowing snapshot fractional progress must match the field and contain finite values in [0, 1)"
    )]
    InvalidCutResiduals,
}

fn slerp_direction(from: Vec3, to: Vec3, t: f32) -> Vec3 {
    let dot = from.dot(to).clamp(-1.0, 1.0);
    if dot > 0.9995 {
        return from.lerp(to, t).normalize();
    }
    let cross = from.cross(to);
    let axis = cross.try_normalize().unwrap_or_else(|| {
        if from.x.abs() < 0.8 {
            from.cross(Vec3::X).normalize()
        } else {
            from.cross(Vec3::Y).normalize()
        }
    });
    let angle = cross.length().atan2(dot);
    (glam::Quat::from_axis_angle(axis, angle * t) * from).normalize()
}

fn encode_octahedral(normal: Vec3) -> (u8, u8) {
    if normal == Vec3::ZERO {
        return (128, 128);
    }
    let mut oct = normal / (normal.x.abs() + normal.y.abs() + normal.z.abs());
    if oct.z < 0.0 {
        let old_x = oct.x;
        oct.x = (1.0 - oct.y.abs()) * old_x.signum();
        oct.y = (1.0 - old_x.abs()) * oct.y.signum();
    }
    (
        ((oct.x * 0.5 + 0.5) * 255.0).round() as u8,
        ((oct.y * 0.5 + 0.5) * 255.0).round() as u8,
    )
}

fn flat_index(cell: CubeCell, resolution: u32) -> usize {
    let side = resolution as usize;
    cell.face.index() * side * side + cell.y as usize * side + cell.x as usize
}

fn cell_from_index(index: usize, resolution: u32) -> CubeCell {
    let side = resolution as usize;
    let face_area = side * side;
    CubeCell {
        face: CubeFace::ALL[index / face_area],
        x: (index % face_area % side) as u32,
        y: (index % face_area / side) as u32,
    }
}

fn tile_key(face: CubeFace, x: u32, y: u32) -> u32 {
    (face as u32) << 28 | (y & 0x3fff) << 14 | (x & 0x3fff)
}

fn decode_tile_key(key: u32) -> (CubeFace, u32, u32) {
    (
        CubeFace::ALL[(key >> 28) as usize],
        key & 0x3fff,
        (key >> 14) & 0x3fff,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        GeneratorConfig, PlanetGenerator, cube_map::direction_to_cell,
        planet::CURRENT_GENERATOR_VERSION,
    };

    fn field() -> MowingField {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(crate::WorldSeed(7), false)
                .unwrap();
        MowingField::from_planet(&planet)
    }

    #[test]
    fn borrowed_dirty_tiles_reconstruct_exact_cells_and_reuse_storage() {
        let mut planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(crate::WorldSeed(7), false)
                .unwrap();
        for resolution in [8, 19, 64] {
            planet.config.mowing_resolution = resolution;
            let mut field = MowingField::from_planet(&planet);
            let mut mirror = field.cells.clone();
            let staging_address = field.dirty_staging.as_ptr();
            let staging_capacity = field.dirty_staging.capacity();
            for pass in 1..=2_u32 {
                // Reverse marking order and repeated marks exercise sorting and
                // deduplication, including each face's partial edge tiles.
                for index in (0..field.cells.len()).rev() {
                    field.cells[index] = PackedMowingCell(
                        (index as u32).wrapping_mul(0x9e37_79b9).wrapping_add(pass),
                    );
                    let cell = cell_from_index(index, resolution);
                    field.mark_dirty(cell);
                    field.mark_dirty(cell);
                }
                let mut previous = None;
                let mut tile_count = 0;
                field.visit_dirty_tiles(|tile| {
                    let address = (tile.face as u8, tile.origin_y, tile.origin_x);
                    assert!(previous.is_none_or(|previous| previous < address));
                    previous = Some(address);
                    assert_eq!(tile.cells.as_ptr(), staging_address);
                    assert_eq!(tile.cells.len(), (tile.width * tile.height) as usize);
                    for y in 0..tile.height {
                        let target = flat_index(
                            CubeCell {
                                face: tile.face,
                                x: tile.origin_x,
                                y: tile.origin_y + y,
                            },
                            resolution,
                        );
                        let source = (y * tile.width) as usize;
                        mirror[target..target + tile.width as usize]
                            .copy_from_slice(&tile.cells[source..source + tile.width as usize]);
                    }
                    tile_count += 1;
                });
                assert_eq!(
                    tile_count,
                    6 * resolution.div_ceil(DEFAULT_DIRTY_TILE_SIZE).pow(2)
                );
                assert_eq!(mirror, field.cells);
                assert_eq!(field.dirty_staging.capacity(), staging_capacity);
                assert!(field.dirty_tiles.is_empty());
                assert!(field.dirty_flags.iter().all(|flag| !flag));
                field.visit_dirty_tiles(|_| panic!("clean field produced an upload"));
                assert!(field.take_dirty_tiles().is_empty());
            }
        }
    }

    #[test]
    fn disc_footprints_match_exhaustive_spherical_caps_at_seams_and_corners() {
        let template = field();
        for center in [Vec3::X, Vec3::new(1.0, 0.0, 1.0), Vec3::ONE] {
            let mut field = template.clone();
            let center = center.normalize();
            let deck_width = 2.2;
            let minimum_dot =
                (deck_width * 0.5 / field.nominal_radius + 1.5 / field.resolution as f32).cos();
            field.stamp(MowingStamp {
                from: center * field.nominal_radius,
                to: center * field.nominal_radius,
                comb_direction: Vec3::Y,
                deck_width,
                cut_delta: 1.0,
                recent_epoch: 1,
            });
            for index in 0..field.cells.len() {
                let cell = cell_from_index(index, field.resolution);
                let expected = field.mowable[index]
                    && cell_center_direction(cell, field.resolution).dot(center) >= minimum_dot;
                assert_eq!(
                    field.cells[index].cut_amount() > 0,
                    expected,
                    "center {center}, cell {cell:?}"
                );
            }
        }
    }

    #[test]
    fn projected_cap_bounds_include_all_cells_across_every_cube_face() {
        let resolution = 32;
        for source_face in CubeFace::ALL {
            for uv in [
                glam::Vec2::ZERO,
                glam::Vec2::new(-1.0, 0.0),
                glam::Vec2::new(1.0, 0.0),
                glam::Vec2::new(0.0, -1.0),
                glam::Vec2::new(0.0, 1.0),
                glam::Vec2::new(-1.0, -1.0),
                glam::Vec2::new(-1.0, 1.0),
                glam::Vec2::new(1.0, -1.0),
                glam::Vec2::ONE,
            ] {
                let center = crate::cube_map::face_uv_to_direction(source_face, uv);
                for angle in [0.02_f32, 0.1, 0.4, 1.2, 2.0] {
                    for target_face in CubeFace::ALL {
                        let bounds = cap_cell_bounds(target_face, center, angle, resolution);
                        for y in 0..resolution {
                            for x in 0..resolution {
                                let direction = cell_center_direction(
                                    CubeCell {
                                        face: target_face,
                                        x,
                                        y,
                                    },
                                    resolution,
                                );
                                if center.dot(direction) >= angle.cos() {
                                    assert!(
                                        bounds.is_some_and(|(min_x, max_x, min_y, max_y)| {
                                            (min_x..=max_x).contains(&x)
                                                && (min_y..=max_y).contains(&y)
                                        }),
                                        "missing {target_face:?} ({x}, {y}), cap {source_face:?} {uv}, {angle}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unchanged_packed_cells_do_not_generate_redundant_gpu_uploads() {
        let mut field = field();
        let stamp = MowingStamp {
            from: Vec3::X * 15.0,
            to: Vec3::X * 15.0,
            comb_direction: Vec3::Y,
            deck_width: 2.2,
            cut_delta: 1.0,
            recent_epoch: 1,
        };
        field.stamp(stamp);
        assert!(!field.take_dirty_tiles().is_empty());
        field.stamp(stamp);
        assert!(field.take_dirty_tiles().is_empty());
    }

    #[test]
    fn fractional_cuts_accumulate_and_survive_snapshot_restore() {
        let mut field = field();
        let stamp = MowingStamp {
            from: Vec3::X * 15.0,
            to: Vec3::X * 15.0,
            comb_direction: Vec3::Y,
            deck_width: 2.2,
            cut_delta: 1.0 / 1024.0,
            recent_epoch: 1,
        };
        field.stamp(stamp);
        let snapshot = field.snapshot();
        assert!(snapshot.cells.iter().all(|cell| cell.cut_amount() == 0));
        assert!(snapshot.cut_residuals.iter().any(|value| *value > 0.0));
        let mut restored = field.clone();
        restored.restore(&snapshot).unwrap();
        for _ in 1..1024 {
            field.stamp(stamp);
            restored.stamp(stamp);
        }
        assert!(field.cut_weight() > 0.0);
        assert_eq!(field.cells, restored.cells);
        assert_eq!(field.cut_residuals, restored.cut_residuals);

        // The same elapsed cut time grouped into larger stamps reaches the
        // same packed result, including the coverage threshold.
        let mut grouped = field.clone();
        grouped.cells.fill(PackedMowingCell::default());
        grouped.cut_residuals.fill(0.0);
        grouped.cut_weight = 0.0;
        grouped.stamp(MowingStamp {
            cut_delta: 1.0,
            ..stamp
        });
        assert_eq!(field.cells, grouped.cells);
        assert_eq!(field.coverage(), grouped.coverage());
    }

    #[test]
    fn subdivided_sweeps_retain_sub_byte_cut_progress() {
        let mut field = field();
        let stamp = MowingStamp {
            from: Vec3::X * 15.0,
            to: Vec3::Y * 15.0,
            comb_direction: Vec3::Y,
            deck_width: 2.2,
            cut_delta: 0.02,
            recent_epoch: 1,
        };
        for _ in 0..20 {
            field.stamp(stamp);
        }
        assert!(field.cells.iter().any(|cell| cell.cut_amount() > 0));
    }

    #[test]
    fn old_snapshots_default_fractional_progress_and_invalid_snapshots_are_atomic() {
        let mut field = field();
        let mut snapshot = field.snapshot();
        let mut legacy_json = serde_json::to_value(&snapshot).unwrap();
        legacy_json.as_object_mut().unwrap().remove("cut_residuals");
        snapshot = serde_json::from_value(legacy_json).unwrap();
        assert!(snapshot.cut_residuals.is_empty());
        snapshot.cut_residuals.clear();
        field.restore(&snapshot).unwrap();
        assert!(field.cut_residuals.iter().all(|value| *value == 0.0));
        let before = field.snapshot();
        for residuals in [
            vec![0.0],
            vec![f32::NAN; field.cells.len()],
            vec![1.0; field.cells.len()],
        ] {
            snapshot.cut_residuals = residuals;
            assert_eq!(
                field.restore(&snapshot),
                Err(SnapshotError::InvalidCutResiduals)
            );
            assert_eq!(field.cells, before.cells);
            assert_eq!(field.cut_residuals, before.cut_residuals);
        }
    }

    #[test]
    #[ignore = "manual shipping-resolution CPU benchmark"]
    fn shipping_resolution_mowing_benchmark() {
        let config = GeneratorConfig {
            mowing_resolution: 512,
            ..GeneratorConfig::test_quality()
        };
        let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config)
            .generate_with_roots(crate::WorldSeed(7), false)
            .unwrap();
        let mut field = MowingField::from_planet(&planet);
        let started = std::time::Instant::now();
        let mut previous = Vec3::X * 15.0;
        for tick in 0..2400 {
            let angle = (tick + 1) as f32 * 0.005;
            let next = Vec3::new(angle.cos(), angle.sin(), 0.2).normalize() * 15.0;
            std::hint::black_box(field.stamp(MowingStamp {
                from: previous,
                to: next,
                comb_direction: next - previous,
                deck_width: 2.2,
                cut_delta: 8.0 / 120.0,
                recent_epoch: (tick / 4) as u8,
            }));
            if tick % 2 == 0 {
                std::hint::black_box(field.take_dirty_tiles());
            }
            previous = next;
        }
        eprintln!(
            "mowing: {:.2} microseconds/tick",
            started.elapsed().as_secs_f64() * 1.0e6 / 2400.0
        );
    }

    #[test]
    fn repeated_full_cut_does_not_double_count() {
        let mut field = field();
        let center = Vec3::new(30.0, 0.0, 0.0);
        let stamp = MowingStamp {
            from: center,
            to: center,
            comb_direction: Vec3::Y,
            deck_width: 2.2,
            cut_delta: 1.0,
            recent_epoch: 1,
        };
        field.stamp(stamp);
        let once = field.cut_weight();
        field.stamp(stamp);
        assert_eq!(field.cut_weight(), once);
    }

    #[test]
    fn cube_face_seam_is_cut_on_both_sides() {
        let mut field = field();
        let direction = Vec3::new(1.0, 0.0, 1.0).normalize();
        field.stamp(MowingStamp {
            from: direction * 30.0,
            to: direction * 30.0,
            comb_direction: Vec3::Y,
            deck_width: 4.0,
            cut_delta: 1.0,
            recent_epoch: 2,
        });
        let positive_x = (0..field.resolution).any(|y| {
            field
                .cell(CubeCell {
                    face: CubeFace::PositiveX,
                    x: 0,
                    y,
                })
                .cut_amount()
                > 0
                || field
                    .cell(CubeCell {
                        face: CubeFace::PositiveX,
                        x: field.resolution - 1,
                        y,
                    })
                    .cut_amount()
                    > 0
        });
        let positive_z = (0..field.resolution).any(|y| {
            field
                .cell(CubeCell {
                    face: CubeFace::PositiveZ,
                    x: 0,
                    y,
                })
                .cut_amount()
                > 0
                || field
                    .cell(CubeCell {
                        face: CubeFace::PositiveZ,
                        x: field.resolution - 1,
                        y,
                    })
                    .cut_amount()
                    > 0
        });
        assert!(positive_x && positive_z);
    }

    #[test]
    fn snapshot_recomputes_authoritative_coverage() {
        let mut original = field();
        let p = Vec3::Y * 30.0;
        original.stamp(MowingStamp {
            from: p,
            to: p,
            comb_direction: Vec3::X,
            deck_width: 3.0,
            cut_delta: 1.0,
            recent_epoch: 4,
        });
        let mut snapshot = original.snapshot();
        snapshot.cut_weight = f64::MAX;
        let mut restored = field();
        restored.restore(&snapshot).unwrap();
        assert!((restored.coverage() - original.coverage()).abs() < 1.0e-12);
    }

    #[test]
    fn hud_coverage_rounds_down() {
        let field = field();
        assert_eq!(field.display_coverage_percent(), 0.0);
    }

    #[test]
    fn cube_corner_stamp_updates_all_three_meeting_faces_and_dirty_tiles() {
        let mut field = field();
        let direction = Vec3::ONE.normalize();
        field.stamp(MowingStamp {
            from: direction * 30.0,
            to: direction * 30.0,
            comb_direction: Vec3::X - Vec3::Y,
            deck_width: 5.0,
            cut_delta: 1.0,
            recent_epoch: 9,
        });
        let tiles = field.take_dirty_tiles();
        for face in [
            CubeFace::PositiveX,
            CubeFace::PositiveY,
            CubeFace::PositiveZ,
        ] {
            assert!(
                tiles.iter().any(|tile| tile.face == face),
                "missing {face:?}"
            );
            assert!((0..field.resolution).any(|y| {
                (0..field.resolution).any(|x| field.cell(CubeCell { face, x, y }).cut_amount() > 0)
            }));
        }
    }

    #[test]
    fn long_fast_diagonal_stamp_has_no_mowable_holes() {
        let mut field = field();
        let from = Vec3::new(1.0, -0.18, -0.18).normalize();
        let to = Vec3::new(1.0, 0.18, 0.18).normalize();
        field.stamp(MowingStamp {
            from: from * 30.0,
            to: to * 30.0,
            comb_direction: Vec3::Y,
            deck_width: 2.2,
            cut_delta: 1.0,
            recent_epoch: 12,
        });
        for sample in 0..=80 {
            let direction = slerp_direction(from, to, sample as f32 / 80.0);
            let cell = direction_to_cell(direction, field.resolution);
            if field.is_mowable(cell) {
                assert!(
                    field.cell(cell).cut_amount() > 0,
                    "hole at sample {sample}: {cell:?}"
                );
            }
        }
    }

    #[test]
    fn antipodal_sweep_remains_finite() {
        for sample in 0..=32 {
            let direction = slerp_direction(Vec3::X, Vec3::NEG_X, sample as f32 / 32.0);
            assert!(direction.is_finite());
            assert!((direction.length() - 1.0).abs() < 1.0e-5);
        }
    }

    #[test]
    fn nearly_antipodal_sweep_reaches_its_actual_endpoint_on_the_correct_plane() {
        let from = Vec3::X;
        let to = glam::Quat::from_rotation_z(std::f32::consts::PI - 0.01) * from;
        assert!(slerp_direction(from, to, 0.0).distance(from) < 1.0e-6);
        assert!(slerp_direction(from, to, 1.0).distance(to) < 1.0e-6);
        let middle = slerp_direction(from, to, 0.5);
        assert!(middle.z.abs() < 1.0e-6);
        assert!(middle.y > 0.99);
    }
}
