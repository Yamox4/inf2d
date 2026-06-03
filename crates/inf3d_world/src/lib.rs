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
use inf3d_worldgen::{build_noise_lod, sample_height, Terrain, WATER_HEIGHT};

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

/// Default world-space distance (in world units, i.e. voxels) past which
/// terrain chunks begin dropping to coarser LODs. Fallback for when no
/// `QualitySettings` resource is present at build time; in practice the value
/// comes from the active preset's `terrain_lod_distance`.
pub const DEFAULT_TERRAIN_LOD_DISTANCE: f32 = 70.0;

/// Edge length (in voxels) of a chunk's interior. `bevy_voxel_world` fixes
/// this at 32; the padded data/mesh shape is this + 2 for the 1-voxel skirt.
const CHUNK_INTERIOR: u32 = 32;

/// Lowest the procedural sea floor descends (world Y). Bounds how deep water
/// columns generate, so deep ocean reads as proper deep blue without meshing
/// endless underwater voxels.
const SEA_FLOOR_MIN: f64 = -8.0;

/// Highest (coarsest) LOD level we will ask for. LOD `n` halves the per-axis
/// voxel resolution `n` times: interior = 32 >> n. We stop at 3 (interior 4,
/// i.e. each voxel spans 8 world units) — past that the surface noise no
/// longer reads as terrain.
const MAX_TERRAIN_LOD: u8 = 3;

/// Padded data/mesh shape for a given LOD. Halving the interior per level
/// makes each generated voxel cover more world space (the library derives the
/// sampling scale as `CHUNK_SIZE / interior`), so far chunks produce far fewer
/// triangles and far fewer voxel lookups. Interior is clamped to >= 4.
fn lod_padded_shape(lod: u8) -> UVec3 {
    let lod = lod.min(MAX_TERRAIN_LOD);
    let interior = (CHUNK_INTERIOR >> lod).max(4);
    padded_chunk_shape_uniform(interior)
}

#[derive(Resource, Clone)]
pub struct MainWorld {
    pub render_distance_chunks: u32,
    /// World-space distance to the first LOD step (mirrors
    /// [`QualitySettings::terrain_lod_distance`]). Each subsequent LOD band is
    /// this distance wide, so LOD `n` starts at `n * terrain_lod_distance`.
    pub terrain_lod_distance: f32,
}

impl Default for MainWorld {
    fn default() -> Self {
        Self {
            render_distance_chunks: DEFAULT_RENDER_DISTANCE_CHUNKS,
            terrain_lod_distance: DEFAULT_TERRAIN_LOD_DISTANCE,
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
        // Always-resident full-detail core radius around the camera. The rest
        // of the ring streams in/out by pure distance (see the despawn/spawn
        // strategies below), never by frustum — so detail doesn't pop at the
        // edge of the (wide) isometric view.
        6
    }

    fn chunk_despawn_strategy(&self) -> ChunkDespawnStrategy {
        // ISOMETRIC FIX: the default `FarAwayOrOutOfView` despawns any chunk
        // that leaves the camera frustum, which makes terrain visibly
        // disappear at the screen edge when the player scrolls/zooms. Despawn
        // purely by distance instead, so chunks only vanish once they are
        // comfortably outside the visible area (at `spawning_distance`).
        ChunkDespawnStrategy::FarAway
    }

    fn chunk_spawn_strategy(&self) -> ChunkSpawnStrategy {
        // Pair with `FarAway`: spawn every chunk within `spawning_distance`
        // regardless of whether it's currently in view, via a flood fill. This
        // fills the whole radial disc around the camera so panning/zooming
        // never reveals an unspawned hole at the frustum edge.
        ChunkSpawnStrategy::Close
    }

    fn spawning_rays(&self) -> usize {
        // `Close` uses a flood fill rather than view rays, so the per-frame
        // random ray budget can be small (the docs recommend lowering it).
        16
    }

    /// Select a voxel LOD from the chunk's distance to the camera. Band width
    /// is `terrain_lod_distance`; `previous_lod` gives us hysteresis so a
    /// chunk hovering on a boundary doesn't thrash between two LODs (and remesh
    /// every frame). We only step a chunk to the next band once it crosses
    /// 60% / 110% of the boundary depending on direction.
    fn chunk_lod(
        &self,
        chunk_position: IVec3,
        previous_lod: Option<LodLevel>,
        camera_position: Vec3,
    ) -> LodLevel {
        let band = self.terrain_lod_distance.max(1.0);
        let chunk_center = chunk_position.as_vec3() * CHUNK_INTERIOR as f32
            + Vec3::splat(CHUNK_INTERIOR as f32 * 0.5);
        let dist = camera_position.distance(chunk_center);

        // Raw band index from distance.
        let raw = (dist / band).floor() as i64;
        let raw_lod = raw.clamp(0, MAX_TERRAIN_LOD as i64) as u8;

        // Hysteresis: keep the previous LOD unless we've clearly moved into a
        // new band. A 0.25-band dead-zone around each boundary prevents
        // boundary flicker / per-frame remeshing as the camera jitters.
        match previous_lod {
            Some(prev) if raw_lod != prev => {
                let hysteresis = band * 0.25;
                if raw_lod > prev {
                    // Coarsen only once well past the upper boundary.
                    if dist > (prev as f32 + 1.0) * band + hysteresis {
                        raw_lod
                    } else {
                        prev
                    }
                } else {
                    // Refine only once well below the lower boundary.
                    if dist < raw_lod as f32 * band - hysteresis + band {
                        raw_lod.min(prev)
                    } else {
                        prev
                    }
                }
            }
            _ => raw_lod,
        }
    }

    fn chunk_data_shape(&self, lod_level: LodLevel) -> UVec3 {
        lod_padded_shape(lod_level)
    }

    fn chunk_meshing_shape(&self, lod_level: LodLevel) -> UVec3 {
        // Mesh at the same resolution we generated data at.
        lod_padded_shape(lod_level)
    }

    fn chunk_regenerate_strategy(&self) -> ChunkRegenerateStrategy {
        // Rebuild voxel data for the requested shape on LOD change so a
        // coarsened chunk keeps only the cheap coarse payload instead of
        // retaining the full-resolution buffer.
        ChunkRegenerateStrategy::Repopulate
    }

    fn voxel_lookup_delegate(&self) -> VoxelLookupDelegate<Self::MaterialIndex> {
        // Consume the `lod` arg: far chunks generate from fewer noise octaves,
        // which is cheaper and matches the coarser geometry. The block
        // positions handed to the closure are already spaced by the LOD's
        // voxel scale (the library derives that from `chunk_data_shape`), so
        // we don't rescale coordinates here.
        Box::new(move |_chunk_pos, lod, _previous| get_voxel_fn(lod))
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
        let (render_distance_chunks, terrain_lod_distance) = app
            .world()
            .get_resource::<QualitySettings>()
            .map(|q| (q.render_distance_chunks, q.terrain_lod_distance))
            .unwrap_or((
                DEFAULT_RENDER_DISTANCE_CHUNKS,
                DEFAULT_TERRAIN_LOD_DISTANCE,
            ));

        let main_world = MainWorld {
            render_distance_chunks,
            terrain_lod_distance,
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
    // Shadows ON: the sun casts directional shadows. This re-renders every
    // visible chunk per cascade, so it's a heavy cost — if FPS suffers, lower
    // `maximum_distance` above or the render/LOD distances.
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
///
/// `lod` selects how many noise octaves feed the height field: coarser (higher)
/// LODs drop high-frequency octaves, which is cheaper and avoids baking detail
/// the downsampled chunk mesh can't show. The seafloor/water threshold logic is
/// LOD-independent so coastlines stay put.
fn get_voxel_fn(lod: u8) -> Box<dyn FnMut(IVec3, Option<WorldVoxel>) -> WorldVoxel + Send + Sync> {
    let noise = build_noise_lod(lod);
    let mut cache = HashMap::<(i32, i32), f64>::new();

    // Bound the per-worker column cache so it can't grow without limit over a
    // long session. A single chunk column spans 32x32 = 1024 entries; we allow
    // a few chunk-areas' worth (~4) before wholesale-clearing. The cache is a
    // pure memoization of `sample_height`, so dropping it only forces a
    // recompute — correctness is unaffected.
    const CACHE_CAP: usize = CHUNK_INTERIOR as usize * CHUNK_INTERIOR as usize * 4;

    Box::new(move |pos: IVec3, _previous| {
        let key = (pos.x, pos.z);
        let surface = match cache.get(&key) {
            Some(&h) => h,
            None => {
                // Evict everything before we exceed the cap (cheap, amortized).
                if cache.len() >= CACHE_CAP {
                    cache.clear();
                }
                let h = sample_height(&noise, pos.x, pos.z);
                cache.insert(key, h);
                h
            }
        };

        // The sea floor follows the noise surface down into basins (so deep
        // water is genuinely deep and reads as deep blue), bounded by
        // SEA_FLOOR_MIN so columns don't generate endlessly far down.
        let solid_top = surface.max(SEA_FLOOR_MIN);
        if (pos.y as f64) < solid_top {
            // Sandy seafloor (3) for underwater columns, land (0) otherwise —
            // matches the Terrain oracle's land/water split (standing height vs
            // WATER_HEIGHT) so coastlines stay consistent with pathfinding.
            let stand_y = surface.floor().max(0.0) + 1.0;
            if stand_y <= WATER_HEIGHT as f64 {
                WorldVoxel::Solid(3)
            } else {
                WorldVoxel::Solid(0)
            }
        } else {
            WorldVoxel::Air
        }
    })
}
