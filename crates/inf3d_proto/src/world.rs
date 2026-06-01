//! Voxel world configuration, procedural terrain, and the shared height oracle
//! used by both meshing (on worker threads) and gameplay (pathfinding/standing).

use std::sync::Arc;

use bevy::{
    light::{CascadeShadowConfigBuilder, VolumetricLight},
    platform::collections::HashMap,
    prelude::*,
};
use bevy_voxel_world::prelude::*;
use noise::{HybridMulti, NoiseFn, Perlin};

/// Chunk radius streamed around the camera. Each chunk is 32 voxels per side, so
/// this is a large view distance; lower it if streaming hitches on weaker GPUs.
pub const RENDER_DISTANCE_CHUNKS: u32 = 40;

/// World-space height of the water surface (the `bevy_water` plane). The lowest
/// terrain tier (the seafloor, material 3) stands at `y = 1`; land tiers stand
/// at `y >= 2`. Water at 1.6 therefore submerges only the seafloor flats and
/// leaves land dry. A column is "water" (unwalkable) when its standing height
/// is below this. Single source of truth shared with `water.rs` + pathfinding.
pub const WATER_HEIGHT: f32 = 1.6;

#[derive(Resource, Clone, Default)]
pub struct MainWorld;

impl VoxelWorldConfig for MainWorld {
    type MaterialIndex = u8;
    type ChunkUserBundle = ();

    fn spawning_distance(&self) -> u32 {
        RENDER_DISTANCE_CHUNKS
    }

    fn min_despawn_distance(&self) -> u32 {
        // Keep a small always-resident core; let the rest stream out past the ring.
        4
    }

    fn voxel_lookup_delegate(&self) -> VoxelLookupDelegate<Self::MaterialIndex> {
        Box::new(move |_chunk_pos, _lod, _previous| get_voxel_fn())
    }

    fn texture_index_mapper(&self) -> Arc<dyn Fn(Self::MaterialIndex) -> [u32; 3] + Send + Sync> {
        Arc::new(|mat| match mat {
            0 => [0, 0, 0],
            1 => [1, 1, 1],
            2 => [2, 2, 2],
            3 => [3, 3, 3],
            _ => [0, 0, 0],
        })
    }
}

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
fn sample_height(noise: &HybridMulti<Perlin>, x: i32, z: i32) -> f64 {
    noise.get([x as f64 / 1000.0, z as f64 / 1000.0]) * 50.0
}

/// Gameplay-side terrain oracle: deterministic surface heights that match the
/// meshed geometry, available regardless of which chunks are currently loaded.
#[derive(Resource)]
pub struct Terrain {
    noise: HybridMulti<Perlin>,
}

impl Terrain {
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

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(VoxelWorldPlugin::with_config(MainWorld))
            .insert_resource(Terrain {
                noise: build_noise(),
            })
            .add_systems(Startup, setup_lighting);
    }
}

fn setup_lighting(mut commands: Commands) {
    info!("inf3d_proto: left-click the ground to move the player (A* over the voxel surface).");

    let cascade_shadow_config = CascadeShadowConfigBuilder {
        maximum_distance: 700.0,
        ..default()
    }
    .build();
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(0.98, 0.95, 0.82),
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 0.0).looking_at(Vec3::new(-0.15, -0.1, 0.15), Vec3::Y),
        cascade_shadow_config,
        // Lets the sun scatter through the volumetric fog (god-ray feel).
        VolumetricLight,
    ));

    // Cool, lifted ambient so shadowed basins read as foggy haze rather than
    // pure black (pairs with the atmospheric fog).
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.80, 0.86, 0.96),
        brightness: 350.0,
        affects_lightmapped_meshes: true,
    });
}

/// Per-chunk voxel lookup closure (runs on worker threads). Must stay in sync
/// with [`sample_height`]: solid where `y < sample`, with a sea floor below 1.
fn get_voxel_fn() -> Box<dyn FnMut(IVec3, Option<WorldVoxel>) -> WorldVoxel + Send + Sync> {
    let noise = build_noise();
    let mut cache = HashMap::<(i32, i32), f64>::new();

    Box::new(move |pos: IVec3, _previous| {
        if pos.y < 1 {
            return WorldVoxel::Solid(3);
        }

        let is_ground = (pos.y as f64)
            < *cache
                .entry((pos.x, pos.z))
                .or_insert_with(|| sample_height(&noise, pos.x, pos.z));

        if is_ground {
            WorldVoxel::Solid(0)
        } else {
            WorldVoxel::Air
        }
    })
}
