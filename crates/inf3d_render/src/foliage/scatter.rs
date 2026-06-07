//! The off-thread per-tile scatter workers — one per streaming layer.
//!
//! Both run on the [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool)
//! and touch no ECS or asset state — only a cloned [`Terrain`] snapshot and the
//! per-variant footprint sizes — so the main thread is left with just entity
//! spawning (see [`super::spawn`]).
//!
//! * [`scatter_solid`] decides, per column, whether a tree or rock sits there,
//!   returning their [`ScatterItem`]s (with inter-prop overlap rejection).
//! * [`scatter_grass`] decides, per column, whether grass sits there, returning
//!   grass [`ScatterItem`]s only (no overlap test — grass may overlap freely).
//!
//! Determinism: each worker seeds its RNG from the SAME per-tile
//! [`tile_seed`](super::tile_seed) (so a tile is stable run to run), but seeds a
//! *separate* `StdRng` per layer. The grass stream is therefore completely
//! independent of the solid stream — a tile's grass is identical whether or not
//! its solid props are currently streamed, and the solid props never move when
//! grass eligibility changes.

use bevy::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use inf3d_worldgen::{Terrain, WATER_HEIGHT};

use super::{
    biome_policy, footprint_radius, tile_seed, ScatterCategory, ScatterItem, SolidVariantSizes, TILE,
};

// Per-column probability of spawning each foliage category.
const TREE_DENSITY: f32 = 0.004;
const GRASS_DENSITY: f32 = 0.018;
const ROCK_DENSITY: f32 = 0.002;

/// Spacing factor between solid props: a candidate must sit at least this multiple
/// of the just-touching distance from every placed prop, so taller/wider canopies
/// leave a clear GAP instead of kissing/clipping. 1.0 = old behavior (touching
/// allowed); 1.3 ≈ a 30% gap. (Within a tile; cross-tile is rare at these low
/// densities.)
const PROP_SPACING: f32 = 1.3;

/// Decide, per column in `tile`, whether a tree or rock sits there. Returns
/// plain tree/rock [`ScatterItem`]s — grass is a separate layer
/// ([`scatter_grass`]) and is never emitted here.
///
/// Determinism: the RNG is seeded from [`tile_seed`] and consumed in a fixed
/// scan order, so the same tile always produces the same solid scatter,
/// independent of the grass layer.
///
/// The returned vec is sorted by (category, variant) so the main thread spawns
/// all instances of one variant contiguously. Bevy auto-batches instances that
/// share a mesh handle + material; spawning grouped (instead of per-column
/// interleaved) keeps those batches from fragmenting. This sort changes only
/// spawn order, never which items exist or where they sit — determinism holds.
pub(super) fn scatter_solid(
    terrain: &Terrain,
    tile: IVec2,
    sizes: &SolidVariantSizes,
) -> Vec<ScatterItem> {
    let mut rng = StdRng::seed_from_u64(tile_seed(tile));

    let base_x = tile.x * TILE;
    let base_z = tile.y * TILE;

    // Footprints (XZ center + radius) of solid props already placed in this
    // tile. Solid props (trees/rocks) must not inter-penetrate, so each
    // candidate is rejected if its footprint disc overlaps a placed one.
    let mut solid_footprints: Vec<(Vec2, f32)> = Vec::new();
    let mut items: Vec<ScatterItem> = Vec::new();

    for lx in 0..TILE {
        for lz in 0..TILE {
            let x = base_x + lx;
            let z = base_z + lz;
            // Single height sample per column (reused for the land test and the
            // placement position).
            let pos = terrain.stand_pos(x, z);
            if pos.y <= WATER_HEIGHT {
                continue;
            }
            // Biome is a pure function of (x, z), so reading it here keeps the
            // scatter deterministic: the same column always classifies the same
            // way run to run. The policy gates BOTH categories' densities and the
            // tree variant subset, and is recorded on every emitted item so
            // `spawn` can pick the biome's tinted material.
            let biome = terrain.biome_at(x, z);
            let policy = biome_policy(biome);

            let yaw = snap_yaw(&mut rng);
            let xz = Vec2::new(pos.x, pos.z);

            // Trees, gated by the biome's tree multiplier. CRUCIAL for
            // determinism: we always advance the RNG the SAME way per column
            // regardless of which (if any) tree variants the biome allows — the
            // `rng.random::<f32>()` density roll and, when it fires, the
            // `random_range` variant roll happen unconditionally. We then MAP the
            // rolled index onto the biome's eligible subset (preserving original
            // indices), so changing the biome's allowed set never shifts the RNG
            // stream and thus never perturbs neighbouring columns/tiles.
            if !sizes.trees.is_empty() && rng.random::<f32>() < TREE_DENSITY * policy.tree_mul {
                // Roll an index across ALL trees (fixed RNG consumption), then
                // restrict to the biome's eligible variants. If none are eligible,
                // skip trees for this column (still consumed the same RNG).
                let rolled = rng.random_range(0..sizes.trees.len());
                if let Some(variant) = pick_eligible_tree(sizes, policy.tree_names, rolled) {
                    if try_place_solid(&mut solid_footprints, xz, sizes.trees[variant]) {
                        items.push(ScatterItem {
                            category: ScatterCategory::Tree,
                            variant,
                            pos,
                            yaw,
                            biome,
                        });
                    }
                }
                continue;
            }
            // Rocks, gated by the biome's rock multiplier (no name filter — every
            // rock is allowed in every biome).
            if !sizes.rocks.is_empty() && rng.random::<f32>() < ROCK_DENSITY * policy.rock_mul {
                let variant = rng.random_range(0..sizes.rocks.len());
                if try_place_solid(&mut solid_footprints, xz, sizes.rocks[variant]) {
                    items.push(ScatterItem {
                        category: ScatterCategory::Rock,
                        variant,
                        pos,
                        yaw,
                        biome,
                    });
                }
            }
        }
    }

    // Group by category+variant for batch-friendly spawn order (see doc above).
    items.sort_by_key(|it| (category_rank(it.category), it.variant));
    items
}

/// Decide, per column in `tile`, whether grass sits there. Returns plain grass
/// [`ScatterItem`]s only — no collider, no blocked cells, no inter-prop overlap
/// test (grass may overlap freely).
///
/// Determinism: each cell seeds its OWN `StdRng` from its coords (via
/// [`tile_seed`] of the cell), so grass is stable run to run AND fully independent
/// per cell — removing one cell's grass (a player edit) can't shift what spawns on
/// any other cell. `variant_count` is the number of loaded grass variants; `0`
/// (no grass assets) yields an empty vec.
pub(super) fn scatter_grass(
    terrain: &Terrain,
    tile: IVec2,
    variant_count: usize,
) -> Vec<ScatterItem> {
    if variant_count == 0 {
        return Vec::new();
    }

    let base_x = tile.x * TILE;
    let base_z = tile.y * TILE;

    let mut items: Vec<ScatterItem> = Vec::new();

    for lx in 0..TILE {
        for lz in 0..TILE {
            let x = base_x + lx;
            let z = base_z + lz;
            // Per-CELL rng (seed = the cell's coords): each cell's grass is fully
            // independent, so dropping one cell's grass (a player edit) never
            // shifts what spawns on any other cell — no "sliding" to neighbors.
            let mut rng = StdRng::seed_from_u64(tile_seed(IVec2::new(x, z)));
            // No grass on player-edited cells: placing/breaking a block clears the
            // grass on/under it. The live blade is also despawned immediately on
            // edit; this keeps it gone if the tile later reloads.
            if terrain.column_edited(x, z) {
                continue;
            }
            let pos = terrain.stand_pos(x, z);
            if pos.y <= WATER_HEIGHT {
                continue;
            }
            // Biome gates grass density: dry biomes (Desert/Snow/Beach) have a 0.0
            // multiplier, so the probability test below can never fire there — no
            // grass at all. Biome is a pure function of (x, z), so this stays
            // deterministic per cell. As with the solid worker, the RNG is consumed
            // identically regardless of biome: `random::<f32>()` and (when it
            // fires) `random_range` run unconditionally, so a 0-multiplier biome
            // still advances this cell's own RNG the same way (each cell seeds its
            // own RNG anyway, so this is belt-and-braces — no cross-cell drift).
            let biome = terrain.biome_at(x, z);
            let grass_mul = biome_policy(biome).grass_mul;

            let yaw = snap_yaw(&mut rng);
            if rng.random::<f32>() < GRASS_DENSITY * grass_mul {
                let variant = rng.random_range(0..variant_count);
                items.push(ScatterItem {
                    category: ScatterCategory::Grass,
                    variant,
                    pos,
                    yaw,
                    biome,
                });
            }
        }
    }

    // Group by variant for batch-friendly spawn order (one grass category, so
    // only the variant index matters). Spawn order only — never placement.
    items.sort_by_key(|it| it.variant);
    items
}

/// Map a tree index `rolled` (drawn uniformly over ALL tree variants) onto the
/// subset eligible for this biome — variants whose name CONTAINS any of
/// `allowed` — returning the ORIGINAL index into the full trees `Vec` (so `spawn`
/// can index `assets.trees[variant]` unchanged). Returns `None` when no loaded
/// variant matches the biome (the caller then skips trees for that column).
///
/// Why map rather than draw from a pre-filtered list: the RNG must be consumed
/// IDENTICALLY regardless of the biome's allowed set, or changing a biome's tree
/// list would shift the RNG stream and perturb every later column in the tile
/// (breaking the "biome is appearance-only, determinism preserved" invariant).
/// So the caller always rolls one index across all trees; we then fold that roll
/// into the eligible subset with a stable modulo, which is a pure function of the
/// (fixed) roll and the (fixed-per-biome) eligible set.
fn pick_eligible_tree(
    sizes: &SolidVariantSizes,
    allowed: &[&str],
    rolled: usize,
) -> Option<usize> {
    // Eligible ORIGINAL indices, in ascending order (stable: `tree_names` is
    // index-parallel to `trees`, and we scan it in order).
    let eligible: Vec<usize> = sizes
        .tree_names
        .iter()
        .enumerate()
        .filter(|(_, name)| allowed.iter().any(|sub| name.contains(sub)))
        .map(|(i, _)| i)
        .collect();
    if eligible.is_empty() {
        return None;
    }
    // Fold the full-range roll into the eligible subset. `rolled` is uniform over
    // `0..trees.len()`, so `rolled % eligible.len()` is a deterministic, well-
    // distributed pick among the eligible variants without consuming extra RNG.
    Some(eligible[rolled % eligible.len()])
}

/// Stable ordering key for grouping spawns by category.
fn category_rank(category: ScatterCategory) -> u8 {
    match category {
        ScatterCategory::Tree => 0,
        ScatterCategory::Rock => 1,
        ScatterCategory::Grass => 2,
    }
}

/// Try to claim a footprint disc for a solid prop. Returns `true` (and records
/// the disc) if it doesn't overlap any previously placed solid prop in the tile;
/// returns `false` to reject the placement (props would inter-penetrate).
fn try_place_solid(placed: &mut Vec<(Vec2, f32)>, center: Vec2, size: Vec3) -> bool {
    let r = footprint_radius(size);
    for (c, pr) in placed.iter() {
        // Require a clear GAP (PROP_SPACING × the just-touching distance), not just
        // non-overlap, so taller/wider canopies don't visually clip into neighbors.
        let min_dist = (r + pr) * PROP_SPACING;
        if center.distance_squared(*c) < min_dist * min_dist {
            return false;
        }
    }
    placed.push((center, r));
    true
}

/// Pick a random cardinal yaw (0 / 90 / 180 / 270°) so props face axis-aligned
/// directions that match the blocky voxel aesthetic.
fn snap_yaw(rng: &mut StdRng) -> f32 {
    let q: u32 = rng.random_range(0..4);
    q as f32 * std::f32::consts::FRAC_PI_2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `SolidVariantSizes` with a couple of small footprints per category and a
    /// representative spread of tree NAMES across the biome families (a leafy
    /// tree, a pine, a cactus, a palm, a stump), so the biome name-subset filter
    /// has eligible variants for every biome — otherwise dry/cold biomes would
    /// scatter no trees and the determinism tests would only exercise the empty
    /// path. The sizes vec is index-parallel to `tree_names`.
    fn test_solid_sizes() -> SolidVariantSizes {
        let s = Vec3::new(0.5, 1.0, 0.5);
        let tree_names = vec![
            "tree_large".to_string(),
            "pine_small".to_string(),
            "cactus".to_string(),
            "palm".to_string(),
            "tree_stump".to_string(),
        ];
        SolidVariantSizes {
            trees: vec![s; tree_names.len()],
            tree_names,
            rocks: vec![s, s],
        }
    }

    /// Extract just the solid (tree/rock) placements as comparable tuples.
    fn solids(items: &[ScatterItem]) -> Vec<(u8, usize, [i32; 3], i32)> {
        items
            .iter()
            .map(|it| {
                (
                    category_rank(it.category),
                    it.variant,
                    [
                        it.pos.x.to_bits() as i32,
                        it.pos.y.to_bits() as i32,
                        it.pos.z.to_bits() as i32,
                    ],
                    it.yaw.to_bits() as i32,
                )
            })
            .collect()
    }

    /// Extract grass placements as comparable tuples.
    fn grass(items: &[ScatterItem]) -> Vec<(usize, [i32; 3], i32)> {
        items
            .iter()
            .map(|it| {
                (
                    it.variant,
                    [
                        it.pos.x.to_bits() as i32,
                        it.pos.y.to_bits() as i32,
                        it.pos.z.to_bits() as i32,
                    ],
                    it.yaw.to_bits() as i32,
                )
            })
            .collect()
    }

    #[test]
    fn solid_scatter_emits_no_grass() {
        // The solid layer must never produce a grass item — grass is a separate
        // layer now.
        let terrain = Terrain::new();
        let sizes = test_solid_sizes();
        for tx in -3..=3 {
            for tz in -3..=3 {
                let items = scatter_solid(&terrain, IVec2::new(tx, tz), &sizes);
                assert!(
                    items
                        .iter()
                        .all(|it| !matches!(it.category, ScatterCategory::Grass)),
                    "solid scatter of ({tx},{tz}) emitted grass"
                );
            }
        }
    }

    #[test]
    fn solid_scatter_is_deterministic() {
        // Same seed in => same solid placements out, across many distinct tiles.
        let terrain = Terrain::new();
        let sizes = test_solid_sizes();
        for tx in -3..=3 {
            for tz in -3..=3 {
                let tile = IVec2::new(tx, tz);
                let a = scatter_solid(&terrain, tile, &sizes);
                let b = scatter_solid(&terrain, tile, &sizes);
                assert_eq!(
                    solids(&a),
                    solids(&b),
                    "solid scatter of {tile:?} not stable"
                );
            }
        }
    }

    #[test]
    fn grass_scatter_is_deterministic() {
        // The new grass layer must be stable per tile (same seed => same grass).
        let terrain = Terrain::new();
        for tx in -3..=3 {
            for tz in -3..=3 {
                let tile = IVec2::new(tx, tz);
                let a = scatter_grass(&terrain, tile, 3);
                let b = scatter_grass(&terrain, tile, 3);
                assert_eq!(grass(&a), grass(&b), "grass scatter of {tile:?} not stable");
                // And every emitted item is grass.
                assert!(
                    a.iter()
                        .all(|it| matches!(it.category, ScatterCategory::Grass)),
                    "grass scatter of {tile:?} emitted a non-grass item"
                );
            }
        }
    }

    #[test]
    fn grass_scatter_disabled_when_no_variants() {
        // radius/disabled is handled by the streamer, but a missing grass asset
        // set (variant_count == 0) must yield no items regardless.
        let terrain = Terrain::new();
        assert!(scatter_grass(&terrain, IVec2::new(0, 0), 0).is_empty());
    }

    #[test]
    fn snap_yaw_returns_cardinal_only() {
        let mut rng = StdRng::seed_from_u64(0xCAFEBABE);
        let valid = [
            0.0,
            std::f32::consts::FRAC_PI_2,
            std::f32::consts::PI,
            std::f32::consts::FRAC_PI_2 * 3.0,
        ];
        for _ in 0..256 {
            let y = snap_yaw(&mut rng);
            assert!(
                valid.iter().any(|v| (y - v).abs() < 1e-5),
                "non-cardinal yaw {y}"
            );
        }
    }

    #[test]
    fn pick_eligible_tree_returns_only_eligible_original_indices() {
        // With the test name spread [tree_large, pine_small, cactus, palm,
        // tree_stump]:
        //  * Desert (["cactus","stump"]) → eligible originals {2, 4}.
        //  * Snow   (["pine"])           → eligible originals {1}.
        //  * Beach  (["palm"])           → eligible originals {3}.
        // Whatever index is rolled, the result must be one of the eligible
        // ORIGINAL indices (never a non-matching variant), so `spawn` indexing
        // `assets.trees[variant]` always lands on a biome-appropriate model.
        let sizes = test_solid_sizes();
        for rolled in 0..sizes.trees.len() {
            let d = pick_eligible_tree(&sizes, &["cactus", "stump"], rolled).expect("desert");
            assert!(d == 2 || d == 4, "desert picked non-cactus/stump index {d}");

            let s = pick_eligible_tree(&sizes, &["pine"], rolled).expect("snow");
            assert_eq!(s, 1, "snow must pick the pine variant");

            let b = pick_eligible_tree(&sizes, &["palm"], rolled).expect("beach");
            assert_eq!(b, 3, "beach must pick the palm variant");
        }
    }

    #[test]
    fn pick_eligible_tree_none_when_biome_matches_no_variant() {
        // A biome whose substrings match no loaded variant yields `None`, so the
        // caller skips trees for that column.
        let sizes = test_solid_sizes();
        for rolled in 0..sizes.trees.len() {
            assert!(pick_eligible_tree(&sizes, &["nonexistent"], rolled).is_none());
        }
        // And an empty allow-set never matches anything.
        assert!(pick_eligible_tree(&sizes, &[], 0).is_none());
    }

    #[test]
    fn solid_scatter_trees_obey_biome_name_subset() {
        // Every TREE the solid worker emits must be a variant eligible for that
        // item's biome (the worker filtered by name). Scan a wide field so several
        // biomes are hit, and check each emitted tree against its biome's policy.
        let terrain = Terrain::new();
        let sizes = test_solid_sizes();
        for tx in -6..=6 {
            for tz in -6..=6 {
                let items = scatter_solid(&terrain, IVec2::new(tx, tz), &sizes);
                for it in &items {
                    if matches!(it.category, ScatterCategory::Tree) {
                        let allowed = super::biome_policy(it.biome).tree_names;
                        let name = &sizes.tree_names[it.variant];
                        assert!(
                            allowed.iter().any(|sub| name.contains(sub)),
                            "tree '{name}' emitted in biome {:?} whose allowed set is {allowed:?}",
                            it.biome
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn grass_never_spawns_in_dry_biomes() {
        // Desert/Snow/Beach have a 0.0 grass multiplier, so no grass item may be
        // emitted on a column in any of those biomes — across a wide field.
        let terrain = Terrain::new();
        for tx in -8..=8 {
            for tz in -8..=8 {
                let items = scatter_grass(&terrain, IVec2::new(tx, tz), 3);
                for it in &items {
                    assert!(
                        super::biome_policy(it.biome).grass_mul > 0.0,
                        "grass emitted in biome {:?} (grass disabled there)",
                        it.biome
                    );
                }
            }
        }
    }
}
