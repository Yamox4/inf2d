//! HLOD (Hierarchical LOD) for far chunks. A chunk farther than `load_radius`
//! from the camera focus stops rendering as a full tilemap and instead becomes
//! a single sprite with a baked texture of the chunk's tile coloring. This
//! drops per-chunk geometry from `CHUNK_SIZE^2` tile entities + diffuse/normal
//! sampling to a single sprite read.
//!
//! Bake strategy: at chunk-load time (or first transition to HLOD), iterate
//! `ChunkData` and paint a 64×64 RGBA8 image where each tile contributes a
//! 2×2 px square colored by [`crate::atlas::BASE_COLOR`] for its `kind`. Store
//! in a [`HlodBakeCache`] keyed on `ChunkPos`. The imposter sprite is sized to
//! `(CHUNK_SIZE * TILE_WIDTH, CHUNK_SIZE * TILE_HEIGHT)` covering the chunk's
//! tile-grid bounding box. The chunk's screen-space footprint is a diamond
//! (the L1 ball of the tile grid), so the square imposter has visible corners
//! outside that diamond — accepted as the slice-1 trade-off; at the distance
//! HLOD activates the LUT / vignette / far-zoom downsampling masks the seams.
//!
//! ## Smooth LOD fade (slice 1, option C)
//!
//! Each chunk tracks a `lod_blend` in `[0.0, 1.0]` driven each frame by the
//! Chebyshev distance to the camera-focus chunk:
//!
//!   `lod_blend = smoothstep(load_radius - W, load_radius + W, dist)`
//!
//! with a width `W` of half a chunk. At `lod_blend = 0` the tilemap is fully
//! visible (imposter alpha = 0); at `lod_blend = 1` the imposter is fully
//! opaque (and the tilemap visibility flips off because the imposter is
//! covering it).
//!
//! For slice 1 we keep the tilemap's `Visibility` binary — it flips from
//! `Visible` to `Hidden` only when `lod_blend` reaches 1.0, by which time the
//! imposter is already fully opaque. The visible "pop" of the original
//! implementation is replaced by the imposter cross-fading in *over* the
//! tilemap during the transition window, then the tilemap disappears under
//! a fully opaque imposter.
//!
//! Future-work: option (A) of the design note would give each chunk its own
//! `LitTilemapMaterial` handle with a `chunk_alpha` uniform so both layers
//! could simultaneously alpha-blend during the transition. That would also
//! eliminate the very last "tilemap hidden under fully-opaque imposter"
//! transition; today it's covered by an opaque imposter so the pop is gone
//! either way, but a future per-chunk material would let the tilemap fade
//! out gracefully even if the imposter were partially translucent.

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use inf2d_core::{ChunkPos, RenderPrepSet, CHUNK_SIZE, TILE_HEIGHT, TILE_WIDTH};
use inf2d_world::{CameraFocus, ChunkData, ChunkLoaded, ChunkManager, StreamingConfig};

use crate::atlas::BASE_COLOR;
use crate::tilemap::ChunkTilemap;

/// One imposter pixel per tile, doubled so the bake stays crisp under filtering.
/// Result image is `HLOD_PX_PER_TILE * CHUNK_SIZE` square.
const HLOD_PX_PER_TILE: u32 = 2;

/// Side length (in pixels) of the baked imposter image.
const HLOD_IMAGE_SIZE: u32 = HLOD_PX_PER_TILE * CHUNK_SIZE;

/// Local Z for the imposter sprite. Sits just above the tilemap ground layer so a
/// chunk in transition (imposter visible, tilemap fading out) doesn't z-fight with
/// its own ground.
const HLOD_LOCAL_Z: f32 = 0.25;

/// Width (in chunks) of the smoothstep transition window centered on
/// `StreamingConfig::load_radius`. The imposter ramps from `alpha = 0` to
/// `alpha = 1` across distance `[load_radius - LOD_TRANSITION_WIDTH,
/// load_radius + LOD_TRANSITION_WIDTH]`. Half a chunk gives a visible but
/// fast cross-fade — at typical camera-pan speeds the chunk crosses the
/// transition zone in well under half a second, which feels like a smooth
/// blend rather than a pop.
const LOD_TRANSITION_WIDTH: f32 = 0.5;

/// Marker on the per-chunk imposter sprite child. Used by visibility / cleanup
/// queries to find it without scanning every child.
///
/// The `lod_blend` field is a per-frame cache of the smoothstep alpha, kept
/// here purely for inspection — the `Sprite::color.a` is the authoritative
/// blend value driving the render, and is rewritten each frame from the
/// distance computation. Storing the value here makes it visible in the
/// debug inspector and lets a future system (e.g. tilemap alpha fade) read
/// the per-chunk blend without redoing the smoothstep math.
#[derive(Component, Debug, Default)]
pub struct HlodImposter {
    /// Current smoothstep blend: 0.0 = tilemap fully visible, 1.0 = imposter
    /// fully opaque. Written each frame by [`update_chunk_visibility`].
    pub lod_blend: f32,
}

/// Cache of baked imposter textures keyed on `ChunkPos`. Survives chunk unload
/// so re-entering an explored area is a free re-attach instead of a re-bake.
#[derive(Resource, Default)]
pub struct HlodBakeCache {
    /// Map of chunk position to its baked imposter `Image` handle. The image
    /// itself is owned by `Assets<Image>`; this map only keeps the strong handle
    /// alive so the GPU upload survives between chunk reloads.
    pub textures: HashMap<ChunkPos, Handle<Image>>,
}

/// `RenderPlugin`-installed sub-plugin that wires the HLOD systems:
///
/// 1. On [`ChunkLoaded`], bake an imposter image (if not cached) and spawn an
///    `HlodImposter` child sprite for that chunk in `Visibility::Hidden`.
/// 2. Each frame after camera focus updates, compute per-chunk `lod_blend`
///    from Chebyshev distance and write it onto each chunk's imposter
///    sprite alpha; flip the tilemap visibility binary at the transition
///    end-points.
pub struct HlodPlugin;

impl Plugin for HlodPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HlodBakeCache>().add_systems(
            Update,
            (bake_hlod_on_load, update_chunk_visibility)
                .chain()
                .in_set(RenderPrepSet),
        );
    }
}

/// React to [`ChunkLoaded`]: bake the imposter image (if not cached) and spawn
/// an [`HlodImposter`] child sprite. Starts at zero alpha and hidden so the
/// per-frame visibility system can fade it in cleanly once the camera pans
/// to a distance that warrants the imposter.
pub fn bake_hlod_on_load(
    mut commands: Commands,
    mut events: MessageReader<ChunkLoaded>,
    chunks: Query<&ChunkData>,
    mut images: ResMut<Assets<Image>>,
    mut cache: ResMut<HlodBakeCache>,
) {
    for ev in events.read() {
        let Ok(data) = chunks.get(ev.entity) else {
            continue;
        };

        let handle = cache
            .textures
            .entry(ev.pos)
            .or_insert_with(|| {
                let image = bake_chunk_imposter_image(data);
                images.add(image)
            })
            .clone();

        let sprite_size = Vec2::new(
            CHUNK_SIZE as f32 * TILE_WIDTH,
            CHUNK_SIZE as f32 * TILE_HEIGHT,
        );

        // Start fully transparent. `update_chunk_visibility` writes the real
        // alpha each frame; spawning at 0 avoids a one-frame flash when the
        // imposter is parented to a chunk that's already inside the load
        // radius.
        let sprite = Sprite {
            image: handle,
            custom_size: Some(sprite_size),
            color: Color::srgba(1.0, 1.0, 1.0, 0.0),
            ..default()
        };

        // The chunk entity's `Transform` is at `chunk_origin_world(pos)`, which is
        // the *bottom* vertex of the chunk's diamond footprint. The diamond
        // extends upward by `CHUNK_SIZE * TILE_HEIGHT` to the top vertex; the
        // bounding-box center is half that distance up.
        let offset = Vec2::new(0.0, (CHUNK_SIZE as f32 * TILE_HEIGHT) * 0.5);

        commands.entity(ev.entity).with_children(|parent| {
            parent.spawn((
                HlodImposter::default(),
                sprite,
                Transform::from_xyz(offset.x, offset.y, HLOD_LOCAL_Z),
                Visibility::Hidden,
                Name::new(format!("HlodImposter({}, {})", ev.pos.x, ev.pos.y)),
            ));
        });
    }
}

/// Per-frame: classify every loaded chunk by Chebyshev distance to the
/// camera-focus chunk and update the imposter's alpha via a smoothstep over
/// the transition window. The tilemap's visibility still flips binary at
/// the transition end-points (slice 1, option C) — `Visible` while the
/// imposter is still partially transparent, `Hidden` once the imposter is
/// fully opaque. Chunks past `unload_radius` are despawned by
/// `inf2d_world::unload_distant_chunks`.
pub fn update_chunk_visibility(
    focus: Res<CameraFocus>,
    cfg: Res<StreamingConfig>,
    manager: Res<ChunkManager>,
    chunks: Query<&Children, With<inf2d_world::Chunk>>,
    mut tilemaps: Query<&mut Visibility, (With<ChunkTilemap>, Without<HlodImposter>)>,
    mut imposters: Query<
        (&mut Visibility, &mut Sprite, &mut HlodImposter),
        Without<ChunkTilemap>,
    >,
) {
    let center = focus.chunk;
    let r = cfg.load_radius as f32;
    let lo = r - LOD_TRANSITION_WIDTH;
    let hi = r + LOD_TRANSITION_WIDTH;

    for (pos, entity) in manager.iter() {
        let Ok(children) = chunks.get(entity) else {
            continue;
        };
        let dist = pos.chebyshev_distance(center) as f32;
        // Smoothstep gives a Hermite curve (no pop at the endpoints) over
        // the half-chunk-wide transition window. Outside the window it
        // clamps to 0 or 1, so the alpha doesn't drift past the band.
        let blend = smoothstep(lo, hi, dist);
        let imposter_visible = blend > 0.0;
        let tilemap_visible = blend < 1.0;

        for child in children.iter() {
            if let Ok(mut vis) = tilemaps.get_mut(child) {
                // Binary flip for the tilemap. The imposter has fully
                // covered the tile detail by the time `blend == 1.0`, so
                // hiding the tilemap then doesn't reveal anything.
                let target = if tilemap_visible {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                };
                if *vis != target {
                    *vis = target;
                }
            } else if let Ok((mut vis, mut sprite, mut imposter)) = imposters.get_mut(child) {
                imposter.lod_blend = blend;
                // Write the alpha each frame regardless of whether the
                // value changed. `Sprite` doesn't track change detection
                // on `color` granularly enough for us to short-circuit
                // here, and the write is a single float assignment plus
                // a `Color::srgba` build — cheap.
                let target_color = Color::srgba(1.0, 1.0, 1.0, blend);
                sprite.color = target_color;

                let target_vis = if imposter_visible {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                };
                if *vis != target_vis {
                    *vis = target_vis;
                }
            }
        }
    }
}

// Standard cubic Hermite smoothstep. Matches WGSL's `smoothstep`: returns
// `0` when `x <= edge0`, `1` when `x >= edge1`, and a Hermite interpolation
// in between. Used to drive the imposter alpha cross-fade across the
// `load_radius ± LOD_TRANSITION_WIDTH` transition window.
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let denom = (edge1 - edge0).max(f32::EPSILON);
    let t = ((x - edge0) / denom).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Bake a chunk's `ChunkData` into an `HLOD_IMAGE_SIZE`² RGBA8 image. Each tile
/// contributes a `HLOD_PX_PER_TILE` square colored by `BASE_COLOR[kind]`.
///
/// Grass tiles (the most common default) are written as fully opaque so the
/// imposter has a uniform alpha; non-solid biome contrast still shows through
/// because each tile's color comes from `BASE_COLOR` directly.
pub fn bake_chunk_imposter_image(data: &ChunkData) -> Image {
    let size = HLOD_IMAGE_SIZE;
    let stride = size * 4;
    let mut buf = vec![0u8; (stride * size) as usize];

    for (local, tile) in data.iter() {
        let color = BASE_COLOR[tile.kind as usize];
        let x0 = local.x * HLOD_PX_PER_TILE;
        let y0 = local.y * HLOD_PX_PER_TILE;
        for dy in 0..HLOD_PX_PER_TILE {
            for dx in 0..HLOD_PX_PER_TILE {
                let px = x0 + dx;
                let py = y0 + dy;
                let off = (py * stride + px * 4) as usize;
                if off + 4 <= buf.len() {
                    buf[off..off + 4].copy_from_slice(&color);
                }
            }
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        buf,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::linear();
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use inf2d_world::{Tile, TileKind};

    #[test]
    fn bake_outputs_expected_size_and_format() {
        let data = ChunkData::filled(Tile::of(TileKind::Grass));
        let image = bake_chunk_imposter_image(&data);
        let ext = image.texture_descriptor.size;
        assert_eq!(ext.width, HLOD_IMAGE_SIZE);
        assert_eq!(ext.height, HLOD_IMAGE_SIZE);
        assert_eq!(ext.depth_or_array_layers, 1);
        assert_eq!(
            image.texture_descriptor.format,
            TextureFormat::Rgba8UnormSrgb
        );
    }

    #[test]
    fn bake_paints_per_tile_base_color() {
        let mut data = ChunkData::filled(Tile::of(TileKind::Grass));
        data.set(
            inf2d_core::LocalTilePos::new(0, 0),
            Tile::of(TileKind::Water),
        );
        let image = bake_chunk_imposter_image(&data);
        let buf = image.data.as_ref().expect("baked image has cpu data");

        let stride = HLOD_IMAGE_SIZE * 4;
        let off = 0_usize;
        let expected = BASE_COLOR[TileKind::Water as usize];
        assert_eq!(&buf[off..off + 4], &expected);

        let off = (HLOD_PX_PER_TILE * 4) as usize;
        let expected = BASE_COLOR[TileKind::Grass as usize];
        assert_eq!(&buf[off..off + 4], &expected);

        let off = (stride + HLOD_PX_PER_TILE * 4) as usize;
        assert_eq!(&buf[off..off + 4], &expected);
    }

    #[test]
    fn smoothstep_is_zero_below_lo_and_one_above_hi() {
        // Outside the band the imposter alpha must clamp cleanly — drifting
        // past 1 (e.g. via an unclamped polynomial) would feed bogus alpha
        // values to the Sprite.
        assert_eq!(smoothstep(2.0, 3.0, 0.0), 0.0);
        assert_eq!(smoothstep(2.0, 3.0, 1.5), 0.0);
        assert_eq!(smoothstep(2.0, 3.0, 5.0), 1.0);
    }

    #[test]
    fn smoothstep_midpoint_is_half() {
        // Hermite midpoint: at the exact center of [edge0, edge1] the
        // cubic returns 0.5 — sanity check that the curve isn't shifted.
        let mid = smoothstep(2.0, 3.0, 2.5);
        assert!((mid - 0.5).abs() < 1e-5);
    }

    #[test]
    fn smoothstep_handles_zero_width_band() {
        // A degenerate band (edge0 == edge1) shouldn't NaN. We replace the
        // denominator with `f32::EPSILON` so the result snaps to 0 below
        // the edge and 1 at-or-above; this keeps the transition usable
        // even if a caller sets `LOD_TRANSITION_WIDTH` to 0.
        let below = smoothstep(2.0, 2.0, 1.9);
        let at = smoothstep(2.0, 2.0, 2.0);
        assert!(below < 0.5);
        assert!(at >= 0.5);
    }
}
