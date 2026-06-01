// Standalone grass material shader: wind-sway + player-shove in the vertex
// stage, simple hard-coded-sun lambert in the fragment stage. We own group 2
// (params at binding 0), so there is no StandardMaterial bind-group collision.

#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    forward_io::{Vertex, VertexOutput},
    mesh_view_bindings::globals,
}

struct GrassParams {
    base_color: vec4<f32>,
    player: vec4<f32>,
    wind_strength: f32,
    bend_radius: f32,
    bend_strength: f32,
    _pad: f32,
}
// Material bind group is group 3 in Bevy 0.18 (group 2 is the mesh bindings).
// Use the shader-def so it tracks the engine's MATERIAL_BIND_GROUP index.
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> gp: GrassParams;

@vertex
fn vertex(in: Vertex) -> VertexOutput {
    var out: VertexOutput;

    let model = mesh_functions::get_world_from_local(in.instance_index);
    var world = mesh_functions::mesh_position_local_to_world(model, vec4<f32>(in.position, 1.0));

    // Blade tip (higher local Y) sways/bends most; the root stays planted.
    let w = clamp(in.position.y / 0.45, 0.0, 1.0);

    // Wind: drifting sine wave keyed off world position + time.
    let phase = globals.time * 1.5 + world.x * 0.4 + world.z * 0.4;
    let wind = vec3<f32>(sin(phase), 0.0, cos(phase * 0.7)) * gp.wind_strength * w;

    // Player shove: push the tip away from the player within bend_radius.
    var shove = vec3<f32>(0.0);
    let to = world.xz - gp.player.xz;
    let d = length(to);
    if (d < gp.bend_radius && d > 0.0001) {
        let push = (1.0 - d / gp.bend_radius) * gp.bend_strength * w;
        shove = vec3<f32>(to.x / d, 0.0, to.y / d) * push;
    }

    world = vec4<f32>(world.xyz + wind + shove, world.w);

    out.position = position_world_to_clip(world.xyz);
    out.world_position = world;
    out.world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
#ifdef VERTEX_UVS_A
    out.uv = in.uv;
#endif
#ifdef VERTEX_COLORS
    out.color = in.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = in.instance_index;
#endif
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let sun = normalize(vec3<f32>(-0.25, 1.0, 0.18));
    // Diffuse + ambient so blades read lit like the ground (normals point up).
    let lambert = max(dot(n, sun), 0.0) * 0.7 + 0.5;

    var tint = vec3<f32>(1.0);
#ifdef VERTEX_COLORS
    tint = in.color.rgb; // baked root→tip gradient
#endif

    return vec4<f32>(gp.base_color.rgb * tint * lambert, 1.0);
}
