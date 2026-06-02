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

/// Build the terrain noise with the canonical parameters. Used in two places:
/// the meshing delegate (per worker thread) and the [`Terrain`] gameplay oracle.
pub fn build_noise() -> HybridMulti<Perlin> {
    let mut noise = HybridMulti::<Perlin>::new(1234);
    noise.octaves = 5;
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
            for dx in -r..=r {
                for dz in -r..=r {
                    // Only the outer ring at radius r (avoids re-checking inner cells).
                    if dx.abs() != r && dz.abs() != r {
                        continue;
                    }
                    let c = IVec2::new(start.x + dx, start.y + dz);
                    if self.is_land(c.x, c.y) {
                        return c;
                    }
                }
            }
        }
        start
    }
}
