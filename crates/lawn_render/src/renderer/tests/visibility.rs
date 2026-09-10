use super::*;

fn visibility(
    camera: Vec3,
    view_proj: Mat4,
    inner_radius: f32,
    blade_extent: f32,
) -> GrassVisibility {
    GrassVisibility {
        camera_position: camera,
        camera_direction: camera.normalize_or_zero(),
        camera_radius: camera.length(),
        inner_radius,
        planes: grass_frustum_planes(view_proj),
        blade_extent,
        base_radius: 15.0,
        base_density: 1.0,
        toy_camera_lod: 1.0,
    }
}

fn point_bound(point: Vec3) -> GrassPatchBounds {
    GrassPatchBounds {
        center: point,
        max_radius: point.length(),
        ..Default::default()
    }
}

/// Independent oracle in homogeneous clip coordinates: this does not extract
/// planes, normalize their normals, or use the culling support calculation.
fn inside_clip(view_proj: Mat4, point: Vec3) -> bool {
    let clip = view_proj * point.extend(1.0);
    clip.w > 0.0
        && clip.x.abs() <= clip.w
        && clip.y.abs() <= clip.w
        && clip.z >= 0.0
        && clip.z <= clip.w
}

#[test]
fn visibility_uses_the_shifted_viewport_and_webgpu_near_plane() {
    let projection = Mat4::perspective_rh(60.0_f32.to_radians(), 0.64, 0.08, 200.0);
    let placement = Mat4::from_translation(Vec3::new(0.6, 0.0, 0.0))
        * Mat4::from_scale(Vec3::new(0.4, 1.0, 1.0));
    let shifted = placement * projection;
    let visible = visibility(Vec3::ZERO, shifted, 0.0, 0.0);
    let left = Vec3::new(-4.0, 0.0, -5.0);
    assert!(!inside_clip(projection, left));
    assert!(inside_clip(shifted, left));
    assert!(visible.contains(&point_bound(left)));
    let right = Vec3::new(4.0, 0.0, -5.0);
    assert!(!inside_clip(shifted, right));
    assert!(!visible.contains(&point_bound(right)));

    for (depth, expected) in [(0.04, false), (0.081, true), (199.0, true), (201.0, false)] {
        let point = Vec3::new(0.0, 0.0, -depth);
        assert_eq!(inside_clip(shifted, point), expected);
        assert_eq!(
            visible.contains(&point_bound(point)),
            expected,
            "depth {depth}"
        );
    }
    let near_crossing = GrassPatchBounds {
        center: Vec3::new(0.0, 0.0, -0.04),
        half_extents: Vec3::new(0.01, 0.01, 0.05),
        radius: 0.06,
        max_radius: 0.10,
    };
    assert!(
        visible.contains(&near_crossing),
        "a box crossing the near plane must survive even when its center is clipped"
    );
}

#[test]
fn visibility_retains_blades_bent_into_the_view_from_clipped_roots() {
    let projection = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.08, 200.0);
    for height_scale in [0.1, 2.25, 3.6] {
        let height = 0.63 * height_scale;
        let root = Vec3::new(
            -30.0_f32.to_radians().tan() * 5.0 - height * 0.90,
            0.0,
            -5.0,
        );
        let bend = height * 0.94;
        let tip = root + Vec3::new(bend, (height * height - bend * bend).sqrt(), 0.0);
        assert!(!inside_clip(projection, root));
        assert!(inside_clip(projection, tip));
        let visible = visibility(
            Vec3::ZERO,
            projection,
            0.0,
            grass_bounds::blade_extent(height_scale),
        );
        assert!(
            visible.contains(&point_bound(root)),
            "fully bent blade was culled at height scale {height_scale}"
        );
    }
}

#[test]
fn visibility_horizon_keeps_every_unoccluded_tip_across_orbit_and_editor_views() {
    let inner_radius = 13.0;
    let blade_extent = grass_bounds::blade_extent(2.25);
    let mut visible_tips = 0;
    let mut rejected_patches = 0;
    for camera in [
        Vec3::new(0.0, 0.0, 25.0),
        Vec3::new(18.0, 14.0, 21.0),
        Vec3::new(-30.0, 0.0, 0.0),
    ] {
        let view = Mat4::look_at_rh(camera, Vec3::ZERO, Vec3::Y);
        for inset in [0.0, 0.58] {
            let projection =
                Mat4::perspective_rh(70.0_f32.to_radians(), 1.6 * (1.0 - inset), 0.08, 200.0);
            let placement = Mat4::from_translation(Vec3::new(inset, 0.0, 0.0))
                * Mat4::from_scale(Vec3::new(1.0 - inset, 1.0, 1.0));
            let view_proj = placement * projection * view;
            let visible = visibility(camera, view_proj, inner_radius, blade_extent);
            for sample in 0..512 {
                let y = 1.0 - 2.0 * (sample as f32 + 0.5) / 512.0;
                let azimuth = sample as f32 * 2.399_963_1;
                let radial = (1.0 - y * y).sqrt();
                let root = Vec3::new(radial * azimuth.cos(), y, radial * azimuth.sin()) * 15.0;
                let bound = point_bound(root);
                rejected_patches += usize::from(!visible.contains(&bound));
                for offset in [
                    Vec3::ZERO,
                    Vec3::X,
                    Vec3::NEG_X,
                    Vec3::Y,
                    Vec3::NEG_Y,
                    Vec3::Z,
                    Vec3::NEG_Z,
                    Vec3::ONE.normalize(),
                    -Vec3::ONE.normalize(),
                ] {
                    let tip = root + offset * blade_extent;
                    let segment = tip - camera;
                    let t = (-camera.dot(segment) / segment.length_squared()).clamp(0.0, 1.0);
                    let closest = camera + segment * t;
                    // The oracle is a line-segment/sphere distance test, not
                    // the angular horizon approximation used by production.
                    if inside_clip(view_proj, tip) && closest.length() > inner_radius + 0.0001 {
                        visible_tips += 1;
                        assert!(
                            visible.contains(&bound),
                            "visible tip at {tip:?} culled from camera {camera:?}, inset {inset}"
                        );
                    }
                }
            }
        }
    }
    assert!(visible_tips > 5_000);
    assert!(
        rejected_patches > 500,
        "test must also exercise useful rejection"
    );
}

#[test]
fn visibility_retains_density_and_handles_bounds_covering_the_origin() {
    let camera = Vec3::new(0.0, 0.0, 50.0);
    let view_proj = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.08, 200.0)
        * Mat4::look_at_rh(camera, Vec3::ZERO, Vec3::Y);
    let mut visible = visibility(camera, view_proj, 13.0, 0.0);
    visible.toy_camera_lod = 0.52;
    let patch = lawn_core::planet::GrassPatch {
        face: CubeFace::PositiveZ,
        tile_x: 0,
        tile_y: 0,
        roots: 10..110,
        center: Vec3::Z,
        angular_radius: 0.1,
    };
    // Bounds center changes visibility, but density remains based on the old
    // patch center at radius15. Using the nearer bound would incorrectly give52.
    assert_eq!(visible.draw_count(&patch, &point_bound(Vec3::Z * 15.0)), 39);
    assert_eq!(visible.draw_count(&patch, &point_bound(Vec3::Z * 40.0)), 39);
    visible.base_density = 0.5;
    assert_eq!(visible.draw_count(&patch, &point_bound(Vec3::Z * 40.0)), 20);
    let origin_bound = GrassPatchBounds {
        center: Vec3::ZERO,
        half_extents: Vec3::splat(1.0),
        radius: 3.0,
        max_radius: 3.0,
    };
    assert!(
        visible.contains(&origin_bound),
        "angular caps covering the origin must disable horizon rejection"
    );
    let mut empty = patch;
    empty.roots = 10..10;
    assert_eq!(visible.draw_count(&empty, &origin_bound), 0);
}

fn mesh_vertex(position: Vec3) -> MeshVertex {
    MeshVertex {
        position: position.to_array(),
        normal: Vec3::Y.to_array(),
        material: 0,
        variation: 0.0,
        detail: [0.0; 4],
    }
}

#[test]
fn visibility_inner_sphere_stays_below_even_the_deepest_vertex() {
    let vertices = [
        mesh_vertex(Vec3::X * 15.0),
        mesh_vertex(Vec3::Y * 14.0),
        mesh_vertex(Vec3::Z * 16.0),
    ];
    for resolution in [16, 48, 128, 512] {
        let radius = terrain_inner_radius(&vertices, resolution);
        assert!(radius > 0.0 && radius < 14.0);
    }
    let collapsed = [mesh_vertex(Vec3::ZERO), mesh_vertex(Vec3::X * 15.0)];
    assert_eq!(terrain_inner_radius(&collapsed, 16), 0.0);
}

/// Squared distance to the actual bounded triangle, independently evaluated in
/// f64. Steep triangle planes may pass near the origin while the triangle is far
/// away, so distances to the extended planes would be needlessly pessimistic.
fn triangle_distance(a: glam::DVec3, b: glam::DVec3, c: glam::DVec3) -> f64 {
    let normal = (b - a).cross(c - a);
    let projection = normal * (normal.dot(a) / normal.length_squared());
    if normal.length_squared() > 0.0
        && [(a, b), (b, c), (c, a)]
            .into_iter()
            .all(|(from, to)| (to - from).cross(projection - from).dot(normal) >= 0.0)
    {
        return projection.length();
    }
    [(a, b), (b, c), (c, a)]
        .into_iter()
        .map(|(from, to)| {
            let edge = to - from;
            let t = if edge.length_squared() > 0.0 {
                (-from.dot(edge) / edge.length_squared()).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (from + edge * t).length()
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
fn visibility_inner_sphere_is_inside_actual_cratered_terrain() {
    let mut crater_vertices = 0;
    for resolution in [8, 24, 64] {
        let planet = PlanetGenerator::new(
            CURRENT_GENERATOR_VERSION,
            GeneratorConfig {
                terrain_resolution: resolution,
                ..GeneratorConfig::test_quality()
            },
        )
        .generate_with_roots(WorldSeed(21), false)
        .unwrap();
        let surface = TerrainSurface::new(&planet);
        let (vertices, indices) = mesh::build_terrain(&planet, &surface);
        crater_vertices += vertices
            .iter()
            .filter(|vertex| vertex.detail[0] > 0.1)
            .count();
        let inner_radius = f64::from(terrain_inner_radius(
            &vertices,
            mesh::terrain_render_resolution(resolution),
        ));
        assert!(inner_radius.is_finite() && inner_radius > 0.0);
        for triangle in indices.chunks_exact(3) {
            let [a, b, c] = [triangle[0], triangle[1], triangle[2]]
                .map(|index| Vec3::from_array(vertices[index as usize].position).as_dvec3());
            assert!(
                inner_radius <= triangle_distance(a, b, c),
                "occluder protruded into an actual triangle at terrain resolution {resolution}"
            );
            for point in [a, b, c, (a + b + c) / 3.0] {
                assert!(point.length() >= inner_radius);
            }
        }
    }
    assert!(crater_vertices > 20, "test must contain real crater bowls");
}
