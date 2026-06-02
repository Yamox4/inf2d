//! Voxel world configuration and scene lighting. Procedural terrain and the
//! shared height oracle live in `inf3d_worldgen`.

use std::sync::Arc;

use bevy::{
    light::{CascadeShadowConfigBuilder, VolumetricLight},
    platform::collections::HashMap,
    prelude::*,
};
use bevy_voxel_world::prelude::*;
use inf3d_core::QualitySettings;
use inf3d_worldgen::{build_noise, sample_height, Terrain};

pub mod terrain_material;

use terrain_material::install_terrain_material;

/// Default chunk radius streamed around the camera when no `QualitySettings`
/// resource is present. Used only as a fallback — in practice `CorePlugin`
/// installs the resource before this plugin builds, and the value comes from
/// the active [`QualityPreset`](inf3d_core::QualityPreset).
///
/// Runtime preset changes do **not** alter render distance: the underlying
/// `VoxelWorldPlugin` reads it once at `with_config` time and cannot be
/// re-registered. Restart the app to apply a new render distance.
pub const DEFAULT_RENDER_DISTANCE_CHUNKS: u32 = 16;

#[derive(Resource, Clone)]
pub struct MainWorld {
    pub render_distance_chunks: u32,
}

impl Default for MainWorld {
    fn default() -> Self {
        Self {
            render_distance_chunks: DEFAULT_RENDER_DISTANCE_CHUNKS,
        }
    }
}

impl VoxelWorldConfig for MainWorld {
    type MaterialIndex = u8;
    type ChunkUserBundle = ();

    fn spawning_distance(&self) -> u32 {
        self.render_distance_chunks
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
        // Read QualitySettings (installed by inf3d_core::CorePlugin earlier in
        // the plugin chain). If absent — e.g. someone forgot to register
        // CorePlugin — we fall back to the default preset's distance.
        let render_distance_chunks = app
            .world()
            .get_resource::<QualitySettings>()
            .map(|q| q.render_distance_chunks)
            .unwrap_or(DEFAULT_RENDER_DISTANCE_CHUNKS);

        let main_world = MainWorld {
            render_distance_chunks,
        };

        // Build the custom voxel terrain material (procedural texture array
        // + forward shader that delegates the prepass to StandardMaterial)
        // and hand the value to `VoxelWorldPlugin::with_material`. The voxel
        // plugin then:
        //   - clones the value into `Assets<TerrainMaterial>`,
        //   - stores the resulting handle in
        //     `VoxelWorldMaterialHandle<TerrainMaterial>`,
        //   - and runs `assign_material::<TerrainMaterial>` on every chunk
        //     entity that needs a material.
        //
        // Crucially this swaps out `StandardVoxelMaterial` (whose
        // `enable_prepass() -> false`) for an `ExtendedMaterial<…, …>` whose
        // extension returns `enable_prepass() -> true`, so voxel terrain
        // finally writes the depth + normal prepass.
        let terrain_material = install_terrain_material(app);

        app.add_plugins(
            VoxelWorldPlugin::with_config(main_world).with_material(terrain_material),
        )
        .insert_resource(Terrain::new())
        .add_systems(Startup, setup_lighting);
    }
}

fn setup_lighting(mut commands: Commands) {
    info!("inf3d: left-click the ground to move the player (A* over the voxel surface).");

    // Cascade max_distance was 700 — at that range each shadow texel covers many
    // world units and produces a grid-like speckle pattern on the voxel terrain
    // (visible as a "second grey layer" over the green), and the shadow pass
    // itself becomes very expensive. 120 is plenty for the iso view.
    let cascade_shadow_config = CascadeShadowConfigBuilder {
        maximum_distance: 120.0,
        ..default()
    }
    .build();
    // Shadows OFF: with ~1700 voxel chunks the 4-cascade pass tanked FPS
    // to ~2. Prepass-aware shadows still re-render every chunk per cascade.
    // Keep the `cascade_shadow_config` wired in case a future High/Ultra
    // preset re-enables this on more powerful hardware.
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(0.98, 0.95, 0.82),
            shadows_enabled: false,
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
        // Sea floor sits below the water line.
        if pos.y < 1 {
            return WorldVoxel::Solid(3);
        }

        let surface = *cache
            .entry((pos.x, pos.z))
            .or_insert_with(|| sample_height(&noise, pos.x, pos.z));

        if (pos.y as f64) < surface {
            WorldVoxel::Solid(0)
        } else {
            WorldVoxel::Air
        }
    })
}
