//! Custom voxel terrain material that writes the depth + normal prepass.
//!
//! ## Why this exists
//!
//! `bevy_voxel_world 0.16`'s default material is an
//! `ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>` (see
//! `bevy_voxel_world::voxel_material`). Two upstream choices conspire to keep
//! the voxel terrain out of Bevy's depth + normal prepass textures, which in
//! turn makes every downstream prepass consumer (`DistanceFog`,
//! `VolumetricFog`, SSAO, motion blur, water SSR, depth-of-field) treat the
//! terrain as if it weren't there:
//!
//! 1. The shipped `voxel_texture.wgsl` declares a `#ifdef PREPASS_PIPELINE`
//!    branch that *imports* `prepass_io::{VertexOutput, FragmentOutput}` but
//!    keeps emitting a custom `CustomVertexOutput` from the vertex stage and
//!    passing it to `deferred_output(in, pbr_input)` — which expects
//!    `prepass_io::VertexOutput`. That branch fails to validate as a prepass
//!    pipeline, so the prepass shader for the voxel material is effectively
//!    broken.
//! 2. `impl Material for StandardVoxelMaterial` overrides `enable_prepass()`
//!    to `false`. When the standalone `Material` impl is used (instead of the
//!    `ExtendedMaterial` wrapper), the material's instances are never queued
//!    into the prepass phases regardless of what the shader says.
//!
//! ## What this module does
//!
//! Ships a parallel material — [`TerrainMaterial`], an
//! `ExtendedMaterial<StandardMaterial, VoxelTerrainExtension>` — that:
//!
//! - Uses our own WGSL ([`terrain_material.wgsl`](./terrain_material.wgsl))
//!   for the **forward pass only**. The fragment logic is byte-for-byte the
//!   same as upstream `voxel_texture.wgsl`: sample the texture array based on
//!   `tex_idx[face]`, multiply by per-vertex AO color, run through
//!   `apply_pbr_lighting` + `main_pass_post_lighting_processing`.
//! - Delegates the **prepass** to Bevy's stock `pbr_prepass.wgsl` via
//!   `ShaderRef::Default` — through `ExtendedMaterial::prepass_*_shader()`
//!   that delegation chains into `StandardMaterial::prepass_*_shader()`,
//!   which is itself `ShaderRef::Default`, resolving to the built-in prepass
//!   shaders. Those shaders only need the standard mesh attributes (POSITION
//!   at 0, UV_0 at 1, NORMAL at 3, COLOR at 7), all of which the
//!   `bevy_voxel_world` mesher already produces (see `meshing.rs`). The
//!   forward-pass `tex_idx` attribute at location 8 is harmlessly ignored by
//!   the prepass.
//! - Returns `true` from `MaterialExtension::enable_prepass()`. Because
//!   `Material for ExtendedMaterial<B, E>` forwards `enable_prepass()` to
//!   the extension (`E::enable_prepass()`), this is what actually makes our
//!   instances participate in the prepass render phase.
//!
//! ## Wiring (see [`crate::WorldPlugin`])
//!
//! Three things have to happen before `bevy_voxel_world` will use this
//! material for its meshes:
//!
//! 1. [`install_terrain_material`] is called during `WorldPlugin::build`,
//!    which:
//!    - Registers the WGSL bytes at a stable uuid handle via
//!      `load_internal_asset!`. That avoids any asset-server round-trip and
//!      gives us synchronous availability on the very first frame.
//!    - Adds `MaterialPlugin::<TerrainMaterial>::default()` so Bevy
//!      compiles the pipeline and runs `specialize_material_meshes` for our
//!      material. (`bevy_voxel_world` only registers a `MaterialPlugin` for
//!      its own default material; using `with_material(..)` on its side
//!      explicitly skips that registration.)
//!    - Procedurally builds a 4-layer `2d_array` `Image` for the texture
//!      array binding and returns the assembled material value.
//! 2. `VoxelWorldPlugin::with_config(main_world).with_material(material)`
//!    consumes the returned value. With `init_custom_materials() = true`
//!    (the default), the voxel plugin adds the value to `Assets<M>`, takes
//!    the resulting handle, and inserts it as
//!    `VoxelWorldMaterialHandle<TerrainMaterial>`.
//! 3. The voxel plugin's `Internals::<C>::assign_material::<TerrainMaterial>`
//!    system finds that resource and attaches the handle to every newly
//!    meshed chunk via `MeshMaterial3d`.
//!
//! ## Why a procedural texture
//!
//! `bevy_voxel_world` ships a default PNG (`shaders/default_texture.png`)
//! that it `include_bytes!`-bakes into its own crate; the bytes are not
//! re-exported and the asset path is private. Rather than hand-shipping a
//! duplicate PNG in our crate, we build a 32x32x4 RGBA8 image at startup
//! with flat tints that match the prior look (earthy palette modulated by
//! the per-vertex AO color the mesher writes into `Mesh::ATTRIBUTE_COLOR`).
//! This also dodges any asset-server load races that would otherwise need
//! `init_custom_materials() = false` plus a manual
//! `VoxelWorldMaterialHandle` insertion after the texture finishes loading.

use bevy::asset::{load_internal_asset, uuid_handle};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{MeshVertexBufferLayoutRef, VertexAttributeDescriptor};
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
    TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use bevy::shader::{ShaderDefVal, ShaderRef};
use bevy_voxel_world::rendering::{vertex_layout, ATTRIBUTE_TEX_INDEX};

/// Stable, type-checked handle to the terrain shader. `uuid_handle!` produces
/// a `Handle::Uuid` whose AssetId is determined entirely by the literal — the
/// same uuid in `ShaderRef::Handle(_)` resolves to the same `Assets<Shader>`
/// slot that `load_internal_asset!` writes to.
const TERRAIN_MATERIAL_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("0c4dfc1c-7b39-4f0a-93a3-2b8f6d18a16e");

/// Stable handle to the custom terrain PREPASS shader
/// ([`terrain_prepass.wgsl`](./terrain_prepass.wgsl)). Replaces the stock
/// StandardMaterial prepass so the depth/normal/motion prepass can DISCARD the same
/// player-built fragments the forward pass dithers away — the half that actually makes
/// the see-through cutout reveal the player (see that file's header).
const TERRAIN_PREPASS_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("9f3b2a1c-7d4e-4c8a-b6f1-3e2d1c0a9b8e");

/// Stable handle to the shared see-through module
/// ([`terrain_xray.wgsl`](./terrain_xray.wgsl), `#define_import_path inf3d::terrain_xray`).
/// Both the forward shader and the prepass `#import` its `xray_should_discard` so they
/// cut the identical set of voxels for the cutaway.
const TERRAIN_XRAY_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("5c8e4a2d-9b1f-4e3a-a7d2-6f0b1c9e8d34");

/// Public type alias for the full material
/// (`ExtendedMaterial<StandardMaterial, VoxelTerrainExtension>`). Downstream
/// code shouldn't usually need to name this — the plugin attaches it
/// automatically — but the type appears in
/// `MaterialPlugin::<TerrainMaterial>::default()` and the
/// `VoxelWorldMaterialHandle<TerrainMaterial>` resource, so we expose it.
pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, VoxelTerrainExtension>;

/// `MaterialExtension` that swaps in our texture-array-sampling forward
/// shader while leaving the prepass to `StandardMaterial`.
///
/// The `#[texture(100, dimension = "2d_array")]` / `#[sampler(101)]` binding
/// indices match upstream `StandardVoxelMaterial` so the WGSL signature is
/// byte-for-byte compatible — meaning the voxel mesher's existing per-vertex
/// `tex_idx` payload still works without touching `meshing.rs`.
#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
pub struct VoxelTerrainExtension {
    #[texture(100, dimension = "2d_array")]
    #[sampler(101)]
    pub voxels_texture: Handle<Image>,
    /// Per-frame see-through ("x-ray") parameters at binding 102. Updated by
    /// `inf3d_render`'s x-ray system; consumed by the shader's built-block cutout.
    #[uniform(102)]
    pub xray: XrayParams,
}

impl Default for VoxelTerrainExtension {
    fn default() -> Self {
        Self {
            voxels_texture: Handle::default(),
            xray: XrayParams::default(),
        }
    }
}

/// Parameters the terrain shaders use to cut away player-built voxels (material index
/// `>= `[`crate::BUILT_MATERIAL_BASE`]) that sit between the camera and the player.
/// The cutaway is computed in WORLD space and per-voxel — see
/// [`terrain_xray.wgsl`](crate::terrain_material). Three `vec4`s keep the std140
/// uniform layout trivially correct. Fed each frame by `inf3d_render::xray`.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct XrayParams {
    /// `xyz` = player body center (world); `w` = enabled (`> 0.5` turns the cutaway on).
    pub player: Vec4,
    /// `xyz` = camera forward (unit, into the scene); `w` = cut radius (world units).
    pub view: Vec4,
    /// `x` = player half-height (world units); `yzw` reserved.
    pub extra: Vec4,
}

impl Default for XrayParams {
    fn default() -> Self {
        Self {
            player: Vec4::ZERO,
            view: Vec4::ZERO,
            extra: Vec4::ZERO,
        }
    }
}

impl MaterialExtension for VoxelTerrainExtension {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(TERRAIN_MATERIAL_SHADER_HANDLE)
    }

    fn vertex_shader() -> ShaderRef {
        ShaderRef::Handle(TERRAIN_MATERIAL_SHADER_HANDLE)
    }

    /// Explicit. The trait default is also `true`, but stating it here makes
    /// the contract — "this material participates in the depth + normal
    /// prepass" — discoverable from the type alone. Combined with
    /// `Material for ExtendedMaterial<B, E>` delegating `enable_prepass()` to
    /// `E::enable_prepass()`, this is what makes downstream prepass consumers
    /// (volumetric fog, SSAO, etc.) see the terrain.
    fn enable_prepass() -> bool {
        true
    }

    /// Custom prepass (see [`terrain_prepass.wgsl`](./terrain_prepass.wgsl)) instead
    /// of the stock `ShaderRef::Default` chain (which would resolve to
    /// `pbr_prepass.wgsl`). It writes the same depth/normal/motion outputs but ALSO
    /// discards player-built fragments near the player, so the prepass doesn't claim
    /// those pixels' depth and the see-through cutout actually reveals the player.
    /// `specialize` adds our `tex_idx` attribute to the prepass vertex layout so this
    /// shader can read the per-voxel material.
    fn prepass_vertex_shader() -> ShaderRef {
        ShaderRef::Handle(TERRAIN_PREPASS_SHADER_HANDLE)
    }

    /// Same custom prepass shader as [`prepass_vertex_shader`](Self::prepass_vertex_shader)
    /// — it defines both the `vertex` and (under `PREPASS_FRAGMENT`) `fragment` entry
    /// points.
    fn prepass_fragment_shader() -> ShaderRef {
        ShaderRef::Handle(TERRAIN_PREPASS_SHADER_HANDLE)
    }

    /// Install the vertex buffer layout with our per-voxel `tex_idx` attribute on
    /// BOTH pipelines — the forward shader samples the texture array with it, and the
    /// custom prepass uses it to identify (and discard) player builds.
    ///
    /// `PrepassPipelineSpecializer::specialize` DOES reach this hook in Bevy 0.18
    /// (verified against `bevy_pbr::prepass`), contrary to an earlier assumption. The
    /// two pipelines need DIFFERENT attribute sets at different shader locations: the
    /// forward pass uses the full voxel layout (`vertex_layout()`), while the prepass
    /// only needs position + normal + `tex_idx` at the prepass-convention locations
    /// (matching `terrain_prepass.wgsl`'s `PrepassVertex`). `get_layout` reads the
    /// mesh's real interleaved offsets/stride, so requesting a subset is correct.
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let is_prepass = descriptor
            .vertex
            .shader_defs
            .contains(&ShaderDefVal::Bool("PREPASS_PIPELINE".into(), true));

        let attrs: Vec<VertexAttributeDescriptor> = if is_prepass {
            vec![
                Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
                Mesh::ATTRIBUTE_NORMAL.at_shader_location(3),
                ATTRIBUTE_TEX_INDEX.at_shader_location(8),
            ]
        } else {
            vertex_layout()
        };
        let vertex_buffer_layout = layout.0.get_layout(&attrs)?;
        descriptor.vertex.buffers = vec![vertex_buffer_layout];
        Ok(())
    }
}

/// Build a 4-layer 32x32 RGBA8 `Image` suitable for use as a
/// `texture_2d_array` in the terrain shader.
///
/// The image is constructed as a vertically-stacked 2D
/// `(LAYER_SIZE x LAYER_SIZE * LAYERS)` image, then
/// `reinterpret_stacked_2d_as_array` slices it back into LAYERS layers —
/// mirroring what `bevy_voxel_world::voxel_material::prepare_voxel_texture`
/// does internally (which we can't call because it is `pub(crate)`).
///
/// Layer order is the discriminant order of
/// [`crate::TerrainMaterialId`] — the single source of truth that
/// [`crate::MainWorld::texture_index_mapper`] also indexes into:
///  - 0 → `Grass`    (top face of land voxels)
///  - 1 → `Dirt`     (side faces of land voxels)
///  - 2 → `Stone`    (bottom faces of land voxels)
///  - 3 → `Seafloor` (all faces of submerged columns)
///
/// Colors are flat tints rather than detail textures; the per-vertex AO
/// color the mesher writes into `Mesh::ATTRIBUTE_COLOR` modulates them to
/// give the characteristic block shading.
fn build_terrain_texture(images: &mut Assets<Image>) -> Handle<Image> {
    /// Side length of each layer in pixels. Square and a power of two so
    /// the stacked-as-array reinterpret has integer math.
    const LAYER_SIZE: u32 = 32;
    /// Number of layers = one row per [`crate::PALETTE`] (= per `TerrainMaterialId`
    /// variant). Colors come from that single table too, so this builder can't
    /// desync from the material indices / labels / per-face mapper — add a block by
    /// adding ONE palette row (the `palette_matches_enum` test guards consistency).
    const LAYERS: u32 = crate::PALETTE.len() as u32;

    let pixels_per_layer = (LAYER_SIZE * LAYER_SIZE) as usize;
    let bytes_per_layer = pixels_per_layer * 4;
    let mut data = Vec::with_capacity(bytes_per_layer * LAYERS as usize);

    // Procedural per-texel detail so voxel faces read as TEXTURED surfaces instead
    // of flat color fills. `texel_brightness` mixes a coarse blotch + fine grain
    // into a brightness multiplier around each layer's base color (deterministic —
    // byte-identical every run). The linear sampler below smooths it into soft
    // shading variation rather than hard pixels.
    for layer in 0..LAYERS as usize {
        let [r, g, b] = crate::PALETTE[layer].color;
        for py in 0..LAYER_SIZE {
            for px in 0..LAYER_SIZE {
                let f = texel_brightness(px, py, layer as u32);
                data.push((r as f32 * f).clamp(0.0, 255.0) as u8);
                data.push((g as f32 * f).clamp(0.0, 255.0) as u8);
                data.push((b as f32 * f).clamp(0.0, 255.0) as u8);
                data.push(0xff);
            }
        }
    }

    let sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        // Linear filtering so the per-texel detail reads as smooth surface shading,
        // not aliased pixels. (Mipmaps would further cut far-distance shimmer, but
        // Bevy 0.18 has no `Image::generate_mipmaps`; the detail amplitude is kept
        // low so this is acceptable — see BACKLOG for a mipmap follow-up.)
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });

    let mut image = Image::new(
        Extent3d {
            width: LAYER_SIZE,
            height: LAYER_SIZE * LAYERS,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::default(),
    );
    image.sampler = sampler;

    // Re-interpret the stacked 2D bytes as a `2d_array` of LAYERS slices.
    // If it ever fails the terrain will render with only the first layer
    // (which the shader would still sample with index 0 from `tex_idx`)
    // — that's an ugly fallback but a working frame, so we warn rather
    // than crash.
    if let Err(err) = image.reinterpret_stacked_2d_as_array(LAYERS) {
        warn!(
            "inf3d_world: failed to reinterpret terrain texture as 2d_array \
             (terrain will render with the first layer only): {err}"
        );
    } else {
        image.texture_view_descriptor = Some(TextureViewDescriptor {
            dimension: Some(TextureViewDimension::D2Array),
            ..default()
        });
    }

    images.add(image)
}

/// Deterministic per-texel brightness multiplier (~0.88..1.12) for the procedural
/// terrain layers, so each voxel face reads as a textured surface instead of a flat
/// color. Combines a coarse blotch (low frequency → broad soft patches) with fine
/// per-texel grain. Pure integer hash, so the texture is byte-identical every run.
fn texel_brightness(px: u32, py: u32, layer: u32) -> f32 {
    fn hash01(x: u32, y: u32, l: u32) -> f32 {
        let mut h =
            x.wrapping_mul(0x27d4_eb2d) ^ y.wrapping_mul(0x1656_67b1) ^ l.wrapping_mul(0x9e37_79b1);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 12;
        (h & 0xffff) as f32 / 65535.0
    }
    let coarse = hash01(px / 8, py / 8, layer); // broad soft patches
    let fine = hash01(px, py, layer.wrapping_add(101)); // fine grain
    let n = coarse * 0.7 + fine * 0.3; // 0..1, coarse-dominated
    0.88 + n * 0.24 // 0.88..1.12 — subtle, surface texture not visual noise
}

/// Register the terrain shader and `MaterialPlugin`, build the procedural
/// texture array, and return the fully-populated [`TerrainMaterial`] value.
///
/// The returned value is meant to be threaded into
/// `VoxelWorldPlugin::with_material(...)` — the voxel plugin will clone it
/// into `Assets<TerrainMaterial>` itself and own the resulting handle via
/// `VoxelWorldMaterialHandle<TerrainMaterial>`.
pub fn install_terrain_material(app: &mut App) -> TerrainMaterial {
    // Register the WGSL bytes synchronously at a uuid handle. `include_str!`
    // (inside the macro) is path-relative to this `.rs` file, so the shader
    // lives at `crates/inf3d_world/src/terrain_material.wgsl`.
    //
    // This is the same pattern `bevy_voxel_world` uses for its own
    // `VOXEL_TEXTURE_SHADER_HANDLE`, so we know it's the supported path for
    // shipping a shader from inside a library crate without an `assets/`
    // directory.
    load_internal_asset!(
        app,
        TERRAIN_MATERIAL_SHADER_HANDLE,
        "terrain_material.wgsl",
        Shader::from_wgsl
    );

    // The matching custom PREPASS shader (depth/normal/motion + the see-through
    // discard). Registered the same way as the forward shader so it's available
    // synchronously on the first frame.
    load_internal_asset!(
        app,
        TERRAIN_PREPASS_SHADER_HANDLE,
        "terrain_prepass.wgsl",
        Shader::from_wgsl
    );

    // Shared see-through module (`#define_import_path inf3d::terrain_xray`) that both
    // shaders above `#import`. Must be registered too so naga_oil can resolve the
    // import when their pipelines compile.
    load_internal_asset!(
        app,
        TERRAIN_XRAY_SHADER_HANDLE,
        "terrain_xray.wgsl",
        Shader::from_wgsl
    );

    // Without this, Bevy never compiles a pipeline for `TerrainMaterial` and
    // chunks would render either blank or with the wrong material. The voxel
    // plugin skips this step on the custom-material path (it only adds a
    // `MaterialPlugin` for its own default `StandardVoxelMaterial`).
    app.add_plugins(MaterialPlugin::<TerrainMaterial>::default());

    let mut images = app.world_mut().resource_mut::<Assets<Image>>();
    let texture_handle = build_terrain_texture(&mut images);
    drop(images);

    ExtendedMaterial {
        // Same low-reflectance / high-roughness base as the upstream default
        // plugin uses for `ExtendedMaterial<StandardMaterial,
        // StandardVoxelMaterial>`. Keeps the lit appearance — under the
        // existing directional sun + ambient — identical.
        base: StandardMaterial {
            reflectance: 0.05,
            metallic: 0.05,
            perceptual_roughness: 0.95,
            ..default()
        },
        extension: VoxelTerrainExtension {
            voxels_texture: texture_handle,
            xray: XrayParams::default(),
        },
    }
}
