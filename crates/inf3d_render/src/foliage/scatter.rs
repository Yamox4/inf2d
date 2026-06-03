//! The off-thread per-tile scatter worker.
//!
//! [`scatter_tile`] runs on the [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool):
//! it decides, per column in a tile, whether the column is land and what prop
//! variant + position + yaw sits there, returning plain [`ScatterItem`]s. It
//! touches no ECS or asset state — only a cloned [`Terrain`] snapshot and the
//! per-variant footprint [`VariantSizes`] — so the main thread is left with just
//! entity spawning (see [`super::spawn`]).

use bevy::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use inf3d_worldgen::{Terrain, WATER_HEIGHT};

use super::{footprint_radius, ScatterCategory, ScatterItem, VariantSizes, TILE};

// Per-column probability of spawning each foliage category.
const TREE_DENSITY: f32 = 0.004;
const GRASS_DENSITY: f32 = 0.018;
const ROCK_DENSITY: f32 = 0.002;

/// Decide, per column in `tile`, whether it's land and what prop variant +
/// position + yaw goes there. Returns plain [`ScatterItem`]s.
///
/// Determinism: the RNG is seeded purely from the tile coordinate and consumed
/// in a fixed scan order, so the same tile always produces the same scatter.
///
/// `cheap_lod`: skip grass entirely. A tile is cheap-LOD when it is past the
/// camera-relative `foliage_lod_distance` OR outside the player-relative
/// `grass_radius_world` (see [`super::stream`]). Grass is the densest,
/// collider-free category, so dropping it is the cheap LOD — those tiles keep
/// only their sparse solid props (trees/rocks still stream to the iso edges).
///
/// The returned vec is sorted by (category, variant) so the main thread spawns
/// all instances of one variant contiguously. Bevy auto-batches instances that
/// share a mesh handle + material; spawning grouped (instead of per-column
/// interleaved) keeps those batches from fragmenting. This sort changes only
/// spawn order, never which items exist or where they sit — determinism holds.
pub(super) fn scatter_tile(
    terrain: &Terrain,
    tile: IVec2,
    sizes: &VariantSizes,
    cheap_lod: bool,
) -> Vec<ScatterItem> {
    let seed = (tile.x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (tile.y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    let mut rng = StdRng::seed_from_u64(seed);

    let base_x = tile.x * TILE;
    let base_z = tile.y * TILE;

    // Footprints (XZ center + radius) of solid props already placed in this
    // tile. Solid props (trees/rocks) must not inter-penetrate, so each
    // candidate is rejected if its footprint disc overlaps a placed one. Grass
    // is exempt and never recorded here — it may overlap freely.
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
            let yaw = snap_yaw(&mut rng);
            let xz = Vec2::new(pos.x, pos.z);

            if !sizes.trees.is_empty() && rng.random::<f32>() < TREE_DENSITY {
                let variant = rng.random_range(0..sizes.trees.len());
                if try_place_solid(&mut solid_footprints, xz, sizes.trees[variant]) {
                    items.push(ScatterItem {
                        category: ScatterCategory::Tree,
                        variant,
                        pos,
                        yaw,
                    });
                }
                continue;
            }
            if !sizes.rocks.is_empty() && rng.random::<f32>() < ROCK_DENSITY {
                let variant = rng.random_range(0..sizes.rocks.len());
                if try_place_solid(&mut solid_footprints, xz, sizes.rocks[variant]) {
                    items.push(ScatterItem {
                        category: ScatterCategory::Rock,
                        variant,
                        pos,
                        yaw,
                    });
                }
                continue;
            }
            if !cheap_lod && !sizes.grass.is_empty() && rng.random::<f32>() < GRASS_DENSITY {
                let variant = rng.random_range(0..sizes.grass.len());
                items.push(ScatterItem {
                    category: ScatterCategory::Grass,
                    variant,
                    pos,
                    yaw,
                });
            }
        }
    }

    // Group by category+variant for batch-friendly spawn order (see doc above).
    items.sort_by_key(|it| (category_rank(it.category), it.variant));
    items
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
        if center.distance_squared(*c) < (r + pr) * (r + pr) {
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
}
