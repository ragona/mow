@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return VertexOutput(vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0), uv);
}

fn highlight(uv: vec2<f32>) -> vec3<f32> {
    let color = min(textureSampleLevel(source, linear_sampler, uv, 0.0).rgb, vec3<f32>(8.0));
    let brightness = max(color.r, max(color.g, color.b));
    let knee = clamp(brightness - 0.75, 0.0, 0.8);
    let contribution = max(brightness - 1.15, knee * knee / 1.6) / max(brightness, 0.0001);
    return color * contribution;
}

@fragment
fn prefilter(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(source));
    // A tent gathers small lamps before downsampling, avoiding blinking highlights.
    var color = highlight(input.uv) * 0.25;
    color += highlight(input.uv + texel * vec2<f32>(-1.5, -1.5)) * 0.1875;
    color += highlight(input.uv + texel * vec2<f32>( 1.5, -1.5)) * 0.1875;
    color += highlight(input.uv + texel * vec2<f32>(-1.5,  1.5)) * 0.1875;
    color += highlight(input.uv + texel * vec2<f32>( 1.5,  1.5)) * 0.1875;
    return vec4<f32>(color, 1.0);
}

fn blur(uv: vec2<f32>, axis: vec2<f32>) -> vec4<f32> {
    let step = axis / vec2<f32>(textureDimensions(source));
    var color = textureSampleLevel(source, linear_sampler, uv, 0.0).rgb * 0.227027;
    color += textureSampleLevel(source, linear_sampler, uv + step * 1.384615, 0.0).rgb * 0.316216;
    color += textureSampleLevel(source, linear_sampler, uv - step * 1.384615, 0.0).rgb * 0.316216;
    color += textureSampleLevel(source, linear_sampler, uv + step * 3.230769, 0.0).rgb * 0.070270;
    color += textureSampleLevel(source, linear_sampler, uv - step * 3.230769, 0.0).rgb * 0.070270;
    return vec4<f32>(color, 1.0);
}

@fragment
fn horizontal(input: VertexOutput) -> @location(0) vec4<f32> {
    return blur(input.uv, vec2<f32>(1.0, 0.0));
}

@fragment
fn vertical(input: VertexOutput) -> @location(0) vec4<f32> {
    return blur(input.uv, vec2<f32>(0.0, 1.0));
}
