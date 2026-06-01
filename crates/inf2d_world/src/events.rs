use bevy::prelude::*;
use inf2d_core::ChunkPos;

/// Emitted in the same `Update` tick where a chunk's entity is spawned and its
/// `ChunkData` resolved. Render and physics layers listen and react (build
/// tilemap, build collider, ...).
#[derive(Message, Debug, Clone, Copy)]
pub struct ChunkLoaded {
    pub pos: ChunkPos,
    pub entity: Entity,
}

/// Emitted just before a chunk entity is despawned. Render/physics layers
/// should use this to release per-chunk GPU resources or colliders attached as
/// children.
#[derive(Message, Debug, Clone, Copy)]
pub struct ChunkUnloaded {
    pub pos: ChunkPos,
    pub entity: Entity,
}
