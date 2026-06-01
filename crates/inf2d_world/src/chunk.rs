use bevy::prelude::*;
use inf2d_core::{ChunkPos, LocalTilePos, CHUNK_SIZE, CHUNK_TILES};

use crate::tile::Tile;

/// Marker component placed on the entity that represents a chunk in the ECS. The entity
/// also carries [`ChunkPos`], [`ChunkData`], and a [`Transform`] positioned at the chunk's
/// world-space origin so child meshes/tilemaps render at the right place.
#[derive(Component, Reflect, Default, Debug)]
#[reflect(Component)]
pub struct Chunk;

/// Dense storage of `CHUNK_SIZE * CHUNK_SIZE` tiles in row-major order
/// (`y * CHUNK_SIZE + x`). Heap-allocated — one `Box` per chunk is cheap, and
/// keeps the stack out of trouble when a chunk gets passed by value.
///
/// Serde derives are intentionally omitted: stdlib `Serialize`/`Deserialize`
/// only cover arrays up to size 32. The save layer (`inf2d_save`, future) will
/// supply manual `serialize_as_bytes` / `deserialize_from_bytes` helpers when
/// persistence is wired.
#[derive(Component, Debug, Clone)]
pub struct ChunkData {
    tiles: Box<[Tile; CHUNK_TILES]>,
}

impl Default for ChunkData {
    fn default() -> Self {
        Self::filled(Tile::default())
    }
}

impl ChunkData {
    /// Build a chunk where every tile is `fill`.
    #[inline]
    pub fn filled(fill: Tile) -> Self {
        Self {
            tiles: Box::new([fill; CHUNK_TILES]),
        }
    }

    /// Build from a flat row-major array (`y * CHUNK_SIZE + x`).
    #[inline]
    pub fn from_array(tiles: Box<[Tile; CHUNK_TILES]>) -> Self {
        Self { tiles }
    }

    #[inline]
    pub fn get(&self, local: LocalTilePos) -> Tile {
        self.tiles[local.index()]
    }

    #[inline]
    pub fn set(&mut self, local: LocalTilePos, tile: Tile) {
        self.tiles[local.index()] = tile;
    }

    /// Read-only access to the raw tile slice. Row-major, length `CHUNK_TILES`.
    #[inline]
    pub fn raw(&self) -> &[Tile] {
        self.tiles.as_slice()
    }

    /// Mutable raw access — for bulk fills inside generators.
    #[inline]
    pub fn raw_mut(&mut self) -> &mut [Tile] {
        self.tiles.as_mut_slice()
    }

    /// Iterate `(local_pos, tile)` over every cell.
    pub fn iter(&self) -> impl Iterator<Item = (LocalTilePos, Tile)> + '_ {
        self.tiles
            .iter()
            .enumerate()
            .map(|(idx, t)| (LocalTilePos::from_index(idx), *t))
    }

    /// Count tiles matching the predicate. Used for collider build heuristics.
    pub fn count_where(&self, pred: impl Fn(Tile) -> bool) -> usize {
        self.tiles.iter().copied().filter(|t| pred(*t)).count()
    }

    /// Convenience: true if any tile is solid.
    pub fn has_solid(&self) -> bool {
        self.tiles.iter().any(|t| t.kind.is_solid())
    }

    /// Count the solid tiles in this chunk. Used by collider builders to log how much
    /// the greedy meshing pass collapsed (e.g. "1024 tiles → 7 rectangles").
    pub fn count_solid(&self) -> usize {
        self.tiles.iter().filter(|t| t.kind.is_solid()).count()
    }
}

/// Bundle assembled when spawning a chunk entity. Keeps the chunk's components together
/// for downstream listeners and the inspector.
#[derive(Bundle)]
pub struct ChunkBundle {
    pub chunk: Chunk,
    pub pos: ChunkPos,
    pub data: ChunkData,
    pub transform: Transform,
    pub visibility: Visibility,
    pub name: Name,
}

impl ChunkBundle {
    pub fn new(pos: ChunkPos, data: ChunkData) -> Self {
        let origin = inf2d_core::chunk_origin_world(pos);
        Self {
            chunk: Chunk,
            pos,
            data,
            transform: Transform::from_xyz(origin.x, origin.y, 0.0),
            visibility: Visibility::Visible,
            name: Name::new(format!("Chunk({}, {})", pos.x, pos.y)),
        }
    }
}

/// Anchor sanity: width of one chunk's tile grid measured in tiles, exposed for math elsewhere.
#[allow(dead_code)]
pub const CHUNK_TILE_SPAN: i32 = CHUNK_SIZE as i32;
