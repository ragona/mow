@group(0) @binding(0) var world_texture: texture_2d<f32>;
@group(0) @binding(1) var world_sampler: sampler;
@group(0) @binding(2) var bloom_texture: texture_2d<f32>;
@group(0) @binding(4) var sky_texture: texture_cube<f32>;

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

fn sky(uv: vec2<f32>, ray: vec3<f32>, sky_lod: f32) -> vec3<f32> {
    // The texture is a complete seamless world, including clouds, stars and a
    // cratered companion moon. Orbiting changes the view, never the sky itself.
    var color = textureSampleLevel(sky_texture, world_sampler, ray, sky_lod).rgb;
    let centered = (uv - vec2<f32>(0.5)) * vec2<f32>(frame.display.x, 1.0);
    // The low garden atmosphere follows the actual planet through zoom and the
    // editor's asymmetric viewport, leaving the distant stars anchored in space.
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
    let far = frame.inverse_view_proj * vec4<f32>(input.uv.x * 2.0 - 1.0, 1.0 - input.uv.y * 2.0, 0.99, 1.0);
    let ray = normalize(far.xyz / far.w - frame.camera_radius.xyz);
    // Evaluate derivatives uniformly before the coverage branch. Explicit LOD
    // keeps subpixel stars quiet without performing a sky fetch behind terrain.
    let major_axis = max(max(abs(ray.x), abs(ray.y)), abs(ray.z));
    let footprint = max(length(dpdx(ray)), length(dpdy(ray))) * f32(textureDimensions(sky_texture).x) * 0.5 / (major_axis * major_axis);
    let sky_lod = max(log2(max(footprint, 0.0001)), 0.0);
    var background = vec3<f32>(0.0);
    if (world.a < 1.0) {
        background = sky(input.uv, ray, sky_lod);
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
