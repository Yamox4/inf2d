//! The streaming system: ring foliage tiles in/out as the player moves.
//!
//! [`stream_foliage`] runs every frame and orchestrates four steps, each a
//! focused helper below:
//!
//! 1. [`clear_all_tiles`] — bail out + unload everything when foliage is off.
//! 2. [`poll_finished_tasks`] — spawn entities for any scatter task that resolved.
//! 3. [`despawn_out_of_band`] — unload tiles outside the (wider) despawn ring.
//! 4. [`start_missing_tasks`] — start up to [`MAX_TILE_TASKS_PER_FRAME`] new
//!    scatter tasks for the nearest missing tiles inside the spawn ring.
//!
//! The despawn ring is a **hysteresis band** wider than the spawn ring
//! ([`DESPAWN_RING_MARGIN`] extra tiles), so on the wide orthographic-iso view
//! props don't pop out the moment the camera nudges or zooms — tiles only unload
//! well outside the visible area. Past [`QualitySettings::foliage_lod_distance`]
//! a tile is streamed as a cheap LOD (grass skipped, only sparse solid props).

use bevy::camera::{Projection, ScalingMode};
use bevy::prelude::*;
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool};

use inf3d_camera::IsoCamera;
use inf3d_core::{BlockedCells, FollowTarget, QualitySettings};
use inf3d_worldgen::Terrain;

use super::scatter::scatter_tile;
use super::spawn::spawn_tile_entities;
use super::{
    FoliageAssets, FoliageField, ScatterCategory, ScatterItem, TileScatterTask, TileState,
    VariantSizes, TILE,
};

/// Minimum ring radius the streamer ever uses, regardless of zoom level.
const RING_MIN: i32 = 2;
/// Fallback ring radius used when the camera entity hasn't spawned yet (or
/// isn't orthographic).
const RING_FALLBACK: i32 = 3;
/// Multiplier from the camera's orthographic `viewport_height` to the
/// world-XZ radius the foliage ring needs to cover. Generous (> the literal
/// half-height) so the spawn ring already covers a margin around the viewport.
const RING_ZOOM_COVERAGE: f32 = 1.1;
/// Extra tiles the *despawn* ring extends past the *spawn* ring (hysteresis).
const DESPAWN_RING_MARGIN: i32 = 2;
/// Maximum number of tile scatter tasks STARTED per frame. Bounding the starts
/// spreads a big ring-fill (first spawn / zoom-out) over several frames instead
/// of flooding the task pool and spawning everything in one stall.
const MAX_TILE_TASKS_PER_FRAME: usize = 3;

/// Stream foliage tiles in/out as the player moves. See the module doc for the
/// per-frame phase breakdown.
pub(super) fn stream_foliage(
    mut commands: Commands,
    assets: Option<Res<FoliageAssets>>,
    terrain: Res<Terrain>,
    mut field: ResMut<FoliageField>,
    mut blocked: ResMut<BlockedCells>,
    settings: Res<QualitySettings>,
    player_q: Query<&Transform, With<FollowTarget>>,
    camera_q: Query<(&Projection, &GlobalTransform), With<IsoCamera>>,
) {
    let Some(assets) = assets else {
        return;
    };
    if !settings.foliage_enabled {
        clear_all_tiles(&mut commands, &mut field, &mut blocked);
        return;
    }
    let Ok(player) = player_q.single() else {
        return;
    };
    let center = IVec2::new(
        (player.translation.x / TILE as f32).floor() as i32,
        (player.translation.z / TILE as f32).floor() as i32,
    );

    let camera = camera_q.single().ok();
    let spawn_ring = compute_ring(camera.map(|(p, _)| p), settings.foliage_ring_max);
    let despawn_ring = spawn_ring + DESPAWN_RING_MARGIN;
    let cam_pos = camera.map(|(_, gt)| gt.translation());

    poll_finished_tasks(&mut commands, &assets, &mut field, &mut blocked);
    despawn_out_of_band(&mut commands, &mut field, &mut blocked, center, despawn_ring);
    start_missing_tasks(&assets, &terrain, &mut field, &settings, center, spawn_ring, cam_pos);
}

/// Foliage disabled: unload every tile and surrender its blocked cells. Pending
/// tasks simply drop (their handle abandons the future, like pathfinding's cancel).
fn clear_all_tiles(commands: &mut Commands, field: &mut FoliageField, blocked: &mut BlockedCells) {
    if field.tiles.is_empty() {
        return;
    }
    for (_, state) in field.tiles.drain() {
        if let TileState::Live(entity, cells) = state {
            commands.entity(entity).despawn();
            for cell in cells {
                blocked.0.remove(&cell);
            }
        }
    }
}

/// Phase 1: poll in-flight scatter tasks; spawn entities for any that finished,
/// recording the voxel cells their SOLID props occupy into [`BlockedCells`] so
/// the pathfinder routes around them.
fn poll_finished_tasks(
    commands: &mut Commands,
    assets: &FoliageAssets,
    field: &mut FoliageField,
    blocked: &mut BlockedCells,
) {
    let mut ready: Vec<(IVec2, Vec<ScatterItem>)> = Vec::new();
    for (tile, state) in field.tiles.iter_mut() {
        if let TileState::Pending(pending) = state {
            if let Some(items) = block_on(poll_once(&mut pending.task)) {
                ready.push((*tile, items));
            }
        }
    }
    for (tile, items) in ready {
        let entity = spawn_tile_entities(commands, assets, tile, &items);
        let mut cells: Vec<IVec2> = Vec::new();
        for item in &items {
            if matches!(item.category, ScatterCategory::Tree | ScatterCategory::Rock) {
                let cell = IVec2::new(item.pos.x.floor() as i32, item.pos.z.floor() as i32);
                if blocked.0.insert(cell) {
                    cells.push(cell);
                }
            }
        }
        field.tiles.insert(tile, TileState::Live(entity, cells));
    }
}

/// Phase 2: despawn tiles outside the wider despawn ring (recursive despawn
/// cascades to every prop under the tile parent) and release their blocked cells.
fn despawn_out_of_band(
    commands: &mut Commands,
    field: &mut FoliageField,
    blocked: &mut BlockedCells,
    center: IVec2,
    despawn_ring: i32,
) {
    field.tiles.retain(|tile, state| {
        let in_band = (tile.x - center.x).abs() <= despawn_ring
            && (tile.y - center.y).abs() <= despawn_ring;
        if !in_band {
            if let TileState::Live(entity, cells) = state {
                commands.entity(*entity).despawn();
                for cell in cells.iter() {
                    blocked.0.remove(cell);
                }
            }
            // Pending tasks outside the band just drop here.
        }
        in_band
    });
}

/// Phase 3: start up to [`MAX_TILE_TASKS_PER_FRAME`] new scatter tasks for the
/// nearest missing tiles in the spawn ring (nearest-to-center first).
fn start_missing_tasks(
    assets: &FoliageAssets,
    terrain: &Terrain,
    field: &mut FoliageField,
    settings: &QualitySettings,
    center: IVec2,
    spawn_ring: i32,
    cam_pos: Option<Vec3>,
) {
    let mut missing: Vec<IVec2> = Vec::new();
    for dx in -spawn_ring..=spawn_ring {
        for dz in -spawn_ring..=spawn_ring {
            let tile = center + IVec2::new(dx, dz);
            if !field.tiles.contains_key(&tile) {
                missing.push(tile);
            }
        }
    }
    if missing.is_empty() {
        return;
    }
    // Round fill order: nearest tiles first. Allocation is bounded to the ring
    // and only happens while it's filling — steady state has no missing tiles.
    missing.sort_by_key(|t| {
        let d = *t - center;
        d.x * d.x + d.y * d.y
    });

    // Snapshot per-variant footprint sizes once; cloned into each task started
    // this frame (bounded to MAX_TILE_TASKS_PER_FRAME).
    let sizes = VariantSizes {
        trees: assets.trees.iter().map(|v| v.size).collect(),
        rocks: assets.rocks.iter().map(|v| v.size).collect(),
        grass: assets.grass.iter().map(|v| v.size).collect(),
    };

    let pool = AsyncComputeTaskPool::get();
    for tile in missing.into_iter().take(MAX_TILE_TASKS_PER_FRAME) {
        let cheap_lod = tile_is_far(tile, cam_pos, settings.foliage_lod_distance);
        // Cheap snapshot — `Terrain` is just noise parameters; `sizes` is a few
        // `Vec3`s per category. Both move into the worker.
        let terrain_snapshot: Terrain = terrain.clone();
        let sizes_snapshot = sizes.clone();
        let task =
            pool.spawn(async move { scatter_tile(&terrain_snapshot, tile, &sizes_snapshot, cheap_lod) });
        field
            .tiles
            .insert(tile, TileState::Pending(TileScatterTask { task }));
    }
}

/// Whether a tile's center lies past `lod_distance` from the camera (so it
/// should stream as the cheap, grass-free LOD). A tile's props all live within
/// ~`TILE` of its center, so a per-tile cull is plenty granular for the iso view.
fn tile_is_far(tile: IVec2, cam_pos: Option<Vec3>, lod_distance: f32) -> bool {
    let Some(cp) = cam_pos else {
        return false;
    };
    if lod_distance <= 0.0 {
        return false;
    }
    let tile_center = Vec2::new(
        (tile.x * TILE) as f32 + TILE as f32 * 0.5,
        (tile.y * TILE) as f32 + TILE as f32 * 0.5,
    );
    let dx = tile_center.x - cp.x;
    let dz = tile_center.y - cp.z;
    dx * dx + dz * dz > lod_distance * lod_distance
}

/// World-XZ ring radius (in tiles) the foliage streamer should cover for the
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
        // blocks = viewport_height * RING_ZOOM_COVERAGE (1.1); tiles = ceil(blocks / TILE).
        // 90 * 1.1 = 99 → ceil(99/16) = 7.
        let proj = ortho_proj(90.0);
        assert_eq!(compute_ring(Some(&proj), 8), 7);
        // 44 * 1.1 = 48.4 → ceil(48.4/16) = 4.
        let proj = ortho_proj(44.0);
        assert_eq!(compute_ring(Some(&proj), 8), 4);
    }
}
