// Voxel terrain PREPASS — writes depth / normal / motion, PLUS the see-through
// discard that the forward pass (terrain_material.wgsl) can't do alone.
//
// ## Why this exists
// `ExtendedMaterial<StandardMaterial, _>` would otherwise delegate the prepass to
// Bevy's stock `pbr_prepass.wgsl`. That writes wall depth for player-built voxels, so
// even though the forward pass cuts those voxels away in front of the player, the
// player BEHIND the wall still fails the depth test (the prepass already claimed those
// pixels) and the gap shows the scene behind the wall instead of the player. This
// shader fixes that by removing the EXACT same voxels in the prepass, so no wall depth
// is written there and the player shows through.
//
// For every NON-discarded fragment it writes the standard prepass outputs (world
// normal, motion vector, clamped ortho depth), so SSAO / motion blur / water depth
// keep working exactly as with the stock prepass.
//
// ## Why it's a trimmed copy (not the full prepass.wgsl)
// The voxel mesh has no skinning, morph targets, or tangents, and the default prepass
// fragment uses neither UVs nor vertex colors — so we only carry position + normal +
// our per-voxel `tex_idx`. The `#ifdef`s below are kept verbatim from bevy_pbr's
// `prepass.wgsl` so the outputs match whatever prepass variant the pipeline compiles.

#import bevy_pbr::{
    prepass_bindings,
    mesh_functions,
    prepass_io::FragmentOutput,
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
}
// The see-through cutaway (binding 102 `xray` uniform + `xray_should_discard`) is the
// SHARED module the forward shader imports too, so both passes cut the identical voxels.
#import inf3d::terrain_xray::xray_should_discard

// Trimmed prepass vertex input: position (0) + normal (3, prepass convention) + our
// per-voxel material index (8). `VoxelTerrainExtension::specialize` builds the prepass
// vertex buffer layout to match exactly these shader locations.
struct PrepassVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(3) normal: vec3<f32>,
    @location(8) tex_idx: vec3<u32>,
}

struct PrepassVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(2) world_normal: vec3<f32>,
    @location(4) world_position: vec4<f32>,
#ifdef MOTION_VECTOR_PREPASS
    @location(5) previous_world_position: vec4<f32>,
#endif
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    @location(6) unclipped_depth: f32,
#endif
    // Per-voxel material (built blocks are uniform, so any component works). Flat —
    // it's an integer id, must never be interpolated.
    @location(10) @interpolate(flat) material: u32,
}

@vertex
fn vertex(vertex: PrepassVertex) -> PrepassVertexOutput {
    var out: PrepassVertexOutput;

    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local, vec4<f32>(vertex.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);

#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0); // clamp depth to avoid clipping
#endif

    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal, vertex.instance_index);

#ifdef MOTION_VECTOR_PREPASS
    let prev_model = mesh_functions::get_previous_world_from_local(vertex.instance_index);
    out.previous_world_position = mesh_functions::mesh_position_local_to_world(
        prev_model, vec4<f32>(vertex.position, 1.0));
#endif

    out.material = vertex.tex_idx.x;
    return out;
}

#ifdef PREPASS_FRAGMENT
@fragment
fn fragment(in: PrepassVertexOutput) -> FragmentOutput {
    // See-through cutaway: discard the SAME voxels the forward pass removes (shared
    // `xray_should_discard`), so no wall depth is written and the player behind shows
    // through. Deterministic per voxel → the two passes can't disagree.
    if xray_should_discard(in.world_position.xyz, in.world_normal, in.material) {
        discard;
    }

    var out: FragmentOutput;

#ifdef NORMAL_PREPASS
    out.normal = vec4(in.world_normal * 0.5 + vec3(0.5), 1.0);
#endif

#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.frag_depth = in.unclipped_depth;
#endif

#ifdef MOTION_VECTOR_PREPASS
    let clip_position_t = view.unjittered_clip_from_world * in.world_position;
    let clip_position = clip_position_t.xy / clip_position_t.w;
    let previous_clip_position_t =
        prepass_bindings::previous_view_uniforms.clip_from_world * in.previous_world_position;
    let previous_clip_position = previous_clip_position_t.xy / previous_clip_position_t.w;
    out.motion_vector = (clip_position - previous_clip_position) * vec2(0.5, -0.5);
#endif

    return out;
}
#endif // PREPASS_FRAGMENT
