struct ForceSource {
    position_radius: vec4<f32>,
    direction_strength: vec4<f32>,
};

struct InteractionParams {
    dt: f32,
    time: f32,
    source_count: u32,
    resolution: u32,
    stiffness: f32,
    damping: f32,
    planet_radius: f32,
    maximum_displacement: f32,
};

@group(0) @binding(0) var<storage, read> displacement_in: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> velocity_in: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> displacement_out: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> velocity_out: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> sources: array<ForceSource>;
@group(0) @binding(5) var<uniform> params: InteractionParams;

fn face_direction(face: u32, uv: vec2<f32>) -> vec3<f32> {
    var p: vec3<f32>;
    switch face {
        case 0u: { p = vec3<f32>(1.0, uv.y, -uv.x); }
        case 1u: { p = vec3<f32>(-1.0, uv.y, uv.x); }
        case 2u: { p = vec3<f32>(uv.x, 1.0, -uv.y); }
        case 3u: { p = vec3<f32>(uv.x, -1.0, uv.y); }
        case 4u: { p = vec3<f32>(uv.x, uv.y, 1.0); }
        default: { p = vec3<f32>(-uv.x, uv.y, -1.0); }
    }
    return normalize(p);
}

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let count = 6u * params.resolution * params.resolution;
    let index = id.x;
    if (index >= count) { return; }
    let face_area = params.resolution * params.resolution;
    let face = index / face_area;
    let local = index % face_area;
    let x = local % params.resolution;
    let y = local / params.resolution;
    let uv = (vec2<f32>(f32(x) + 0.5, f32(y) + 0.5) / f32(params.resolution)) * 2.0 - vec2<f32>(1.0);
    let normal = face_direction(face, uv);
    let point = normal * params.planet_radius;
    var applied = vec3<f32>(0.0);
    for (var source_index = 0u; source_index < params.source_count; source_index += 1u) {
        let source = sources[source_index];
        let delta = point - source.position_radius.xyz;
        let radius = source.position_radius.w;
        let distance_squared = dot(delta, delta);
        // Almost every cell is outside the small source disks. Avoid square
        // roots, normalization, and spring forcing for those sources.
        if (distance_squared >= radius * radius) { continue; }
        let distance = sqrt(distance_squared);
        let falloff = 1.0 - smoothstep(0.0, radius, distance);
        let tangent_delta = delta - normal * dot(delta, normal);
        // A broad soft core has no preferred direction at the center. The old
        // normalization turned tiny center crossings into a full-force flip.
        let core = radius * 0.20;
        let radial = tangent_delta * inverseSqrt(dot(tangent_delta, tangent_delta) + core * core);
        let directed = source.direction_strength.xyz - normal * dot(source.direction_strength.xyz, normal);
        // Coherent pressure rings travel outward, always pushing outward. A
        // small swirl gives a stationary hover life without per-blade noise.
        let pressure = 0.90 + 0.10 * sin(distance * 2.2 - params.time * 4.0);
        let swirl = cross(normal, radial) * 0.09;
        applied += (radial + swirl + directed) * source.direction_strength.w * falloff * pressure;
    }
    var displacement = displacement_in[index].xyz;
    var velocity = velocity_in[index].xyz;
    // Exact critically damped spring step for this frame's pressure. Unlike
    // Euler integration it keeps the same soft response at 30, 60 and 120 Hz.
    let equilibrium = applied / params.stiffness;
    let omega = params.damping * 0.5;
    let offset = displacement - equilibrium;
    let spring_velocity = velocity + offset * omega;
    let decay = exp(-omega * params.dt);
    displacement = equilibrium + (offset + spring_velocity * params.dt) * decay;
    velocity = (velocity - spring_velocity * (omega * params.dt)) * decay;
    velocity -= normal * dot(velocity, normal);
    displacement -= normal * dot(displacement, normal);
    let length_squared = dot(displacement, displacement);
    if (length_squared > params.maximum_displacement * params.maximum_displacement) {
        let limit_direction = normalize(displacement);
        displacement = limit_direction * params.maximum_displacement;
        // Preserve tangential movement around the limit, removing only the
        // outward velocity that would repeatedly push through it.
        velocity -= limit_direction * max(dot(velocity, limit_direction), 0.0);
    }
    displacement_out[index] = vec4<f32>(displacement, 0.0);
    velocity_out[index] = vec4<f32>(velocity, 0.0);
}
