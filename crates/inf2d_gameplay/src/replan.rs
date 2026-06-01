//! Pathfinding replan in response to chunk streaming.
//!
//! The world streams chunks in and out around the camera. A path resolved a few
//! frames ago can become stale in two ways:
//!
//! 1. A chunk that the path crosses has been **unloaded** — the visible terrain
//!    is gone, and the walker should not blindly follow ghost waypoints into
//!    fog-of-war.
//! 2. A chunk that the path crosses has just **loaded** — what was previously
//!    optimistic "unloaded = walkable" fog may now be a solid wall or cliff,
//!    and we may want a shorter / different route.
//!
//! For each chunk lifecycle event we walk every `MoveTarget` and re-issue a
//! [`PathRequest`] from the entity's *current* logical tile to its *original*
//! goal (stored on the target when the first path was applied — see
//! [`crate::apply_paths`]). The new path then replaces the old one on the next
//! tick when the [`PathFound`] response comes back.
//!
//! ## Goal-chunk unloaded
//!
//! If the goal's own chunk is currently unloaded, issuing a fresh request would
//! either fail (no walkable goal) or, given the optimistic walkability policy,
//! produce a path through unknown terrain that we can't validate. We instead
//! park the entity: clear its [`MoveTarget::path`] and tag it with
//! [`PendingReplan`]. Subsequent [`ChunkLoaded`] events will retry until the
//! goal chunk streams back in.
//!
//! ## Ordering
//!
//! The system runs in `SimulationSet` **before** [`crate::apply_paths`] so any
//! [`PathRequest`] written this frame is solved by the pathfinder later in the
//! same simulation tick and applied without a frame of visual lag.

use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use inf2d_core::{ChunkPos, WorldTile};
use inf2d_pathfinding::PathRequest;
use inf2d_world::{ChunkLoaded, ChunkManager, ChunkUnloaded};

use crate::{MoveTarget, Player};

/// Iteration budget for replan requests. Matches the budget used by
/// [`crate::handle_click_to_move`] so a replan can't fail where the original
/// click could have succeeded.
const REPLAN_MAX_ITERATIONS: usize = 5000;

/// Marker placed on an entity whose path was invalidated by a chunk event but
/// whose goal chunk is currently unloaded. While present, the replan system
/// will retry on every subsequent chunk event until the goal chunk streams
/// back in and a request can be issued.
///
/// Removed automatically once a new [`PathRequest`] is dispatched.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct PendingReplan;

/// Inspect every chunk lifecycle event this frame and replan paths whose
/// waypoints (or goal) intersect a changed chunk.
///
/// See the module docs for the full policy. In short:
///
/// - Collect the set of all chunks that were loaded or unloaded this frame.
/// - For each [`MoveTarget`] with a stored goal, replan if any waypoint sits in
///   a changed chunk, if the goal's chunk is itself a changed chunk, or if the
///   entity was already parked with [`PendingReplan`] and one of the loaded
///   chunks is its goal chunk.
/// - If the goal chunk is currently unloaded at replan time, clear the path
///   and (re)apply [`PendingReplan`]. Otherwise dispatch a fresh
///   [`PathRequest`] from `Player::current_tile` to the original goal and
///   remove [`PendingReplan`] if present.
pub fn replan_paths_on_chunk_change(
    mut commands: Commands,
    mut loaded: MessageReader<ChunkLoaded>,
    mut unloaded: MessageReader<ChunkUnloaded>,
    manager: Res<ChunkManager>,
    mut targets: Query<(
        Entity,
        &Player,
        &mut MoveTarget,
        Option<&PendingReplan>,
    )>,
    mut requests: MessageWriter<PathRequest>,
) {
    let mut changed: HashSet<ChunkPos> = HashSet::default();
    let mut loaded_set: HashSet<ChunkPos> = HashSet::default();
    for ev in loaded.read() {
        changed.insert(ev.pos);
        loaded_set.insert(ev.pos);
    }
    for ev in unloaded.read() {
        changed.insert(ev.pos);
    }
    if changed.is_empty() {
        return;
    }

    for (entity, player, mut target, pending) in &mut targets {
        let Some(goal) = target.goal else {
            // No active travel intent — nothing to replan.
            continue;
        };
        let goal_chunk = ChunkPos::from_tile(goal.0);

        let path_intersects = target
            .path
            .iter()
            .any(|(tile, _)| changed.contains(&ChunkPos::from_tile(*tile)));
        let goal_in_changed = changed.contains(&goal_chunk);
        // A parked entity (PendingReplan) has an empty path; only the goal
        // chunk re-loading can unblock it, so gate retries on that specifically.
        let pending_unblocked = pending.is_some() && loaded_set.contains(&goal_chunk);

        if !path_intersects && !goal_in_changed && !pending_unblocked {
            continue;
        }

        if !manager.is_loaded(goal_chunk) {
            // Goal is in fog — clear the active path and wait. Don't drop the
            // stored goal: it's the seed for the eventual retry once the goal
            // chunk streams in.
            target.path.clear();
            target.velocity = Vec2::ZERO;
            if pending.is_none() {
                commands.entity(entity).insert(PendingReplan);
            }
            continue;
        }

        dispatch_replan(entity, player, &goal.0, &mut requests);
        if pending.is_some() {
            commands.entity(entity).remove::<PendingReplan>();
        }
    }
}

/// Helper: emit a single replan [`PathRequest`] from the player's current
/// logical tile to the supplied goal. Pulled out so the system body stays
/// readable and so the request shape is defined in exactly one place.
fn dispatch_replan(
    entity: Entity,
    player: &Player,
    goal: &WorldTile,
    requests: &mut MessageWriter<PathRequest>,
) {
    requests.write(PathRequest {
        requester: entity,
        start: player.current_tile,
        goal: *goal,
        max_iterations: REPLAN_MAX_ITERATIONS,
    });
}
