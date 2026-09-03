struct FrameUniform {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    camera_time: vec4<f32>,
    light_epoch: vec4<f32>,
    options: vec4<f32>,
    locator: vec4<f32>,
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
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || depth > 1.0) {
        return 1.0;
    }
    return textureSampleCompare(shadow_map, shadow_sampler, uv, depth - 0.0015);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(input.normal);
    let light = normalize(-frame.light_epoch.xyz);
    let diffuse = max(dot(n, light), 0.0);
    let shadow = mix(0.44, 1.0, shadow_factor(input.shadow_position));
    var base: vec3<f32>;
    switch input.material {
        case 0u: { base = mix(vec3<f32>(0.115, 0.27, 0.075), vec3<f32>(0.17, 0.35, 0.095), input.variation); }
        case 1u: { base = mix(vec3<f32>(0.28, 0.25, 0.22), vec3<f32>(0.43, 0.39, 0.34), input.variation); }
        case 2u: { base = vec3<f32>(0.92, 0.48, 0.16); }
        case 3u: { base = vec3<f32>(0.84, 0.88, 0.82); }
        case 4u: { base = vec3<f32>(0.13, 0.18, 0.19); }
        case 5u: { base = vec3<f32>(0.12, 0.68, 0.82) * 1.7; }
        case 6u: { base = vec3<f32>(1.0, 0.08, 0.035) * 2.2; }
        case 7u: { base = vec3<f32>(0.50, 0.88, 0.16) * 1.35; }
        default: { base = vec3<f32>(0.18, 0.23, 0.25); }
    }
    let view = normalize(frame.camera_time.xyz - input.world_position);
    let rim = pow(1.0 - max(dot(n, view), 0.0), 3.0);
    let color = base * (0.36 + diffuse * 0.78 * shadow) + rim * vec3<f32>(0.08, 0.12, 0.1);
    return vec4<f32>(color, 1.0);
}
