// Lit tilemap vertex shader.
//
// Mirrors bevy_ecs_tilemap's stock `tilemap_vertex.wgsl` byte-for-byte except
// it emits a `LitMeshVertexOutput` (vs the stock `MeshVertexOutput`) so the
// fragment stage can read `world_position` for per-pixel light math.
//
// The stock vertex shader computes `mesh_data.world_position` for the
// clip-space transform and then throws it away; here we keep it.

#import bevy_ecs_tilemap::common::{VertexInput, tilemap_data, mesh}
#import bevy_ecs_tilemap::mesh_output::MeshOutput
#import bevy_sprite::mesh2d_view_bindings::{view, globals}

#ifdef SQUARE
    #import bevy_ecs_tilemap::square::get_mesh
#endif

#ifdef ISO_DIAMOND
    #import bevy_ecs_tilemap::diamond_iso::get_mesh
#endif

#ifdef ISO_STAGGERED
    #import bevy_ecs_tilemap::staggered_iso::get_mesh
#endif

#ifdef COLUMN_EVEN_HEX
    #import bevy_ecs_tilemap::column_even_hex::get_mesh
#endif

#ifdef COLUMN_HEX
    #import bevy_ecs_tilemap::column_hex::get_mesh
#endif

#ifdef COLUMN_ODD_HEX
    #import bevy_ecs_tilemap::column_odd_hex::get_mesh
#endif

#ifdef ROW_EVEN_HEX
    #import bevy_ecs_tilemap::row_even_hex::get_mesh
#endif

#ifdef ROW_HEX
    #import bevy_ecs_tilemap::row_hex::get_mesh
#endif

#ifdef ROW_ODD_HEX
    #import bevy_ecs_tilemap::row_odd_hex::get_mesh
#endif

// Mirrors `LitMeshVertexOutput` in lit_tile.wgsl. Keep field locations in sync.
struct LitMeshVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) tile_id: i32,
    @location(3) storage_position: vec2<u32>,
    @location(4) world_position: vec4<f32>,
};

@vertex
fn vertex(vertex_input: VertexInput) -> LitMeshVertexOutput {
    var out: LitMeshVertexOutput;
    let animation_speed = vertex_input.position.z;

    let mesh_data: MeshOutput = get_mesh(vertex_input.v_index, vec3(vertex_input.position.xy, 0.0));

    let frames: f32 = f32(vertex_input.uv.w - vertex_input.uv.z);

    var current_animation_frame = fract(globals.time * animation_speed) * frames;

    current_animation_frame = clamp(f32(vertex_input.uv.z) + current_animation_frame, f32(vertex_input.uv.z), f32(vertex_input.uv.w));

    let texture_index: u32 = u32(current_animation_frame);

    #ifdef ATLAS
    let columns: u32 = u32(round((tilemap_data.texture_size.x - tilemap_data.spacing.x) / (tilemap_data.tile_size.x + tilemap_data.spacing.x)));
    let sprite_sheet_x: f32 = tilemap_data.spacing.x + floor(f32(texture_index % columns)) * (tilemap_data.tile_size.x + tilemap_data.spacing.x);
    let sprite_sheet_y: f32 = tilemap_data.spacing.y + floor(f32(texture_index / columns)) * (tilemap_data.tile_size.y + tilemap_data.spacing.y);

    let start_u: f32 = sprite_sheet_x / tilemap_data.texture_size.x;
    let end_u: f32 = (sprite_sheet_x + tilemap_data.tile_size.x) / tilemap_data.texture_size.x;
    let start_v: f32 = sprite_sheet_y / tilemap_data.texture_size.y;
    let end_v: f32 = (sprite_sheet_y + tilemap_data.tile_size.y) / tilemap_data.texture_size.y;
    #else
    let start_u: f32 = 0.0;
    let end_u: f32 = 1.0;
    let start_v: f32 = 0.0;
    let end_v: f32 = 1.0;
    #endif

    var atlas_uvs: array<vec4<f32>, 4>;

    var x1: array<vec4<f32>, 8> = array<vec4<f32>, 8>(
        vec4<f32>(start_u, end_v, 0.0, 1.0),
        vec4<f32>(end_u, end_v, 1.0, 1.0),
        vec4<f32>(start_u, start_v, 0.0, 0.0),
        vec4<f32>(end_u, start_v, 1.0, 0.0),
        vec4<f32>(end_u, start_v, 1.0, 0.0),
        vec4<f32>(end_u, end_v, 1.0, 1.0),
        vec4<f32>(start_u, start_v, 0.0, 0.0),
        vec4<f32>(start_u, end_v, 0.0, 1.0)
    );

    var x2: array<vec4<f32>, 8> = array<vec4<f32>, 8>(
        vec4<f32>(start_u, start_v, 0.0, 0.0),
        vec4<f32>(end_u, start_v, 1.0, 0.0),
        vec4<f32>(start_u, end_v, 0.0, 1.0),
        vec4<f32>(end_u, end_v, 1.0, 1.0),
        vec4<f32>(start_u, start_v, 0.0, 0.0),
        vec4<f32>(start_u, end_v, 0.0, 1.0),
        vec4<f32>(end_u, start_v, 1.0, 0.0),
        vec4<f32>(end_u, end_v, 1.0, 1.0)
    );

    var x3: array<vec4<f32>, 8> = array<vec4<f32>, 8>(
        vec4<f32>(end_u, start_v, 1.0, 0.0),
        vec4<f32>(start_u, start_v, 0.0, 0.0),
        vec4<f32>(end_u, end_v, 1.0, 1.0),
        vec4<f32>(start_u, end_v, 0.0, 1.0),
        vec4<f32>(start_u, end_v, 0.0, 1.0),
        vec4<f32>(start_u, start_v, 0.0, 0.0),
        vec4<f32>(end_u, end_v, 1.0, 1.0),
        vec4<f32>(end_u, start_v, 1.0, 0.0)
    );

    var x4: array<vec4<f32>, 8> = array<vec4<f32>, 8>(
        vec4<f32>(end_u, end_v, 1.0, 1.0),
        vec4<f32>(start_u, end_v, 0.0, 1.0),
        vec4<f32>(end_u, start_v, 1.0, 0.0),
        vec4<f32>(start_u, start_v, 0.0, 0.0),
        vec4<f32>(end_u, end_v, 1.0, 1.0),
        vec4<f32>(end_u, start_v, 1.0, 0.0),
        vec4<f32>(start_u, end_v, 0.0, 1.0),
        vec4<f32>(start_u, start_v, 0.0, 0.0),
    );

    atlas_uvs = array<vec4<f32>, 4>(
        x1[u32(vertex_input.uv.y)],
        x2[u32(vertex_input.uv.y)],
        x3[u32(vertex_input.uv.y)],
        x4[u32(vertex_input.uv.y)]
    );

    out.uv = atlas_uvs[vertex_input.v_index % 4u];
    out.tile_id = i32(texture_index);
    out.position = view.clip_from_world * mesh_data.world_position;
    out.color = vertex_input.color;
    out.storage_position = vec2<u32>(vertex_input.position.xy);
    out.world_position = mesh_data.world_position;
    return out;
}
