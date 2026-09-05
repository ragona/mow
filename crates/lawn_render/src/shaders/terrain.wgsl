struct FrameUniform {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    camera_time: vec4<f32>,
    light_epoch: vec4<f32>,
    options: vec4<f32>,
    locator: vec4<f32>,
    mower_position: vec4<f32>,
    mower_forward: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: FrameUniform;
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) material: u32,
    @location(3) variation: f32,
    @location(4) detail: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) @interpolate(flat) material: u32,
    @location(3) variation: f32,
    @location(4) shadow_position: vec4<f32>,
    @location(5) detail: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world = vec4<f32>(input.position, 1.0);
    output.clip_position = frame.view_proj * world;
    output.world_position = input.position;
    output.normal = input.normal;
    output.material = input.material;
    output.variation = input.variation;
    output.shadow_position = frame.light_view_proj * world;
    output.detail = input.detail;
    return output;
}

fn shadow_factor(position: vec4<f32>) -> f32 {
    let projected = position.xyz / position.w;
    let uv = projected.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    let depth = projected.z;
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || depth < 0.0 || depth > 1.0) {
        return 1.0;
    }
    // Four bilinear comparison taps soften contact edges without a wide blur.
    let texel = vec2<f32>(0.75) / vec2<f32>(textureDimensions(shadow_map));
    let reference = depth - 0.0015;
    return (
        textureSampleCompareLevel(shadow_map, shadow_sampler, uv + vec2<f32>(-texel.x, -texel.y), reference)
        + textureSampleCompareLevel(shadow_map, shadow_sampler, uv + vec2<f32>(texel.x, -texel.y), reference)
        + textureSampleCompareLevel(shadow_map, shadow_sampler, uv + vec2<f32>(-texel.x, texel.y), reference)
        + textureSampleCompareLevel(shadow_map, shadow_sampler, uv + texel, reference)
    ) * 0.25;
}

fn mower_contact(world_position: vec3<f32>) -> f32 {
    let up = normalize(frame.mower_position.xyz + vec3<f32>(0.0, 0.0001, 0.0));
    let delta = world_position - frame.mower_position.xyz;
    let height = dot(delta, up);
    let planar = delta - up * height;
    let forward = frame.mower_forward.xyz;
    let along = dot(planar, forward);
    let ellipse = max(dot(planar, planar) - along * along * 0.16, 0.0) / 2.9;
    let footprint = 1.0 - smoothstep(0.02, 1.0, ellipse);
    // The radial gate also excludes terrain on the far side of the planet.
    let nearby = 1.0 - smoothstep(1.6, 3.2, abs(height));
    return 1.0 - footprint * nearby * frame.mower_position.w * 0.43;
}

struct StoneSurface {
    color: vec3<f32>,
    normal: vec3<f32>,
};

fn stone_grain(cell: vec3<f32>) -> f32 {
    let p = fract(cell * vec3<f32>(0.1031, 0.1030, 0.0973));
    let q = p + vec3<f32>(dot(p, p.yzx + vec3<f32>(33.33)));
    return fract((q.x + q.y) * q.z);
}

fn stone_surface(
    position: vec3<f32>,
    normal: vec3<f32>,
    pixel_width: vec3<f32>,
    rock_coverage: f32,
    detail: vec4<f32>,
    detail_width: vec2<f32>,
) -> StoneSurface {
    // Intersecting oblique triangular fields make angular meteor slabs. Their
    // piecewise planar gradients add broad facets, not glittery bump noise.
    let axis_a = vec3<f32>(0.47, 0.73, -0.29);
    let axis_b = vec3<f32>(-0.61, 0.23, 0.51);
    let phase = vec2<f32>(dot(position, axis_a), dot(position, axis_b) + 1.7);
    let fold = fract(phase * vec2<f32>(0.23, 0.19)) - vec2<f32>(0.5);
    let wave = abs(fold) * 4.0 - vec2<f32>(1.0);
    let crossing = wave.y * 0.9;
    let mineral = max(wave.x, crossing) + min(wave.x, crossing) * 0.35;
    let gradient_a = axis_a * select(-0.92, 0.92, fold.x >= 0.0);
    let gradient_b = axis_b * select(-0.684, 0.684, fold.y >= 0.0);
    let gradient = select(gradient_b + gradient_a * 0.35, gradient_a + gradient_b * 0.35, wave.x >= crossing);
    let mineral_width = max(dot(pixel_width, abs(axis_a) * 0.92 + abs(axis_b) * 0.684), 0.0001);
    let plane = smoothstep(-mineral_width * 0.65, mineral_width * 0.65, mineral);
    var color = mix(vec3<f32>(0.072, 0.12, 0.245), vec3<f32>(0.145, 0.145, 0.265), plane);

    // Real geometry supplies the bowl and rim masks. Dust settles in bowls;
    // exposed rim chips are pale, with stable individual crater hues.
    let bowl = clamp(detail.x, 0.0, 1.0);
    let rim = clamp(detail.y, 0.0, 1.0);
    let crater_dust = mix(vec3<f32>(0.032, 0.068, 0.135), vec3<f32>(0.060, 0.053, 0.12), detail.z);
    let wall_edge = 0.065 + detail_width.x * 0.65;
    let floor_edge = 0.12 + detail_width.x * 0.65;
    let inner_wall = smoothstep(0.10 - wall_edge, 0.10 + wall_edge, bowl);
    let bowl_floor = smoothstep(0.62 - floor_edge, 0.62 + floor_edge, bowl);
    // Dust transitions smoothly across the wall and floor so the real cavity
    // geometry supplies the form. Only the exposed rim chips stay crisp.
    color = mix(color, color * 0.62 + crater_dust * 0.12, inner_wall * (0.65 + bowl * 0.20));
    color = mix(color, crater_dust * 0.92, bowl_floor * 0.82);
    let rim_edge = max(detail_width.y * 0.65, 0.012);
    let chipped_threshold = 0.70 + wave.y * 0.08;
    let rim_chips = smoothstep(chipped_threshold - rim_edge, chipped_threshold + rim_edge, rim);
    color = mix(color, vec3<f32>(0.23, 0.275, 0.36), rim_chips * 0.16);

    // A dark hairline and its pale mineral lip read as a fracture at gameplay
    // distance. Integrate their visibility into the pixel footprint as they
    // become subpixel instead of allowing the fine edges to shimmer.
    let fracture = (1.0 - smoothstep(0.045 - mineral_width, 0.045 + mineral_width, abs(mineral)))
        * min(1.0, 0.045 / mineral_width);
    let lip = (1.0 - smoothstep(0.025 - mineral_width, 0.025 + mineral_width, abs(mineral - 0.066)))
        * min(1.0, 0.025 / mineral_width);
    let fracture_exposure = 1.0 - bowl * 0.55;
    color = mix(color, vec3<f32>(0.025, 0.05, 0.10), fracture * 0.62 * fracture_exposure);
    color = mix(color, vec3<f32>(0.43, 0.52, 0.66), lip * 0.72 * fracture_exposure);

    // Sparse octahedral inclusions are tiny copper chips within the rock,
    // rather than emissive sparks. Their world-space cells never animate.
    let ore_position = position * 2.4;
    let ore_cell = floor(ore_position);
    let ore_shape = dot(abs(fract(ore_position) - vec3<f32>(0.5)), vec3<f32>(1.0));
    let ore_width = max(dot(pixel_width, vec3<f32>(2.4)), 0.0001);
    let ore = (1.0 - smoothstep(0.32 - ore_width, 0.32 + ore_width, ore_shape))
        * min(1.0, 0.16 / ore_width) * select(0.0, 1.0, stone_grain(ore_cell) > 0.82);
    color = mix(color, vec3<f32>(0.54, 0.235, 0.115), ore * (1.0 - bowl * 0.8));

    // Overgrowth follows crevices near the actual grass boundary, leaving the
    // exposed asteroid face readable. Keep its silhouette crisp and restrained.
    let near_grass = 1.0 - smoothstep(0.58, 0.88, rock_coverage);
    let crevice = 1.0 - smoothstep(0.04, 0.15, abs(mineral));
    let moss = near_grass * crevice * (0.34 + 0.16 * wave.y);
    color = mix(color, vec3<f32>(0.055, 0.16, 0.11), moss);

    let grain_visibility = 1.0 - smoothstep(0.35, 0.90, length(pixel_width) * 22.0);
    let grain = stone_grain(floor(position * 22.0)) - 0.5;
    color += vec3<f32>(grain * 0.014 * grain_visibility);

    let tangent_gradient = gradient - normal * dot(gradient, normal);
    let detail_visibility = (1.0 - smoothstep(0.3, 0.9, mineral_width)) * (1.0 - bowl * 0.6);
    let detail_normal = normalize(normal - tangent_gradient * (0.07 * detail_visibility));
    return StoneSurface(color, detail_normal);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Derivatives remain outside material branches and early emissive returns.
    let pixel_width = fwidth(input.world_position);
    let coverage_width = max(fwidth(input.variation) * 0.65, 0.0001);
    let detail_width = fwidth(input.detail.xy);
    var n = normalize(input.normal);
    var base: vec3<f32>;
    var gloss = 0.0;
    var gloss_power = 24.0;
    switch input.material {
        case 0u: {
            let radial = normalize(input.world_position);
            let region = 0.5 + 0.5 * sin(dot(radial, vec3<f32>(5.2, 7.8, 3.6)));
            base = mix(vec3<f32>(0.065, 0.22, 0.085), vec3<f32>(0.16, 0.33, 0.09), region);
            // Terrain variation carries interpolated rock coverage. A narrow
            // screen-space threshold makes a crisp continuous material edge.
            let rock = smoothstep(0.5 - coverage_width, 0.5 + coverage_width, input.variation);
            if (rock > 0.0) {
                let stone = stone_surface(input.world_position, n, pixel_width, input.variation, input.detail, detail_width);
                base = mix(base, stone.color, rock);
                n = normalize(mix(n, stone.normal, rock));
            }
        }
        case 1u: {
            let stone = stone_surface(input.world_position, n, pixel_width, 1.0, input.detail, detail_width);
            base = stone.color;
            n = stone.normal;
        }
        case 2u: {
            base = vec3<f32>(0.82, 0.16, 0.105);
            gloss = 0.85;
            gloss_power = 72.0;
        }
        case 3u: {
            base = vec3<f32>(0.88, 0.82, 0.65);
            gloss = 0.45;
            gloss_power = 48.0;
        }
        case 4u: {
            base = vec3<f32>(0.028, 0.074, 0.083);
            gloss = 0.07;
            gloss_power = 18.0;
        }
        // These are lights, so their color remains visible on the night side.
        case 5u: {
            return vec4<f32>(vec3<f32>(0.12, 1.05, 1.28) * (1.55 + frame.mower_forward.w * 0.9), 1.0);
        }
        case 6u: { return vec4<f32>(2.4, 0.13, 0.045, 1.0); }
        case 7u: { return vec4<f32>(0.62, 1.25, 0.20, 1.0); }
        default: { base = vec3<f32>(0.12, 0.20, 0.23); }
    }
    let light = normalize(-frame.light_epoch.xyz);
    let diffuse = max(dot(n, light), 0.0);
    let shadow = shadow_factor(input.shadow_position);
    let view = normalize(frame.camera_time.xyz - input.world_position);
    let fresnel = 1.0 - max(dot(n, view), 0.0);
    let rim = fresnel * fresnel * fresnel;
    var highlight = 0.0;
    if (gloss > 0.0) {
        let half_vector = normalize(light + view + n * 0.0001);
        highlight = pow(max(dot(n, half_vector), 0.0), gloss_power) * gloss * shadow * diffuse;
    }
    // Cool fill lifts the night side and cast shadows while tapering away
    // quadratically on sunlit faces, preserving the warm direct-light contrast.
    let unlit = 1.0 - diffuse * shadow;
    let ambient = vec3<f32>(0.29, 0.34, 0.48) * (1.0 + 0.85 * unlit * unlit);
    let sunshine = vec3<f32>(1.22, 1.04, 0.76);
    var color = base * (ambient + sunshine * diffuse * shadow);
    color += sunshine * highlight;
    color += vec3<f32>(0.12, 0.19, 0.23) * rim * mix(0.22, 0.65, gloss);
    if (input.material <= 1u) {
        color *= mower_contact(input.world_position);
    }
    return vec4<f32>(color, 1.0);
}
