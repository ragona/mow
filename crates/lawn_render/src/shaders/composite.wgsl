@group(0) @binding(0) var world_texture: texture_2d<f32>;
@group(0) @binding(1) var world_sampler: sampler;
@group(0) @binding(2) var bloom_texture: texture_2d<f32>;

struct CompositeUniform {
    inverse_view_proj: mat4x4<f32>,
    camera_radius: vec4<f32>,
    sun_time: vec4<f32>,
    display: vec4<f32>,
};
@group(0) @binding(3) var<uniform> frame: CompositeUniform;

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
    let aspect = frame.display.x;
    let centered = (uv - vec2<f32>(0.5)) * vec2<f32>(aspect, 1.0);
    // A softly painted dusk backdrop, with a warm horizon beneath the garden.
    var color = mix(vec3<f32>(0.075, 0.13, 0.23), vec3<f32>(0.40, 0.24, 0.20), smoothstep(0.10, 1.2, uv.y));
    color += vec3<f32>(0.055, 0.035, 0.025) * exp(-dot(centered, centered) * 1.8);
    let grid = uv * vec2<f32>(aspect, 1.0) * 110.0;
    let cell = floor(grid);
    let point = fract(grid) - vec2<f32>(hash21(cell), hash21(cell + vec2<f32>(21.0)));
    let star = (1.0 - smoothstep(0.015, 0.11, length(point))) * select(0.0, 0.22, hash21(cell + vec2<f32>(7.0, 19.0)) > 0.985);
    color += vec3<f32>(0.85, 0.90, 1.0) * star * (1.0 - uv.y * 0.65);
    // Aspect correction keeps the little companion moon round at any window size.
    let moon_delta = (uv - vec2<f32>(0.84, 0.17)) * vec2<f32>(aspect, 1.0);
    let moon_distance = length(moon_delta);
    let moon = 1.0 - smoothstep(0.030, 0.032, moon_distance);
    let moon_shade = clamp(0.88 - moon_delta.x * 9.0 - moon_delta.y * 6.0, 0.40, 1.0);
    color += vec3<f32>(0.14, 0.075, 0.035) * exp(-moon_distance * 22.0);
    color = mix(color, vec3<f32>(0.78, 0.59, 0.35) * moon_shade, moon);

    // Reconstruct the camera ray so the halo follows the planet through orbit,
    // camera zoom, and the editor's asymmetric viewport without a second mesh.
    let far = frame.inverse_view_proj * vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.99, 1.0);
    let ray = normalize(far.xyz / far.w - frame.camera_radius.xyz);
    let along = max(dot(-frame.camera_radius.xyz, ray), 0.0);
    let closest = frame.camera_radius.xyz + ray * along;
    let distance = length(closest);
    let radius = frame.camera_radius.w;
    let halo = exp(-max(distance - radius, 0.0) / frame.display.z) * smoothstep(radius - 0.7, radius + 0.1, distance);
    let sun_side = clamp(dot(closest / max(distance, 0.001), frame.sun_time.xyz) * 0.5 + 0.5, 0.0, 1.0);
    color += mix(vec3<f32>(0.10, 0.23, 0.32), vec3<f32>(0.55, 0.32, 0.12), sun_side) * halo * 0.45;
    return color * mix(1.0, 0.86, smoothstep(0.45, 1.3, length(centered)));
}

fn composite_color(input: VertexOutput) -> vec3<f32> {
    let world = textureSample(world_texture, world_sampler, input.uv);
    var bloom = vec3<f32>(0.0);
    if (frame.display.w > 0.0) {
        bloom = textureSampleLevel(bloom_texture, world_sampler, input.uv, 0.0).rgb;
    }
    var background = vec3<f32>(0.0);
    if (world.a < 1.0) {
        background = sky(input.uv);
    }
    // Transparent clearing, alpha blending, and MSAA resolving leave RGB
    // premultiplied by coverage. Multiplying it again darkens silhouettes.
    let hdr = world.rgb + bloom * frame.display.w + background * (1.0 - world.a);
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
