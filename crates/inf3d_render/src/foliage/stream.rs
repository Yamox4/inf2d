//! The streaming systems: ring foliage tiles in/out as the player moves.
//!
//! Foliage streams as **two independent layers**, each its own system:
//!
//! ## Solid layer — [`stream_solid`]
//!
//! Trees + rocks, ringed by the camera's orthographic zoom. Runs every frame and
//! orchestrates four phases, each a focused helper below:
//!
//! 1. [`clear_solid_tiles`] — bail out + unload everything when foliage is off.
//! 2. [`poll_solid_tasks`] — spawn entities for any solid scatter task that
//!    resolved, recording footprint-inflated [`BlockedCells`] for the pathfinder.
//! 3. [`despawn_solid_out_of_band`] — unload solid tiles outside the (wider)
//!    despawn ring, releasing their blocked cells.
//! 4. [`start_solid_tasks`] — start up to [`MAX_SOLID_TASKS_PER_FRAME`] new
//!    scatter tasks for the nearest missing tiles inside the spawn ring.
//!
//! The despawn ring is a **hysteresis band** wider than the spawn ring
//! ([`DESPAWN_RING_MARGIN`] extra tiles), so on the wide orthographic-iso view
//! props don't pop out the moment the camera nudges or zooms — tiles only unload
//! well outside the visible area. The spawn ring scales with the camera's
//! orthographic viewport ([`compute_ring`]) so zooming out fills trees/rocks to
//! the iso-view edges, clamped to fixed settings' `foliage_ring_max`.
//!
//! ## Grass layer — [`stream_grass`]
//!
//! Grass only, ringed in a **player-centered, zoom-INDEPENDENT** disc whose
//! radius is `ceil(grass_radius_world / TILE)` tiles ([`grass_ring_tiles`]). The
//! grass disc simply follows the player at a fixed size, so zooming out — which
//! enlarges the *solid* ring — never enlarges the grass carpet. Phases mirror the
//! solid layer ([`poll_grass_tasks`] / [`despawn_grass_out_of_band`] /
//! [`start_grass_tasks`]) but grass records NO blocked cells and gets NO collider,
//! and a grass tile despawns the instant it leaves the ring (no hysteresis band:
//! the disc is small and player-centered, so there's no zoom thrash to absorb).
//!
//! Because the two layers never share a tile field, grass appearing/disappearing
//! never disturbs a tree/rock collider or its blocked cells, and a solid tile
//! never re-streams for a grass reason. This replaces the old single-field design
//! whose `restream_changed_tiles` re-scattered the WHOLE tile (churning solid
//! colliders + `BlockedCells`) whenever grass eligibility crossed the radius.

use std::cmp::Reverse;

use bevy::camera::{Projection, ScalingMode};
use bevy::prelude::*;
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool};

use inf3d_camera::IsoCamera;
use inf3d_core::{BlockedCells, FollowTarget, PropSurfaces, QualitySettings};
use inf3d_physics::PLAYER_RADIUS;
use inf3d_worldgen::{Terrain, WorldGen, WorldKind};

use super::scatter::{scatter_grass, scatter_solid};
use super::spawn::spawn_tile_entities;
use super::{
    footprint_radius, is_low_prop, FoliageAssets, GrassField, GrassTileState, ScatterCategory,
    ScatterItem, SolidField, SolidTileState, SolidVariantSizes, TileScatterTask, TILE,
};

/// Minimum solid ring radius the streamer ever uses, regardless of zoom level.
const RING_MIN: i32 = 2;
/// Fallback solid ring radius used when the camera entity hasn't spawned yet (or
/// isn't orthographic).
const RING_FALLBACK: i32 = 3;
/// Multiplier from the camera's orthographic `viewport_height` to the
/// world-XZ radius the solid foliage ring needs to cover. Kept well above the
/// literal half-height so the spawn ring reaches a MARGIN beyond the visible
/// viewport — props are scattered before they scroll into view, so they don't
/// "pop in" at the screen edge as the player walks. (Raised 1.1 -> 1.35; pairs
/// with the higher `foliage_ring_max` cap so this margin isn't clamped away when
/// zoomed out.)
const RING_ZOOM_COVERAGE: f32 = 1.35;
/// Extra tiles the *despawn* ring extends past the *spawn* ring (hysteresis) for
/// the solid layer.
const DESPAWN_RING_MARGIN: i32 = 2;

/// Maximum number of SOLID tile scatter tasks STARTED per frame. Bounding the
/// starts spreads a big ring-fill (first spawn / zoom-out) over several frames
/// instead of flooding the task pool and spawning everything in one stall.
const MAX_SOLID_TASKS_PER_FRAME: usize = 3;
/// Maximum number of SOLID tiles UNLOADED per frame. A tile-boundary crossing
/// pushes a whole ring-edge row out of band at once; despawning all of it in one
/// frame is a periodic walk hitch that gets worse the further you zoom out (larger
/// ring → longer edge). Budgeting the unloads spreads that cost over a few frames.
/// Out-of-band tiles already sit well outside the view (hysteresis margin), so
/// letting a few linger an extra frame or two is invisible.
const MAX_SOLID_DESPAWNS_PER_FRAME: usize = 4;

/// Maximum number of GRASS tile scatter tasks STARTED per frame. The grass disc
/// is small (fixed world radius) so it rarely needs the full budget, but bounding
/// the starts keeps the initial fill / a fast walk from spawning a whole edge of
/// grass tiles in one frame.
const MAX_GRASS_TASKS_PER_FRAME: usize = 3;
/// Maximum number of GRASS tiles UNLOADED per frame. The grass ring has no
/// hysteresis band, so a single step can push a whole edge row out at once;
/// budgeting the despawns spreads that over a few frames. The leaked tiles sit
/// exactly at the disc edge for an extra frame or two — invisible at the iso view.
const MAX_GRASS_DESPAWNS_PER_FRAME: usize = 4;

/// Stream the SOLID layer (trees + rocks) in/out as the player moves. See the
/// module doc for the per-frame phase breakdown.
pub(super) fn stream_solid(
    mut commands: Commands,
    assets: Option<Res<FoliageAssets>>,
    terrain: Res<Terrain>,
    mut field: ResMut<SolidField>,
    mut blocked: ResMut<BlockedCells>,
    mut props: ResMut<PropSurfaces>,
    settings: Res<QualitySettings>,
    player_q: Query<&Transform, With<FollowTarget>>,
    camera_q: Query<&Projection, With<IsoCamera>>,
) {
    let Some(assets) = assets else {
        return;
    };
    if !settings.foliage_enabled {
        clear_solid_tiles(&mut commands, &mut field, &mut blocked, &mut props);
        return;
    }
    let Ok(player) = player_q.single() else {
        return;
    };
    let center = tile_of(player.translation);

    let projection = camera_q.single().ok();
    let spawn_ring = compute_ring(projection, settings.foliage_ring_max);
    let despawn_ring = spawn_ring + DESPAWN_RING_MARGIN;

    poll_solid_tasks(&mut commands, &assets, &mut field, &mut blocked, &mut props);
    despawn_solid_out_of_band(
        &mut commands,
        &mut field,
        &mut blocked,
        &mut props,
        center,
        despawn_ring,
    );
    start_solid_tasks(&assets, &terrain, &mut field, center, spawn_ring);
}

/// React to player block edits by despawning the grass blade(s) on the edited
/// cell(s) — and ONLY those. Grass blades are individual entities (one per cell),
/// so this leaves the rest of the tile completely untouched: no flicker, no
/// re-scatter, nothing shifts on far cells. A tile that later reloads also won't
/// re-add the grass, because [`scatter_grass`] skips edited cells
/// (`Terrain::column_edited`).
pub(super) fn invalidate_grass_on_edit(
    mut commands: Commands,
    mut edits: MessageReader<crate::edit::BlockEdited>,
    blades: Query<(Entity, &super::spawn::GrassBlade)>,
) {
    // Usually one edit per frame; a small Vec scan is cheaper than a HashSet.
    let edited: Vec<IVec2> = edits.read().map(|e| e.cell).collect();
    if edited.is_empty() {
        return;
    }
    for (entity, blade) in &blades {
        if edited.contains(&blade.cell) {
            commands.entity(entity).despawn();
        }
    }
}

/// Stream the GRASS layer in/out as the player moves. Grass rings a fixed-size
/// player-centered disc ([`grass_ring_tiles`]); it records no blocked cells and
/// gets no collider, so leaving the disc just cascade-despawns the tile parent.
pub(super) fn stream_grass(
    mut commands: Commands,
    assets: Option<Res<FoliageAssets>>,
    terrain: Res<Terrain>,
    mut field: ResMut<GrassField>,
    settings: Res<QualitySettings>,
    player_q: Query<&Transform, With<FollowTarget>>,
) {
    let Some(assets) = assets else {
        return;
    };
    // Grass off when foliage is disabled entirely OR the grass radius is
    // non-positive. Either way, unload any grass that's still live.
    if !settings.foliage_enabled || settings.grass_radius_world <= 0.0 {
        clear_grass_tiles(&mut commands, &mut field);
        return;
    }
    let Ok(player) = player_q.single() else {
        return;
    };
    let center = tile_of(player.translation);
    let ring = grass_ring_tiles(settings.grass_radius_world);

    poll_grass_tasks(&mut commands, &assets, &mut field);
    despawn_grass_out_of_band(&mut commands, &mut field, center, ring);
    start_grass_tasks(&assets, &terrain, &mut field, center, ring);
}

/// Tile coordinate containing a world position (floor-divide by [`TILE`]).
fn tile_of(world: Vec3) -> IVec2 {
    IVec2::new(
        (world.x / TILE as f32).floor() as i32,
        (world.z / TILE as f32).floor() as i32,
    )
}

// ---------------------------------------------------------------------------
// Solid layer
// ---------------------------------------------------------------------------

/// Foliage disabled: unload every solid tile and surrender its claims — both the
/// tall props' [`BlockedCells`] and the low props' [`PropSurfaces`] (releasing
/// each list balances the refcounts this tile took). Pending tasks simply drop
/// (their handle abandons the future, like pathfinding's cancel).
fn clear_solid_tiles(
    commands: &mut Commands,
    field: &mut SolidField,
    blocked: &mut BlockedCells,
    props: &mut PropSurfaces,
) {
    if field.tiles.is_empty() {
        return;
    }
    for (_, state) in field.tiles.drain() {
        if let SolidTileState::Live {
            entity,
            cells,
            prop_cells,
        } = state
        {
            commands.entity(entity).despawn();
            for cell in cells {
                blocked.release(cell);
            }
            for cell in prop_cells {
                props.release(cell);
            }
        }
    }
}

/// When the base world backend switches (Test ↔ Normal ↔ City), despawn ALL
/// streamed foliage so the streamers re-scatter it at the NEW world's surface
/// heights this same frame. Without this, props scattered for the old world linger
/// at stale heights — floating above the flat lab, or buried/floating on the
/// procedural world — because the player often re-enters at the same XZ, so the
/// tiles are still "in range" and never despawned (the "props carry over / float
/// when switching worlds" bug). Foliage only streams in-game, so the very first
/// observation (no previous kind) has nothing to clear — it just records the
/// baseline.
pub(super) fn clear_foliage_on_world_change(
    world_gen: Res<WorldGen>,
    mut last_kind: Local<Option<WorldKind>>,
    mut solid: ResMut<SolidField>,
    mut grass: ResMut<GrassField>,
    mut blocked: ResMut<BlockedCells>,
    mut props: ResMut<PropSurfaces>,
    mut commands: Commands,
) {
    let kind = world_gen.kind();
    if *last_kind == Some(kind) {
        return;
    }
    if last_kind.is_some() {
        clear_solid_tiles(&mut commands, &mut solid, &mut blocked, &mut props);
        clear_grass_tiles(&mut commands, &mut grass);
    }
    *last_kind = Some(kind);
}

/// Solid phase 1: poll in-flight scatter tasks; spawn entities for any that
/// finished, routing each spawned solid prop by its height (see [`is_low_prop`]):
///
/// * **TALL prop** → [`mark_blocked_footprint`] records into [`BlockedCells`]
///   every voxel cell the PLAYER CAPSULE can't occupy because the prop sits there
///   (footprint inflated by `PLAYER_RADIUS`), so the pathfinder routes the whole
///   capsule (not a point) around it. The cell adjacent to a fat tree would clip
///   the trunk with the capsule's edge even though its own center is clear — the
///   inflation is what makes `BlockedCells` mean "cells the capsule center can't
///   reach".
/// * **LOW prop** → [`mark_prop_surface_footprint`] claims into [`PropSurfaces`]
///   the cells the prop actually SITS ON (the *un-inflated* `footprint_radius`, no
///   player-capsule margin — we want a step exactly under the prop, not a no-go
///   ring around it), so physics + A\* fold it into the walkable surface as a
///   1-voxel step the player climbs ONTO instead of routing around.
///
/// Each claim (in either store) is recorded into the tile's matching `Live` list
/// — with duplicates, since two props in the same tile can claim the same cell —
/// so releasing the list on despawn decrements the shared refcount exactly as many
/// times as this tile incremented it, never clearing a cell a neighbouring tile's
/// prop still occupies across the boundary.
fn poll_solid_tasks(
    commands: &mut Commands,
    assets: &FoliageAssets,
    field: &mut SolidField,
    blocked: &mut BlockedCells,
    props: &mut PropSurfaces,
) {
    let mut ready: Vec<(IVec2, Vec<ScatterItem>)> = Vec::new();
    for (tile, state) in field.tiles.iter_mut() {
        if let SolidTileState::Pending(pending) = state {
            if let Some(items) = block_on(poll_once(&mut pending.task)) {
                ready.push((*tile, items));
            }
        }
    }
    for (tile, items) in ready {
        let entity = spawn_tile_entities(commands, assets, tile, &items);
        let mut cells: Vec<IVec2> = Vec::new();
        let mut prop_cells: Vec<IVec2> = Vec::new();
        for item in &items {
            // The solid worker only emits trees/rocks; match exhaustively and
            // skip anything else defensively (grass is never recorded here).
            let size = match item.category {
                ScatterCategory::Tree => assets.trees[item.variant].size,
                ScatterCategory::Rock => assets.rocks[item.variant].size,
                ScatterCategory::Grass => continue,
            };
            // Height routes the prop: a low prop is a climbable step (PropSurfaces,
            // un-inflated footprint, no collider); a tall prop is an obstacle
            // (BlockedCells, inflated footprint, collider — see `spawn_prop`).
            if is_low_prop(size) {
                mark_prop_surface_footprint(props, &mut prop_cells, item.pos, size);
            } else {
                mark_blocked_footprint(blocked, &mut cells, item.pos, size);
            }
        }
        field.tiles.insert(
            tile,
            SolidTileState::Live {
                entity,
                cells,
                prop_cells,
            },
        );
    }
}

/// Claim in [`BlockedCells`] every integer cell `(x, z)` whose center lies
/// within `footprint_radius(size) + PLAYER_RADIUS` of the prop's XZ position, and
/// append EVERY claimed cell to `cells` (duplicates included) so releasing the
/// list on despawn decrements the shared refcount exactly as many times as this
/// tile incremented it. The radius inflation by the player capsule is what makes a
/// blocked cell mean "the capsule center can't sit here" rather than "the prop's
/// own column" — adjacent cells whose centers are clear but within a capsule-width
/// of the trunk are still impassable.
///
/// Recording every claim (not just the first per cell) is what makes the
/// refcount correct: a cell can be inside two props' inflated discs — both in
/// THIS tile (so `cells` carries it twice) and in a neighbouring tile (which
/// records its own claim independently). The shared count only drops to zero —
/// freeing the cell for the pathfinder — once every claiming prop, in any tile,
/// has been released.
///
/// This runs only when a tile spawns (not per frame), so the small scan over the
/// inflated footprint's bounding box stays off the hot path.
fn mark_blocked_footprint(
    blocked: &mut BlockedCells,
    cells: &mut Vec<IVec2>,
    pos: Vec3,
    size: Vec3,
) {
    let reach = footprint_radius(size) + PLAYER_RADIUS;
    let reach_sq = reach * reach;
    // Cell centers sit at integer+0.5; scan the integer cells whose centers can
    // fall within `reach` of the prop (bounding box of the inflated footprint).
    let min_x = (pos.x - reach - 0.5).floor() as i32;
    let max_x = (pos.x + reach - 0.5).ceil() as i32;
    let min_z = (pos.z - reach - 0.5).floor() as i32;
    let max_z = (pos.z + reach - 0.5).ceil() as i32;
    for cx in min_x..=max_x {
        for cz in min_z..=max_z {
            let center_x = cx as f32 + 0.5;
            let center_z = cz as f32 + 0.5;
            let dx = center_x - pos.x;
            let dz = center_z - pos.z;
            if dx * dx + dz * dz <= reach_sq {
                let cell = IVec2::new(cx, cz);
                // One claim per prop disc; record the claim so the tile releases
                // exactly this many on despawn. `claim` increments the refcount
                // unconditionally — duplicates are intentional.
                blocked.claim(cell);
                cells.push(cell);
            }
        }
    }
}

/// Claim in [`PropSurfaces`] every integer cell `(x, z)` whose center lies within
/// the prop's *un-inflated* `footprint_radius(size)` of its XZ position, and append
/// EVERY claimed cell to `prop_cells` (duplicates included) so releasing the list on
/// despawn balances the refcount exactly as many times as this tile incremented it.
///
/// The LOW-prop counterpart to [`mark_blocked_footprint`], with one deliberate
/// difference: there is **no `+ PLAYER_RADIUS`** inflation. A blocked cell means
/// "the capsule center can't sit here", so it inflates by the capsule radius; a
/// *surface* cell means "the player stands one voxel UP here, on the prop", so we
/// want exactly the cells the prop actually covers — inflating would falsely mark a
/// ring of clear ground around a pebble as a step. A low prop carries no collider
/// either (see `spawn_prop`), so the player walks onto it as a single step.
///
/// Like the blocked path, recording every claim (not just the first per cell) keeps
/// the refcount correct when one cell sits under two low props — within this tile
/// (`prop_cells` carries it twice) or across a tile boundary (each tile records its
/// own claim). The cell stays a step until the last claimant releases. Runs only
/// when a tile spawns, so the small footprint scan stays off the hot path.
fn mark_prop_surface_footprint(
    props: &mut PropSurfaces,
    prop_cells: &mut Vec<IVec2>,
    pos: Vec3,
    size: Vec3,
) {
    let reach = footprint_radius(size);
    let reach_sq = reach * reach;
    // Cell centers sit at integer+0.5; scan the integer cells whose centers can
    // fall within `reach` of the prop (bounding box of the un-inflated footprint).
    let min_x = (pos.x - reach - 0.5).floor() as i32;
    let max_x = (pos.x + reach - 0.5).ceil() as i32;
    let min_z = (pos.z - reach - 0.5).floor() as i32;
    let max_z = (pos.z + reach - 0.5).ceil() as i32;
    for cx in min_x..=max_x {
        for cz in min_z..=max_z {
            let center_x = cx as f32 + 0.5;
            let center_z = cz as f32 + 0.5;
            let dx = center_x - pos.x;
            let dz = center_z - pos.z;
            if dx * dx + dz * dz <= reach_sq {
                let cell = IVec2::new(cx, cz);
                // One claim per prop disc; record it so the tile releases exactly
                // this many on despawn. `claim` increments unconditionally —
                // duplicates are intentional (refcount, see doc above).
                props.claim(cell);
                prop_cells.push(cell);
            }
        }
    }
}

/// Solid phase 2: unload tiles outside the wider despawn ring, **budgeted** to
/// [`MAX_SOLID_DESPAWNS_PER_FRAME`] per frame (farthest-out first). Despawning a
/// whole ring-edge row at once on each boundary crossing was a periodic walk
/// hitch that scaled with zoom; spreading it removes the spike. A recursive
/// despawn cascades to every prop under the tile parent, and we release the tile's
/// claims back to the shared stores — both the tall props' [`BlockedCells`] and the
/// low props' [`PropSurfaces`] — so the pathfinder/physics see the cells freed.
fn despawn_solid_out_of_band(
    commands: &mut Commands,
    field: &mut SolidField,
    blocked: &mut BlockedCells,
    props: &mut PropSurfaces,
    center: IVec2,
    despawn_ring: i32,
) {
    let mut out_of_band: Vec<IVec2> = field
        .tiles
        .keys()
        .copied()
        .filter(|t| (t.x - center.x).abs() > despawn_ring || (t.y - center.y).abs() > despawn_ring)
        .collect();
    if out_of_band.is_empty() {
        return;
    }
    // Farthest-out first: those are the least likely to re-enter the band if the
    // player reverses direction, so we never thrash a tile that's about to come
    // back into view.
    out_of_band.sort_by_key(|t| {
        let d = *t - center;
        Reverse(d.x * d.x + d.y * d.y)
    });

    for tile in out_of_band.into_iter().take(MAX_SOLID_DESPAWNS_PER_FRAME) {
        // Pending tasks just drop (the future is abandoned); Live tiles despawn
        // and surrender both their blocked cells and their prop-surface cells.
        if let Some(SolidTileState::Live {
            entity,
            cells,
            prop_cells,
        }) = field.tiles.remove(&tile)
        {
            commands.entity(entity).despawn();
            for cell in cells {
                blocked.release(cell);
            }
            for cell in prop_cells {
                props.release(cell);
            }
        }
    }
}

/// Solid phase 3: start up to [`MAX_SOLID_TASKS_PER_FRAME`] new scatter tasks for
/// the nearest missing tiles in the spawn ring (nearest-to-center first).
fn start_solid_tasks(
    assets: &FoliageAssets,
    terrain: &Terrain,
    field: &mut SolidField,
    center: IVec2,
    spawn_ring: i32,
) {
    let Some(missing) = nearest_missing(center, spawn_ring, |t| field.tiles.contains_key(t)) else {
        return;
    };

    // Snapshot per-variant footprint sizes once; cloned into each task started
    // this frame (bounded to MAX_SOLID_TASKS_PER_FRAME).
    let sizes = SolidVariantSizes {
        trees: assets.trees.iter().map(|v| v.size).collect(),
        rocks: assets.rocks.iter().map(|v| v.size).collect(),
    };

    let pool = AsyncComputeTaskPool::get();
    for tile in missing.into_iter().take(MAX_SOLID_TASKS_PER_FRAME) {
        // Cheap snapshot — `Terrain` is just noise parameters; `sizes` is a few
        // `Vec3`s per category. Both move into the worker.
        let terrain_snapshot: Terrain = terrain.clone();
        let sizes_snapshot = sizes.clone();
        let task =
            pool.spawn(async move { scatter_solid(&terrain_snapshot, tile, &sizes_snapshot) });
        field
            .tiles
            .insert(tile, SolidTileState::Pending(TileScatterTask { task }));
    }
}

// ---------------------------------------------------------------------------
// Grass layer
// ---------------------------------------------------------------------------

/// Grass off (disabled / radius 0): unload every live grass tile.
/// Grass records no blocked cells, so there's nothing to release.
fn clear_grass_tiles(commands: &mut Commands, field: &mut GrassField) {
    if field.tiles.is_empty() {
        return;
    }
    for (_, state) in field.tiles.drain() {
        if let GrassTileState::Live { entity } = state {
            commands.entity(entity).despawn();
        }
    }
}

/// Grass phase 1: poll in-flight grass scatter tasks; spawn the grass meshes for
/// any that finished. No colliders, no blocked cells — the player walks through
/// grass.
fn poll_grass_tasks(commands: &mut Commands, assets: &FoliageAssets, field: &mut GrassField) {
    let mut ready: Vec<(IVec2, Vec<ScatterItem>)> = Vec::new();
    for (tile, state) in field.tiles.iter_mut() {
        if let GrassTileState::Pending(pending) = state {
            if let Some(items) = block_on(poll_once(&mut pending.task)) {
                ready.push((*tile, items));
            }
        }
    }
    for (tile, items) in ready {
        let entity = spawn_tile_entities(commands, assets, tile, &items);
        field.tiles.insert(tile, GrassTileState::Live { entity });
    }
}

/// Grass phase 2: unload grass tiles outside the (tight) grass ring, **budgeted**
/// to [`MAX_GRASS_DESPAWNS_PER_FRAME`] per frame (farthest-out first). The grass
/// disc has no hysteresis band — it's a small player-centered circle, so tiles
/// drop the moment they leave it (the disc just trails the player). Budgeting the
/// count avoids a burst when a step pushes a whole edge row out at once.
fn despawn_grass_out_of_band(
    commands: &mut Commands,
    field: &mut GrassField,
    center: IVec2,
    ring: i32,
) {
    let mut out_of_band: Vec<IVec2> = field
        .tiles
        .keys()
        .copied()
        .filter(|t| (t.x - center.x).abs() > ring || (t.y - center.y).abs() > ring)
        .collect();
    if out_of_band.is_empty() {
        return;
    }
    out_of_band.sort_by_key(|t| {
        let d = *t - center;
        Reverse(d.x * d.x + d.y * d.y)
    });
    for tile in out_of_band.into_iter().take(MAX_GRASS_DESPAWNS_PER_FRAME) {
        if let Some(GrassTileState::Live { entity }) = field.tiles.remove(&tile) {
            commands.entity(entity).despawn();
        }
    }
}

/// Grass phase 3: start up to [`MAX_GRASS_TASKS_PER_FRAME`] new grass scatter
/// tasks for the nearest missing tiles in the grass ring (nearest-to-center
/// first). The worker only needs the grass variant *count* (placement is by
/// index) plus the terrain snapshot — no footprint sizes (grass has no overlap
/// test, no collider).
fn start_grass_tasks(
    assets: &FoliageAssets,
    terrain: &Terrain,
    field: &mut GrassField,
    center: IVec2,
    ring: i32,
) {
    let Some(missing) = nearest_missing(center, ring, |t| field.tiles.contains_key(t)) else {
        return;
    };

    let variant_count = assets.grass.len();
    let pool = AsyncComputeTaskPool::get();
    for tile in missing.into_iter().take(MAX_GRASS_TASKS_PER_FRAME) {
        let terrain_snapshot: Terrain = terrain.clone();
        let task = pool.spawn(async move { scatter_grass(&terrain_snapshot, tile, variant_count) });
        field
            .tiles
            .insert(tile, GrassTileState::Pending(TileScatterTask { task }));
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Collect every tile in the `ring`-radius square around `center` that `present`
/// reports as not yet streamed, sorted nearest-to-center first. Returns `None`
/// when nothing is missing (steady state) so the caller can early-out without an
/// empty-vec allocation on the common path.
///
/// Shared by both layers' `start_*` phases — same round fill order, different
/// fields/rings.
fn nearest_missing(
    center: IVec2,
    ring: i32,
    present: impl Fn(&IVec2) -> bool,
) -> Option<Vec<IVec2>> {
    let mut missing: Vec<IVec2> = Vec::new();
    for dx in -ring..=ring {
        for dz in -ring..=ring {
            let tile = center + IVec2::new(dx, dz);
            if !present(&tile) {
                missing.push(tile);
            }
        }
    }
    if missing.is_empty() {
        return None;
    }
    // Round fill order: nearest tiles first. Allocation is bounded to the ring
    // and only happens while it's filling — steady state has no missing tiles.
    missing.sort_by_key(|t| {
        let d = *t - center;
        d.x * d.x + d.y * d.y
    });
    Some(missing)
}

/// Grass-ring radius in TILES for a given world radius: `ceil(radius / TILE)`,
/// clamped to `>= 0`. The grass disc is therefore the smallest tile square that
/// fully covers the `grass_radius_world` circle around the player — independent of
/// camera zoom. A non-positive radius yields `0` (handled upstream as "no grass").
fn grass_ring_tiles(radius_world: f32) -> i32 {
    if radius_world <= 0.0 {
        return 0;
    }
    (radius_world / TILE as f32).ceil().max(0.0) as i32
}

/// World-XZ ring radius (in tiles) the SOLID streamer should cover for the
/// camera's current orthographic zoom, clamped to `[RING_MIN, quality_ring_max]`.
fn compute_ring(projection: Option<&Projection>, quality_ring_max: i32) -> i32 {
    let max = quality_ring_max.max(RING_MIN);
    let raw = match projection {
        Some(Projection::Orthographic(ortho)) => match ortho.scaling_mode {
            ScalingMode::FixedVertical { viewport_height } => {
                let blocks = viewport_height * RING_ZOOM_COVERAGE;
                let tiles = (blocks / TILE as f32).ceil();
                if tiles.is_finite() {
                    tiles as i32
                } else {
                    RING_FALLBACK
                }
            }
            _ => RING_FALLBACK,
        },
        _ => RING_FALLBACK,
    };
    raw.clamp(RING_MIN, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ortho_proj(vh: f32) -> Projection {
        Projection::Orthographic(bevy::camera::OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: vh,
            },
            ..bevy::camera::OrthographicProjection::default_3d()
        })
    }

    #[test]
    fn compute_ring_falls_back_when_no_camera() {
        assert_eq!(compute_ring(None, 8), RING_FALLBACK);
    }

    #[test]
    fn compute_ring_respects_minimum() {
        let proj = ortho_proj(4.0);
        assert_eq!(compute_ring(Some(&proj), 8), RING_MIN);
    }

    #[test]
    fn compute_ring_respects_quality_cap() {
        let proj = ortho_proj(400.0);
        assert_eq!(compute_ring(Some(&proj), 4), 4);
    }

    #[test]
    fn compute_ring_scales_with_zoom() {
        // blocks = viewport_height * RING_ZOOM_COVERAGE (1.35); tiles = ceil(blocks / TILE).
        // 90 * 1.35 = 121.5 → ceil(121.5/16) = 8, so full zoom-out is covered
        // without the ring being clamped below the view.
        let proj = ortho_proj(90.0);
        assert_eq!(compute_ring(Some(&proj), 8), 8);
        // 44 * 1.35 = 59.4 → ceil(59.4/16) = 4.
        let proj = ortho_proj(44.0);
        assert_eq!(compute_ring(Some(&proj), 8), 4);
    }

    #[test]
    fn grass_ring_zero_when_disabled() {
        // radius 0.0 / negative → no grass ring at all.
        assert_eq!(grass_ring_tiles(0.0), 0);
        assert_eq!(grass_ring_tiles(-5.0), 0);
    }

    #[test]
    fn grass_ring_is_ceil_radius_over_tile() {
        // TILE == 16. ceil(28/16) = 2, ceil(44/16) = 3, ceil(60/16) = 4.
        assert_eq!(grass_ring_tiles(28.0), 2);
        assert_eq!(grass_ring_tiles(44.0), 3);
        assert_eq!(grass_ring_tiles(60.0), 4);
        // Exact multiple stays put: ceil(32/16) = 2.
        assert_eq!(grass_ring_tiles(32.0), 2);
        // Just over a multiple rounds up: ceil(33/16) = 3.
        assert_eq!(grass_ring_tiles(33.0), 3);
    }

    #[test]
    fn grass_ring_is_zoom_independent() {
        // The grass ring depends ONLY on grass_radius_world, never on a camera
        // projection — the whole point of the split layer. Same radius → same
        // ring regardless of any zoom state the solid layer would react to.
        assert_eq!(grass_ring_tiles(44.0), grass_ring_tiles(44.0));
        assert_ne!(grass_ring_tiles(28.0), grass_ring_tiles(60.0));
    }

    /// Whether a tile lies inside the grass ring around `center` (the same
    /// square-radius test [`despawn_grass_out_of_band`] / [`nearest_missing`]
    /// use). Local to the test so we can assert the in-ring/out-of-ring boundary.
    fn in_grass_ring(tile: IVec2, center: IVec2, ring: i32) -> bool {
        (tile.x - center.x).abs() <= ring && (tile.y - center.y).abs() <= ring
    }

    #[test]
    fn grass_tiles_within_radius_are_in_ring_beyond_are_not() {
        // radius 44 → ring 3 tiles. Player at the origin tile.
        let center = IVec2::new(0, 0);
        let ring = grass_ring_tiles(44.0);
        assert_eq!(ring, 3);
        // Inside the ring (within 3 tiles in both axes).
        assert!(in_grass_ring(IVec2::new(0, 0), center, ring));
        assert!(in_grass_ring(IVec2::new(3, 0), center, ring));
        assert!(in_grass_ring(IVec2::new(-3, 3), center, ring));
        // Beyond the ring on either axis.
        assert!(!in_grass_ring(IVec2::new(4, 0), center, ring));
        assert!(!in_grass_ring(IVec2::new(0, -4), center, ring));
    }

    #[test]
    fn tile_of_floor_divides() {
        // Negative coords must floor (not truncate toward zero) so tiles tile
        // the plane without a double-wide row at the origin.
        assert_eq!(tile_of(Vec3::new(0.0, 0.0, 0.0)), IVec2::new(0, 0));
        assert_eq!(tile_of(Vec3::new(15.9, 0.0, 0.1)), IVec2::new(0, 0));
        assert_eq!(tile_of(Vec3::new(16.0, 0.0, 0.0)), IVec2::new(1, 0));
        assert_eq!(tile_of(Vec3::new(-0.1, 0.0, -16.0)), IVec2::new(-1, -1));
    }

    #[test]
    fn low_prop_claims_own_cell_and_release_frees_it() {
        // A small low prop centered on a cell claims (at least) that cell into
        // `PropSurfaces`, and releasing the recorded list balances the refcount so
        // the cell is no longer a step. Mirrors how `poll_solid_tasks` records and
        // `despawn_solid_out_of_band` releases the tile's `prop_cells`.
        let mut props = PropSurfaces::default();
        let mut prop_cells: Vec<IVec2> = Vec::new();
        // Prop centered at cell (3,5)'s center, small footprint (radius 0.25).
        let pos = Vec3::new(3.5, 2.0, 5.5);
        let size = Vec3::new(0.5, 0.6, 0.5);
        mark_prop_surface_footprint(&mut props, &mut prop_cells, pos, size);

        // It must have claimed its own cell as a walkable step.
        let own = IVec2::new(3, 5);
        assert!(props.contains(own), "low prop did not claim its own cell");
        assert_eq!(props.step(own), 1, "claimed cell should read as a 1-step");
        assert!(
            prop_cells.contains(&own),
            "own cell not recorded for release"
        );

        // Releasing exactly the recorded list frees every claim this prop made —
        // the cell is no longer a step (refcount back to zero).
        for cell in &prop_cells {
            props.release(*cell);
        }
        assert!(
            !props.contains(own),
            "releasing the recorded list must free the claimed cell"
        );
        assert_eq!(props.step(own), 0);
    }

    #[test]
    fn low_prop_footprint_is_not_player_inflated() {
        // Unlike `mark_blocked_footprint` (which inflates by PLAYER_RADIUS), the
        // prop-surface footprint is the prop's BARE footprint: a pebble must not
        // mark a whole capsule-width ring of clear ground as a step. With radius
        // 0.25 only the prop's own cell qualifies — its neighbours' centers sit
        // 1.0 away, far outside the disc.
        let mut props = PropSurfaces::default();
        let mut prop_cells: Vec<IVec2> = Vec::new();
        let pos = Vec3::new(3.5, 2.0, 5.5);
        let size = Vec3::new(0.5, 0.6, 0.5);
        mark_prop_surface_footprint(&mut props, &mut prop_cells, pos, size);

        assert_eq!(
            prop_cells,
            vec![IVec2::new(3, 5)],
            "bare footprint should claim only the prop's own cell"
        );
        // A neighbouring cell that PLAYER_RADIUS inflation WOULD have caught stays
        // clear under the un-inflated footprint.
        assert!(!props.contains(IVec2::new(4, 5)));
    }
}
