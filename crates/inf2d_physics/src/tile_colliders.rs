use avian2d::prelude::*;
use bevy::prelude::*;
use inf2d_core::{chunk_origin_world, tile_to_world, ChunkPos, WorldTile, CHUNK_SIZE, TILE_HEIGHT, TILE_WIDTH};
use inf2d_world::{ChunkData, ChunkLoaded};

use crate::layers::GameLayer;

/// Marker on the child entity that carries a chunk's compound collider. The chunk
/// entity itself stays purely logical; physics shapes live on this child so they can
/// be despawned alongside the chunk via Bevy's recursive despawn.
#[derive(Component, Debug, Default)]
pub struct ChunkCollider;

const CHUNK_SIZE_USIZE: usize = CHUNK_SIZE as usize;

/// Four screen-space vertices (CCW) of a single isometric tile's diamond, centered at
/// origin. Suitable input for `Collider::convex_polygon`. Kept as a thin helper so
/// debug overlays / single-tile fixtures can request the same diamond shape the
/// greedy mesher emits for a 1×1 region.
pub(crate) fn diamond_polygon() -> Vec<Vec2> {
    vec![
        Vec2::new(0.0, TILE_HEIGHT * 0.5),
        Vec2::new(-TILE_WIDTH * 0.5, 0.0),
        Vec2::new(0.0, -TILE_HEIGHT * 0.5),
        Vec2::new(TILE_WIDTH * 0.5, 0.0),
    ]
}

/// One axis-aligned-in-tile-space rectangle produced by greedy meshing. `x0, y0` are
/// the inclusive top-left tile coordinates inside a chunk; `w, h` are the rectangle's
/// extents in tiles (so `(x0..x0+w, y0..y0+h)` are all solid).
///
/// Public only inside the crate so tests in the integration-test directory can build
/// expected rectangles directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GreedyRect {
    pub x0: i32,
    pub y0: i32,
    pub w: i32,
    pub h: i32,
}

/// Run the standard greedy-meshing pass over a solidity mask: first extend each
/// run horizontally as far as it goes inside the same row, then try to extend that
/// run downward over identical rows below. Any contiguous tile-aligned rectangle
/// collapses to a single output rect; L-shapes (and worse) produce as few rects as
/// the greedy scan happens to find — never optimal but always correct.
///
/// Indexed as `solid[y][x]` to match how the collider builder fills it.
pub(crate) fn greedy_mesh_solids(
    solid: &[[bool; CHUNK_SIZE_USIZE]; CHUNK_SIZE_USIZE],
) -> Vec<GreedyRect> {
    let mut used = [[false; CHUNK_SIZE_USIZE]; CHUNK_SIZE_USIZE];
    let mut out: Vec<GreedyRect> = Vec::new();

    for y in 0..CHUNK_SIZE_USIZE {
        for x in 0..CHUNK_SIZE_USIZE {
            if !solid[y][x] || used[y][x] {
                continue;
            }

            // Find the horizontal run on this row.
            let mut x_end = x + 1;
            while x_end < CHUNK_SIZE_USIZE && solid[y][x_end] && !used[y][x_end] {
                x_end += 1;
            }

            // Try to extend the run downward, row by row, while every column in the
            // run is still solid and unused.
            let mut y_end = y + 1;
            'vert: while y_end < CHUNK_SIZE_USIZE {
                for xi in x..x_end {
                    if !solid[y_end][xi] || used[y_end][xi] {
                        break 'vert;
                    }
                }
                y_end += 1;
            }

            // Mark every cell of the merged rectangle as used so subsequent scans
            // skip them.
            for yi in y..y_end {
                for xi in x..x_end {
                    used[yi][xi] = true;
                }
            }

            out.push(GreedyRect {
                x0: x as i32,
                y0: y as i32,
                w: (x_end - x) as i32,
                h: (y_end - y) as i32,
            });
        }
    }

    out
}

/// Build the (collider, transform) pair for a chunk's solid tiles. Returns `None` if
/// the chunk contains no solid tiles, so callers can skip spawning an empty child.
///
/// The collider is a compound of one convex polygon per greedy-meshed rectangle of
/// contiguous solid tiles. A 1×1 region produces the same diamond as the previous
/// per-tile build; a 3×2 region produces a single parallelogram covering all six
/// tiles in screen space. Each rectangle's four screen-space corners (the iso
/// projections of the tile-coord rectangle's corners) form a convex polygon, which
/// `Collider::convex_hull` accepts directly.
///
/// The returned transform is identity — every sub-shape's `Position` is baked in
/// chunk-local space, and the chunk entity's own `Transform` supplies the world
/// offset.
pub fn build_chunk_collider_components(
    data: &ChunkData,
    chunk_pos: ChunkPos,
) -> Option<(Collider, Transform)> {
    let mut solid = [[false; CHUNK_SIZE_USIZE]; CHUNK_SIZE_USIZE];
    for (local, tile) in data.iter() {
        if tile.kind.is_solid() {
            solid[local.y as usize][local.x as usize] = true;
        }
    }

    let rects = greedy_mesh_solids(&solid);
    if rects.is_empty() {
        return None;
    }

    let chunk_origin = chunk_origin_world(chunk_pos);
    let parts: Vec<(Position, Rotation, Collider)> = rects
        .iter()
        .map(|r| {
            // Project the four tile-coord corners of the merged rectangle into screen
            // space and shift them into chunk-local space. Iso-projection of an
            // axis-aligned tile-coord rectangle is a (convex) parallelogram in screen
            // space, so the four projected corners are exactly its outline.
            //
            // Tile coordinates here are relative to the *world* tile grid, then made
            // chunk-local by subtracting `chunk_origin`; this matches how the previous
            // per-tile build positioned each diamond.
            let world_corners = [
                local_corner(chunk_pos, r.x0, r.y0),
                local_corner(chunk_pos, r.x0 + r.w, r.y0),
                local_corner(chunk_pos, r.x0 + r.w, r.y0 + r.h),
                local_corner(chunk_pos, r.x0, r.y0 + r.h),
            ];
            let local_corners: [Vec2; 4] = world_corners.map(|v| v - chunk_origin);

            // Center of a parallelogram is the midpoint of either diagonal.
            let center = (local_corners[0] + local_corners[2]) * 0.5;
            let polygon: Vec<Vec2> = local_corners.iter().map(|v| *v - center).collect();

            let collider = Collider::convex_hull(polygon)
                .expect("merged greedy rectangle projects to a convex parallelogram");

            (Position(center), Rotation::radians(0.0), collider)
        })
        .collect();

    tracing::debug!(
        "greedy chunk collider {:?}: {} rects (was {} per-tile diamonds)",
        chunk_pos,
        parts.len(),
        data.count_solid()
    );

    Some((Collider::compound(parts), Transform::IDENTITY))
}

/// Project a tile-corner at `(tx, ty)` *within* `chunk_pos` (in tile-grid coords
/// where `(0, 0)` is the chunk's origin tile) into world space.
#[inline]
fn local_corner(chunk_pos: ChunkPos, tx: i32, ty: i32) -> Vec2 {
    tile_to_world(WorldTile::new(
        chunk_pos.x * CHUNK_SIZE as i32 + tx,
        chunk_pos.y * CHUNK_SIZE as i32 + ty,
    ))
}

/// Listen for [`ChunkLoaded`] events and attach a `ChunkCollider` child under each
/// loaded chunk that has at least one solid tile. Chunks with no solid tiles are
/// skipped to avoid creating empty compound colliders.
pub fn attach_chunk_colliders(
    mut commands: Commands,
    mut events: MessageReader<ChunkLoaded>,
    chunks: Query<&ChunkData>,
) {
    for ev in events.read() {
        let Ok(data) = chunks.get(ev.entity) else {
            continue;
        };
        if !data.has_solid() {
            continue;
        }
        let Some((collider, transform)) = build_chunk_collider_components(data, ev.pos) else {
            continue;
        };

        let child = commands
            .spawn((
                ChunkCollider,
                RigidBody::Static,
                collider,
                transform,
                CollisionLayers::new(
                    GameLayer::Terrain,
                    [
                        GameLayer::Player,
                        GameLayer::Mob,
                        GameLayer::Projectile,
                    ],
                ),
                Name::new(format!("ChunkCollider({}, {})", ev.pos.x, ev.pos.y)),
            ))
            .id();
        commands.entity(ev.entity).add_child(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inf2d_core::TILE_HEIGHT;
    use inf2d_world::{Tile, TileKind};

    #[test]
    fn diamond_polygon_has_four_ccw_vertices() {
        let v = diamond_polygon();
        assert_eq!(v.len(), 4);
        assert_eq!(v[0], Vec2::new(0.0, TILE_HEIGHT * 0.5));
        assert_eq!(v[1], Vec2::new(-TILE_WIDTH * 0.5, 0.0));
        assert_eq!(v[2], Vec2::new(0.0, -TILE_HEIGHT * 0.5));
        assert_eq!(v[3], Vec2::new(TILE_WIDTH * 0.5, 0.0));

        // Shoelace area: positive => CCW winding.
        let mut area = 0.0_f32;
        for i in 0..v.len() {
            let a = v[i];
            let b = v[(i + 1) % v.len()];
            area += a.x * b.y - b.x * a.y;
        }
        assert!(area > 0.0, "diamond polygon should be CCW (area > 0)");
    }

    #[test]
    fn build_returns_none_for_all_grass_chunk() {
        let data = ChunkData::filled(Tile::of(TileKind::Grass));
        assert!(build_chunk_collider_components(&data, ChunkPos::new(0, 0)).is_none());
    }

    #[test]
    fn build_returns_some_for_single_water_tile() {
        let mut data = ChunkData::filled(Tile::of(TileKind::Grass));
        data.set(
            inf2d_core::LocalTilePos::new(3, 4),
            Tile::of(TileKind::Water),
        );
        assert!(build_chunk_collider_components(&data, ChunkPos::new(0, 0)).is_some());
    }

    // ---- greedy_mesh_solids unit tests --------------------------------------

    fn empty_mask() -> [[bool; CHUNK_SIZE_USIZE]; CHUNK_SIZE_USIZE] {
        [[false; CHUNK_SIZE_USIZE]; CHUNK_SIZE_USIZE]
    }

    fn full_mask() -> [[bool; CHUNK_SIZE_USIZE]; CHUNK_SIZE_USIZE] {
        [[true; CHUNK_SIZE_USIZE]; CHUNK_SIZE_USIZE]
    }

    #[test]
    fn greedy_empty_chunk_produces_no_rects() {
        let mask = empty_mask();
        let rects = greedy_mesh_solids(&mask);
        assert!(rects.is_empty(), "empty mask should produce no rectangles");
    }

    #[test]
    fn greedy_all_solid_chunk_collapses_to_one_rect() {
        let mask = full_mask();
        let rects = greedy_mesh_solids(&mask);
        assert_eq!(rects.len(), 1, "full mask should collapse to one rect");
        let r = rects[0];
        assert_eq!(r.x0, 0);
        assert_eq!(r.y0, 0);
        assert_eq!(r.w, CHUNK_SIZE as i32);
        assert_eq!(r.h, CHUNK_SIZE as i32);
    }

    #[test]
    fn greedy_single_tile_is_one_rect_of_one_by_one() {
        let mut mask = empty_mask();
        mask[5][7] = true;
        let rects = greedy_mesh_solids(&mask);
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        assert_eq!(r.x0, 7);
        assert_eq!(r.y0, 5);
        assert_eq!(r.w, 1);
        assert_eq!(r.h, 1);
    }

    #[test]
    fn greedy_l_shape_produces_two_rects() {
        // Shape:
        //   X X X
        //   X . .
        //   X . .
        // Greedy horizontal-first scan finds the top row (3×1) and then the left
        // column below (1×2) — never one rect, because the L isn't rectangular.
        let mut mask = empty_mask();
        for x in 0..3 {
            mask[0][x] = true;
        }
        mask[1][0] = true;
        mask[2][0] = true;

        let rects = greedy_mesh_solids(&mask);
        assert_eq!(
            rects.len(),
            2,
            "L-shape can never collapse below 2 axis-aligned rectangles"
        );

        // Order is deterministic (top-to-bottom, left-to-right): the 3×1 row first,
        // then the 1×2 leg.
        assert!(rects.contains(&GreedyRect {
            x0: 0,
            y0: 0,
            w: 3,
            h: 1,
        }));
        assert!(rects.contains(&GreedyRect {
            x0: 0,
            y0: 1,
            w: 1,
            h: 2,
        }));
    }

    #[test]
    fn greedy_two_adjacent_rows_of_same_width_merge_vertically() {
        // 4×2 solid block in the top-left → must collapse to ONE rect, not two
        // rows-as-rectangles, because the vertical-merge pass joins identical
        // x-spans.
        let mut mask = empty_mask();
        for y in 0..2 {
            for x in 0..4 {
                mask[y][x] = true;
            }
        }
        let rects = greedy_mesh_solids(&mask);
        assert_eq!(rects.len(), 1);
        assert_eq!(
            rects[0],
            GreedyRect {
                x0: 0,
                y0: 0,
                w: 4,
                h: 2,
            }
        );
    }
}

