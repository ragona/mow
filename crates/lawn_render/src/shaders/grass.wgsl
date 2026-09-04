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
@group(1) @binding(0) var mowing: texture_2d_array<u32>;
@group(1) @binding(1) var<storage, read> interaction: array<vec4<f32>>;

struct VertexInput {
    @location(0) local_position: vec3<f32>,
    @location(1) root_position: vec3<f32>,
    @location(2) packed_normal_seed: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) shadow_position: vec4<f32>,
    @location(4) normalized_height: f32,
};

struct FaceUv {
    uv: vec2<f32>,
    face: u32,
};

fn direction_to_face_uv(direction: vec3<f32>) -> FaceUv {
    let d = normalize(direction);
    let a = abs(d);
    var result: FaceUv;
    if (a.x >= a.y && a.x >= a.z) {
        if (d.x >= 0.0) { result = FaceUv(vec2<f32>(-d.z, d.y) / a.x, 0u); }
        else { result = FaceUv(vec2<f32>(d.z, d.y) / a.x, 1u); }
    } else if (a.y >= a.z) {
        if (d.y >= 0.0) { result = FaceUv(vec2<f32>(d.x, -d.z) / a.y, 2u); }
        else { result = FaceUv(vec2<f32>(d.x, d.z) / a.y, 3u); }
    } else {
        if (d.z >= 0.0) { result = FaceUv(vec2<f32>(d.x, d.y) / a.z, 4u); }
        else { result = FaceUv(vec2<f32>(-d.x, d.y) / a.z, 5u); }
    }
    return result;
}

fn decode_oct(encoded: vec2<f32>) -> vec3<f32> {
    var n = vec3<f32>(encoded.x, encoded.y, 1.0 - abs(encoded.x) - abs(encoded.y));
    if (n.z < 0.0) {
        let old_x = n.x;
        n.x = (1.0 - abs(n.y)) * select(-1.0, 1.0, old_x >= 0.0);
        n.y = (1.0 - abs(old_x)) * select(-1.0, 1.0, n.y >= 0.0);
    }
    return normalize(n);
}

fn sample_mowing(direction: vec3<f32>) -> vec4<u32> {
    let address = direction_to_face_uv(direction);
    let dimensions = textureDimensions(mowing);
    let pixel = vec2<i32>(clamp(
        floor((address.uv * 0.5 + vec2<f32>(0.5)) * vec2<f32>(dimensions)),
        vec2<f32>(0.0), vec2<f32>(dimensions - vec2<u32>(1u))
    ));
    return textureLoad(mowing, pixel, i32(address.face), 0);
}

fn interaction_index(direction: vec3<f32>) -> u32 {
    let address = direction_to_face_uv(direction);
    let resolution = u32(frame.options.y);
    let pixel = vec2<u32>(clamp(
        floor((address.uv * 0.5 + vec2<f32>(0.5)) * f32(resolution)),
        vec2<f32>(0.0), vec2<f32>(f32(resolution - 1u))
    ));
    return address.face * resolution * resolution + pixel.y * resolution + pixel.x;
}

fn hash01(value: u32) -> f32 {
    var x = value;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return f32(x) / 4294967295.0;
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let packed = input.packed_normal_seed;
    let normal_encoded = vec2<f32>(f32(packed & 0x3ffu), f32((packed >> 10u) & 0x3ffu)) / 1023.0 * 2.0 - vec2<f32>(1.0);
    let surface_normal = decode_oct(normal_encoded);
    let seed = packed >> 20u;
    var axis = vec3<f32>(1.0, 0.0, 0.0);
    if (abs(surface_normal.y) < abs(surface_normal.x) && abs(surface_normal.y) <= abs(surface_normal.z)) { axis = vec3<f32>(0.0, 1.0, 0.0); }
    if (abs(surface_normal.z) < abs(surface_normal.x) && abs(surface_normal.z) < abs(surface_normal.y)) { axis = vec3<f32>(0.0, 0.0, 1.0); }
    let tangent = normalize(cross(axis, surface_normal));
    let bitangent = normalize(cross(surface_normal, tangent));
    let angle = hash01(seed * 747796405u + 2891336453u) * 6.2831853;
    let rotated_tangent = tangent * cos(angle) + bitangent * sin(angle);
    let rotated_bitangent = -tangent * sin(angle) + bitangent * cos(angle);

    let state = sample_mowing(input.root_position);
    let cut = f32(state.x) / 255.0;
    let root_direction = normalize(input.root_position);
    let regional_variation = 0.5 + 0.5 * sin(dot(root_direction, vec3<f32>(8.7, 11.3, 6.1)) * 2.6);
    let individual_variation = hash01(seed * 1597334677u);
    let height_scale = max(frame.options.w, 0.1);
    let uncut_height = mix(0.52, 0.84, individual_variation * 0.68 + regional_variation * 0.32) * height_scale;
    let blade_height = mix(uncut_height, 0.085, cut);
    let tip = input.local_position.y;
    let camera_distance = distance(frame.camera_time.xyz, input.root_position);
    let distant_width = mix(1.0, 1.42, smoothstep(20.0, 58.0, camera_distance));
    let tall_width = mix(1.0, sqrt(height_scale), 0.55);
    let horizontal = (rotated_tangent * input.local_position.x + rotated_bitangent * input.local_position.z) * distant_width * tall_width;
    let wind_phase = dot(normalize(input.root_position), vec3<f32>(13.1, 9.7, 17.3)) * 5.0 + frame.camera_time.w * 1.25;
    let wind = (rotated_tangent * sin(wind_phase) + rotated_bitangent * cos(wind_phase * 0.73)) * (0.055 * sqrt(height_scale));
    let comb = decode_oct(vec2<f32>(state.yz) / 255.0 * 2.0 - vec2<f32>(1.0));
    let comb_tangent = normalize(comb - surface_normal * dot(comb, surface_normal) + rotated_tangent * 0.0001);
    let interaction_offset = interaction[interaction_index(input.root_position)].xyz;
    let interaction_scale = mix(1.0, height_scale, 0.35);
    let bend = (wind * (1.0 - cut) + comb_tangent * cut * 0.13 + interaction_offset * interaction_scale) * tip * tip;
    let world_position = input.root_position + surface_normal * (tip * blade_height) + horizontal + bend;

    var output: VertexOutput;
    output.clip_position = frame.view_proj * vec4<f32>(world_position, 1.0);
    output.world_position = world_position;
    output.normal = normalize(surface_normal * 0.72 + normalize(horizontal + rotated_tangent * 0.001) * 0.28 - bend * 0.8);
    let patch_variation = hash01(seed * 2246822519u) * 0.62 + regional_variation * 0.38;
    let high_contrast = frame.options.z;
    let tall_color = mix(vec3<f32>(0.20, 0.48, 0.11), vec3<f32>(0.32, 0.62, 0.16), patch_variation);
    let short_color = mix(vec3<f32>(0.14, 0.34, 0.075), vec3<f32>(0.35, 0.56, 0.13), abs(dot(comb_tangent, normalize(frame.light_epoch.xyz))));
    output.color = mix(tall_color, short_color * mix(1.0, 1.35, high_contrast), cut);
    output.shadow_position = frame.light_view_proj * vec4<f32>(world_position, 1.0);
    output.normalized_height = tip;
    return output;
}

fn shadow_factor(position: vec4<f32>) -> f32 {
    let projected = position.xyz / position.w;
    let uv = projected.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) { return 1.0; }
    return textureSampleCompare(shadow_map, shadow_sampler, uv, projected.z - 0.0015);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(input.normal);
    let light = normalize(-frame.light_epoch.xyz);
    let diffuse = max(dot(normal, light), 0.0);
    let transmission = pow(max(dot(-normal, light), 0.0), 2.0) * 0.22;
    let root_darkening = smoothstep(0.0, 0.42, input.normalized_height);
    let shadow = mix(0.48, 1.0, shadow_factor(input.shadow_position));
    return vec4<f32>(input.color * (0.3 + diffuse * 0.82 * shadow + transmission) * mix(0.72, 1.0, root_darkening), 1.0);
}
