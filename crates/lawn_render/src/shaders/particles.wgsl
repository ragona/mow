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
    @location(0) local: vec2<f32>,
    @location(1) position_size: vec4<f32>,
    @location(2) velocity_life: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) life: f32,
    @location(1) variation: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let radial = normalize(input.position_size.xyz);
    let camera_direction = normalize(frame.camera_time.xyz - input.position_size.xyz);
    let side = normalize(cross(camera_direction, radial) + vec3<f32>(0.00001));
    let along = normalize(input.velocity_life.xyz + radial * 0.35);
    let position = input.position_size.xyz
        + side * input.local.x * input.position_size.w
        + along * input.local.y * input.position_size.w * 2.8;
    var output: VertexOutput;
    output.clip_position = frame.view_proj * vec4<f32>(position, 1.0);
    output.life = input.velocity_life.w;
    output.variation = fract(dot(input.position_size.xyz, vec3<f32>(0.13, 0.31, 0.73)));
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = mix(vec3<f32>(0.35, 0.64, 0.12), vec3<f32>(0.67, 0.81, 0.20), input.variation);
    return vec4<f32>(color, smoothstep(0.0, 0.22, input.life) * 0.9);
}
