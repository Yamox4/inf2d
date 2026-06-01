use bevy::prelude::*;
use inf2d_core::{world_to_tile, ChunkPos, WorldTile, HEIGHT_STEP_PX};
use inf2d_world::{ChunkData, ChunkManager};

use crate::rig::CameraRig;

/// Per-frame snapshot of where the cursor sits in the world. All fields are
/// `None` when the cursor is outside the window (or no camera exists yet); when
/// the cursor is over the viewport, all are populated consistently.
#[derive(Resource, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Resource)]
pub struct CursorPick {
    /// Cursor position projected onto the camera's world plane.
    pub world: Option<Vec2>,
    /// The tile that contains [`world`](Self::world), accounting for elevation.
    pub tile: Option<WorldTile>,
    /// The chunk that owns [`tile`](Self::tile).
    pub chunk: Option<ChunkPos>,
    /// Height (in stair-step units) of the tile we resolved against. Exposed so
    /// consumers can render at the correct elevation without re-querying the
    /// chunk. `Some(0)` for ground-plane / fallback resolves.
    pub height: Option<i32>,
}

/// Highest height the resolver will probe before giving up. Must cover whatever
/// `BiomeParams::max_height_steps` ends up being in worldgen plus a small
/// margin. The loop is cheap — even 32 candidates is a handful of `HashMap`
/// lookups per frame.
pub const MAX_PICK_HEIGHT: i32 = 24;

/// Project the OS cursor through the camera rig's viewport into world space,
/// then resolve the **elevated** tile that visually owns the cursor pixel.
///
/// Iso silhouettes mean a single screen point can project onto multiple tiles at
/// different heights. We iterate `MAX_PICK_HEIGHT..=0` (high → low): for each
/// candidate `h`, the tile whose elevated diamond sits at world point `c` is the
/// one whose ground-plane projection is `c - (0, h * HEIGHT_STEP_PX)`. If that
/// tile's stored height equals `h`, we picked it. Top-down iteration means the
/// topmost visible tile wins when several heights collide on the same pixel —
/// which matches what the player sees.
///
/// Falls through to a height `-1` probe (recessed water tiles) and finally a
/// ground-plane pick, so the highlight always has something to draw rather than
/// silently going `None` whenever a chunk is mid-stream.
pub fn update_cursor_pick(
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<CameraRig>>,
    manager: Res<ChunkManager>,
    chunks: Query<&ChunkData>,
    mut pick: ResMut<CursorPick>,
) {
    // Reset on failure so a previous-frame stale pick doesn't linger.
    let Ok(window) = windows.single() else {
        *pick = CursorPick::default();
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        *pick = CursorPick::default();
        return;
    };
    let Ok((camera, cam_xform)) = cameras.single() else {
        *pick = CursorPick::default();
        return;
    };
    let Ok(world) = camera.viewport_to_world_2d(cam_xform, cursor) else {
        *pick = CursorPick::default();
        return;
    };

    // Helper: probe a single height. Returns the resolved tile + chunk if a
    // chunk is loaded there AND its stored height matches `h`.
    let probe = |h: i32| -> Option<(WorldTile, ChunkPos, i32)> {
        let unprojected = world - Vec2::new(0.0, h as f32 * HEIGHT_STEP_PX);
        let candidate = world_to_tile(unprojected);
        let chunk_pos = ChunkPos::from_tile(candidate);
        let entity = manager.get(chunk_pos)?;
        let data = chunks.get(entity).ok()?;
        let local = chunk_pos.local_of(candidate);
        let stored_h = data.get(local).height as i32;
        (stored_h == h).then_some((candidate, chunk_pos, h))
    };

    // Iterate heights top-down so the topmost visible tile wins ties.
    let mut resolved: Option<(WorldTile, ChunkPos, i32)> = None;
    for h in (0..=MAX_PICK_HEIGHT).rev() {
        if let Some(hit) = probe(h) {
            resolved = Some(hit);
            break;
        }
    }

    // Also try the negative side for recessed water (`height = -1`).
    if resolved.is_none() {
        resolved = probe(-1);
    }

    // Last-ditch fallback: ground-plane pick so the highlight still draws even
    // when the cursor is over un-streamed chunks or off-grid background.
    if resolved.is_none() {
        let candidate = world_to_tile(world);
        resolved = Some((candidate, ChunkPos::from_tile(candidate), 0));
    }

    // The fallback above guarantees `Some`, but destructure with `let-else` to
    // keep the codebase free of `unwrap()` in non-test code.
    let Some((tile, chunk, height)) = resolved else {
        *pick = CursorPick::default();
        return;
    };

    pick.world = Some(world);
    pick.tile = Some(tile);
    pick.chunk = Some(chunk);
    pick.height = Some(height);
}
