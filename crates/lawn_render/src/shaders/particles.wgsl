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
    @location(0) local: vec2<f32>,
    @location(1) position_size: vec4<f32>,
    @location(2) velocity_life: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) life: f32,
    @location(1) variation: f32,
    @location(2) sunlight: f32,
    @location(3) tip: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let radial = normalize(input.position_size.xyz);
    let camera_direction = normalize(frame.camera_time.xyz - input.position_size.xyz);
    let auxiliary = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(camera_direction.y) > 0.9);
    let fallback_side = normalize(cross(camera_direction, auxiliary));
    let radial_side = cross(camera_direction, radial);
    let side = normalize(select(radial_side, fallback_side, dot(radial_side, radial_side) < 0.0001));
    let upright = normalize(cross(side, camera_direction));
    // Size provides a stable per-clipping seed; hashing its moving position
    // would make the color flicker as the clipping drifts through the air.
    let variation = fract(input.position_size.w * 167.3);
    // Derive tumble from lifetime so pausing the CPU particles also freezes
    // their orientation, independently of the scene's ambient animation clock.
    let tumble = (1.0 - input.velocity_life.w) * (5.0 + variation * 4.0) + variation * 6.2831853;
    let angle = sin(tumble) * 0.8;
    let crosswise = side * cos(angle) + upright * sin(angle);
    let along = upright * cos(angle) - side * sin(angle);
    let position = input.position_size.xyz
        + crosswise * input.local.x * input.position_size.w
        + along * (input.local.y - 0.4) * input.position_size.w * 1.8;
    var output: VertexOutput;
    output.clip_position = frame.view_proj * vec4<f32>(position, 1.0);
    output.life = input.velocity_life.w;
    output.variation = variation;
    output.sunlight = clamp(dot(radial, normalize(-frame.light_epoch.xyz)) * 1.5 + 0.2, 0.0, 1.0);
    output.tip = input.local.y;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let fresh_green = mix(vec3<f32>(0.22, 0.48, 0.075), vec3<f32>(0.65, 0.79, 0.18), input.variation);
    let golden_tip = vec3<f32>(0.93, 0.76, 0.24);
    let base = mix(fresh_green, golden_tip, smoothstep(0.76, 1.0, input.variation) * input.tip);
    let light = vec3<f32>(0.32, 0.39, 0.55) + vec3<f32>(1.10, 0.92, 0.62) * input.sunlight;
    let color = base * light * (0.84 + input.tip * 0.25);
    return vec4<f32>(color, smoothstep(0.0, 0.24, input.life) * 0.95);
}
