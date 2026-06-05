//! The flat test-world stamper: writes a curated set of WALK and BUILD scenarios
//! into the shared [`VoxelOverrides`] store so every movement + editing behavior is
//! reachable a few steps from spawn. Pure data — it only `place`/`remove`s voxels;
//! the mesher + oracle pick the edits up through the shared store.
//!
//! Layout (flat ground's topmost solid is [`FLAT_SURFACE_Y`]; the player stands at
//! `+1`). The player spawns near `(0,0)`. Two lanes run along +X:
//! - WALK lane (z ≈ -1..1): a 1-step, a 2-high wall (must detour), a 3-step
//!   staircase up to a plateau (its far edge is a ledge/drop), a 2-deep pit, a
//!   1-per-cell ramp, a water well, and a roofed passage (level-aware nav).
//! - BUILD lane (z ≈ 9..13): an open pad, a wall face to place onto, a mound to
//!   dig, a tall wall to carve a niche + cap into (the wall-climb regression), and
//!   a breakable pillar.

use bevy::prelude::IVec3;
use inf3d_world::TerrainMaterialId;
use inf3d_worldgen::{VoxelOverrides, FLAT_SURFACE_Y};

/// Topmost solid Y of the flat ground (the player stands on `TOP + 1`).
const TOP: i32 = FLAT_SURFACE_Y;

/// Structures are stamped as `Built*` (player-build) materials, never natural terrain
/// ones, so the whole test layout reads as player-placed: it exercises the see-through
/// cutout (material index >= `BUILT_MATERIAL_BASE`) and stays distinct from the flat
/// ground. `BuiltStone`/`BuiltDirt` look like stone/dirt but are the placeable
/// variants the picker also uses.
fn stone() -> u8 {
    TerrainMaterialId::BuiltStone as u8
}
fn dirt() -> u8 {
    TerrainMaterialId::BuiltDirt as u8
}

/// Place a solid box (inclusive ranges) of `mat`.
fn fill(o: &VoxelOverrides, x0: i32, x1: i32, y0: i32, y1: i32, z0: i32, z1: i32, mat: u8) {
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                o.place(IVec3::new(x, y, z), mat);
            }
        }
    }
}

/// Remove a box (inclusive ranges) — carve down into the flat ground.
fn dig(o: &VoxelOverrides, x0: i32, x1: i32, y0: i32, y1: i32, z0: i32, z1: i32) {
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                o.remove(IVec3::new(x, y, z));
            }
        }
    }
}

/// Stamp the full test layout into `o`. Called by New Game after `clear_all`.
pub fn stamp_test_map(o: &VoxelOverrides) {
    stamp_walk_lane(o);
    stamp_build_lane(o);
}

/// WALK scenarios along +X at z ≈ -1..1.
fn stamp_walk_lane(o: &VoxelOverrides) {
    // (1) 1-voxel step up (and, walking off the far side, a 1-voxel step down).
    // A single climbable rise — STEP_HEIGHT allows it.
    fill(o, 5, 6, TOP + 1, TOP + 1, -1, 1, stone());

    // (2) 2-high wall: above the 1-voxel step cap, so it CANNOT be climbed —
    // pathfinding must detour around the open z-ends.
    fill(o, 10, 10, TOP + 1, TOP + 2, -2, 2, stone());

    // (3) 3-step staircase up to a plateau; each step rises 1 (climbable). The
    // plateau's far (+x) edge is a 3-voxel ledge/drop.
    fill(o, 14, 14, TOP + 1, TOP + 1, -1, 1, stone());
    fill(o, 15, 15, TOP + 1, TOP + 2, -1, 1, stone());
    fill(o, 16, 16, TOP + 1, TOP + 3, -1, 1, stone());
    fill(o, 17, 19, TOP + 1, TOP + 3, -1, 1, stone()); // plateau (top = TOP+3)

    // (4) 2-deep pit: drop in; the 2-high walls block climbing back out (a fall
    // + can't-step-up test).
    dig(o, 23, 25, TOP - 1, TOP, -1, 1);

    // (5) 1-per-cell ramp: each cell one higher than the last — a smooth climb.
    fill(o, 28, 28, TOP + 1, TOP + 1, -1, 1, dirt());
    fill(o, 29, 29, TOP + 1, TOP + 2, -1, 1, dirt());
    fill(o, 30, 30, TOP + 1, TOP + 3, -1, 1, dirt());
    fill(o, 31, 31, TOP + 1, TOP + 4, -1, 1, dirt());

    // (6) Water well: carve a column down so the standing height drops at/under
    // the water line (stand y = 1 <= WATER_HEIGHT) — the global water plane shows
    // and the cell reads as unwalkable water; the dry rim is the walkable shore.
    dig(o, 34, 36, 1, TOP, -1, 1);

    // (7) Roofed passage: 4 corner pillars + a roof leave a 2-tall covered area
    // (>= STAND_HEADROOM), so the pathfinder/standing follows the player UNDER the
    // roof instead of onto it — the level-aware navigation case.
    for (px, pz) in [(40, -1), (40, 1), (43, -1), (43, 1)] {
        fill(o, px, px, TOP + 1, TOP + 3, pz, pz, stone());
    }
    fill(o, 40, 43, TOP + 3, TOP + 3, -1, 1, stone()); // roof
}

/// BUILD scenarios along +X at z ≈ 9..13 (left of spawn's walking lane).
fn stamp_build_lane(o: &VoxelOverrides) {
    // (1) Open flat build pad — nothing to stamp; corner dirt markers frame it so
    // it's obvious where to left-click-place.
    for (mx, mz) in [(5, 9), (9, 9), (5, 13), (9, 13)] {
        fill(o, mx, mx, TOP + 1, TOP + 1, mz, mz, dirt());
    }

    // (2) Wall face: a 3-high wall to left-click-place blocks onto its side
    // (exercises placing on the hovered face normal).
    fill(o, 14, 14, TOP + 1, TOP + 3, 9, 13, stone());

    // (3) Dig-out mound: a low stone mound to right-click-carve into.
    fill(o, 19, 22, TOP + 1, TOP + 2, 10, 12, stone());

    // (4) Niche + cap regression: a tall wall — dig a 2-tall niche into its base
    // and place a cap above your head to confirm the wall-climb bug stays fixed
    // (you must NOT get launched to the top).
    fill(o, 27, 27, TOP + 1, TOP + 6, 9, 13, stone());

    // (5) Breakable pillar: the classic 3-high stone pillar to right-click away.
    fill(o, 33, 33, TOP + 1, TOP + 3, 11, 11, stone());
}
