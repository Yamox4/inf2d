use std::sync::Arc;

use bevy::prelude::*;
use inf2d_core::ChunkPos;

use crate::chunk::ChunkData;
use crate::tile::{Tile, TileKind};

/// Anything that can produce a [`ChunkData`] for a given chunk coordinate. Implementations
/// must be **deterministic**: the same `(generator, chunk)` pair must produce the same data
/// every time, so chunks can be regenerated on demand without persistence.
///
/// The blanket bounds (`Send + Sync + 'static`) let the trait object live inside a Bevy
/// resource and be shared across worker threads — required for the async chunk-generation
/// task pool used by `streaming`.
pub trait Generator: Send + Sync + 'static {
    fn generate(&self, chunk: ChunkPos) -> ChunkData;
}

/// Owns the active generator behind an `Arc` so it can be cheaply cloned into
/// `AsyncComputeTaskPool` tasks without moving the resource. The world streaming layer
/// schedules generation off the main thread; if missing, `WorldPlugin` installs a
/// [`FlatGenerator`] fallback.
#[derive(Resource, Clone)]
pub struct ActiveGenerator(Arc<dyn Generator + Send + Sync>);

impl ActiveGenerator {
    /// Wrap any `Generator` in a shared, thread-safe handle.
    pub fn new<G: Generator>(generator: G) -> Self {
        Self(Arc::new(generator))
    }

    /// Run the generator inline on the calling thread. Async chunk loading uses
    /// [`ActiveGenerator::shared`] to capture the `Arc` into a task closure instead.
    #[inline]
    pub fn generate(&self, chunk: ChunkPos) -> ChunkData {
        self.0.generate(chunk)
    }

    /// Cheap clone of the inner `Arc` for capture inside a worker task. The returned
    /// value is `Send + Sync + 'static`, so it can cross a task-pool boundary safely.
    #[inline]
    pub fn shared(&self) -> Arc<dyn Generator + Send + Sync> {
        Arc::clone(&self.0)
    }
}

/// Trivial generator that fills every tile of every chunk with the same kind. Useful for
/// tests, the default fallback, and proving the pipeline before the real biome generator
/// is registered.
pub struct FlatGenerator {
    kind: TileKind,
}

impl FlatGenerator {
    pub const fn new(kind: TileKind) -> Self {
        Self { kind }
    }
}

impl Generator for FlatGenerator {
    fn generate(&self, _chunk: ChunkPos) -> ChunkData {
        ChunkData::filled(Tile::of(self.kind))
    }
}
