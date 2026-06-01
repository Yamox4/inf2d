//! Per-chunk `bevy_ecs_tilemap` spawning and teardown.
//!
//! On [`ChunkLoaded`], one or more child entities are attached to the chunk — one
//! [`ChunkTilemap`] per distinct tile elevation step found in the chunk. Each tilemap holds
//! only the tiles at its height, is offset upward in screen-Y by `height * HEIGHT_STEP_PX`,
//! and bumped a small amount in render-Z so back-to-front sorting between height layers is
//! deterministic. The chunk entity's `Transform` (placed at `chunk_origin_world(pos)` by
//! `inf2d_world`) supplies the world-space XY offset, so the tilemap children stay at local
//! `(0, height_offset, layer_z)` and tile positions inside each tilemap land at the correct
//! world coordinates.
//!
//! ## Why one tilemap per height level
//!
//! `bevy_ecs_tilemap` renders every tile in a single tilemap at a single Z and without any
//! per-tile vertex offset. To get the Tactics Ogre / FFT "stair-stepping plateaus" look, we
//! split the chunk's tiles into one tilemap per elevation step and shift each layer's
//! transform — the GPU still batches each layer, so we go from "one draw call per chunk" to
//! "one draw call per `(chunk, height)` pair" rather than per-tile, which is cheap in
//! practice because most chunks only span 2–4 distinct elevations.
//!
//! ## Hierarchy
//!
//! ```text
//! Chunk entity (Transform at chunk_origin_world)
//! ├── ChunkTilemap(height=-1) ─ tilemap children for sunken water/cave tiles
//! ├── ChunkTilemap(height=0)  ─ ground-plane tiles
//! ├── ChunkTilemap(height=1)  ─ first plateau
//! └── ...
//! ```
//!
//! On [`ChunkUnloaded`] the world streamer despawns the chunk entity; the `ChildOf`
//! relationship cascades the despawn through every height-layer tilemap and its tile
//! entities.

use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy_ecs_tilemap::prelude::*;
use inf2d_core::{HEIGHT_STEP_PX, CHUNK_SIZE};
use inf2d_world::{ChunkData, ChunkLoaded, ChunkUnloaded, Tile};

use crate::atlas::TileAtlas;
use crate::layers::RenderLayer;
use crate::lit_tile_material::LitTileMaterialHandle;

/// Marker component on a per-chunk tilemap entity (the child of a chunk).
///
/// One marker is spawned per `(chunk, height)` pair — the `height` field records which
/// elevation step this tilemap renders, so debug overlays and the unload sweep can
/// distinguish layers without re-reading the tilemap's `Transform`.
#[derive(Component, Debug, Default)]
pub struct ChunkTilemap {
    /// Elevation step (in `HEIGHT_STEP_PX` units) that this layer renders.
    pub height: i8,
}

/// Per-layer Z separation between adjacent height levels. Small enough that no other
/// `RenderLayer` constant is crossed (the next layer up, `WATER`, sits at `0.5`), large
/// enough to break ties cleanly between a tile at height N and the tilemap underneath it.
const HEIGHT_LAYER_Z_STEP: f32 = 0.01;

/// `Update` system: react to [`ChunkLoaded`] by spawning one tilemap child per distinct
/// elevation in the chunk's [`ChunkData`]. Skips silently if the atlas isn't ready yet or
/// the chunk entity has already been despawned in the same frame.
pub fn spawn_chunk_tilemap(
    mut commands: Commands,
    mut events: MessageReader<ChunkLoaded>,
    chunks: Query<&ChunkData>,
    atlas: Option<Res<TileAtlas>>,
    lit_material: Option<Res<LitTileMaterialHandle>>,
) {
    let Some(atlas) = atlas else {
        // Atlas built on Startup; if a ChunkLoaded fires before that runs (test harness,
        // headless tooling), drop the event — the world streamer is the source of truth
        // and will re-emit on next chunk relaunch, but in normal play this branch is dead.
        return;
    };
    let Some(lit_material) = lit_material else {
        // Same guard as `atlas`: the shared lit material is created by
        // `LightingPlugin::setup_shared_material`, ordered `.before(RenderPrepSet)`
        // so this system observes it from the very first frame the atlas exists.
        // This branch is the headless / pre-atlas race fallback.
        return;
    };

    for ev in events.read() {
        let Ok(data) = chunks.get(ev.entity) else {
            tracing::warn!(
                "ChunkLoaded for entity {:?} ({:?}) but ChunkData missing — skipping tilemap spawn",
                ev.entity,
                ev.pos,
            );
            continue;
        };

        // Bucket tiles by elevation step. Most chunks span only a handful of distinct
        // heights, but the hash map handles up to `u8::MAX` worth of layers cleanly.
        let mut layers: HashMap<i8, Vec<(TilePos, Tile)>> = HashMap::default();
        for (local, tile) in data.iter() {
            let tile_pos = TilePos {
                x: local.x,
                y: local.y,
            };
            layers.entry(tile.height).or_default().push((tile_pos, tile));
        }

        let map_size = TilemapSize {
            x: CHUNK_SIZE,
            y: CHUNK_SIZE,
        };

        for (height, tiles) in layers {
            let tilemap_entity = commands.spawn_empty().id();
            let mut storage = TileStorage::empty(map_size);

            for (tile_pos, tile) in tiles {
                let tile_entity = commands
                    .spawn(TileBundle {
                        position: tile_pos,
                        tilemap_id: TilemapId(tilemap_entity),
                        texture_index: TileTextureIndex(tile.kind.atlas_index()),
                        ..Default::default()
                    })
                    .id();
                storage.set(&tile_pos, tile_entity);
                commands.entity(tilemap_entity).add_child(tile_entity);
            }

            // Layer offset: shift the whole sub-tilemap up by `height * HEIGHT_STEP_PX` so
            // its diamonds visually sit on the terrace, and bump Z by a fractional step so
            // higher layers always sort in front of lower ones at the same screen-Y.
            let y_offset = height as f32 * HEIGHT_STEP_PX;
            let z_offset = RenderLayer::GROUND + height as f32 * HEIGHT_LAYER_Z_STEP;

            commands.entity(tilemap_entity).insert((
                ChunkTilemap { height },
                Name::new(format!(
                    "ChunkTilemap({}, {}) h={}",
                    ev.pos.x, ev.pos.y, height
                )),
                MaterialTilemapBundle::<crate::lit_tile_material::LitTilemapMaterial> {
                    grid_size: TilemapGridSize::from(atlas.tile_size),
                    map_type: TilemapType::Isometric(IsoCoordSystem::Diamond),
                    size: map_size,
                    storage,
                    texture: TilemapTexture::Single(atlas.handle.clone()),
                    tile_size: atlas.tile_size,
                    anchor: TilemapAnchor::None,
                    transform: Transform::from_xyz(0.0, y_offset, z_offset),
                    material: MaterialTilemapHandle(lit_material.0.clone()),
                    ..Default::default()
                },
            ));

            commands.entity(ev.entity).add_child(tilemap_entity);
        }
    }
}

/// `Update` system: react to [`ChunkUnloaded`]. The world streamer despawns the chunk
/// entity in the same tick; Bevy 0.18's `ChildOf` relationship cascades the despawn
/// through every height-layer tilemap and its tile entities. This system exists to log
/// the cleanup and to provide a hook point if a future revision needs to release
/// GPU-side caches.
pub fn despawn_chunk_tilemap(
    mut events: MessageReader<ChunkUnloaded>,
    tilemaps: Query<(Entity, &ChildOf), With<ChunkTilemap>>,
    mut commands: Commands,
) {
    for ev in events.read() {
        // Defensive sweep: if a height-layer tilemap survived despawn cascade for any
        // reason (e.g., the chunk entity was already gone before the event fired),
        // explicitly despawn the orphaned tilemap. Safe no-op when the cascade already ran.
        for (tilemap_entity, parent) in tilemaps.iter() {
            if parent.parent() == ev.entity {
                commands.entity(tilemap_entity).try_despawn();
            }
        }
        tracing::trace!(
            "chunk unloaded at {:?} (entity {:?}) — tilemap teardown dispatched",
            ev.pos,
            ev.entity
        );
    }
}
