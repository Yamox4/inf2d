use bevy::prelude::*;
use bevy::platform::collections::HashMap;
use inf2d_core::ChunkPos;
use serde::{Deserialize, Serialize};

/// Maps loaded chunk coordinates to their Bevy entity. Used by every system that needs
/// "is this chunk loaded?" or "give me the entity for chunk X" — including the streaming
/// system itself, render listeners that want to attach children to chunks, etc.
#[derive(Resource, Debug, Default)]
pub struct ChunkManager {
    loaded: HashMap<ChunkPos, Entity>,
}

impl ChunkManager {
    #[inline]
    pub fn get(&self, pos: ChunkPos) -> Option<Entity> {
        self.loaded.get(&pos).copied()
    }

    #[inline]
    pub fn is_loaded(&self, pos: ChunkPos) -> bool {
        self.loaded.contains_key(&pos)
    }

    #[inline]
    pub fn loaded_count(&self) -> usize {
        self.loaded.len()
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (ChunkPos, Entity)> + '_ {
        self.loaded.iter().map(|(p, e)| (*p, *e))
    }

    pub(crate) fn insert(&mut self, pos: ChunkPos, entity: Entity) {
        self.loaded.insert(pos, entity);
    }

    pub(crate) fn remove(&mut self, pos: ChunkPos) -> Option<Entity> {
        self.loaded.remove(&pos)
    }
}

/// Tunables for the streaming window. `load_radius` is the Chebyshev distance in chunks
/// at which we *start* loading at full tile-detail; `hlod_radius` (must be ≥ `load_radius`)
/// is where chunks fall back to a single baked HLOD imposter sprite; `unload_radius`
/// (must be > `hlod_radius`) is where we release them entirely. The hysteresis stops
/// thrashing at each boundary.
#[derive(Resource, Reflect, Debug, Clone, Copy, Serialize, Deserialize)]
#[reflect(Resource)]
pub struct StreamingConfig {
    pub load_radius: i32,
    /// Chunks farther than `load_radius` but within `hlod_radius` render as a single
    /// pre-baked low-resolution sprite (a "Hierarchical LOD imposter") instead of a full
    /// per-tile tilemap. Must satisfy `load_radius ≤ hlod_radius < unload_radius`.
    pub hlod_radius: i32,
    pub unload_radius: i32,
    /// Max chunk-generation tasks **spawned** per frame, so a teleport doesn't queue
    /// thousands of work items in a single tick. Completed tasks promote to entities
    /// the same frame they finish — this cap only limits scheduling rate.
    pub max_loads_per_frame: usize,
    /// Max chunks to despawn per frame.
    pub max_unloads_per_frame: usize,
    /// Hard ceiling on tasks alive at once inside `PendingChunkGenerations`. Prevents
    /// the queue from growing unboundedly when the camera moves faster than the worker
    /// pool can drain it; scheduling stalls until completed tasks pull the in-flight
    /// count back below this number.
    pub max_pending_tasks: usize,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            // Tightened from 3/8/10 to 2/5/7 — full-detail chunks are 25 (5×5)
            // instead of 49, HLOD imposters cover the rest, and the unload ring
            // is small enough that idle entity count stays bounded under ~60k
            // even when every loaded chunk fully streams in.
            load_radius: 2,
            hlod_radius: 5,
            unload_radius: 7,
            max_loads_per_frame: 4,
            max_unloads_per_frame: 8,
            max_pending_tasks: 32,
        }
    }
}
