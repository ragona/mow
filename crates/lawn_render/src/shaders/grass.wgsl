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

fn sample_mowing(address: FaceUv) -> vec4<u32> {
    let dimensions = textureDimensions(mowing);
    let pixel = vec2<i32>(clamp(
        floor((address.uv * 0.5 + vec2<f32>(0.5)) * vec2<f32>(dimensions)),
        vec2<f32>(0.0), vec2<f32>(dimensions - vec2<u32>(1u))
    ));
    return textureLoad(mowing, pixel, i32(address.face), 0);
}

fn interaction_index(address: FaceUv) -> u32 {
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

    let address = direction_to_face_uv(input.root_position);
    let state = sample_mowing(address);
    let cut = f32(state.x) / 255.0;
    let root_direction = normalize(input.root_position);
    // Match the terrain's broad, continuous garden patches across cube faces.
    let regional_variation = 0.5 + 0.5 * sin(dot(root_direction, vec3<f32>(5.2, 7.8, 3.6)));
    let individual_variation = hash01(seed * 1597334677u);
    let height_scale = max(frame.options.w, 0.1);
    let sqrt_height_scale = sqrt(height_scale);
    let uncut_height = mix(0.39, 0.63, individual_variation * 0.50 + regional_variation * 0.50) * height_scale;
    // Custom worlds can have extremely short grass. Cutting must never make
    // those blades taller, and their comb bend must shrink with the stubble.
    let stubble_height = min(0.064, uncut_height * 0.22);
    let blade_height = mix(uncut_height, stubble_height, cut);
    let tip = input.local_position.y;
    let camera_delta = frame.camera_time.xyz - input.root_position;
    let camera_distance = length(camera_delta);
    let view = camera_delta / max(camera_distance, 0.0001);
    let distant_width = mix(1.0, 1.42, smoothstep(20.0, 58.0, camera_distance));
    let tall_width = mix(1.0, sqrt_height_scale, 0.55);
    let horizontal = (rotated_tangent * input.local_position.x + rotated_bitangent * input.local_position.z) * distant_width * tall_width;
    let wind_phase = dot(root_direction, vec3<f32>(13.1, 9.7, 17.3)) * 5.0 + frame.camera_time.w * 1.25;
    let wind = (rotated_tangent * sin(wind_phase) + rotated_bitangent * cos(wind_phase * 0.73)) * (0.043 * sqrt_height_scale);
    let comb = decode_oct(vec2<f32>(state.yz) / 255.0 * 2.0 - vec2<f32>(1.0));
    let projected_comb = comb - surface_normal * dot(comb, surface_normal);
    let comb_length_sq = dot(projected_comb, projected_comb);
    let comb_tangent = select(tangent, projected_comb * inverseSqrt(max(comb_length_sq, 0.000001)), comb_length_sq > 0.000001);
    let interaction_offset = interaction[interaction_index(address)].xyz;
    let interaction_scale = mix(1.0, height_scale, 0.35);
    // Interaction is stored as a world-space offset, so attenuate it by the
    // remaining flexible blade length. Squaring the ratio keeps the broad wash
    // dramatic in tall grass while preventing tiny stubble from stretching.
    let remaining_height_ratio = clamp(blade_height / max(uncut_height, 0.001), 0.0, 1.0);
    let wash_response = remaining_height_ratio * remaining_height_ratio;
    let comb_bend = stubble_height * (0.095 / 0.064);
    let requested_bend = wind * (1.0 - cut) + comb_tangent * cut * comb_bend + interaction_offset * interaction_scale * wash_response;
    let tangent_bend = requested_bend - surface_normal * dot(requested_bend, surface_normal);
    // Rotor wash bends a blade within its own length, even when interaction
    // forces accumulate. Lower the tip as it leans instead of stretching it.
    let bend_limit = blade_height * 0.8;
    let bounded_bend = tangent_bend * min(1.0, bend_limit * inverseSqrt(max(dot(tangent_bend, tangent_bend), 0.000001)));
    let bend = bounded_bend * tip * tip;
    let upright_height = tip * blade_height;
    let bent_height = sqrt(max(upright_height * upright_height - dot(bend, bend), 0.0));
    let world_position = input.root_position + surface_normal * bent_height + horizontal + bend;

    var output: VertexOutput;
    output.clip_position = frame.view_proj * vec4<f32>(world_position, 1.0);
    output.world_position = world_position;
    output.normal = normalize(surface_normal * 0.72 + normalize(horizontal + rotated_tangent * 0.001) * 0.28 - bend * 0.8);
    let patch_variation = individual_variation * 0.16 + regional_variation * 0.84;
    let high_contrast = frame.options.z;
    let tall_color = mix(vec3<f32>(0.06, 0.245, 0.10), vec3<f32>(0.16, 0.395, 0.145), patch_variation);
    // A signed response preserves opposite mowing passes. A small sun term
    // keeps the brush direction visible with the nearly overhead chase camera.
    let brush_light = normalize(-frame.light_epoch.xyz);
    let brush_response = clamp(dot(comb_tangent, view) * 1.65 + dot(comb_tangent, brush_light) * 0.35, -1.0, 1.0);
    let brushed = 0.5 + 0.5 * brush_response;
    let short_color = mix(vec3<f32>(0.045, 0.20, 0.09), vec3<f32>(0.17, 0.36, 0.14), brushed);
    output.color = mix(tall_color, short_color * mix(1.0, 1.35, high_contrast), cut);
    // Evaluate the broad contact shade per vertex; grass fragments keep a
    // single hardware-filtered shadow lookup and no procedural color noise.
    let mower_up = normalize(frame.mower_position.xyz + vec3<f32>(0.0, 0.0001, 0.0));
    let mower_delta = input.root_position - frame.mower_position.xyz;
    let mower_height = dot(mower_delta, mower_up);
    let mower_planar = mower_delta - mower_up * mower_height;
    let along = dot(mower_planar, frame.mower_forward.xyz);
    let ellipse = max(dot(mower_planar, mower_planar) - along * along * 0.16, 0.0) / 2.9;
    let contact = (1.0 - smoothstep(0.02, 1.0, ellipse))
        * (1.0 - smoothstep(1.6, 3.2, abs(mower_height))) * frame.mower_position.w;
    output.color *= 1.0 - contact * 0.32;
    output.shadow_position = frame.light_view_proj * vec4<f32>(world_position, 1.0);
    output.normalized_height = tip;
    return output;
}

fn shadow_factor(position: vec4<f32>) -> f32 {
    let projected = position.xyz / position.w;
    let uv = projected.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || projected.z < 0.0 || projected.z > 1.0) { return 1.0; }
    return textureSampleCompareLevel(shadow_map, shadow_sampler, uv, projected.z - 0.0015);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(input.normal);
    let light = normalize(-frame.light_epoch.xyz);
    let diffuse = max(dot(normal, light), 0.0);
    let backlit = max(dot(-normal, light), 0.0);
    let transmission = backlit * backlit * 0.38;
    let tip_light = smoothstep(0.12, 1.0, input.normalized_height);
    let shadow = shadow_factor(input.shadow_position);
    let ambient = vec3<f32>(0.27, 0.35, 0.47);
    let sunshine = vec3<f32>(1.18, 1.06, 0.73);
    let root_darkening = mix(0.53, 1.0, smoothstep(0.0, 0.68, input.normalized_height));
    var color = input.color * (ambient + sunshine * diffuse * shadow) * root_darkening;
    color += input.color * vec3<f32>(1.15, 1.12, 0.62) * transmission * tip_light * shadow;
    // Soft warm tips give the grass a velvet finish without specular sparkle.
    color += vec3<f32>(0.042, 0.049, 0.017) * tip_light * tip_light * diffuse * shadow;
    return vec4<f32>(color, 1.0);
}
