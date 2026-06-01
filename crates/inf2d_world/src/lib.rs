#![deny(unsafe_code)]
//! Chunked infinite tile world.
//!
//! - [`Tile`] / [`TileKind`] describe a single tile.
//! - [`ChunkData`] is the dense `CHUNK_SIZE * CHUNK_SIZE` tile grid owned by one chunk.
//! - [`Chunk`] is the marker component on the Bevy entity for a chunk.
//! - [`ChunkManager`] tracks `ChunkPos → Entity` for loaded chunks.
//! - [`Generator`] is the trait that produces a `ChunkData` for a `ChunkPos`; implement
//!   it in another crate (`inf2d_worldgen`) and register the result via [`ActiveGenerator`].
//! - [`WorldPlugin`] wires the streaming systems and events. Chunk generation runs on
//!   the [`bevy::tasks::AsyncComputeTaskPool`] so a slow generator never stalls the main
//!   thread; completed work is collected and turned into entities once per frame.

mod chunk;
mod events;
mod generator;
mod manager;
mod props;
mod streaming;
mod tile;

use bevy::prelude::*;
use inf2d_core::SimulationSet;

pub use chunk::{Chunk, ChunkData};
pub use events::{ChunkLoaded, ChunkUnloaded};
pub use generator::{ActiveGenerator, FlatGenerator, Generator};
pub use manager::{ChunkManager, StreamingConfig};
pub use props::{spawn_chunk_props, Prop, Tree, WorldSeed};
pub use streaming::{
    CameraFocus, ChunkStreamFocus, ChunkStreamSet, PendingChunkGenerations,
};
pub use tile::{Tile, TileKind};

/// Plugin: registers events, the chunk manager resource, default streaming config,
/// and the streaming systems. Without an [`ActiveGenerator`] inserted, streaming
/// falls back to a [`FlatGenerator`] of `TileKind::Grass`.
pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Chunk>()
            .register_type::<Tile>()
            .register_type::<TileKind>()
            .register_type::<StreamingConfig>()
            .register_type::<CameraFocus>()
            .register_type::<Prop>()
            .register_type::<Tree>()
            .register_type::<WorldSeed>()
            .init_resource::<ChunkManager>()
            .init_resource::<StreamingConfig>()
            .init_resource::<CameraFocus>()
            .init_resource::<PendingChunkGenerations>()
            .add_message::<ChunkLoaded>()
            .add_message::<ChunkUnloaded>();

        if !app.world().contains_resource::<ActiveGenerator>() {
            app.insert_resource(ActiveGenerator::new(FlatGenerator::new(TileKind::Grass)));
        }

        app.configure_sets(Update, ChunkStreamSet.in_set(SimulationSet));

        app.add_systems(
            Update,
            (
                streaming::update_camera_focus,
                streaming::schedule_chunk_generations,
                streaming::collect_completed_generations,
                streaming::unload_distant_chunks,
                props::spawn_chunk_props,
            )
                .chain()
                .in_set(ChunkStreamSet),
        );
    }
}
