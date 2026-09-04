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
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) @interpolate(flat) material: u32,
    @location(3) variation: f32,
    @location(4) shadow_position: vec4<f32>,
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

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    var base: vec3<f32>;
    var gloss = 0.0;
    var gloss_power = 24.0;
    switch input.material {
        case 0u: {
            let radial = normalize(input.world_position);
            let region = 0.5 + 0.5 * sin(dot(radial, vec3<f32>(5.2, 7.8, 3.6)));
            base = mix(vec3<f32>(0.065, 0.22, 0.085), vec3<f32>(0.16, 0.33, 0.09), region * 0.85 + input.variation * 0.15);
        }
        case 1u: {
            base = mix(vec3<f32>(0.46, 0.43, 0.36), vec3<f32>(0.76, 0.70, 0.58), input.variation);
            gloss = 0.035;
            gloss_power = 12.0;
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
    let n = normalize(input.normal);
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
