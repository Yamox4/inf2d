#![deny(unsafe_code)]
//! Real-time WGSL water surface — **one shader quad per chunk**.
//!
//! Replaces the previous per-tile spawning. Each chunk now produces one quad
//! covering the chunk's full tile-grid bounding box; the quad samples a
//! per-chunk **water mask** texture (32×32 R8, 255 = water tile, 0 = dry)
//! and `discard`s every fragment outside the water area. The remaining
//! fragments run the existing shimmer/spec/depth pipeline.
//!
//! Mask cost is `CHUNK_SIZE^2 * 1 byte = 1 KB` per chunk on the CPU, plus
//! the same on the GPU. A chunk with zero water tiles spawns no quad at all
//! — the [`build_water_mask_and_height`] builder returns `None`.
//!
//! ## Why per-chunk materials
//!
//! Each chunk needs its own `WaterMaterial` because the mask texture is
//! per-chunk. The per-frame uniform update (`time`, `sun_angle`,
//! `sun_strength`, `moon_strength`) walks every loaded `WaterMaterial`
//! asset and patches the values in place — the cost is `O(num_water_chunks)`
//! per frame which, with the new load-radius of 5, peaks at ~121 hash-map
//! lookups even when every chunk has water. The colors and per-chunk
//! state (the mask handle) don't change so the upload size per chunk is
//! tiny.
//!
//! ## Z layering
//!
//! The previous "one quad per tile" scheme rode each quad just above its
//! own tile's tilemap. The merged quad now rides a single Z that's safely
//! above the lowest water tile in the chunk and below the lowest possible
//! elevated terrain. We anchor the quad at the chunk-local origin and
//! offset its Z to `WATER_QUAD_LOCAL_Z = -0.005`, between the recessed
//! water tilemap at `-0.01` and the ground plane at `0.0`. Cliffs are
//! already skipped on water-facing drops, so this single-Z choice
//! doesn't z-fight with any cliff geometry.
//!
//! ## Cross-chunk noise continuity
//!
//! The shader still samples noise in WORLD space via `mesh.world_position`,
//! so adjacent chunks' water surfaces continue each other's ripples
//! seamlessly — the merged-quad refactor doesn't change that property.

use bevy::asset::{embedded_asset, RenderAssetUsages};
use bevy::image::ImageSampler;
use bevy::math::primitives::Rectangle;
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dPlugin, MeshMaterial2d};
use inf2d_core::{CHUNK_SIZE, HEIGHT_STEP_PX, TILE_HEIGHT, TILE_WIDTH};
use inf2d_world::{ChunkData, ChunkLoaded, TileKind};

use crate::daynight::{sun_strength_for_hour, TimeOfDay};

/// Local Z for the merged per-chunk water quad. Sits at `-0.005`, between
/// the recessed water tilemap (`-0.01`) and the ground plane (`0.0`). The
/// previous per-tile scheme used the tile's own height to drive Z; the
/// merged quad sits at a single Z because it covers tiles at every water
/// height simultaneously, and the cliff system now skips water-facing
/// drops so we don't need a per-height sort key for water specifically.
const WATER_QUAD_LOCAL_Z: f32 = -0.005;

/// Pixel width / height of the per-chunk water mask. One pixel per tile —
/// `CHUNK_SIZE * CHUNK_SIZE = 1024` bytes per chunk.
const WATER_MASK_SIZE: u32 = CHUNK_SIZE;

/// Plugin: registers the [`WaterMaterial`] pipeline, embeds the WGSL,
/// builds the shared chunk-quad mesh in `Startup`, spawns per-chunk
/// shader quads on [`ChunkLoaded`], and ticks every loaded material's
/// uniform each frame.
pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "water.wgsl");

        app.add_plugins(Material2dPlugin::<WaterMaterial>::default())
            .add_systems(Startup, build_water_assets)
            .add_systems(
                Update,
                (spawn_water_quads, drive_water_uniforms).chain(),
            );
    }
}

/// `@group(2) @binding(0)` uniform block. Field order MUST match
/// `WaterUniforms` in `water.wgsl`. `AsBindGroup` derives a single
/// concatenated UBO from the `#[uniform(0)]`-tagged fields in declaration
/// order.
///
/// The chunk-local mask texture lives at bindings 1/2 (texture + sampler).
/// Adding the mask was a deliberate trade: one extra texture per chunk in
/// exchange for ~100x fewer water entities. The mask is 32×32 R8 = 1 KB,
/// well under typical 4 MB VRAM budgets even at 1000 chunks loaded.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct WaterMaterial {
    /// Elapsed seconds. Drives the two scrolling noise inputs.
    #[uniform(0)]
    pub time: f32,
    /// Sun azimuth in radians, derived from [`TimeOfDay::hours`]. Controls
    /// the specular highlight direction; rotates across the day so
    /// highlights sweep.
    #[uniform(0)]
    pub sun_angle: f32,
    /// Specular intensity multiplier. 1.0 at solar noon, 0.0 at midnight,
    /// smooth crossfade between.
    #[uniform(0)]
    pub sun_strength: f32,
    /// Moonlight specular intensity. Complement of `sun_strength`.
    #[uniform(0)]
    pub moon_strength: f32,
    /// Deep-water RGB (alpha = surface opacity). Used where shimmer is high.
    #[uniform(0)]
    pub base_color: LinearRgba,
    /// Shallow / sunlit water RGB. Crossfaded with `base_color` by shimmer.
    #[uniform(0)]
    pub shallow_color: LinearRgba,
    /// Per-chunk water-mask image. Sampled in the fragment shader at the
    /// quad's local UV; fragments whose mask value is below 0.5 are
    /// `discard`ed so the merged quad shows only its water tiles.
    #[texture(1)]
    #[sampler(2)]
    pub mask: Handle<Image>,
}

impl Default for WaterMaterial {
    fn default() -> Self {
        Self {
            time: 0.0,
            sun_angle: 0.0,
            sun_strength: 1.0,
            moon_strength: 0.0,
            // Deep ocean blue, slightly desaturated so it doesn't fight the UI.
            base_color: LinearRgba::new(0.10, 0.28, 0.55, 0.78),
            // Tropical shallow teal — what the shimmer peaks look like.
            shallow_color: LinearRgba::new(0.36, 0.78, 0.85, 0.78),
            mask: Handle::default(),
        }
    }
}

impl Material2d for WaterMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://inf2d_render/water.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        // Translucent so the tilemap's "pond bottom" water tile shows through
        // the shimmering surface.
        AlphaMode2d::Blend
    }
}

/// Cached GPU handle for the single chunk-sized quad mesh every per-chunk
/// water material references.
///
/// One quad mesh shared across every chunk is fine because the rectangle's
/// world dimensions are fixed (`CHUNK_SIZE * TILE_WIDTH × CHUNK_SIZE *
/// TILE_HEIGHT`); per-chunk variation comes from the `Transform` placing it
/// and the per-chunk mask texture sampled in the shader.
#[derive(Resource, Clone)]
pub struct WaterAssets {
    /// Chunk-sized axis-aligned quad. The shader uses the world-space
    /// position to drive noise (cross-chunk continuity) and the local UV
    /// to sample the per-chunk mask.
    pub mesh: Handle<Mesh>,
}

/// Marker on the per-chunk water-quad child entity.
#[derive(Component, Debug)]
pub struct WaterTileQuad;

/// `Startup` system: cache the shared chunk-sized quad mesh in [`WaterAssets`].
fn build_water_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    // Chunk-sized rectangle. Width is `CHUNK_SIZE * TILE_WIDTH`, height is
    // `CHUNK_SIZE * TILE_HEIGHT` — the bounding box of the chunk's diamond
    // tile grid in screen-space units. The diamond mask the previous
    // per-tile shader applied is gone; the per-chunk water mask now does
    // the in/out-of-water test.
    let w = CHUNK_SIZE as f32 * TILE_WIDTH;
    let h = CHUNK_SIZE as f32 * TILE_HEIGHT;
    let mesh = meshes.add(Mesh::from(Rectangle::new(w, h)));
    commands.insert_resource(WaterAssets { mesh });
}

/// `Update` system: react to [`ChunkLoaded`] by checking whether the chunk
/// has any water tiles. If so, build a 32×32 R8 mask image, register a
/// per-chunk [`WaterMaterial`] referencing the mask, and spawn one quad as
/// a child of the chunk.
///
/// The quad is parented to the chunk entity so that `ChildOf` cascade
/// despawn fires when the streamer drops the chunk. The mask image and
/// material asset get dropped when the chunk despawns their last strong
/// handle; `Assets<Image>` and `Assets<WaterMaterial>` clean up the GPU
/// resources on the next render extract.
pub fn spawn_water_quads(
    mut commands: Commands,
    mut events: MessageReader<ChunkLoaded>,
    chunks: Query<&ChunkData>,
    assets: Option<Res<WaterAssets>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
) {
    let Some(assets) = assets else {
        // Startup hasn't run yet — drop silently.
        return;
    };

    for ev in events.read() {
        let Ok(data) = chunks.get(ev.entity) else {
            tracing::warn!(
                "ChunkLoaded for entity {:?} ({:?}) but ChunkData missing — skipping water quad spawn",
                ev.entity,
                ev.pos,
            );
            continue;
        };

        let Some((mask_image, water_height_step)) = build_water_mask_and_height(data) else {
            // No water tiles in this chunk — skip the quad entirely.
            continue;
        };

        let mask_handle = images.add(mask_image);

        let material_handle = materials.add(WaterMaterial {
            mask: mask_handle,
            ..WaterMaterial::default()
        });

        // Place the merged quad's center in chunk-local space. The chunk's
        // own `Transform` lives at `chunk_origin_world(pos)` — the bottom
        // vertex of its diamond. The chunk's diamond extends upward in
        // screen-Y by `CHUNK_SIZE * TILE_HEIGHT`; centering the quad at
        // half that distance up aligns its UV-Y axis (0 at bottom, 1 at
        // top) with the chunk's tile rows.
        //
        // The quad is also lifted by `water_height_step * HEIGHT_STEP_PX`
        // so the shimmer surface sits on top of the recessed water
        // tilemap rather than floating above it.
        let center_x = 0.0;
        let center_y = (CHUNK_SIZE as f32 * TILE_HEIGHT) * 0.5
            + water_height_step as f32 * HEIGHT_STEP_PX;

        commands.entity(ev.entity).with_children(|parent| {
            parent.spawn((
                WaterTileQuad,
                Mesh2d(assets.mesh.clone()),
                MeshMaterial2d(material_handle),
                Transform::from_xyz(center_x, center_y, WATER_QUAD_LOCAL_Z),
                Visibility::default(),
                Name::new(format!("WaterChunkQuad({}, {})", ev.pos.x, ev.pos.y)),
            ));
        });
    }
}

/// Build the per-chunk water mask. Returns `None` when the chunk has no
/// water tiles (cheap early-out: skip the quad entirely so chunks with
/// pure-land terrain don't allocate an image or a material).
///
/// The mask is `WATER_MASK_SIZE × WATER_MASK_SIZE = CHUNK_SIZE × CHUNK_SIZE`
/// R8 pixels: byte `y * CHUNK_SIZE + x` is 255 if `(x, y)` is a water tile,
/// 0 otherwise. The shader samples this with linear filtering for soft
/// shoreline edges and `discard`s any fragment whose mask value falls below
/// 0.5.
///
/// Also returns the **water height step** found in the chunk (the height
/// every water tile shares; world-gen places water tiles at a single
/// `water_height` per the [`inf2d_worldgen::biome::height_for`] rule). The
/// returned value is the `i8` step count; the caller multiplies by
/// [`HEIGHT_STEP_PX`] to offset the quad in screen-Y so the shimmer rides
/// on the actual water surface. If the chunk has water tiles at multiple
/// heights (not currently produced by worldgen, but defensible against
/// future generators), the first water tile's height is used; mismatched
/// heights would produce a faintly misaligned shimmer band, which is a
/// future-work item.
fn build_water_mask_and_height(data: &ChunkData) -> Option<(Image, i8)> {
    let size = WATER_MASK_SIZE;
    let mut buf = vec![0u8; (size * size) as usize];
    let mut found_water = false;
    let mut water_height: i8 = 0;

    for (local, tile) in data.iter() {
        if tile.kind != TileKind::Water {
            continue;
        }
        if !found_water {
            water_height = tile.height;
            found_water = true;
        }
        let off = (local.y * size + local.x) as usize;
        if off < buf.len() {
            buf[off] = 255;
        }
    }

    if !found_water {
        return None;
    }

    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        buf,
        // R8Unorm: one byte per pixel, sampled as a 0..1 float. sRGB is
        // unnecessary for a binary mask, and using a non-sRGB format keeps
        // the bilinear filter linear in the value we care about.
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    // Nearest filtering: the shader samples the mask at the exact tile
    // center (`(floor(lx) + 0.5) / CHUNK_SIZE`), so a point sample returns
    // the tile's binary water value with no edge bleed. Linear filtering
    // would slightly bleed water value across tile boundaries, softening
    // the coastline by half a tile.
    image.sampler = ImageSampler::nearest();
    Some((image, water_height))
}

/// `Update` system: write the per-frame uniform values onto every loaded
/// [`WaterMaterial`] asset. Costs `O(num_water_chunks)` hash-map lookups
/// plus a small in-place struct write each — bounded by ~150 at the
/// current load radius even if every chunk has water.
pub fn drive_water_uniforms(
    time: Res<Time>,
    tod: Res<TimeOfDay>,
    mut materials: ResMut<Assets<WaterMaterial>>,
) {
    let t = time.elapsed_secs();
    let sun_angle = sun_angle_for_hour(tod.hours);
    let sun = sun_strength_for_hour(tod.hours);
    let moon = 1.0 - sun;

    for (_, material) in materials.iter_mut() {
        material.time = t;
        material.sun_angle = sun_angle;
        material.sun_strength = sun;
        material.moon_strength = moon;
    }
}

/// Map hour-of-day → sun azimuth in radians. Sunrise at 06:00 → 0 rad (east),
/// noon at 12:00 → PI/2 (straight up in 2D screen-space terms), sunset at
/// 18:00 → PI (west). Outside daylight the angle keeps advancing — the
/// strength curve from [`sun_strength_for_hour`] gates the visible spec.
fn sun_angle_for_hour(h: f32) -> f32 {
    let frac = (h / 24.0).clamp(0.0, 1.0);
    frac * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2
}

#[cfg(test)]
mod tests {
    use super::*;
    use inf2d_world::{Tile, TileKind};

    #[test]
    fn sun_angle_advances_monotonically() {
        let a = sun_angle_for_hour(6.0);
        let b = sun_angle_for_hour(12.0);
        let c = sun_angle_for_hour(18.0);
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn default_material_has_translucent_base_color() {
        let mat = WaterMaterial::default();
        assert!(mat.base_color.alpha < 1.0);
        assert!(mat.base_color.alpha > 0.0);
    }

    #[test]
    fn water_mask_skips_chunks_without_water() {
        // A chunk of pure grass should produce no mask — chunks with zero
        // water tiles must not allocate a per-chunk image.
        let data = ChunkData::filled(Tile::of(TileKind::Grass));
        assert!(build_water_mask_and_height(&data).is_none());
    }

    #[test]
    fn water_mask_marks_only_water_tiles() {
        // Single water tile in a sea of grass: the mask should be all
        // zeros except for one 255 at the water tile's index.
        let mut data = ChunkData::filled(Tile::of(TileKind::Grass));
        data.set(
            inf2d_core::LocalTilePos::new(3, 4),
            Tile::of(TileKind::Water),
        );
        let (image, _) = build_water_mask_and_height(&data).expect("mask present");
        let buf = image.data.as_ref().expect("mask has cpu data");
        assert_eq!(buf.len(), (CHUNK_SIZE * CHUNK_SIZE) as usize);
        // (3, 4) is byte index 4 * CHUNK_SIZE + 3.
        let idx = (4 * CHUNK_SIZE + 3) as usize;
        assert_eq!(buf[idx], 255);
        // Every other pixel is 0.
        let lit: usize = buf.iter().filter(|&&b| b > 0).count();
        assert_eq!(lit, 1);
    }

    #[test]
    fn water_mask_picks_up_water_height() {
        // The first water tile's height is what `drive_water_uniforms`
        // would offset the quad by. A custom -2 height should round-trip.
        let mut data = ChunkData::filled(Tile::of(TileKind::Grass));
        data.set(
            inf2d_core::LocalTilePos::new(0, 0),
            Tile::with_height(TileKind::Water, -2),
        );
        let (_, h) = build_water_mask_and_height(&data).expect("mask present");
        assert_eq!(h, -2);
    }

    #[test]
    fn water_mask_is_r8_unorm() {
        // The mask must be a single-channel non-sRGB format so linear
        // filtering sees the raw 0/255 values.
        let mut data = ChunkData::filled(Tile::of(TileKind::Grass));
        data.set(
            inf2d_core::LocalTilePos::new(0, 0),
            Tile::of(TileKind::Water),
        );
        let (image, _) = build_water_mask_and_height(&data).expect("mask present");
        assert_eq!(image.texture_descriptor.format, TextureFormat::R8Unorm);
        assert_eq!(image.texture_descriptor.size.width, CHUNK_SIZE);
        assert_eq!(image.texture_descriptor.size.height, CHUNK_SIZE);
    }
}
