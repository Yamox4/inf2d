// Per-chunk cliff face material.
//
// The mesh built by `build_chunk_cliff_mesh` concatenates every cliff
// parallelogram for a single chunk into one vertex/index buffer. Each vertex
// carries its biome × side tint baked into the `COLOR` attribute (premultiplied
// by per-side AO brightness on the CPU), so the fragment shader is a simple
// passthrough: emit the interpolated vertex color, optionally multiplied by
// the material's uniform tint.
//
// Bevy's mesh2d pipeline only exposes `VertexOutput.color` when the
// `VERTEX_COLORS` shader-def is set, which happens automatically because our
// mesh has `Mesh::ATTRIBUTE_COLOR`. See `bevy_sprite_render-0.18.1`
// `mesh2d/mesh.rs::specialize` for the pipeline branch that injects the
// define.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct ChunkCliffUniforms {
    tint: vec4<f32>,
};

@group(2) @binding(0) var<uniform> material: ChunkCliffUniforms;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
#ifdef VERTEX_COLORS
    return mesh.color * material.tint;
#else
    // Fallback for the (impossible in practice) case where the pipeline forgot
    // to emit `VERTEX_COLORS` — render flat tint rather than crash.
    return material.tint;
#endif
}
