#![deny(unsafe_code)]
//! Normal-mapped tile lighting — the visual capstone for the isometric renderer.
//!
//! Each chunk tilemap shares a single [`LitTilemapMaterial`] instance, stored on the
//! [`LitTileMaterialHandle`] resource. The material binds the procedural normal atlas
//! and a frame-updated [`LightingUniforms`] block; the WGSL fragment shader
//! ([`lit_tile.wgsl`](../../lit_tile.wgsl)) lights every tile against the sun direction
//! derived from [`TimeOfDay`] plus the nearest [`PointLight2D`] entities.
//!
//! ## Sharing one material across all chunks
//!
//! Sixty-four loaded chunks * a thousand tiles each easily fits in a single bind group:
//! we don't need per-chunk uniforms because lighting is computed in world space and is
//! the same for every tile on the map. Sharing a single material handle means the
//! per-frame update is one `Assets::get_mut` followed by an in-place write of a small
//! struct — `O(num_lights)` work regardless of how many chunks are loaded.
//!
//! ## Sun arc
//!
//! `TimeOfDay::hours ∈ [0, 24)` maps to a sun direction that rises at 6:00 in the
//! east, peaks overhead near noon, and sets at 18:00 in the west. Outside daylight
//! hours the sun is below the horizon and the lighting is driven mostly by ambient
//! moonlight plus point lights.
//!
//! ## Light packing
//!
//! Up to [`MAX_TILE_LIGHTS`] point lights ship to the GPU each frame. When more
//! entities are present, the closest-to-camera N are selected so torches near the
//! viewport always make it in.

use bevy::asset::{embedded_asset, Asset};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use bevy_ecs_tilemap::prelude::{MaterialTilemap, MaterialTilemapPlugin};

use crate::atlas::TileAtlas;
use crate::daynight::TimeOfDay;
use crate::lights::PointLight2D;

/// Maximum number of point lights the lit tile shader can consider per frame.
///
/// Larger values are cheap RAM-wise but every light costs a per-tile per-pixel loop
/// iteration in WGSL; 16 is plenty for torchlit dungeons without melting the GPU.
/// This constant is mirrored as `LIT_TILE_MAX_LIGHTS` in `lit_tile.wgsl`.
pub const MAX_TILE_LIGHTS: usize = 16;

/// Embedded asset paths for the WGSL shaders. Resolved by the asset server during
/// pipeline specialization on the render thread. The strings here must match the
/// crate-relative paths registered via `embedded_asset!` in [`LightingPlugin::build`].
const LIT_TILE_FRAGMENT_PATH: &str = "embedded://inf2d_render/lit_tile.wgsl";
const LIT_TILE_VERTEX_PATH: &str = "embedded://inf2d_render/lit_tile_vertex.wgsl";

/// A single point light, GPU-packed.
#[derive(ShaderType, Clone, Copy, Debug, Default)]
pub struct PackedLight {
    /// `xy` = world position; `z` = unused; `w` = radius (world units).
    pub pos: Vec4,
    /// `rgb` = linear color; `a` = intensity multiplier.
    pub color: Vec4,
}

/// All per-frame lighting state shipped to the lit tilemap material.
///
/// `encase` (the std-layout encoder underneath `ShaderType`) automatically inserts
/// the alignment padding required between scalars and the trailing array, so the
/// Rust struct and the WGSL `struct LightingUniforms` stay in lockstep without
/// hand-written `_pad` fields. Field order must remain stable; reordering will
/// silently misalign the GPU view.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct LightingUniforms {
    /// `xyz` = sun direction in world space (+Z up); `w` = pad.
    pub sun_dir: Vec4,
    /// `rgb` = sun color; `a` = intensity multiplier.
    pub sun_color: Vec4,
    /// `rgb` = ambient color; `a` = ambient strength.
    pub ambient: Vec4,
    /// Number of valid entries in [`lights`](Self::lights).
    pub num_lights: u32,
    /// Packed light buffer; only the first `num_lights` entries are read by the shader.
    pub lights: [PackedLight; MAX_TILE_LIGHTS],
}

impl Default for LightingUniforms {
    fn default() -> Self {
        Self {
            sun_dir: Vec4::new(0.0, 0.0, 1.0, 0.0),
            sun_color: Vec4::new(1.0, 0.98, 0.92, 1.0),
            ambient: Vec4::new(0.5, 0.5, 0.55, 0.4),
            num_lights: 0,
            lights: [PackedLight::default(); MAX_TILE_LIGHTS],
        }
    }
}

/// Custom [`MaterialTilemap`] used by every per-chunk tilemap. Binds the procedural
/// normal atlas (sampled with `tile_id` so it lines up with the diffuse atlas under
/// the texture-array path) plus a uniform block carrying the sun and per-frame
/// point lights.
#[derive(AsBindGroup, Asset, TypePath, Clone, Debug, Default)]
pub struct LitTilemapMaterial {
    /// Normal atlas, structurally identical to the diffuse atlas. Bevy's
    /// `AsBindGroup` derives a `texture_2d_array` view because the workspace
    /// builds `bevy_ecs_tilemap` without the `atlas` feature, which is the
    /// dimensionality the engine uses for tilemap textures internally.
    #[texture(0, dimension = "2d_array")]
    #[sampler(1)]
    pub normal: Handle<Image>,
    /// Per-frame lighting state. Updated in place by [`refresh_lighting_uniforms`].
    #[uniform(2)]
    pub lighting: LightingUniforms,
}

impl MaterialTilemap for LitTilemapMaterial {
    fn vertex_shader() -> ShaderRef {
        LIT_TILE_VERTEX_PATH.into()
    }

    fn fragment_shader() -> ShaderRef {
        LIT_TILE_FRAGMENT_PATH.into()
    }
}

/// Convenience handle to the shared [`LitTilemapMaterial`] used by every chunk.
/// Inserted once during plugin setup; consumed by `spawn_chunk_tilemap`.
#[derive(Resource, Clone, Debug)]
pub struct LitTileMaterialHandle(pub Handle<LitTilemapMaterial>);

/// Plugin that wires the lit tilemap material, loads the WGSL shaders, and runs
/// the per-frame lighting refresh.
pub struct LightingPlugin;

impl Plugin for LightingPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "lit_tile.wgsl");
        embedded_asset!(app, "lit_tile_vertex.wgsl");

        app.add_plugins(MaterialTilemapPlugin::<LitTilemapMaterial>::default())
            // `setup_shared_material` is an `Update` system because the lit material
            // depends on the procedural normal atlas, which is built in a `Startup`
            // system. We order it BEFORE `spawn_chunk_tilemap`-style consumers via
            // `.before(RenderPrepSet)` so the `LitTileMaterialHandle` resource is
            // present the very first frame any chunk tries to spawn. The early-return
            // on `existing` makes the system a one-shot.
            .add_systems(
                Update,
                setup_shared_material.before(inf2d_core::RenderPrepSet),
            )
            .add_systems(Update, refresh_lighting_uniforms);
    }
}

/// `Update` system: lazily build the single shared material once the
/// procedural normal atlas exists, then short-circuit on every subsequent frame.
/// Runs in `Update` rather than `Startup` so we always read the *real* normal
/// handle out of [`TileAtlas`] (which is built in `Startup` and may not have
/// landed yet when other `Startup` systems run).
///
/// The chunk-tilemap spawner gates on the resulting [`LitTileMaterialHandle`] —
/// `ChunkLoaded` events that fire before this system runs are absorbed by the
/// world streamer and re-emitted on the next tick, so a one-frame deferral is
/// invisible at runtime.
fn setup_shared_material(
    mut commands: Commands,
    atlas: Option<Res<TileAtlas>>,
    existing: Option<Res<LitTileMaterialHandle>>,
    mut materials: ResMut<Assets<LitTilemapMaterial>>,
) {
    if existing.is_some() {
        return;
    }
    let Some(atlas) = atlas else {
        return;
    };
    let handle = materials.add(LitTilemapMaterial {
        normal: atlas.normal_handle.clone(),
        lighting: LightingUniforms::default(),
    });
    commands.insert_resource(LitTileMaterialHandle(handle));
}

/// `Update` system: refresh the shared material's [`LightingUniforms`] each frame
/// from [`TimeOfDay`] and every active [`PointLight2D`]. Runs in `O(num_lights)`
/// — independent of how many chunks or tiles are loaded.
pub fn refresh_lighting_uniforms(
    tod: Res<TimeOfDay>,
    lights: Query<(&GlobalTransform, &PointLight2D)>,
    cameras: Query<&GlobalTransform, With<Camera2d>>,
    handle: Option<Res<LitTileMaterialHandle>>,
    mut materials: ResMut<Assets<LitTilemapMaterial>>,
) {
    let Some(handle) = handle else {
        return;
    };
    let Some(material) = materials.get_mut(&handle.0) else {
        return;
    };

    let (sun_dir, sun_color, ambient) = sun_for_hour(tod.hours);

    material.lighting.sun_dir = Vec4::new(sun_dir.x, sun_dir.y, sun_dir.z, 0.0);
    material.lighting.sun_color = sun_color;
    material.lighting.ambient = ambient;

    // Pack the closest lights to the first camera (if any). Without a camera we
    // still pack lights — they just get the entity insertion order. This keeps
    // headless test rigs deterministic.
    let cam_pos = cameras
        .iter()
        .next()
        .map(|t| t.translation().truncate())
        .unwrap_or(Vec2::ZERO);

    let mut packed: Vec<(f32, PackedLight)> = Vec::with_capacity(MAX_TILE_LIGHTS * 2);
    for (transform, light) in lights.iter() {
        let pos = transform.translation().truncate();
        let dist = pos.distance_squared(cam_pos);
        let linear = light.color.to_linear();
        packed.push((
            dist,
            PackedLight {
                pos: Vec4::new(pos.x, pos.y, 0.0, light.radius),
                color: Vec4::new(linear.red, linear.green, linear.blue, light.intensity),
            },
        ));
    }
    packed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let n = packed.len().min(MAX_TILE_LIGHTS);
    material.lighting.num_lights = n as u32;
    let mut buffer = [PackedLight::default(); MAX_TILE_LIGHTS];
    for (slot, (_, light)) in packed.into_iter().take(n).enumerate() {
        buffer[slot] = light;
    }
    material.lighting.lights = buffer;
}

/// Map `hours ∈ [0, 24)` to a `(sun_dir, sun_color, ambient)` triple. Pure function;
/// safe to call from tests or systems without touching the GPU.
fn sun_for_hour(hours: f32) -> (Vec3, Vec4, Vec4) {
    // Sun arc: 6h → east (cos = +1), 12h → overhead (cos = 0, sin = +1), 18h → west (cos = -1).
    // Outside [6, 18] the sun is "below the horizon" and contributes little direct light.
    let h = hours.rem_euclid(24.0);
    let day_t = ((h - 6.0).clamp(0.0, 12.0)) / 12.0;
    let theta = day_t * std::f32::consts::PI;
    // Bias `z` upward so even at sunrise/sunset the dome normal still catches some sun.
    let sun_dir = Vec3::new(theta.cos(), theta.sin(), 0.7).normalize();

    // Three palette anchors: dawn/dusk warm, noon neutral, midnight cool moonlight.
    let noon_color = Vec4::new(1.0, 0.98, 0.92, 1.0);
    let dusk_color = Vec4::new(1.0, 0.55, 0.30, 0.7);
    let night_color = Vec4::new(0.20, 0.25, 0.40, 0.15);

    // Ambient floors are raised so biome colors stay readable at every phase.
    // Even at midnight the world should be legible blue moonlight, not muddy.
    let noon_amb = Vec4::new(0.70, 0.70, 0.72, 0.85);
    let dusk_amb = Vec4::new(0.65, 0.55, 0.45, 0.75);
    let night_amb = Vec4::new(0.40, 0.45, 0.55, 0.55);

    // Three-way interpolation: noon (12) ↔ dusk (6/18) ↔ midnight (0/24).
    // Distance from noon, normalized to [0, 1] across the half-day.
    let dist_from_noon = (h - 12.0).abs() / 12.0; // 0 at noon, 1 at midnight
    let (sun_color, ambient) = if dist_from_noon <= 0.5 {
        // 6..18 — between noon and dusk
        let t = dist_from_noon * 2.0; // 0 at noon, 1 at dawn/dusk
        (
            lerp4(noon_color, dusk_color, t),
            lerp4(noon_amb, dusk_amb, t),
        )
    } else {
        // 18..6 — between dusk and midnight
        let t = (dist_from_noon - 0.5) * 2.0; // 0 at dusk, 1 at midnight
        (
            lerp4(dusk_color, night_color, t),
            lerp4(dusk_amb, night_amb, t),
        )
    };
    (sun_dir, sun_color, ambient)
}

#[inline]
fn lerp4(a: Vec4, b: Vec4, t: f32) -> Vec4 {
    let t = t.clamp(0.0, 1.0);
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lighting_uniforms_default_is_noon_ish() {
        let u = LightingUniforms::default();
        assert_eq!(u.num_lights, 0);
        assert!(u.sun_color.x > 0.5);
    }

    #[test]
    fn sun_rises_east_and_sets_west() {
        let (east, _, _) = sun_for_hour(6.0);
        let (west, _, _) = sun_for_hour(18.0);
        // East at sunrise: cos(0) = +1 ⇒ x > 0; west at sunset: cos(pi) = -1 ⇒ x < 0.
        assert!(east.x > 0.5, "sunrise should face east, got {east:?}");
        assert!(west.x < -0.5, "sunset should face west, got {west:?}");
    }

    #[test]
    fn noon_is_bright_midnight_is_dim() {
        let (_, noon, _) = sun_for_hour(12.0);
        let (_, midnight, _) = sun_for_hour(0.0);
        assert!(noon.w > 0.5);
        assert!(midnight.w < 0.5);
    }

    #[test]
    fn max_lights_matches_wgsl_constant() {
        // The WGSL declares `LIT_TILE_MAX_LIGHTS = 16u`. If this assertion breaks,
        // update lit_tile.wgsl in lockstep.
        assert_eq!(MAX_TILE_LIGHTS, 16);
    }
}

