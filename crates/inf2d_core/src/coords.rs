use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Tiles per chunk along one axis. Total tiles per chunk = `CHUNK_SIZE * CHUNK_SIZE`.
pub const CHUNK_SIZE: u32 = 32;

/// Convenience: total tiles per chunk.
pub const CHUNK_TILES: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;

/// A signed, world-wide tile coordinate. Independent of chunking — `WorldTile { x: -5, y: 13 }`
/// is a real tile somewhere in the world regardless of which chunk it belongs to.
#[derive(
    Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default,
)]
#[reflect(Component, Hash, PartialEq)]
pub struct WorldTile {
    pub x: i32,
    pub y: i32,
}

impl WorldTile {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Signed chunk coordinate. A chunk owns the `CHUNK_SIZE x CHUNK_SIZE` block of tiles
/// starting at `(x * CHUNK_SIZE, y * CHUNK_SIZE)` and extending positively in both axes.
#[derive(
    Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default,
)]
#[reflect(Component, Hash, PartialEq)]
pub struct ChunkPos {
    pub x: i32,
    pub y: i32,
}

impl ChunkPos {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Chunk that owns the given world tile.
    #[inline]
    pub fn from_tile(tile: WorldTile) -> Self {
        Self {
            x: tile.x.div_euclid(CHUNK_SIZE as i32),
            y: tile.y.div_euclid(CHUNK_SIZE as i32),
        }
    }

    /// World tile at this chunk's local `(0, 0)`.
    #[inline]
    pub fn origin_tile(self) -> WorldTile {
        WorldTile::new(self.x * CHUNK_SIZE as i32, self.y * CHUNK_SIZE as i32)
    }

    /// World tile at this chunk's local center.
    #[inline]
    pub fn center_tile(self) -> WorldTile {
        WorldTile::new(
            self.x * CHUNK_SIZE as i32 + CHUNK_SIZE as i32 / 2,
            self.y * CHUNK_SIZE as i32 + CHUNK_SIZE as i32 / 2,
        )
    }

    /// Convert a world tile to its `(local_x, local_y)` inside this chunk, regardless of
    /// whether the tile actually belongs here. Always in `0..CHUNK_SIZE`.
    #[inline]
    pub fn local_of(self, tile: WorldTile) -> LocalTilePos {
        let lx = tile.x.rem_euclid(CHUNK_SIZE as i32) as u32;
        let ly = tile.y.rem_euclid(CHUNK_SIZE as i32) as u32;
        LocalTilePos { x: lx, y: ly }
    }

    /// Chebyshev distance in chunk coordinates — useful for square-ring streaming windows.
    #[inline]
    pub fn chebyshev_distance(self, other: ChunkPos) -> i32 {
        (self.x - other.x).abs().max((self.y - other.y).abs())
    }
}

/// Position of a tile inside a single chunk. Always `0..CHUNK_SIZE` on both axes.
#[derive(
    Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default,
)]
#[reflect(Component, Hash, PartialEq)]
pub struct LocalTilePos {
    pub x: u32,
    pub y: u32,
}

impl LocalTilePos {
    #[inline]
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }

    /// Flat row-major index into a `CHUNK_TILES` array.
    #[inline]
    pub fn index(self) -> usize {
        (self.y * CHUNK_SIZE + self.x) as usize
    }

    #[inline]
    pub fn from_index(idx: usize) -> Self {
        let idx = idx as u32;
        Self {
            x: idx % CHUNK_SIZE,
            y: idx / CHUNK_SIZE,
        }
    }

    /// Promote local coords to the world-wide `WorldTile` they represent inside the given chunk.
    #[inline]
    pub fn to_world(self, chunk: ChunkPos) -> WorldTile {
        WorldTile::new(
            chunk.x * CHUNK_SIZE as i32 + self.x as i32,
            chunk.y * CHUNK_SIZE as i32 + self.y as i32,
        )
    }
}
