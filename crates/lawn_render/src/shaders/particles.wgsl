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
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;

struct VertexInput {
    @location(0) local: vec2<f32>,
    @location(1) position_size: vec4<f32>,
    @location(2) velocity_life: vec4<f32>,
    @location(3) appearance: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) life: f32,
    @location(1) variation: f32,
    @location(2) sunlight: f32,
    @location(3) local: vec2<f32>,
    @location(4) @interpolate(flat) kind: u32,
    @location(5) opacity: f32,
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
    let variation = input.appearance.y;
    let kind = u32(input.appearance.x);
    let life = input.velocity_life.w;
    // Lifetime freezes along with the CPU simulation when paused. Neither
    // particle orientation nor the soft occasion accents use the scene clock.
    let tumble = (1.0 - life) * (5.0 + variation * 4.0) + variation * 6.2831853;
    let angle = select(0.0, sin(tumble) * 0.8, kind == 0u);
    let crosswise = side * cos(angle) + upright * sin(angle);
    let along = upright * cos(angle) - side * sin(angle);
    let taper = select(1.0, mix(1.0, 0.5, input.local.y), kind == 0u);
    let length = select(1.0, 1.8, kind == 0u);
    let expansion = select(1.0, 1.0 + (1.0 - life) * 0.75, kind == 1u);
    let position = input.position_size.xyz
        + crosswise * input.local.x * input.position_size.w * taper * expansion
        + along * (input.local.y - 0.5) * input.position_size.w * length * expansion;
    var output: VertexOutput;
    output.clip_position = frame.view_proj * vec4<f32>(position, 1.0);
    output.life = life;
    output.variation = variation;
    output.sunlight = clamp(dot(radial, normalize(-frame.light_epoch.xyz)) * 1.5 + 0.2, 0.0, 1.0);
    output.local = input.local;
    output.kind = kind;
    output.opacity = input.appearance.z;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let fade = smoothstep(0.0, 0.3, input.life);
    let light = vec3<f32>(0.32, 0.39, 0.55) + vec3<f32>(1.10, 0.92, 0.62) * input.sunlight;
    if input.kind == 0u {
        let fresh_green = mix(vec3<f32>(0.22, 0.48, 0.075), vec3<f32>(0.65, 0.79, 0.18), input.variation);
        let golden_tip = vec3<f32>(0.93, 0.76, 0.24);
        let base = mix(fresh_green, golden_tip, smoothstep(0.76, 1.0, input.variation) * input.local.y);
        let color = base * light * (0.84 + input.local.y * 0.25);
        return vec4<f32>(color, fade * input.opacity);
    }
    let local = vec2<f32>(input.local.x, input.local.y - 0.5) * 2.0;
    let radius = length(local);
    if input.kind == 1u {
        let stone = mix(vec3<f32>(0.32, 0.34, 0.32), vec3<f32>(0.67, 0.56, 0.40), input.variation);
        let soft_disc = 1.0 - smoothstep(0.08, 1.0, radius);
        let arrival = smoothstep(0.0, 0.10, 1.0 - input.life);
        return vec4<f32>(stone * light, soft_disc * fade * arrival * input.opacity);
    }
    let soft_disc = 1.0 - smoothstep(0.35, 1.0, radius);
    let core = 1.0 - smoothstep(0.0, 0.40, radius);
    let mint = mix(vec3<f32>(0.56, 1.0, 0.84), vec3<f32>(0.92, 1.0, 0.90), core);
    let cream = mix(vec3<f32>(1.0, 0.69, 0.28), vec3<f32>(1.0, 0.94, 0.65), core);
    let color = select(mint, cream, input.kind == 3u) * (1.0 + core * 0.25);
    let arrival = smoothstep(0.0, 0.12, 1.0 - input.life);
    return vec4<f32>(color, soft_disc * fade * arrival * input.opacity);
}
