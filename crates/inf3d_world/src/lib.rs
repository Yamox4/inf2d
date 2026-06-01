//! Voxel world configuration and scene lighting. Procedural terrain and the
//! shared height oracle live in `inf3d_worldgen`.

use std::sync::Arc;

use bevy::{
    light::{CascadeShadowConfigBuilder, VolumetricLight},
    platform::collections::HashMap,
    prelude::*,
};
use bevy_voxel_world::prelude::*;
use inf3d_worldgen::{build_noise, sample_height, Terrain};

/// Chunk radius streamed around the camera. Each chunk is 32 voxels per side, so
/// this is a large view distance; lower it if streaming hitches on weaker GPUs.
pub const RENDER_DISTANCE_CHUNKS: u32 = 40;

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

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(VoxelWorldPlugin::with_config(MainWorld))
            .insert_resource(Terrain::new())
            .add_systems(Startup, setup_lighting);
    }
}

fn setup_lighting(mut commands: Commands) {
    info!("inf3d: left-click the ground to move the player (A* over the voxel surface).");

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
