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
    let fragment = kind >= 4u && kind <= 6u;
    var angle = select(0.0, sin(tumble) * 0.8, kind == 0u);
    if fragment {
        angle = variation * 6.2831853 + (1.0 - life) * (7.0 + variation * 5.0) * input.appearance.w;
    }
    let crosswise = side * cos(angle) + upright * sin(angle);
    let along = upright * cos(angle) - side * sin(angle);
    let taper = select(1.0, mix(1.0, 0.5, input.local.y), kind == 0u);
    let length = select(1.0, 1.8, kind == 0u);
    let expansion = select(1.0, 1.0 + (1.0 - life) * 0.75, kind == 1u);
    // Modest foreshortening makes the cut metal pieces tumble as facets;
    // reduced motion holds their initial orientation and width still.
    let turning = select(1.0, 0.55 + abs(cos(angle * 1.4)) * 0.45, fragment);
    let position = input.position_size.xyz
        + crosswise * input.local.x * input.position_size.w * taper * expansion * turning
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
    if input.kind == 4u || input.kind == 5u {
        // Clipped corners and two broad tones suggest body panels, rather
        // than a flat square confetti sprite or a lingering intact mower.
        let edge = max(abs(local.x), abs(local.y));
        let corner = abs(local.x) + abs(local.y);
        let shape = (1.0 - smoothstep(0.88, 1.0, edge))
            * (1.0 - smoothstep(1.35, 1.58, corner));
        let blue = mix(vec3<f32>(0.10, 0.20, 0.65), vec3<f32>(0.30, 0.45, 0.92), input.variation);
        let cream = vec3<f32>(0.76, 0.81, 0.96);
        let base = select(blue, cream, input.kind == 5u);
        let facet = select(0.76, 1.15, local.x * 0.6 + local.y > 0.08);
        let bevel = smoothstep(0.67, 0.85, edge) * 0.18;
        return vec4<f32>(base * light * facet + vec3<f32>(bevel), shape * fade * input.opacity);
    }
    if input.kind == 6u {
        let disc = 1.0 - smoothstep(0.86, 1.0, radius);
        let ring = smoothstep(0.60, 0.74, radius);
        let body = vec3<f32>(0.075, 0.12, 0.23) * light;
        let pad_light = vec3<f32>(0.48, 0.67, 1.5);
        return vec4<f32>(mix(body, pad_light, ring), disc * fade * input.opacity);
    }
    if input.kind == 7u {
        let diamond = abs(local.x) + abs(local.y);
        let shape = 1.0 - smoothstep(0.48, 1.0, diamond);
        let core = 1.0 - smoothstep(0.05, 0.35, radius);
        let color = mix(vec3<f32>(0.20, 1.00, 1.18), vec3<f32>(0.80, 1.30, 1.45), core);
        return vec4<f32>(color, shape * fade * input.opacity);
    }
    let soft_disc = 1.0 - smoothstep(0.35, 1.0, radius);
    let core = 1.0 - smoothstep(0.0, 0.40, radius);
    let mint = mix(vec3<f32>(0.56, 1.0, 0.84), vec3<f32>(0.92, 1.0, 0.90), core);
    let cream = mix(vec3<f32>(1.0, 0.69, 0.28), vec3<f32>(1.0, 0.94, 0.65), core);
    let color = select(mint, cream, input.kind == 3u) * (1.0 + core * 0.25);
    let arrival = smoothstep(0.0, 0.12, 1.0 - input.life);
    return vec4<f32>(color, soft_disc * fade * arrival * input.opacity);
}
