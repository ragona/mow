//! A small, immutable space panorama. Expensive cloud noise and the star catalog
//! are baked once, so every visible sky pixel needs only one filtered cube lookup.

use glam::{Vec2, Vec3};

const RESOLUTION: u32 = 1024;
const STAR_COUNT: u32 = 5_000;

#[derive(Debug)]
pub(crate) struct SkyMap {
    view: wgpu::TextureView,
}

impl SkyMap {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let started = std::time::Instant::now();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("painted nebula and stars cubemap"),
            size: wgpu::Extent3d {
                width: RESOLUTION,
                height: RESOLUTION,
                depth_or_array_layers: 6,
            },
            mip_level_count: RESOLUTION.ilog2() + 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let levels = bake(RESOLUTION);
        let mut width = RESOLUTION;
        for (level, pixels) in levels.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(width),
                },
                wgpu::Extent3d {
                    width,
                    height: width,
                    depth_or_array_layers: 6,
                },
            );
            width = (width / 2).max(1);
        }
        tracing::info!(
            milliseconds = started.elapsed().as_secs_f64() * 1_000.0,
            bytes = levels.iter().map(Vec::len).sum::<usize>(),
            "Baked space panorama"
        );
        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::Cube),
                ..Default::default()
            }),
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
}

fn hash(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^ (value >> 16)
}

fn unit_hash(value: u32) -> f32 {
    (hash(value) >> 8) as f32 / 16_777_215.0
}

fn noise(point: Vec3) -> f32 {
    let cell = point.floor().as_ivec3();
    let fraction = point - point.floor();
    let weight = fraction * fraction * (Vec3::splat(3.0) - fraction * 2.0);
    let sample = |x: i32, y: i32, z: i32| {
        unit_hash(
            (cell.x.wrapping_add(x) as u32).wrapping_mul(0x8da6_b343)
                ^ (cell.y.wrapping_add(y) as u32).wrapping_mul(0xd816_3841)
                ^ (cell.z.wrapping_add(z) as u32).wrapping_mul(0xcb1a_b31f),
        )
    };
    let blend = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let plane = |z| {
        blend(
            blend(sample(0, 0, z), sample(1, 0, z), weight.x),
            blend(sample(0, 1, z), sample(1, 1, z), weight.x),
            weight.y,
        )
    };
    blend(plane(0), plane(1), weight.z)
}

fn cloud_noise(mut point: Vec3) -> f32 {
    let mut value = 0.0;
    let mut amplitude = 0.54;
    for _ in 0..5 {
        value += noise(point) * amplitude;
        // Rotation/translation prevents octaves lining up into grid-like clouds.
        point = Vec3::new(point.y + point.z * 0.32, point.z - point.x * 0.32, point.x) * 1.94
            + Vec3::new(7.1, 3.7, 11.3);
        amplitude *= 0.47;
    }
    value
}

fn smooth(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn nebula(direction: Vec3) -> Vec3 {
    let drift = noise(direction * 2.7 + Vec3::splat(13.4));
    let bend = noise(direction * 4.5 - Vec3::splat(7.8));
    let band = direction.dot(Vec3::new(0.31, 0.83, 0.464)) + (drift - 0.5) * 0.62;
    let envelope = (-band * band * 5.0).exp();
    let cloud = cloud_noise(direction * 5.8 + Vec3::new(drift, bend, drift - bend) * 2.8);
    let billow = smooth(0.26, 0.71, cloud);
    let filament = smooth(0.48, 0.73, cloud) * envelope;
    let violet = smooth(0.30, 0.73, drift);
    let warm = smooth(0.56, 0.77, bend) * filament;
    let dust = smooth(0.40, 0.69, noise(direction * 8.7 + Vec3::splat(5.1)));
    let base = Vec3::new(0.011, 0.018, 0.041);
    let cloud_color = Vec3::new(0.012, 0.080, 0.091).lerp(Vec3::new(0.108, 0.043, 0.147), violet);
    base + cloud_color * (billow * envelope * 0.83 + billow * 0.12)
        + Vec3::new(0.13, 0.077, 0.11) * filament * 0.48
        + Vec3::new(0.19, 0.085, 0.036) * warm
        - Vec3::new(0.007, 0.011, 0.014) * dust * envelope
}

// WebGPU cube layer order and orientation: +X, -X, +Y, -Y, +Z, -Z.
// These are deliberately independent of the simulation's terrain cube basis.
fn face_direction(face: usize, uv: Vec2) -> Vec3 {
    match face {
        0 => Vec3::new(1.0, -uv.y, -uv.x),
        1 => Vec3::new(-1.0, -uv.y, uv.x),
        2 => Vec3::new(uv.x, 1.0, uv.y),
        3 => Vec3::new(uv.x, -1.0, -uv.y),
        4 => Vec3::new(uv.x, -uv.y, 1.0),
        _ => Vec3::new(-uv.x, -uv.y, -1.0),
    }
    .normalize()
}

fn project_face(face: usize, direction: Vec3) -> (Vec2, f32) {
    let (uv, depth) = match face {
        0 => (Vec2::new(-direction.z, -direction.y), direction.x),
        1 => (Vec2::new(direction.z, -direction.y), -direction.x),
        2 => (Vec2::new(direction.x, direction.z), direction.y),
        3 => (Vec2::new(direction.x, -direction.z), -direction.y),
        4 => (Vec2::new(direction.x, -direction.y), direction.z),
        _ => (Vec2::new(-direction.x, -direction.y), -direction.z),
    };
    (uv / depth, depth)
}

fn companion_moon(direction: Vec3, sky: Vec3, pixel_angle: f32) -> Vec3 {
    let center = Vec3::new(-0.76, 0.15, -0.63).normalize();
    let angular_radius = 0.054;
    let distance = direction.distance(center);
    if distance > angular_radius + pixel_angle {
        return sky;
    }
    let right = center.cross(Vec3::Y).normalize();
    let up = right.cross(center);
    let uv = Vec2::new(direction.dot(right), direction.dot(up)) / angular_radius;
    let depth = (1.0 - uv.length_squared()).max(0.0).sqrt();
    let normal = Vec3::new(uv.x, uv.y, depth);
    let light = Vec3::new(-0.66, 0.61, 0.43).normalize();
    let mut material = 0.92 + (noise(normal * 9.0) - 0.5) * 0.18;
    let mut rim_light = 0.0;
    for (position, radius) in [
        (Vec2::new(-0.36, 0.18), 0.25),
        (Vec2::new(0.14, -0.41), 0.18),
        (Vec2::new(0.42, 0.35), 0.14),
        (Vec2::new(-0.30, -0.61), 0.09),
        (Vec2::new(0.05, 0.56), 0.08),
    ] {
        let delta = uv - position;
        let radial = delta.length() / radius;
        material -= (1.0 - smooth(0.48, 1.0, radial)) * 0.20;
        let rim = (1.0 - smooth(0.10, 0.30, (radial - 1.0).abs())) * 0.12;
        rim_light += rim * (0.4 - delta.normalize_or_zero().dot(light.truncate()));
    }
    let moon = Vec3::new(0.40, 0.33, 0.27) * (0.19 + normal.dot(light).max(0.0) * 0.87) * material
        + Vec3::splat(rim_light.max(0.0));
    sky.lerp(
        moon,
        1.0 - smooth(
            angular_radius - pixel_angle * 0.5,
            angular_radius + pixel_angle * 0.5,
            distance,
        ),
    )
}

fn bake(resolution: u32) -> Vec<Vec<u8>> {
    #[cfg(test)]
    let started = std::time::Instant::now();
    assert!(resolution.is_power_of_two());
    let width = resolution as usize;
    let mut pixels = vec![Vec3::ZERO; width * width * 6];
    #[cfg(test)]
    let allocated = started.elapsed();
    // Only the startup bake uses workers; there is no sky work or allocation on
    // subsequent frames. The number of workers is bounded by the six faces.
    let workers = if resolution >= 128 {
        std::thread::available_parallelism().map_or(1, |count| count.get().min(6))
    } else {
        1
    };
    let faces_per_worker = 6_usize.div_ceil(workers);
    std::thread::scope(|scope| {
        for (batch, output) in pixels
            .chunks_mut(faces_per_worker * width * width)
            .enumerate()
        {
            scope.spawn(move || {
                for (local_face, face_pixels) in output.chunks_mut(width * width).enumerate() {
                    let face = batch * faces_per_worker + local_face;
                    for y in 0..width {
                        for x in 0..width {
                            let uv = (Vec2::new(x as f32, y as f32) + Vec2::splat(0.5))
                                * (2.0 / resolution as f32)
                                - Vec2::ONE;
                            face_pixels[y * width + x] = nebula(face_direction(face, uv));
                        }
                    }
                }
            });
        }
    });
    #[cfg(test)]
    let clouds_finished = started.elapsed();
    paint_stars(&mut pixels, resolution);
    #[cfg(test)]
    let stars_finished = started.elapsed();
    // Paint the distant moon after the star field so it actually occludes stars.
    for face in 0..6 {
        let (uv, depth) = project_face(face, Vec3::new(-0.76, 0.15, -0.63).normalize());
        if depth < 0.4 || uv.abs().max_element() > 1.3 {
            continue;
        }
        let center = (uv + Vec2::ONE) * (resolution as f32 * 0.5);
        let extent = (resolution as f32 * 0.11 / depth).ceil() as i64;
        for y in
            (center.y as i64 - extent).max(0)..(center.y as i64 + extent).min(i64::from(resolution))
        {
            for x in (center.x as i64 - extent).max(0)
                ..(center.x as i64 + extent).min(i64::from(resolution))
            {
                let uv = (Vec2::new(x as f32, y as f32) + Vec2::splat(0.5))
                    * (2.0 / resolution as f32)
                    - Vec2::ONE;
                let index = (face * width + y as usize) * width + x as usize;
                pixels[index] = companion_moon(
                    face_direction(face, uv),
                    pixels[index],
                    1.5 / resolution as f32,
                );
            }
        }
    }
    #[cfg(test)]
    let moon_finished = started.elapsed();
    #[cfg(test)]
    let mut encoding = std::time::Duration::ZERO;
    let mut levels = Vec::with_capacity(resolution.ilog2() as usize + 1);
    let mut width = width;
    loop {
        #[cfg(test)]
        let encoding_started = std::time::Instant::now();
        levels.push(encode_srgb(&pixels));
        #[cfg(test)]
        {
            encoding += encoding_started.elapsed();
        }
        if width == 1 {
            break;
        }
        let next_width = width / 2;
        // Compact each mip into the already-read prefix. Every destination is
        // before its source block, so this retains exact linear-light filtering
        // while avoiding another large float image and its zero-fill.
        for face in 0..6 {
            for y in 0..next_width {
                for x in 0..next_width {
                    let source = (face * width + y * 2) * width + x * 2;
                    pixels[(face * next_width + y) * next_width + x] = (pixels[source]
                        + pixels[source + 1]
                        + pixels[source + width]
                        + pixels[source + width + 1])
                        * 0.25;
                }
            }
        }
        pixels.truncate(next_width * next_width * 6);
        width = next_width;
    }
    #[cfg(test)]
    if resolution == RESOLUTION {
        let total = started.elapsed();
        println!(
            "Sky bake phases: allocate={:.2}ms, clouds={:.2}ms, stars={:.2}ms, moon={:.2}ms, encode={:.2}ms, mip/filter={:.2}ms, total={:.2}ms",
            allocated.as_secs_f64() * 1_000.0,
            clouds_finished.saturating_sub(allocated).as_secs_f64() * 1_000.0,
            stars_finished.saturating_sub(clouds_finished).as_secs_f64() * 1_000.0,
            moon_finished.saturating_sub(stars_finished).as_secs_f64() * 1_000.0,
            encoding.as_secs_f64() * 1_000.0,
            total
                .saturating_sub(moon_finished)
                .saturating_sub(encoding)
                .as_secs_f64()
                * 1_000.0,
            total.as_secs_f64() * 1_000.0,
        );
    }
    levels
}

fn paint_stars(pixels: &mut [Vec3], resolution: u32) {
    let width = resolution as usize;
    for star in 0..STAR_COUNT {
        let z = unit_hash(star * 7 + 1) * 2.0 - 1.0;
        let azimuth = unit_hash(star * 7 + 2) * std::f32::consts::TAU;
        let radial = (1.0 - z * z).sqrt();
        let direction = Vec3::new(radial * azimuth.cos(), z, radial * azimuth.sin());
        let rank = unit_hash(star * 7 + 3);
        let bright = rank > 0.996;
        let angular_width = if bright { 0.0016 } else { 0.00055_f32 };
        // Integrate unresolved stars over a minimum pixel footprint, reducing
        // subpixel dropouts before the energy-preserving linear mip filter.
        let footprint = angular_width.max(0.70 / resolution as f32);
        let intensity = if bright {
            1.8
        } else {
            0.13 + rank.powi(5) * 0.82
        };
        let tint =
            Vec3::new(0.57, 0.76, 1.0).lerp(Vec3::new(1.0, 0.79, 0.51), unit_hash(star * 7 + 4));
        let right = direction.any_orthonormal_vector();
        let up = direction.cross(right);
        for face in 0..6 {
            let (uv, depth) = project_face(face, direction);
            if depth < 0.4 {
                continue;
            }
            let center = (uv + Vec2::ONE) * (resolution as f32 * 0.5);
            let extent = (footprint * resolution as f32 * if bright { 5.0 } else { 2.0 }
                / (depth * depth))
                .ceil() as i64
                + 1;
            let x_start = (center.x.floor() as i64 - extent).max(0);
            let x_end = (center.x.ceil() as i64 + extent).min(i64::from(resolution));
            let y_start = (center.y.floor() as i64 - extent).max(0);
            let y_end = (center.y.ceil() as i64 + extent).min(i64::from(resolution));
            for y in y_start..y_end {
                for x in x_start..x_end {
                    let uv = (Vec2::new(x as f32, y as f32) + Vec2::splat(0.5))
                        * (2.0 / resolution as f32)
                        - Vec2::ONE;
                    let delta = face_direction(face, uv) - direction;
                    let distance_sq = delta.length_squared() / (footprint * footprint);
                    let core = (-distance_sq * 1.8).exp();
                    let mut glow = core;
                    if bright {
                        let local = Vec2::new(delta.dot(right), delta.dot(up)) / footprint;
                        let rays = (-(local.x.abs() + local.y.abs()) * 0.85).exp()
                            * (-local.x.abs().min(local.y.abs()).powi(2) * 6.0).exp();
                        glow += rays * 0.34 + (-distance_sq * 0.16).exp() * 0.055;
                    }
                    let pixel = &mut pixels[(face * width + y as usize) * width + x as usize];
                    // Clamp before filtering so mip levels preserve the energy
                    // of the actual stored base level, including bright cores.
                    *pixel = (*pixel + tint * glow * intensity).min(Vec3::ONE);
                }
            }
        }
    }
}

fn reference_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let srgb = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round() as u8
}

#[derive(Clone, Copy, Debug)]
struct SrgbBin {
    value: u8,
    lower: f32,
    upper: f32,
}

/// The linear interval of each bin is smaller than the narrowest sRGB byte
/// interval. Its lookup is therefore at most one byte away; comparing against
/// exact float boundaries corrects it without an approximation or a per-channel
/// transcendental. The immutable 48 KiB table is reused for every sky mip.
#[derive(Debug)]
struct SrgbEncoder {
    bins: Box<[SrgbBin]>,
}

impl SrgbEncoder {
    const BIN_COUNT: usize = 4096;

    fn new() -> Self {
        let mut boundaries = [0.0; 257];
        boundaries[256] = f32::INFINITY;
        for (index, boundary) in boundaries.iter_mut().enumerate().take(256).skip(1) {
            // Binary search float encodings to retain the previous powf/round
            // result even at representable values adjacent to a byte boundary.
            let mut low = 0;
            let mut high = 1.0_f32.to_bits();
            while low < high {
                let middle = low + (high - low) / 2;
                if reference_srgb(f32::from_bits(middle)) < index as u8 {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
            *boundary = f32::from_bits(low);
        }
        let bins = (0..Self::BIN_COUNT)
            .map(|index| {
                let value = reference_srgb(index as f32 / (Self::BIN_COUNT - 1) as f32);
                SrgbBin {
                    value,
                    lower: boundaries[value as usize],
                    upper: boundaries[value as usize + 1],
                }
            })
            .collect();
        Self { bins }
    }

    fn encode(&self, value: f32) -> u8 {
        let value = value.clamp(0.0, 1.0);
        let bin = self.bins[(value * (Self::BIN_COUNT - 1) as f32) as usize];
        bin.value + u8::from(value >= bin.upper) - u8::from(value < bin.lower)
    }
}

fn encode_srgb(pixels: &[Vec3]) -> Vec<u8> {
    static ENCODER: std::sync::OnceLock<SrgbEncoder> = std::sync::OnceLock::new();
    // Benchmark-only reference path permits timing and whole-cubemap checksum
    // comparisons against the previous encoder in the same compiled binary.
    #[cfg(test)]
    if std::env::var_os("LAWN_SKY_REFERENCE_ENCODER").is_some() {
        return pixels
            .iter()
            .flat_map(|color| {
                [
                    reference_srgb(color.x),
                    reference_srgb(color.y),
                    reference_srgb(color.z),
                    255,
                ]
            })
            .collect();
    }
    let encoder = ENCODER.get_or_init(SrgbEncoder::new);
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for color in pixels {
        bytes.extend([
            encoder.encode(color.x),
            encoder.encode(color.y),
            encoder.encode(color.z),
            255,
        ]);
    }
    bytes
}

/// A diagnostic panorama using the shipping face basis, for comparing actual
/// hardware cube sampling with known world directions in composite GPU tests.
#[cfg(test)]
pub(crate) fn direction_color_cube(resolution: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((resolution * resolution * 6) as usize);
    for face in 0..6 {
        for y in 0..resolution {
            for x in 0..resolution {
                let uv = (Vec2::new(x as f32, y as f32) + Vec2::splat(0.5))
                    * (2.0 / resolution as f32)
                    - Vec2::ONE;
                pixels.push(face_direction(face, uv) * 0.5 + Vec3::splat(0.5));
            }
        }
    }
    encode_srgb(&pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_srgb_encoder_preserves_bytes_including_rounding_boundaries() {
        let encoder = SrgbEncoder::new();
        for bin in &encoder.bins {
            for boundary in [bin.lower, bin.upper] {
                if boundary.is_finite() {
                    for offset in -16..=16 {
                        let bits = boundary.to_bits().saturating_add_signed(offset);
                        let value = f32::from_bits(bits);
                        assert_eq!(encoder.encode(value), reference_srgb(value), "{value:?}");
                    }
                }
            }
        }
        for index in 0..=1_048_576 {
            let value = index as f32 / 1_048_576.0;
            assert_eq!(encoder.encode(value), reference_srgb(value), "{value:?}");
        }
        for value in [
            -1.0,
            0.0,
            1.0,
            2.0,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NAN,
        ] {
            assert_eq!(encoder.encode(value), reference_srgb(value));
        }
    }

    #[test]
    fn sky_cube_basis_roundtrips_and_agrees_on_every_edge_and_corner() {
        for face in 0..6 {
            for y in -4_i32..=4 {
                for x in -4_i32..=4 {
                    let uv = Vec2::new(x as f32, y as f32) / 4.0;
                    let direction = face_direction(face, uv);
                    let (projected, depth) = project_face(face, direction);
                    assert!(depth > 0.0);
                    assert!(projected.distance(uv) < 1.0e-6);
                    if x.abs() == 4 || y.abs() == 4 {
                        let mut neighbors = 0;
                        for other in 0..6 {
                            if other == face {
                                continue;
                            }
                            let (neighbor_uv, neighbor_depth) = project_face(other, direction);
                            if neighbor_depth > 0.0 && neighbor_uv.abs().max_element() <= 1.000_001
                            {
                                let neighbor = face_direction(other, neighbor_uv);
                                assert!(neighbor.distance(direction) < 1.0e-6);
                                assert!(nebula(neighbor).distance(nebula(direction)) < 1.0e-5);
                                neighbors += 1;
                            }
                        }
                        assert!(neighbors >= 1, "unmatched cube edge on face {face}");
                    }
                }
            }
        }
        for (face, expected) in [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::Z,
            Vec3::NEG_Z,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(face_direction(face, Vec2::ZERO), expected);
        }
    }

    #[test]
    fn sky_bake_is_deterministic_and_has_complete_bounded_mips() {
        let levels = bake(32);
        assert_eq!(levels, bake(32));
        assert_eq!(levels.len(), 6);
        for (mip, pixels) in levels.iter().enumerate() {
            let width = 32 >> mip;
            assert_eq!(pixels.len(), width * width * 6 * 4);
            assert!(pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
        }
        let production_bytes: usize = (0..=RESOLUTION.ilog2())
            .map(|mip| (RESOLUTION >> mip).pow(2) as usize * 6 * 4)
            .sum();
        assert!(production_bytes <= 32 * 1024 * 1024);
    }

    #[test]
    #[ignore = "full-resolution startup benchmark; run with --release --nocapture"]
    fn benchmark_full_sky_bake() {
        let started = std::time::Instant::now();
        let levels = bake(RESOLUTION);
        println!(
            "Sky bake: {:.2} ms; {} bytes for {} mip levels",
            started.elapsed().as_secs_f64() * 1_000.0,
            levels.iter().map(Vec::len).sum::<usize>(),
            levels.len()
        );
        let checksum = levels
            .iter()
            .flatten()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
            });
        println!("Sky mip byte checksum: {checksum:016x}");
        assert_eq!(levels[0].len(), (RESOLUTION * RESOLUTION * 6 * 4) as usize);
    }
}
