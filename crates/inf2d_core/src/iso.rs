use bevy::math::Vec2;

use crate::coords::{ChunkPos, WorldTile};

/// Width of a single tile diamond on screen, in world units.
pub const TILE_WIDTH: f32 = 64.0;

/// Height of a single tile diamond on screen, in world units. 2:1 dimetric ratio.
pub const TILE_HEIGHT: f32 = 32.0;

/// World-space rise per logical height step, in pixels. One height step shifts the tile's
/// screen anchor upward by this many units — matches the 2:1 dimetric stair-step you see in
/// Tactics Ogre / FFT, where each terrace clearly reads as "one tile up" without any cliff
/// geometry. Exposed for renderers, picking, and shaders.
pub const HEIGHT_STEP_PX: f32 = TILE_HEIGHT * 0.5;

/// Convert a logical tile coordinate to its screen-space anchor (the diamond's left vertex).
///
/// This is the standard 2:1 dimetric "isometric" projection used by Diablo, AoE2, SC1, etc.
/// X axis runs to the lower-right on screen, Y axis to the upper-right.
///
/// Height-unaware — returns the anchor at the `z = 0` (ground) plane. Callers that need to
/// account for per-tile elevation should use [`tile_to_world_with_height`] instead.
#[inline]
pub fn tile_to_world(tile: WorldTile) -> Vec2 {
    let x = (tile.x - tile.y) as f32 * (TILE_WIDTH * 0.5);
    let y = (tile.x + tile.y) as f32 * (TILE_HEIGHT * 0.5);
    Vec2::new(x, y)
}

/// Screen-space anchor for a tile at logical `(x, y, z)`. Each unit of `height` shifts the
/// diamond up by [`HEIGHT_STEP_PX`] — i.e. the 2:1 dimetric stair-step used by Tactics Ogre
/// / FFT. The XY base is the ground-plane projection from [`tile_to_world`]; height only
/// influences the screen-Y component.
#[inline]
pub fn tile_to_world_with_height(tile: WorldTile, height: i32) -> Vec2 {
    let base = tile_to_world(tile);
    Vec2::new(base.x, base.y + height as f32 * HEIGHT_STEP_PX)
}

/// World-space center of the given tile's diamond.
#[inline]
pub fn tile_center_world(tile: WorldTile) -> Vec2 {
    tile_to_world(tile)
}

/// Convert a world-space point back to the tile it sits on. Uses floor semantics so
/// every world point maps to exactly one tile — no gaps, no overlaps at edges.
#[inline]
pub fn world_to_tile(world: Vec2) -> WorldTile {
    let fx = world.x / (TILE_WIDTH * 0.5);
    let fy = world.y / (TILE_HEIGHT * 0.5);
    let tx = ((fx + fy) * 0.5).floor() as i32;
    let ty = ((fy - fx) * 0.5).floor() as i32;
    WorldTile::new(tx, ty)
}

/// World-space center of the given chunk — useful for camera follow targets, chunk gizmos,
/// and distance comparisons against the camera focal point.
#[inline]
pub fn chunk_center_world(chunk: ChunkPos) -> Vec2 {
    tile_to_world(chunk.center_tile())
}

/// World-space anchor (left-vertex of the (0,0) diamond) of the given chunk. This is
/// where a `bevy_ecs_tilemap` Tilemap entity with `TilemapAnchor::None` should be placed
/// so that its local tile `(0,0)` aligns with the chunk's world tile `(cx*CHUNK_SIZE, cy*CHUNK_SIZE)`.
#[inline]
pub fn chunk_origin_world(chunk: ChunkPos) -> Vec2 {
    tile_to_world(chunk.origin_tile())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_tile_world_tile() {
        for x in -10..10 {
            for y in -10..10 {
                let tile = WorldTile::new(x, y);
                let world = tile_to_world(tile);
                // Nudge slightly into the diamond center so floor semantics land us in the right tile.
                let inside = world + Vec2::new(0.1, 0.1);
                let back = world_to_tile(inside);
                assert_eq!(tile, back, "roundtrip failed for {tile:?}");
            }
        }
    }

    #[test]
    fn height_zero_matches_ground() {
        let tile = WorldTile::new(3, -4);
        let flat = tile_to_world(tile);
        let lifted = tile_to_world_with_height(tile, 0);
        assert_eq!(flat, lifted);
    }

    #[test]
    fn one_height_step_is_half_tile_height() {
        let tile = WorldTile::new(2, 5);
        let flat = tile_to_world(tile);
        let up = tile_to_world_with_height(tile, 1);
        assert!((up.x - flat.x).abs() < f32::EPSILON);
        assert!((up.y - flat.y - HEIGHT_STEP_PX).abs() < f32::EPSILON);
    }

    #[test]
    fn negative_height_drops_below_ground() {
        let tile = WorldTile::new(0, 0);
        let down = tile_to_world_with_height(tile, -2);
        assert!((down.y + 2.0 * HEIGHT_STEP_PX).abs() < f32::EPSILON);
    }
}
