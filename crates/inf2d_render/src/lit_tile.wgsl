// Lit tilemap fragment shader.
//
// Companion to `LitTilemapMaterial`. The accompanying vertex shader
// (`lit_tile_vertex.wgsl`) builds the same mesh as bevy_ecs_tilemap's stock
// vertex stage but additionally forwards the per-vertex `world_position` so
// this fragment stage can index lights by their world-space position.
//
// The pipeline layout is dictated by bevy_ecs_tilemap's `MaterialTilemapPlugin`:
//
// ```text
//   @group(0) = view (view_uniforms, globals)
//   @group(1) = mesh + tilemap_data (mesh.model, tilemap_data)
//   @group(2) = diffuse atlas (texture_2d_array<f32>, sampler)
//   @group(3) = THIS material (normal atlas, lighting uniform)
// ```
//
// All references to lighting math use a +Z-up convention.

// `LitMeshVertexOutput` mirrors `bevy_ecs_tilemap::vertex_output::MeshVertexOutput`
// but tacks on the per-fragment world position so we can compute distances to
// point lights in world space. Order MUST match `lit_tile_vertex.wgsl`.
struct LitMeshVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) tile_id: i32,
    @location(3) storage_position: vec2<u32>,
    @location(4) world_position: vec4<f32>,
};

// Diffuse atlas bound by bevy_ecs_tilemap itself. With the `atlas` feature OFF
// (the default for this workspace), the engine slices the source image into a
// 2D array texture indexed by `tile_id`.
@group(2) @binding(0) var diffuse_atlas: texture_2d_array<f32>;
@group(2) @binding(1) var diffuse_sampler: sampler;

// Normal atlas. Provided by `LitTilemapMaterial` and built so the same
// `tile_id` selects the matching slice of `diffuse_atlas`.
@group(3) @binding(0) var normal_atlas: texture_2d_array<f32>;
@group(3) @binding(1) var normal_sampler: sampler;

// Cap mirrored from `MAX_TILE_LIGHTS` in lit_tile_material.rs. WGSL requires a
// compile-time array length, so the count is duplicated; tests assert that the
// Rust constant stays in sync.
const LIT_TILE_MAX_LIGHTS: u32 = 16u;

struct PackedLight {
    // xy = world position, z = unused, w = radius
    pos: vec4<f32>,
    // rgb = color, a = intensity multiplier
    color: vec4<f32>,
};

struct LightingUniforms {
    // xyz = unit sun direction (+z is up). w padding.
    sun_dir: vec4<f32>,
    // rgb = sun color, a = intensity multiplier
    sun_color: vec4<f32>,
    // rgb = ambient color, a = ambient strength
    ambient: vec4<f32>,
    // Number of valid entries in `lights`. WGSL inserts implicit padding so
    // `lights` aligns to a 16-byte boundary; encase on the Rust side does the
    // same dance automatically.
    num_lights: u32,
    lights: array<PackedLight, LIT_TILE_MAX_LIGHTS>,
};

@group(3) @binding(2) var<uniform> lighting: LightingUniforms;

@fragment
fn fragment(in: LitMeshVertexOutput) -> @location(0) vec4<f32> {
    // Sample the diffuse atlas exactly the way bevy_ecs_tilemap's default
    // shader does: array texture indexed by `tile_id`, UV = `in.uv.xy`.
    let albedo = textureSample(diffuse_atlas, diffuse_sampler, in.uv.xy, in.tile_id) * in.color;
    if (albedo.a < 0.01) {
        discard;
    }

    let normal_raw = textureSample(normal_atlas, normal_sampler, in.uv.xy, in.tile_id).rgb;
    // Decode [0, 1]^3 → [-1, 1]^3 and renormalize defensively (linear interpolation
    // between texels denormalizes the vector slightly).
    let n = normalize(normal_raw * 2.0 - vec3<f32>(1.0, 1.0, 1.0));

    // Directional sun + flat ambient.
    let sun_dir = normalize(lighting.sun_dir.xyz);
    let sun_dot = max(0.0, dot(n, sun_dir));
    var lit = lighting.ambient.rgb * lighting.ambient.a
            + lighting.sun_color.rgb * lighting.sun_color.a * sun_dot;

    // Point lights. We approximate each light as straight overhead at the lit
    // point: brightness scales with `n.z * falloff(distance)`. This is the
    // textbook cheap "2D point light on a normal-mapped plane" trick — it
    // looks like a spotlight from above without paying per-light per-pixel
    // 3D math.
    let world_pos = in.world_position.xy;
    let n_count = min(lighting.num_lights, LIT_TILE_MAX_LIGHTS);
    for (var i: u32 = 0u; i < n_count; i = i + 1u) {
        let light = lighting.lights[i];
        let dist = length(light.pos.xy - world_pos);
        let radius = light.pos.w;
        if (radius <= 0.0 || dist >= radius) {
            continue;
        }
        let r = dist / radius;
        // Squared linear falloff: smooth dim-to-zero at the rim, bright core.
        let falloff = (1.0 - r) * (1.0 - r);
        let contrib = light.color.rgb * light.color.a * falloff * max(0.0, n.z);
        lit = lit + contrib;
    }

    return vec4<f32>(albedo.rgb * lit, albedo.a);
}
