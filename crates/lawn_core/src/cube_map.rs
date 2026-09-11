//! Seam-safe cube-map addressing used by terrain, grass, mowing, and interaction.

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum CubeFace {
    PositiveX = 0,
    NegativeX = 1,
    PositiveY = 2,
    NegativeY = 3,
    PositiveZ = 4,
    NegativeZ = 5,
}

impl CubeFace {
    pub const ALL: [Self; 6] = [
        Self::PositiveX,
        Self::NegativeX,
        Self::PositiveY,
        Self::NegativeY,
        Self::PositiveZ,
        Self::NegativeZ,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubeAddress {
    pub face: CubeFace,
    /// Face coordinates in `[-1, 1]` for directions on this face.
    pub uv: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CubeCell {
    pub face: CubeFace,
    pub x: u32,
    pub y: u32,
}

#[must_use]
pub fn direction_to_face_uv(direction: Vec3) -> CubeAddress {
    let d = direction.normalize_or_zero();
    let a = d.abs();
    if a.x >= a.y && a.x >= a.z {
        if d.x >= 0.0 {
            CubeAddress {
                face: CubeFace::PositiveX,
                uv: Vec2::new(-d.z, d.y) / a.x,
            }
        } else {
            CubeAddress {
                face: CubeFace::NegativeX,
                uv: Vec2::new(d.z, d.y) / a.x,
            }
        }
    } else if a.y >= a.z {
        if d.y >= 0.0 {
            CubeAddress {
                face: CubeFace::PositiveY,
                uv: Vec2::new(d.x, -d.z) / a.y,
            }
        } else {
            CubeAddress {
                face: CubeFace::NegativeY,
                uv: Vec2::new(d.x, d.z) / a.y,
            }
        }
    } else if d.z >= 0.0 {
        CubeAddress {
            face: CubeFace::PositiveZ,
            uv: Vec2::new(d.x, d.y) / a.z,
        }
    } else {
        CubeAddress {
            face: CubeFace::NegativeZ,
            uv: Vec2::new(-d.x, d.y) / a.z,
        }
    }
}

#[must_use]
pub fn face_uv_to_direction(face: CubeFace, uv: Vec2) -> Vec3 {
    let p = match face {
        CubeFace::PositiveX => Vec3::new(1.0, uv.y, -uv.x),
        CubeFace::NegativeX => Vec3::new(-1.0, uv.y, uv.x),
        CubeFace::PositiveY => Vec3::new(uv.x, 1.0, -uv.y),
        CubeFace::NegativeY => Vec3::new(uv.x, -1.0, uv.y),
        CubeFace::PositiveZ => Vec3::new(uv.x, uv.y, 1.0),
        CubeFace::NegativeZ => Vec3::new(-uv.x, uv.y, -1.0),
    };
    p.normalize()
}

#[must_use]
pub fn cell_center_direction(cell: CubeCell, resolution: u32) -> Vec3 {
    face_uv_to_direction(cell.face, cell_center_uv(cell.x, cell.y, resolution))
}

#[must_use]
pub fn cell_center_uv(x: u32, y: u32, resolution: u32) -> Vec2 {
    Vec2::new(
        (x as f32 + 0.5).mul_add(2.0 / resolution as f32, -1.0),
        (y as f32 + 0.5).mul_add(2.0 / resolution as f32, -1.0),
    )
}

#[must_use]
pub fn direction_to_cell(direction: Vec3, resolution: u32) -> CubeCell {
    let address = direction_to_face_uv(direction);
    let scaled = ((address.uv + Vec2::ONE) * 0.5 * resolution as f32)
        .floor()
        .clamp(Vec2::ZERO, Vec2::splat(resolution.saturating_sub(1) as f32));
    CubeCell {
        face: address.face,
        x: scaled.x as u32,
        y: scaled.y as u32,
    }
}

/// Resolves a face-local offset through the cube projection, crossing edges and
/// corners without special-case adjacency tables.
#[must_use]
pub fn offset_cell(cell: CubeCell, dx: i32, dy: i32, resolution: u32) -> CubeCell {
    let x = i64::from(cell.x) + i64::from(dx);
    let y = i64::from(cell.y) + i64::from(dy);
    if (0..i64::from(resolution)).contains(&x) && (0..i64::from(resolution)).contains(&y) {
        return CubeCell {
            face: cell.face,
            x: x as u32,
            y: y as u32,
        };
    }
    let uv = Vec2::new(
        (cell.x as f32 + 0.5 + dx as f32).mul_add(2.0 / resolution as f32, -1.0),
        (cell.y as f32 + 0.5 + dy as f32).mul_add(2.0 / resolution as f32, -1.0),
    );
    direction_to_cell(face_uv_to_direction(cell.face, uv), resolution)
}

#[must_use]
pub fn tangent_frame(normal: Vec3) -> (Vec3, Vec3) {
    let axis = if normal.x.abs() <= normal.y.abs() && normal.x.abs() <= normal.z.abs() {
        Vec3::X
    } else if normal.y.abs() <= normal.z.abs() {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let tangent = axis.cross(normal).normalize();
    (tangent, normal.cross(tangent).normalize())
}

fn cube_area_term(x: f64, y: f64) -> f64 {
    (x * y).atan2((x * x + y * y + 1.0).sqrt())
}

/// Exact solid angle of one projected cube-map texel.
#[must_use]
pub fn cell_solid_angle(x: u32, y: u32, resolution: u32) -> f64 {
    let r = f64::from(resolution);
    let x0 = 2.0 * f64::from(x) / r - 1.0;
    let x1 = 2.0 * f64::from(x + 1) / r - 1.0;
    let y0 = 2.0 * f64::from(y) / r - 1.0;
    let y1 = 2.0 * f64::from(y + 1) / r - 1.0;
    cube_area_term(x1, y1) - cube_area_term(x0, y1) - cube_area_term(x1, y0)
        + cube_area_term(x0, y0)
}

/// Face-local solid angles in row-major order. Every cube face uses the same
/// values, and adjacent cells share corner terms. Evaluate each corner once
/// while retaining the exact arithmetic order of `cell_solid_angle`.
pub(crate) fn face_solid_angles(resolution: u32) -> Vec<f64> {
    let side = resolution as usize;
    let r = f64::from(resolution);
    let coordinates: Vec<_> = (0..=resolution)
        .map(|value| 2.0 * f64::from(value) / r - 1.0)
        .collect();
    let mut previous: Vec<_> = coordinates
        .iter()
        .map(|&x| cube_area_term(x, coordinates[0]))
        .collect();
    let mut current = vec![0.0; side + 1];
    let mut angles = Vec::with_capacity(side * side);
    for &y in &coordinates[1..] {
        for (value, &x) in current.iter_mut().zip(&coordinates) {
            *value = cube_area_term(x, y);
        }
        for x in 0..side {
            angles.push(current[x + 1] - current[x] - previous[x + 1] + previous[x]);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    angles
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CubeGrid<T> {
    resolution: u32,
    cells: Vec<T>,
}

impl<T: Clone> CubeGrid<T> {
    /// Creates a six-face grid initialized to `value`.
    ///
    /// # Panics
    ///
    /// Panics when `resolution` is zero.
    #[must_use]
    pub fn new(resolution: u32, value: T) -> Self {
        assert!(resolution > 0, "cube grid resolution must be non-zero");
        Self {
            resolution,
            cells: vec![value; 6 * resolution as usize * resolution as usize],
        }
    }
}

impl<T> CubeGrid<T> {
    #[must_use]
    pub const fn resolution(&self) -> u32 {
        self.resolution
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    #[must_use]
    pub fn flat_index(&self, cell: CubeCell) -> usize {
        let side = self.resolution as usize;
        cell.face.index() * side * side + cell.y as usize * side + cell.x as usize
    }

    #[must_use]
    pub fn cell_from_index(&self, index: usize) -> CubeCell {
        let side = self.resolution as usize;
        let face_area = side * side;
        CubeCell {
            face: CubeFace::ALL[index / face_area],
            x: (index % face_area % side) as u32,
            y: (index % face_area / side) as u32,
        }
    }

    #[must_use]
    pub fn get(&self, cell: CubeCell) -> &T {
        &self.cells[self.flat_index(cell)]
    }

    #[must_use]
    pub fn get_mut(&mut self, cell: CubeCell) -> &mut T {
        let index = self.flat_index(cell);
        &mut self.cells[index]
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.cells.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.cells.iter_mut()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.cells
    }

    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.cells
    }

    pub fn cells(&self) -> impl Iterator<Item = CubeCell> + '_ {
        (0..self.len()).map(|index| self.cell_from_index(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_round_trip_is_stable() {
        for face in CubeFace::ALL {
            for y in 0..32 {
                for x in 0..32 {
                    let cell = CubeCell { face, x, y };
                    let direction = cell_center_direction(cell, 32);
                    assert_eq!(direction_to_cell(direction, 32), cell);
                }
            }
        }
    }

    #[test]
    fn texel_solid_angles_cover_sphere() {
        let resolution = 32;
        let face: f64 = (0..resolution)
            .flat_map(|y| (0..resolution).map(move |x| cell_solid_angle(x, y, resolution)))
            .sum();
        let sphere = face * 6.0;
        assert!((sphere - std::f64::consts::TAU * 2.0).abs() < 1.0e-10);
    }

    #[test]
    fn cached_solid_angles_preserve_exact_texel_weights() {
        for resolution in [8, 24, 31, 64, 512] {
            let angles = face_solid_angles(resolution);
            assert_eq!(angles.len(), resolution as usize * resolution as usize);
            for y in 0..resolution {
                for x in 0..resolution {
                    let angle = angles[(y * resolution + x) as usize];
                    assert!(angle > 0.0);
                    assert_eq!(
                        angle.to_bits(),
                        cell_solid_angle(x, y, resolution).to_bits()
                    );
                }
            }
        }
    }

    #[test]
    fn offsets_cross_every_face_without_invalid_cells() {
        for face in CubeFace::ALL {
            for &(x, y) in &[(0, 0), (15, 0), (0, 15), (15, 15)] {
                let origin = CubeCell { face, x, y };
                for dy in -4..=4 {
                    for dx in -4..=4 {
                        let cell = offset_cell(origin, dx, dy, 16);
                        assert!(cell.x < 16 && cell.y < 16);
                    }
                }
            }
        }
    }

    #[test]
    fn interior_offset_fast_path_matches_cube_projection() {
        for resolution in [8, 24, 64] {
            for face in CubeFace::ALL {
                for y in 0..resolution {
                    for x in 0..resolution {
                        let cell = CubeCell { face, x, y };
                        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-3, 2)] {
                            let uv = Vec2::new(
                                (x as f32 + 0.5 + dx as f32).mul_add(2.0 / resolution as f32, -1.0),
                                (y as f32 + 0.5 + dy as f32).mul_add(2.0 / resolution as f32, -1.0),
                            );
                            assert_eq!(
                                offset_cell(cell, dx, dy, resolution),
                                direction_to_cell(face_uv_to_direction(face, uv), resolution)
                            );
                        }
                    }
                }
            }
        }
    }
}
