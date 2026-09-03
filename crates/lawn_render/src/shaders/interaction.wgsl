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
        let distance = length(delta);
        let falloff = smoothstep(source.position_radius.w, 0.0, distance);
        let radial = normalize(delta - normal * dot(delta, normal) + vec3<f32>(0.00001));
        let directed = source.direction_strength.xyz - normal * dot(source.direction_strength.xyz, normal);
        applied += (radial + directed) * source.direction_strength.w * falloff;
    }
    var displacement = displacement_in[index].xyz;
    var velocity = velocity_in[index].xyz;
    let acceleration = applied - params.stiffness * displacement - params.damping * velocity;
    velocity += acceleration * params.dt;
    displacement += velocity * params.dt;
    velocity -= normal * dot(velocity, normal);
    displacement -= normal * dot(displacement, normal);
    let length_squared = dot(displacement, displacement);
    if (length_squared > params.maximum_displacement * params.maximum_displacement) {
        displacement = normalize(displacement) * params.maximum_displacement;
        velocity *= 0.5;
    }
    displacement_out[index] = vec4<f32>(displacement, 0.0);
    velocity_out[index] = vec4<f32>(velocity, 0.0);
}
