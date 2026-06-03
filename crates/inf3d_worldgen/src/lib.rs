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

/// Raw terrain height sample (in voxel units) at a world column. Solid voxels
/// fill `y < sample` (plus a sea floor below `y = 1`).
pub fn sample_height(noise: &HybridMulti<Perlin>, x: i32, z: i32) -> f64 {
    noise.get([x as f64 / 1000.0, z as f64 / 1000.0]) * 50.0
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

    /// Y index of the topmost solid voxel in column `(x, z)`.
    pub fn surface_y(&self, x: i32, z: i32) -> i32 {
        (sample_height(&self.noise, x, z).floor() as i32).max(0)
    }

    /// World-space point at the center-top of column `(x, z)` — where an entity
    /// standing on the surface should rest its feet.
    pub fn stand_pos(&self, x: i32, z: i32) -> Vec3 {
        let top = self.surface_y(x, z);
        Vec3::new(x as f32 + 0.5, (top + 1) as f32, z as f32 + 0.5)
    }

    /// Whether a column is walkable land (its surface stands above the water
    /// line). Seafloor flats sit under the water and are not walkable.
    pub fn is_land(&self, x: i32, z: i32) -> bool {
        self.stand_pos(x, z).y > WATER_HEIGHT
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
