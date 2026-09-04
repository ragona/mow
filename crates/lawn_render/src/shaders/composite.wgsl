@group(0) @binding(0) var world_texture: texture_2d<f32>;
@group(0) @binding(1) var world_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    var output: VertexOutput;
    output.clip_position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    output.uv = vec2<f32>(x, y);
    return output;
}

fn aces_film(color: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((color * (a * color + b)) / (color * (c * color + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn linear_to_srgb(color: vec3<f32>) -> vec3<f32> {
    let low = color * 12.92;
    let high = 1.055 * pow(color, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, color <= vec3<f32>(0.0031308));
}

fn hash21(point: vec2<f32>) -> f32 {
    let p = fract(point * vec2<f32>(123.34, 456.21));
    return fract((p.x + p.y) * (p.x + p.y + 45.32));
}

fn sky(uv: vec2<f32>) -> vec3<f32> {
    let centered = uv - vec2<f32>(0.5);
    var color = mix(vec3<f32>(0.012, 0.023, 0.060), vec3<f32>(0.035, 0.072, 0.125), 1.0 - uv.y);
    let cell = floor(uv * vec2<f32>(240.0, 135.0));
    let star = select(0.0, pow(hash21(cell + vec2<f32>(7.0, 19.0)), 18.0) * 0.75, hash21(cell) > 0.993);
    color += vec3<f32>(0.78, 0.88, 1.0) * star;
    let moon_delta = uv - vec2<f32>(0.82, 0.18);
    let moon = 1.0 - smoothstep(0.066, 0.071, length(moon_delta));
    let moon_shade = clamp(0.82 - moon_delta.x * 4.0 - moon_delta.y * 1.5, 0.28, 1.0);
    color = mix(color, vec3<f32>(0.72, 0.63, 0.43) * moon_shade, moon);
    let vignette = 1.0 - smoothstep(0.45, 0.82, length(centered));
    return color * mix(0.72, 1.0, vignette);
}

fn composite_color(input: VertexOutput) -> vec3<f32> {
    let world = textureSample(world_texture, world_sampler, input.uv);
    // Transparent clearing, alpha blending, and MSAA resolving leave RGB
    // premultiplied by coverage. Multiplying it again darkens silhouettes.
    let hdr = world.rgb + sky(input.uv) * (1.0 - world.a);
    return aces_film(hdr);
}

@fragment
fn fs_main_gamma_framebuffer(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(linear_to_srgb(composite_color(input)), 1.0);
}

@fragment
fn fs_main_linear_framebuffer(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(composite_color(input), 1.0);
}
