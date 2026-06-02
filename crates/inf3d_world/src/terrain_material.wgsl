// Voxel terrain material — forward path only.
//
// This shader is used ONLY for the forward (main) pass. The prepass uses
// Bevy's stock `pbr_prepass.wgsl` (via `ExtendedMaterial<StandardMaterial, _>`'s
// default delegation), which means depth + normal prepass output, alpha
// discard, and motion vectors all work the same as any StandardMaterial.
// That in turn means `DistanceFog`, `VolumetricFog`, SSAO, motion blur, water
// SSR, and anything else that samples the depth/normal prepass textures will
// see the voxel terrain. The previous `voxel_texture.wgsl` (which this
// supersedes) declared a `#ifdef PREPASS_PIPELINE` branch in its vertex stage
// emitting a custom struct that did not match `prepass_io::VertexOutput`; we
// avoid that bug by not overriding the prepass shaders at all.
//
// The forward fragment logic is identical to upstream
// `bevy_voxel_world::shaders::voxel_texture.wgsl`: pick a texture face from
// the world-space normal's Y, sample the texture array, multiply by the
// per-vertex AO color, and run it through full PBR lighting +
// post-processing so atmospheric fog, tonemapping, etc. all apply.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    mesh_functions,
    forward_io::{VertexOutput, FragmentOutput},
    view_transformations::position_world_to_clip,
}
#import bevy_pbr::pbr_bindings
#import bevy_render::instance_index::get_instance_index

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var mat_array_texture: texture_2d_array<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var mat_array_texture_sampler: sampler;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
#ifdef VERTEX_POSITIONS
    @location(0) position: vec3<f32>,
#endif
#ifdef VERTEX_NORMALS
    @location(1) normal: vec3<f32>,
#endif
#ifdef VERTEX_UVS
    @location(2) uv: vec2<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(3) uv_b: vec2<f32>,
#endif
#ifdef VERTEX_TANGENTS
    @location(4) tangent: vec4<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef MORPH_TARGETS
    @builtin(vertex_index) index: u32,
#endif

    @location(8) tex_idx: vec3<u32>
};

struct CustomVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
#ifdef VERTEX_UVS
    @location(2) uv: vec2<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(3) uv_b: vec2<f32>,
#endif
#ifdef VERTEX_TANGENTS
    @location(4) world_tangent: vec4<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    @location(6) @interpolate(flat) instance_index: u32,
#endif

    @location(8) tex_idx: vec3<u32>,
}

@vertex
fn vertex(vertex: Vertex) -> CustomVertexOutput {
    var out: CustomVertexOutput;
    var model = mesh_functions::get_world_from_local(vertex.instance_index);

    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal, vertex.instance_index);

    out.world_position = mesh_functions::mesh_position_local_to_world(
        model, vec4<f32>(vertex.position, 1.0));

    out.position = position_world_to_clip(out.world_position.xyz);

#ifdef VERTEX_UVS
    out.uv = vertex.uv;
#endif

#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        model,
        vertex.tangent,
        vertex.instance_index
    );
#endif

#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif

    out.tex_idx = vertex.tex_idx;

    return out;
}

@fragment
fn fragment(
    in: CustomVertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var standard_in: VertexOutput;
    standard_in.position = in.position;
    standard_in.world_normal = in.world_normal;
    standard_in.world_position = in.world_position;
#ifdef VERTEX_UVS
    standard_in.uv = in.uv;
#endif
#ifdef VERTEX_COLORS
    standard_in.color = in.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    standard_in.instance_index = in.instance_index;
#endif
    var pbr_input = pbr_input_from_standard_material(standard_in, is_front);

    var tex_face = 0;

    // determine texture index based on normal
    if in.world_normal.y == 0.0 {
        tex_face = 1;
    } else if in.world_normal.y < 0.0 {
        tex_face = 2;
    }

#ifdef VERTEX_UVS
    pbr_input.material.base_color = textureSample(mat_array_texture, mat_array_texture_sampler, in.uv, in.tex_idx[tex_face]);
#endif
#ifdef VERTEX_COLORS
    pbr_input.material.base_color = pbr_input.material.base_color * in.color;
#endif

    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    return out;
}
