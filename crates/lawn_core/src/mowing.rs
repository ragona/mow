//! CPU-authoritative six-face mowing field.

use std::collections::{HashSet, VecDeque};

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::{
    cube_map::{
        CubeCell, CubeFace, cell_center_direction, cell_solid_angle, direction_to_cell, offset_cell,
    },
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MowingFieldSnapshot {
    pub resolution: u32,
    pub cells: Vec<PackedMowingCell>,
    pub cut_weight: f64,
}

/// All scoring state is maintained incrementally on the CPU. `mowable` and
/// `weights` never need to be mirrored back from the GPU.
#[derive(Clone, Debug)]
pub struct MowingField {
    resolution: u32,
    cells: Vec<PackedMowingCell>,
    mowable: Vec<bool>,
    weights: Vec<f32>,
    total_mowable_weight: f64,
    cut_weight: f64,
    dirty_tile_size: u32,
    dirty_tiles: HashSet<u32>,
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
        for face in CubeFace::ALL {
            for y in 0..resolution {
                for x in 0..resolution {
                    let cell = CubeCell { face, x, y };
                    let index = flat_index(cell, resolution);
                    let direction = cell_center_direction(cell, resolution);
                    let terrain = planet.terrain_cell(direction);
                    if terrain.material == SurfaceMaterial::Grass {
                        let radial_alignment = terrain.normal.dot(direction).clamp(0.35, 1.0);
                        let weight = (cell_solid_angle(x, y, resolution)
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
            mowable,
            weights,
            total_mowable_weight,
            cut_weight: 0.0,
            dirty_tile_size: DEFAULT_DIRTY_TILE_SIZE,
            dirty_tiles: HashSet::new(),
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
            cut_weight: self.cut_weight,
        }
    }

    /// Restores an authoritative field snapshot and recomputes its coverage.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::ResolutionMismatch`] if the snapshot dimensions
    /// do not exactly match this field.
    pub fn restore(&mut self, snapshot: &MowingFieldSnapshot) -> Result<(), SnapshotError> {
        if snapshot.resolution != self.resolution || snapshot.cells.len() != self.cells.len() {
            return Err(SnapshotError::ResolutionMismatch);
        }
        self.cells.clone_from(&snapshot.cells);
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
        let tiles_per_face = self.resolution.div_ceil(self.dirty_tile_size);
        for face in CubeFace::ALL {
            for y in 0..tiles_per_face {
                for x in 0..tiles_per_face {
                    self.dirty_tiles.insert(tile_key(face, x, y));
                }
            }
        }
        Ok(())
    }

    /// Apply a swept circular deck footprint. Spatial subdivision is at most one
    /// quarter deck width and each subdivision receives its share of elapsed cut
    /// time, making results insensitive to physics/render frame grouping.
    pub fn stamp(&mut self, stamp: MowingStamp) -> StampResult {
        if stamp.deck_width <= 0.0 || stamp.cut_delta <= 0.0 {
            return StampResult::default();
        }
        let from_direction = stamp.from.normalize_or_zero();
        let to_direction = stamp.to.normalize_or_zero();
        if from_direction == Vec3::ZERO || to_direction == Vec3::ZERO {
            return StampResult::default();
        }
        let angle = from_direction.dot(to_direction).clamp(-1.0, 1.0).acos();
        let distance = angle * self.nominal_radius;
        let max_interval = stamp.deck_width * 0.25;
        let samples = (distance / max_interval).ceil().max(1.0) as u32;
        let cut_delta = stamp.cut_delta / samples as f32;
        let mut result = StampResult::default();
        let mut touched = HashSet::new();
        for sample_index in 0..samples {
            let t = (sample_index as f32 + 0.5) / samples as f32;
            let direction = slerp_direction(from_direction, to_direction, t);
            self.stamp_disc(
                direction,
                stamp.comb_direction,
                stamp.deck_width * 0.5,
                cut_delta,
                stamp.recent_epoch,
                &mut touched,
                &mut result,
            );
            // Cells can legitimately receive cut time from overlapping samples;
            // clear only duplicate candidate mappings within the next disc.
            touched.clear();
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn stamp_disc(
        &mut self,
        center_direction: Vec3,
        comb_direction: Vec3,
        radius: f32,
        cut_delta: f32,
        recent_epoch: u8,
        touched: &mut HashSet<usize>,
        result: &mut StampResult,
    ) {
        let center_cell = direction_to_cell(center_direction, self.resolution);
        // Conservative bound at cube corners, where angular texel size is smallest.
        let cell_radius =
            (radius * self.resolution as f32 / self.nominal_radius * 0.9).ceil() as i32 + 2;
        let angular_radius = radius / self.nominal_radius;
        let comb = encode_octahedral(
            (comb_direction - center_direction * comb_direction.dot(center_direction))
                .try_normalize()
                .unwrap_or(Vec3::ZERO),
        );
        let cut_increment = (cut_delta.clamp(0.0, 1.0) * 255.0).round() as u8;
        for dy in -cell_radius..=cell_radius {
            for dx in -cell_radius..=cell_radius {
                let cell = offset_cell(center_cell, dx, dy, self.resolution);
                let index = flat_index(cell, self.resolution);
                if !touched.insert(index) {
                    continue;
                }
                let direction = cell_center_direction(cell, self.resolution);
                if center_direction.dot(direction).clamp(-1.0, 1.0).acos()
                    > angular_radius + 1.5 / self.resolution as f32
                {
                    continue;
                }
                if !self.mowable[index] {
                    result.touched_rock = true;
                    continue;
                }
                result.touched_grass_cells += 1;
                let previous = self.cells[index];
                let was_covered = previous.cut_amount() >= CUT_COVERAGE_THRESHOLD;
                let cut = previous.cut_amount().saturating_add(cut_increment);
                self.cells[index] = PackedMowingCell::packed(cut, comb, recent_epoch);
                if !was_covered && cut >= CUT_COVERAGE_THRESHOLD {
                    result.newly_covered_cells += 1;
                    let weight = f64::from(self.weights[index]);
                    result.newly_cut_weight += weight;
                    self.cut_weight += weight;
                }
                self.mark_dirty(cell);
            }
        }
    }

    fn mark_dirty(&mut self, cell: CubeCell) {
        self.dirty_tiles.insert(tile_key(
            cell.face,
            cell.x / self.dirty_tile_size,
            cell.y / self.dirty_tile_size,
        ));
    }

    /// Drain compact tile uploads for the GPU mirror.
    pub fn take_dirty_tiles(&mut self) -> Vec<DirtyTileUpdate> {
        let mut keys: Vec<_> = self.dirty_tiles.drain().collect();
        keys.sort_unstable();
        keys.into_iter()
            .map(|key| {
                let (face, tile_x, tile_y) = decode_tile_key(key);
                let origin_x = tile_x * self.dirty_tile_size;
                let origin_y = tile_y * self.dirty_tile_size;
                let width = self.dirty_tile_size.min(self.resolution - origin_x);
                let height = self.dirty_tile_size.min(self.resolution - origin_y);
                let mut cells = Vec::with_capacity((width * height) as usize);
                for y in origin_y..origin_y + height {
                    for x in origin_x..origin_x + width {
                        cells.push(self.cell(CubeCell { face, x, y }).0);
                    }
                }
                DirtyTileUpdate {
                    face,
                    origin_x,
                    origin_y,
                    width,
                    height,
                    cells,
                }
            })
            .collect()
    }

    /// Weighted centroid of the largest connected uncut region, used by the 95%
    /// locator assist. Returns `None` only when no required grass remains.
    #[must_use]
    pub fn largest_uncut_direction(&self) -> Option<Vec3> {
        let mut visited = vec![false; self.cells.len()];
        let mut best_weight = 0.0;
        let mut best_centroid = Vec3::ZERO;
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
            }
        }
        (best_weight > 0.0).then(|| best_centroid.normalize())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("mowing snapshot resolution does not match this planet")]
    ResolutionMismatch,
}

fn slerp_direction(from: Vec3, to: Vec3, t: f32) -> Vec3 {
    let dot = from.dot(to).clamp(-1.0, 1.0);
    if dot > 0.9995 {
        return from.lerp(to, t).normalize();
    }
    if dot < -0.9995 {
        let axis = if from.x.abs() < 0.8 {
            from.cross(Vec3::X).normalize()
        } else {
            from.cross(Vec3::Y).normalize()
        };
        return glam::Quat::from_axis_angle(axis, std::f32::consts::PI * t)
            .mul_vec3(from)
            .normalize();
    }
    let theta = dot.acos();
    let sin_theta = theta.sin();
    (from * ((1.0 - t) * theta).sin() + to * (t * theta).sin()) / sin_theta
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
    use crate::{GeneratorConfig, PlanetGenerator, planet::CURRENT_GENERATOR_VERSION};

    fn field() -> MowingField {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(crate::WorldSeed(7), false)
                .unwrap();
        MowingField::from_planet(&planet)
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
}
