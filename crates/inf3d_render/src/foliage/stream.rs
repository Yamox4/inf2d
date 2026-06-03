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
//! well outside the visible area. A tile streams as a cheap, grass-free LOD when
//! it is either past [`QualitySettings::foliage_lod_distance`] from the camera OR
//! outside [`QualitySettings::grass_radius_world`] from the **player** (the dense
//! grass carpet is capped to a fixed world circle around the player so zooming
//! out — which enlarges the ring — can't blow up the grass count). Trees and
//! rocks ignore the cheap LOD and still fill the ring to the iso-view edges.

use std::cmp::Reverse;

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
/// Maximum number of tiles UNLOADED per frame. A tile-boundary crossing pushes a
/// whole ring-edge row out of band at once; despawning all of it in one frame is
/// a periodic walk hitch that gets worse the further you zoom out (larger ring →
/// longer edge). Budgeting the unloads spreads that cost over a few frames.
/// Out-of-band tiles already sit well outside the view (hysteresis margin), so
/// letting a few linger an extra frame or two is invisible.
const MAX_TILE_DESPAWNS_PER_FRAME: usize = 4;

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
    start_missing_tasks(
        &assets,
        &terrain,
        &mut field,
        &settings,
        center,
        spawn_ring,
        cam_pos,
        player.translation,
    );
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

/// Phase 2: unload tiles outside the wider despawn ring, **budgeted** to
/// [`MAX_TILE_DESPAWNS_PER_FRAME`] per frame (farthest-out first). Despawning a
/// whole ring-edge row at once on each boundary crossing was a periodic walk
/// hitch that scaled with zoom; spreading it removes the spike. A recursive
/// despawn cascades to every prop under the tile parent, and we release the
/// tile's blocked cells back to the pathfinder.
fn despawn_out_of_band(
    commands: &mut Commands,
    field: &mut FoliageField,
    blocked: &mut BlockedCells,
    center: IVec2,
    despawn_ring: i32,
) {
    let mut out_of_band: Vec<IVec2> = field
        .tiles
        .keys()
        .copied()
        .filter(|t| {
            (t.x - center.x).abs() > despawn_ring || (t.y - center.y).abs() > despawn_ring
        })
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

    for tile in out_of_band.into_iter().take(MAX_TILE_DESPAWNS_PER_FRAME) {
        // Pending tasks just drop (the future is abandoned); Live tiles despawn
        // and surrender their blocked cells.
        if let Some(TileState::Live(entity, cells)) = field.tiles.remove(&tile) {
            commands.entity(entity).despawn();
            for cell in cells {
                blocked.0.remove(&cell);
            }
        }
    }
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
    player_pos: Vec3,
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
        // A tile streams grass-free (cheap LOD) when it is either past the
        // camera-relative `foliage_lod_distance` OR outside the player-relative
        // `grass_radius_world`. The radius cap is the zoom-out brake: zooming out
        // grows the ring (more tiles, hence more trees/rocks to the iso edges),
        // but grass stays bounded to a fixed world circle around the player.
        let cheap_lod = tile_is_far(tile, cam_pos, settings.foliage_lod_distance)
            || tile_outside_grass_radius(tile, player_pos, settings.grass_radius_world);
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

/// Whether a tile lies entirely outside `grass_radius` (world units) of the
/// player, so it should stream grass-free. The dense grass carpet is capped to
/// this fixed world circle around the player regardless of zoom — the brake that
/// stops grass count (and frame-time) from exploding as zoom-out enlarges the
/// ring. We test the player's distance to the tile's nearest XZ point (not its
/// center) so grass fills right up to the radius edge instead of vanishing a
/// half-tile early. `radius <= 0.0` disables grass entirely (Potato preset).
fn tile_outside_grass_radius(tile: IVec2, player_pos: Vec3, radius: f32) -> bool {
    if radius <= 0.0 {
        return true;
    }
    // Tile XZ bounds [min, max) in world units.
    let min_x = (tile.x * TILE) as f32;
    let min_z = (tile.y * TILE) as f32;
    let max_x = min_x + TILE as f32;
    let max_z = min_z + TILE as f32;
    // Nearest point of the tile's XZ rectangle to the player, then its distance.
    let nx = player_pos.x.clamp(min_x, max_x);
    let nz = player_pos.z.clamp(min_z, max_z);
    let dx = nx - player_pos.x;
    let dz = nz - player_pos.z;
    dx * dx + dz * dz > radius * radius
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
    fn grass_radius_zero_caps_all_tiles() {
        // radius 0.0 (Potato) → every tile is grass-free, even the player's own.
        let player = Vec3::new(8.0, 0.0, 8.0);
        assert!(tile_outside_grass_radius(IVec2::new(0, 0), player, 0.0));
        assert!(tile_outside_grass_radius(IVec2::new(5, 5), player, 0.0));
    }

    #[test]
    fn grass_radius_caps_far_tiles_only() {
        // Player at the origin tile; a small radius keeps the home tile (nearest
        // point is the player itself, distance 0) but drops a distant one.
        let player = Vec3::new(0.0, 0.0, 0.0);
        let radius = 28.0;
        assert!(!tile_outside_grass_radius(IVec2::new(0, 0), player, radius));
        // Tile (4,0): nearest XZ x = 64 (4*16), dx = 64 > 28 → outside.
        assert!(tile_outside_grass_radius(IVec2::new(4, 0), player, radius));
    }

    #[test]
    fn grass_radius_uses_nearest_tile_point() {
        // Player near a tile boundary: an adjacent tile whose nearest edge is
        // within the radius still gets grass even though its center is farther.
        let player = Vec3::new(15.0, 0.0, 8.0);
        // Tile (1,0) spans x ∈ [16, 32): nearest x = 16, dx = 1 ≤ radius.
        assert!(!tile_outside_grass_radius(IVec2::new(1, 0), player, 4.0));
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
