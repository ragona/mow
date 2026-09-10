struct FrameUniform {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    camera_time: vec4<f32>,
    light_epoch: vec4<f32>,
    options: vec4<f32>,
    locator: vec4<f32>,
    mower_position: vec4<f32>,
    mower_forward: vec4<f32>,
    rival_position: vec4<f32>,
    rival_forward: vec4<f32>,
};
@group(0) @binding(0) var<uniform> frame: FrameUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) material: u32,
    @location(3) variation: f32,
};

@vertex
fn vs_main(input: VertexInput) -> @builtin(position) vec4<f32> {
    return frame.light_view_proj * vec4<f32>(input.position, 1.0);
}
