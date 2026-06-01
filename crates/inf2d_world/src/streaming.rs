use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::tasks::futures_lite::future;
use bevy::tasks::{AsyncComputeTaskPool, Task};
use inf2d_core::{world_to_tile, ChunkPos};
use serde::{Deserialize, Serialize};

use crate::chunk::{ChunkBundle, ChunkData};
use crate::events::{ChunkLoaded, ChunkUnloaded};
use crate::generator::ActiveGenerator;
use crate::manager::{ChunkManager, StreamingConfig};

/// Ordered set the chunk streaming systems run inside. Sits inside
/// [`inf2d_core::SimulationSet`]. Render/physics listeners that react to chunk events
/// should run after this set in the same frame.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub struct ChunkStreamSet;

/// The chunk the streamer treats as "you are here". The camera plugin writes to this each
/// frame from the camera transform. Decoupling streaming from any specific camera entity
/// keeps the world crate free of camera assumptions.
#[derive(Resource, Reflect, Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[reflect(Resource)]
pub struct CameraFocus {
    pub chunk: ChunkPos,
    pub world: Vec2,
}

/// Marker on a Bevy entity carrying a `Transform` whose `translation.xy()` is the streamer's
/// focal point. The camera plugin tags its main rig entity with this so we don't have to
/// hard-code a `Camera2d` lookup here.
#[derive(Component, Default)]
pub struct ChunkStreamFocus;

/// In-flight chunk-generation work queued on the [`AsyncComputeTaskPool`]. Keyed by
/// `ChunkPos` so the scheduler can dedupe and the collector can match completed tasks
/// back to their target chunk.
///
/// The map is bounded by `StreamingConfig::max_pending_tasks`. Dropping an entry cancels
/// the underlying task — used when a chunk has left the streaming window before its
/// generator finished.
#[derive(Resource, Default)]
pub struct PendingChunkGenerations {
    pub tasks: HashMap<ChunkPos, Task<ChunkData>>,
}

impl PendingChunkGenerations {
    /// How many tasks are currently scheduled (alive or finished but not yet collected).
    #[inline]
    pub fn in_flight(&self) -> usize {
        self.tasks.len()
    }
}

/// Recompute `CameraFocus` from whichever entity carries `ChunkStreamFocus`. If none exists
/// (e.g. very early frames), focus stays at the origin and chunks load around `(0, 0)`.
pub fn update_camera_focus(
    mut focus: ResMut<CameraFocus>,
    q: Query<&GlobalTransform, With<ChunkStreamFocus>>,
) {
    if let Ok(t) = q.single() {
        let world = t.translation().truncate();
        focus.world = world;
        focus.chunk = ChunkPos::from_tile(world_to_tile(world));
    }
}

/// Discover chunks inside the spawn window (the larger of `load_radius` and `hlod_radius`,
/// so HLOD imposters also have `ChunkData` to bake from) that are neither loaded nor
/// already being generated, and spawn an [`AsyncComputeTaskPool`] task for each
/// (closest-first). Bounded per-frame by `max_loads_per_frame` and overall by
/// `max_pending_tasks`.
///
/// The closure captures only an `Arc<dyn Generator>` and a `Copy` `ChunkPos`, both `Send`,
/// so no main-thread state crosses into the worker.
pub fn schedule_chunk_generations(
    focus: Res<CameraFocus>,
    cfg: Res<StreamingConfig>,
    generator: Res<ActiveGenerator>,
    manager: Res<ChunkManager>,
    mut pending: ResMut<PendingChunkGenerations>,
) {
    // Spawn radius is the larger of `load_radius` and `hlod_radius` so HLOD chunks have
    // a `ChunkData` available for the renderer to bake against.
    let spawn_radius = cfg.load_radius.max(cfg.hlod_radius);
    let center = focus.chunk;

    // How many new tasks we may spawn this frame, after honoring the global in-flight
    // ceiling. Saturating sub avoids underflow when the cap was lowered at runtime.
    let pending_headroom = cfg.max_pending_tasks.saturating_sub(pending.in_flight());
    let frame_budget = cfg.max_loads_per_frame.min(pending_headroom);
    if frame_budget == 0 {
        return;
    }

    let mut candidates: Vec<ChunkPos> =
        Vec::with_capacity(((spawn_radius * 2 + 1) * (spawn_radius * 2 + 1)) as usize);
    for dy in -spawn_radius..=spawn_radius {
        for dx in -spawn_radius..=spawn_radius {
            let pos = ChunkPos::new(center.x + dx, center.y + dy);
            if !manager.is_loaded(pos) && !pending.tasks.contains_key(&pos) {
                candidates.push(pos);
            }
        }
    }
    // Prefer chunks closest to focus, so the world appears around the player first.
    candidates.sort_by_key(|p| p.chebyshev_distance(center));
    candidates.truncate(frame_budget);

    if candidates.is_empty() {
        return;
    }

    let pool = AsyncComputeTaskPool::get();
    for pos in candidates {
        let generator_handle = generator.shared();
        let pos_copy = pos;
        let task = pool.spawn(async move { generator_handle.generate(pos_copy) });
        pending.tasks.insert(pos, task);
        tracing::debug!("async chunk task for {:?} scheduled", pos);
    }
}

/// Promote every completed chunk-generation task into a real chunk entity. Pure
/// non-blocking polls (`poll_once`) — if nothing finished this frame, the system is a
/// cheap walk of the map. No per-frame cap: promotion is cheap (one entity spawn +
/// one event); the rate limiter is `schedule_chunk_generations`.
pub fn collect_completed_generations(
    mut commands: Commands,
    mut pending: ResMut<PendingChunkGenerations>,
    mut manager: ResMut<ChunkManager>,
    mut loaded_events: MessageWriter<ChunkLoaded>,
) {
    if pending.tasks.is_empty() {
        return;
    }

    let completed: Vec<(ChunkPos, ChunkData)> = pending
        .tasks
        .iter_mut()
        .filter_map(|(pos, task)| {
            future::block_on(future::poll_once(task)).map(|data| (*pos, data))
        })
        .collect();

    for (pos, data) in completed {
        pending.tasks.remove(&pos);
        let entity = commands.spawn(ChunkBundle::new(pos, data)).id();
        manager.insert(pos, entity);
        loaded_events.write(ChunkLoaded { pos, entity });
        tracing::debug!("async chunk task for {:?} completed", pos);
    }
}

/// Despawn chunks past `unload_radius`, capped by `max_unloads_per_frame`. Also drops any
/// pending generation task for a chunk that has left the streaming window — dropping the
/// `Task` cancels it; any work already mid-flight produces a `ChunkData` discarded when
/// its handle goes away.
pub fn unload_distant_chunks(
    mut commands: Commands,
    focus: Res<CameraFocus>,
    cfg: Res<StreamingConfig>,
    mut manager: ResMut<ChunkManager>,
    mut pending: ResMut<PendingChunkGenerations>,
    mut unloaded_events: MessageWriter<ChunkUnloaded>,
) {
    let center = focus.chunk;

    // Cancel pending tasks that are now beyond the unload radius. Dropping the `Task`
    // cancels it; if it had already completed, the produced data is simply discarded.
    let pending_cancel: Vec<ChunkPos> = pending
        .tasks
        .keys()
        .copied()
        .filter(|p| p.chebyshev_distance(center) > cfg.unload_radius)
        .collect();
    for pos in pending_cancel {
        pending.tasks.remove(&pos);
        tracing::debug!("async chunk task for {:?} cancelled (out of range)", pos);
    }

    let mut to_unload: Vec<(ChunkPos, Entity)> = manager
        .iter()
        .filter(|(p, _)| p.chebyshev_distance(center) > cfg.unload_radius)
        .collect();
    to_unload.sort_by_key(|(p, _)| -p.chebyshev_distance(center));
    to_unload.truncate(cfg.max_unloads_per_frame);

    for (pos, entity) in to_unload {
        unloaded_events.write(ChunkUnloaded { pos, entity });
        manager.remove(pos);
        commands.entity(entity).despawn();
    }
}
