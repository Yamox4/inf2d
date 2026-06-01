use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Discriminator for a tile's biome / surface. The render layer uses this to choose an atlas
/// index; the physics layer uses it to decide whether the tile contributes a collider.
#[derive(
    Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default,
)]
#[repr(u8)]
pub enum TileKind {
    #[default]
    Grass = 0,
    Sand = 1,
    Water = 2,
    Stone = 3,
    Dirt = 4,
    Snow = 5,
    /// Stair / ramp tile: visually a sandstone tile with horizontal step lines and
    /// pathfinding-wise it bridges two-step elevation gaps that ordinary tiles can't
    /// cross. Worldgen spawns these as "landing" tiles between two adjacent tiles that
    /// differ in height by 2 steps; the pathfinder treats `|Δh| ≤ 2` as walkable when
    /// either endpoint is a stair.
    ///
    /// Discriminant `= 9` (not `6`) so [`TileKind::atlas_index`] (which is just
    /// `self as u32`) lands stairs at atlas slot 9 — slots 6, 7, 8 are already reserved
    /// for the animated water frames painted by the render crate.
    Stairs = 9,
}

impl TileKind {
    /// Stable atlas slot. Must match the order the render crate paints the procedural atlas.
    #[inline]
    pub const fn atlas_index(self) -> u32 {
        self as u32
    }

    /// Whether the tile contributes a solid collider. Water blocks movement (you can't walk
    /// into the ocean in slice 1), stone blocks too. Stairs are explicitly traversable —
    /// they are the bridging surface between height bands.
    #[inline]
    pub const fn is_solid(self) -> bool {
        matches!(self, TileKind::Water | TileKind::Stone)
    }

    pub const ALL: [TileKind; 7] = [
        TileKind::Grass,
        TileKind::Sand,
        TileKind::Water,
        TileKind::Stone,
        TileKind::Dirt,
        TileKind::Snow,
        TileKind::Stairs,
    ];
}

/// Per-tile data. Cheap to copy; `ChunkData` packs `CHUNK_SIZE * CHUNK_SIZE` of these.
///
/// `height` is a signed integer **step count** (not a pixel value). One step corresponds to
/// `inf2d_core::HEIGHT_STEP_PX` pixels of upward screen offset in the 2:1 dimetric
/// projection. `0` = ground level, positive = up (plateaus, mountains), negative = down
/// (recessed water tiles, caves). Stored as `i8` to keep the per-cell struct under a word
/// on every target — `CHUNK_TILES * sizeof(Tile)` stays in the same ballpark.
#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Tile {
    pub kind: TileKind,
    pub height: i8,
}

impl Tile {
    /// Convenience constructor: a tile of the given `kind` at ground level (`height = 0`).
    #[inline]
    pub const fn of(kind: TileKind) -> Self {
        Self { kind, height: 0 }
    }

    /// Construct a tile with an explicit height step. Use this in worldgen to terrace the
    /// map; `height` is in step units (one step = `HEIGHT_STEP_PX` pixels in screen Y).
    #[inline]
    pub const fn with_height(kind: TileKind, height: i8) -> Self {
        Self { kind, height }
    }
}
