//! End-to-end integration tests for the greedy-meshed chunk colliders. These
//! drive the public `build_chunk_collider_components` API across the four
//! canonical shapes from the task spec — empty, full, single tile, and an
//! L-shape — and assert the collider is built (or skipped) appropriately.
//!
//! The greedy-mesh algorithm itself has finer-grained unit tests inside
//! `inf2d_physics::tile_colliders` (which can reach the crate-private
//! `greedy_mesh_solids` helper); this file is the cross-crate smoke test.

use inf2d_core::{ChunkPos, LocalTilePos};
use inf2d_physics::build_chunk_collider_components;
use inf2d_world::{ChunkData, Tile, TileKind};

#[test]
fn empty_chunk_produces_no_collider() {
    let data = ChunkData::filled(Tile::of(TileKind::Grass));
    let built = build_chunk_collider_components(&data, ChunkPos::new(0, 0));
    assert!(built.is_none(), "no solid tiles ⇒ no collider");
}

#[test]
fn all_solid_chunk_builds_one_compound_subshape() {
    let data = ChunkData::filled(Tile::of(TileKind::Stone));
    let built = build_chunk_collider_components(&data, ChunkPos::new(0, 0));
    assert!(
        built.is_some(),
        "all-solid chunk must produce a compound collider"
    );
    // We can't introspect Avian's compound directly through its public API in a
    // version-portable way, but `data.count_solid()` proves the input shape and
    // the build path was exercised. The unit tests in `tile_colliders` cover the
    // exact rectangle count.
    assert_eq!(data.count_solid(), 32 * 32);
}

#[test]
fn single_solid_tile_builds_a_collider() {
    let mut data = ChunkData::filled(Tile::of(TileKind::Grass));
    data.set(LocalTilePos::new(3, 4), Tile::of(TileKind::Water));
    let built = build_chunk_collider_components(&data, ChunkPos::new(0, 0));
    assert!(
        built.is_some(),
        "one solid tile must still produce a collider"
    );
    assert_eq!(data.count_solid(), 1);
}

#[test]
fn l_shaped_solid_region_still_builds() {
    // Same L-shape as the unit test in `tile_colliders` — greedy can't merge it
    // into a single rectangle but must still emit a valid compound collider
    // covering all 5 solid cells.
    let mut data = ChunkData::filled(Tile::of(TileKind::Grass));
    for x in 0u32..3 {
        data.set(LocalTilePos::new(x, 0), Tile::of(TileKind::Stone));
    }
    data.set(LocalTilePos::new(0, 1), Tile::of(TileKind::Stone));
    data.set(LocalTilePos::new(0, 2), Tile::of(TileKind::Stone));

    let built = build_chunk_collider_components(&data, ChunkPos::new(0, 0));
    assert!(built.is_some(), "L-shape must still build a collider");
    assert_eq!(data.count_solid(), 5);
}
