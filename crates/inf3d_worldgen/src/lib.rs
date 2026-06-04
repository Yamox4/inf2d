//! Procedural terrain generation and the shared height oracle used by both
//! meshing (on worker threads) and gameplay (pathfinding/standing).

use bevy::prelude::*;
use noise::{HybridMulti, NoiseFn, Perlin};

/// World-space height of the water surface (the `bevy_water` plane). The lowest
/// terrain tier (the seafloor, material 3) stands at `y = 1`; land tiers stand
/// at `y >= 2`. Water at 1.6 therefore submerges only the seafloor flats and
/// leaves land dry. A column is "water" (unwalkable) when its standing height
/// is below this. Single source of truth shared with `water.rs` + pathfinding.
pub const WATER_HEIGHT: f32 = 1.6;

/// Canonical octave count at full (LOD 0) detail.
pub const TERRAIN_OCTAVES: usize = 5;

/// Build the terrain noise with the canonical parameters. Used in two places:
/// the meshing delegate (per worker thread) and the [`Terrain`] gameplay oracle.
pub fn build_noise() -> HybridMulti<Perlin> {
    build_noise_lod(0)
}

/// Build the terrain noise with an octave count reduced for the given voxel
/// LOD level. Coarser LODs (larger `lod`) sample fewer octaves, which both
/// cheapens generation on the worker threads and avoids encoding
/// high-frequency surface detail that a downsampled (coarse) chunk mesh can't
/// represent anyway. The gameplay oracle ([`Terrain`]) always uses LOD 0 so
/// pathfinding/standing stay consistent with the finest visible geometry.
///
/// At least two octaves are always kept so the broad landmass shape survives.
pub fn build_noise_lod(lod: u8) -> HybridMulti<Perlin> {
    let mut noise = HybridMulti::<Perlin>::new(1234);
    noise.octaves = TERRAIN_OCTAVES.saturating_sub(lod as usize).max(2);
    noise.frequency = 1.1;
    noise.lacunarity = 2.8;
    noise.persistence = 0.4;
    noise
}

/// Vertical bias (in voxels) added to EVERY terrain height sample — THE knob for
/// the land/water balance. The terrain noise (`HybridMulti<Perlin>`) sits near 0,
/// and a column is WATER whenever its height is below 1.0 (see
/// [`ColumnKind::from_height`] + [`WATER_HEIGHT`]), so with no bias roughly half
/// the world falls below the water line and reads as ocean. Lifting every column
/// shifts the coastline out to lower-noise columns → more dry land. Coasts stay
/// gentle (a column right at the waterline still has height ~1, because the noise
/// is smooth) and deep basins stay ocean. Each +1 lowers the water cutoff by
/// ~0.02 in noise units. Raise for more land, lower (toward 0) for more water.
pub const LAND_BIAS: f64 = 6.0;

/// Raw terrain height sample (in voxel units) at a world column. Solid voxels
/// fill `y < sample` (plus a sea floor below `y = 1`). Includes [`LAND_BIAS`], so
/// meshing and the [`Terrain`] oracle stay consistent (both go through here).
pub fn sample_height(noise: &HybridMulti<Perlin>, x: i32, z: i32) -> f64 {
    noise.get([x as f64 / 1000.0, z as f64 / 1000.0]) * 50.0 + LAND_BIAS
}

/// Resolved classification of a single terrain column. The single source of
/// truth for the land/water/seafloor *classification*: both the [`Terrain`]
/// gameplay oracle and `inf3d_world::get_voxel_fn` (the meshing delegate on
/// worker threads) derive their land/water answer from [`column_kind`], so the
/// surface a player stands/pathfinds on agrees with the material picked for a
/// voxel — given the same sampled height. The height itself is not identical
/// everywhere: meshing may sample LOD-reduced noise for far chunks (shifting
/// their visual coastline), whereas the oracle always samples LOD-0; see
/// `inf3d_world::get_voxel_fn` for the full caveats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnKind {
    /// Y index of the topmost solid voxel in the column (>= 0).
    pub surface_y: i32,
    /// Whether the column's standing height sits below [`WATER_HEIGHT`], i.e.
    /// it is submerged seafloor (unwalkable) rather than dry land.
    pub is_water: bool,
}

impl ColumnKind {
    /// World Y an entity standing on this column rests its feet at: the top
    /// face of the topmost solid voxel.
    pub fn stand_y(self) -> i32 {
        self.surface_y + 1
    }

    /// Classify a column from an already-sampled raw height (voxel units). The
    /// pure core of the land/water split, factored out so the meshing closure
    /// — which memoizes the raw `sample_height` per worker — can reuse its
    /// cached value instead of re-sampling the noise, while still going through
    /// the *exact* same logic as [`column_kind`].
    pub fn from_height(height: f64) -> Self {
        let surface_y = (height.floor() as i32).max(0);
        // Standing height = top face of the topmost solid voxel. A column is
        // water when an entity standing there would be at or below the water
        // line. LOD-independent (it only depends on the standing height vs
        // `WATER_HEIGHT`) so coastlines stay put across LODs.
        let is_water = (surface_y + 1) as f32 <= WATER_HEIGHT;
        Self {
            surface_y,
            is_water,
        }
    }
}

/// Classify a single column. The `noise` is the only per-call input so callers
/// can pass an LOD-reduced noise (meshing worker) or the canonical LOD-0 noise
/// (gameplay oracle); both run the *same* classification, but on whatever height
/// their noise samples — so an LOD-reduced caller can land on a slightly
/// different coastline than the LOD-0 oracle (the oracle is the navigation
/// authority). Thin wrapper over [`ColumnKind::from_height`] for callers that
/// haven't already sampled the height.
pub fn column_kind(noise: &HybridMulti<Perlin>, x: i32, z: i32) -> ColumnKind {
    ColumnKind::from_height(sample_height(noise, x, z))
}

/// Gameplay-side terrain oracle: deterministic surface heights that match the
/// meshed geometry, available regardless of which chunks are currently loaded.
///
/// `Clone` is cheap (just a copy of the noise parameters) so worker threads can
/// snapshot the oracle and run searches off the main thread.
#[derive(Resource, Clone)]
pub struct Terrain {
    noise: HybridMulti<Perlin>,
}

impl Terrain {
    /// Construct a terrain oracle from the canonical noise parameters.
    pub fn new() -> Self {
        Self {
            noise: build_noise(),
        }
    }

    /// Classify the column at `(x, z)` from the oracle's (LOD-0) noise. The one
    /// helper that all the public accessors below delegate to, so the oracle
    /// applies the *same* land/water classification as the meshing closure
    /// (which calls [`column_kind`] directly). The oracle always samples LOD-0,
    /// so it is the authority navigation trusts where an LOD-reduced far chunk's
    /// visual coastline would differ.
    fn column(&self, x: i32, z: i32) -> ColumnKind {
        column_kind(&self.noise, x, z)
    }

    /// Y index of the topmost solid voxel in column `(x, z)`.
    pub fn surface_y(&self, x: i32, z: i32) -> i32 {
        self.column(x, z).surface_y
    }

    /// World-space point at the center-top of column `(x, z)` — where an entity
    /// standing on the surface should rest its feet.
    pub fn stand_pos(&self, x: i32, z: i32) -> Vec3 {
        let kind = self.column(x, z);
        Vec3::new(x as f32 + 0.5, kind.stand_y() as f32, z as f32 + 0.5)
    }

    /// Whether a column is walkable land (its surface stands above the water
    /// line). Seafloor flats sit under the water and are not walkable.
    pub fn is_land(&self, x: i32, z: i32) -> bool {
        !self.column(x, z).is_water
    }

    /// Nearest land column to `start` (spiral ring search), so entities never
    /// spawn in the water. Falls back to `start` if nothing is found nearby.
    pub fn nearest_land(&self, start: IVec2) -> IVec2 {
        if self.is_land(start.x, start.y) {
            return start;
        }
        for r in 1..256i32 {
            // Walk only the perimeter of the radius-`r` square (O(perimeter),
            // not O((2r+1)^2)). We preserve the original visit order — outer
            // loop over `dx` ascending, inner over `dz` ascending — so ties
            // (equidistant land cells) resolve to the same cell as before:
            //   * on the left/right edge columns (|dx| == r) every dz is on the
            //     ring, so we scan the full -r..=r column;
            //   * on interior columns (|dx| < r) only dz == -r and dz == r lie
            //     on the ring.
            for dx in -r..=r {
                if dx.abs() == r {
                    for dz in -r..=r {
                        let c = IVec2::new(start.x + dx, start.y + dz);
                        if self.is_land(c.x, c.y) {
                            return c;
                        }
                    }
                } else {
                    for dz in [-r, r] {
                        let c = IVec2::new(start.x + dx, start.y + dz);
                        if self.is_land(c.x, c.y) {
                            return c;
                        }
                    }
                }
            }
        }
        start
    }
}
