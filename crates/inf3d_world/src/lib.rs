//! Voxel world configuration and scene lighting. Procedural terrain and the
//! shared height oracle live in `inf3d_worldgen`.

use std::sync::Arc;

use bevy::{
    light::{CascadeShadowConfigBuilder, DirectionalLightShadowMap},
    platform::collections::HashMap,
    prelude::*,
};
use bevy_voxel_world::prelude::*;
use inf3d_core::QualitySettings;
use inf3d_worldgen::{build_noise_lod, sample_height, ColumnKind, Terrain};

pub mod terrain_material;

use terrain_material::install_terrain_material;

/// Canonical voxel material palette. The single source of truth for what each
/// `MainWorld::MaterialIndex` (`u8`) value means; consumed by [`get_voxel_fn`]
/// (which voxel value to emit), [`MainWorld::texture_index_mapper`] (which
/// texture-array layers to sample per face), the texture palette order in
/// [`terrain_material::install_terrain_material`]'s `build_terrain_texture`,
/// and `inf3d_ui::material_name` (which must align its labels to these values).
///
/// The discriminants double as both the meshing material index *and* the
/// texture-array layer index for a single-texture (all-face) material, so the
/// numeric order here is also the layer order in the procedural texture array.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum TerrainMaterialId {
    /// Dry-land surface. Top face shows grass; exposed sides show dirt and the
    /// bottom shows stone (see [`MainWorld::texture_index_mapper`]).
    Grass = 0,
    /// Earthy mid-tone. Used on the exposed *sides* of land voxels.
    Dirt = 1,
    /// Grey rock. Used on the *bottom* faces of land voxels (and reads as the
    /// underground filler beneath the surface layer).
    Stone = 2,
    /// Sandy seafloor for submerged columns. All faces sample this single
    /// layer; it shows through the translucent water plane.
    Seafloor = 3,
}

impl TerrainMaterialId {
    /// Texture-array layer index for a uniform (all-face) material — equal to
    /// the discriminant. Per-face mixing for land happens in
    /// [`MainWorld::texture_index_mapper`].
    const fn layer(self) -> u32 {
        self as u32
    }

    /// Canonical player-facing label for this material, shown in the HUD's
    /// hovered-tile readout (`inf3d_ui::material_name`). The single source of
    /// truth for these strings: keeping them here means a discriminant or
    /// meaning change to the enum can't silently desync the HUD labels.
    ///
    /// Note `Seafloor` reads as "Water" to the player: it's the voxel under a
    /// submerged column, i.e. what the cursor lands on over the (unwalkable)
    /// water plane, so the gameplay-relevant name is "Water".
    pub const fn label(self) -> &'static str {
        // Derived from the single PALETTE table (indexed by discriminant), so a
        // material's label can never desync from its texture / index — see PALETTE.
        PALETTE[self as usize].label
    }

    /// Map a raw `MainWorld::MaterialIndex` (`u8`) back to its
    /// `TerrainMaterialId`, returning `None` for indices outside the palette.
    /// Lets consumers (e.g. the HUD) recover the canonical variant — and its
    /// [`label`](Self::label) — without hand-keeping a parallel match on the
    /// discriminants.
    pub const fn from_index(index: u8) -> Option<Self> {
        // A valid index is exactly a row in the single PALETTE table.
        if (index as usize) < PALETTE.len() {
            Some(PALETTE[index as usize].id)
        } else {
            None
        }
    }
}

/// One terrain material's data — the SINGLE source of truth for the palette, one
/// row per [`TerrainMaterialId`] variant IN DISCRIMINANT ORDER. Adding a block =
/// add an enum variant AND a row here (in order); the `palette_matches_enum` test
/// fails loudly if they ever drift, so a new material can't silently mis-texture.
/// The HUD label ([`TerrainMaterialId::label`]), [`TerrainMaterialId::from_index`],
/// the procedural texture-array colors + layer count (in
/// [`terrain_material`](crate::terrain_material)), and the per-face layer mapper
/// ([`MainWorld::texture_index_mapper`]) are ALL derived from this one table.
pub(crate) struct MaterialDef {
    /// The variant this row describes. Must satisfy `PALETTE[i].id as u8 == i`.
    pub id: TerrainMaterialId,
    /// Player-facing HUD label.
    pub label: &'static str,
    /// RGB of this material's own texture-array layer (procedurally tinted).
    pub color: [u8; 3],
    /// Texture-array layers sampled per face — `[top, side, bottom]`. A uniform
    /// material repeats its own layer; land mixes grass-cap / dirt-side / stone-base.
    pub faces: [u32; 3],
}

/// The canonical material palette. Order MUST match the [`TerrainMaterialId`]
/// discriminants (guarded by the `palette_matches_enum` test).
pub(crate) const PALETTE: [MaterialDef; 4] = [
    MaterialDef {
        id: TerrainMaterialId::Grass,
        label: "Grass",
        color: [0x4f, 0x7a, 0x35],
        faces: [
            TerrainMaterialId::Grass.layer(),
            TerrainMaterialId::Dirt.layer(),
            TerrainMaterialId::Stone.layer(),
        ],
    },
    MaterialDef {
        id: TerrainMaterialId::Dirt,
        label: "Dirt",
        color: [0x6b, 0x4a, 0x2c],
        faces: [TerrainMaterialId::Dirt.layer(); 3],
    },
    MaterialDef {
        id: TerrainMaterialId::Stone,
        label: "Stone",
        color: [0x6e, 0x6f, 0x72],
        faces: [TerrainMaterialId::Stone.layer(); 3],
    },
    MaterialDef {
        // Seafloor reads as "Water" to the player (the voxel under a submerged
        // column). Sandy/tan shows through bevy_water's translucent surface.
        id: TerrainMaterialId::Seafloor,
        label: "Water",
        color: [0xd4, 0xc1, 0x88],
        faces: [TerrainMaterialId::Seafloor.layer(); 3],
    },
];

/// Fixed-high chunk radius streamed around the camera when no `QualitySettings`
/// resource is present. Used only as a fallback — in practice `CorePlugin`
/// installs the resource before this plugin builds.
pub const DEFAULT_RENDER_DISTANCE_CHUNKS: u32 = 10;

/// Default world-space distance (in world units, i.e. voxels) past which
/// terrain chunks begin dropping to coarser LODs. Fallback for when no
/// `QualitySettings` resource is present at build time; the camera raises this
/// dynamically from the current orthographic footprint.
pub const DEFAULT_TERRAIN_LOD_DISTANCE: f32 = 165.0;

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
    /// Ground-space point that terrain LOD should be centered on. This is
    /// intentionally separate from the render camera eye: the iso camera sits high
    /// and far from the player to avoid clipping through mountains, and using that
    /// eye position for LOD makes chunks around the player look "far" and coarsen
    /// right underfoot. The camera plugin updates this to the followed player; if
    /// it has not run yet, `chunk_lod` falls back to the camera XZ position.
    pub lod_focus_xz: Option<Vec2>,
    /// World-space distance to the first LOD step (mirrors
    /// [`QualitySettings::terrain_lod_distance`]). Each subsequent LOD band is
    /// this distance wide, so LOD `n` starts at `n * terrain_lod_distance`.
    pub terrain_lod_distance: f32,
}

impl Default for MainWorld {
    fn default() -> Self {
        Self {
            render_distance_chunks: DEFAULT_RENDER_DISTANCE_CHUNKS,
            lod_focus_xz: None,
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
        //
        // Scale from the active spawn radius so the protected core never becomes
        // as large as the whole spawned disc. Keep a >= 1 core but always leave a
        // couple of rings that can stream by distance.
        self.render_distance_chunks.saturating_sub(2).max(1)
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

    fn max_spawn_per_frame(&self) -> usize {
        // STUTTER FIX: telemetry showed per-frame chunk spawn/despawn BURSTS up
        // to chunk+1143 in a single frame (the catastrophic initial-fill /
        // fast-travel backlog dumped at once) — the hard hitch. The library
        // default is 10000 (effectively unbounded), so it drains the whole
        // backlog in one frame. We cap it to just above the cost of ONE normal
        // chunk-boundary crossing so ordinary walking never clamps (and never
        // spills fill into following frames), while the initial fill / teleport
        // bursts still spread over several frames.
        //
        // A boundary crossing reveals a perpendicular FACE of the spawned disc;
        // derive the cap from the active `render_distance_chunks` plus a small
        // margin so ordinary walking does not clamp the spawn queue.
        let span = 2 * self.render_distance_chunks as usize + 1;
        span * span + 32
    }

    fn chunk_y_bounds(&self) -> Option<(i32, i32)> {
        // PERF (vertical clamp — the big structural win): stock bevy_voxel_world
        // spawns a full 3D SPHERE of chunk entities around the camera, but this
        // is a heightfield world — all terrain lives in a shallow, FIXED band of
        // chunk layers regardless of where the (always-overhead) camera sits, so
        // most of the sphere is empty air above / fully-solid invisible chunks
        // below. This (vendored-fork) hook clamps streaming to that band.
        //
        // Chunks are 32 voxels tall. Solid terrain spans `SEA_FLOOR_MIN` (-8,
        // chunk -1) up through the noise surface (`sample_height` = noise*50,
        // realistically < ~100, chunk <= 3). Below chunk -1 every column is
        // fully solid and has no exposed faces (invisible); the player walks the
        // analytic surface, so we never need those underground chunks. Band
        // `[-1, 3]` (world y -32..127) covers every visible face with headroom
        // for the tallest peaks while dropping the ~12 wasted layers the sphere
        // would otherwise stream.
        Some((-1, 3))
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
        // LOD is a ground/readability decision, not an eye-distance decision.
        // The iso camera can be high and horizontally offset from the player; using
        // full 3D distance to the camera eye makes the area around the player count
        // as far terrain and causes coarse LODs to "load on top of" gameplay.
        // Center the rings on the followed ground focus instead, ignoring height.
        let focus = self
            .lod_focus_xz
            .unwrap_or_else(|| Vec2::new(camera_position.x, camera_position.z));
        let dist = focus.distance(Vec2::new(chunk_center.x, chunk_center.z));

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
        // The returned `[u32; 3]` is `[top, side, bottom]` — the shader samples
        // `tex_idx[tex_face]` where `tex_face` is 0/1/2 for top/side/bottom (picked
        // per-vertex from the axis-aligned face normal). Per-face layers come
        // straight from the single PALETTE table; an unknown index falls back to
        // the grass land cap so a stray material still reads as terrain.
        Arc::new(|mat| {
            PALETTE
                .get(mat as usize)
                .map(|d| d.faces)
                .unwrap_or(PALETTE[TerrainMaterialId::Grass as usize].faces)
        })
    }
}

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        // Read QualitySettings (installed by inf3d_core::CorePlugin earlier in
        // the plugin chain). If absent — e.g. someone forgot to register
        // CorePlugin — we fall back to fixed high defaults.
        let (render_distance_chunks, terrain_lod_distance) = app
            .world()
            .get_resource::<QualitySettings>()
            .map(|q| (q.render_distance_chunks, q.terrain_lod_distance))
            .unwrap_or((DEFAULT_RENDER_DISTANCE_CHUNKS, DEFAULT_TERRAIN_LOD_DISTANCE));

        let main_world = MainWorld {
            render_distance_chunks,
            lod_focus_xz: None,
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

        app.add_plugins(VoxelWorldPlugin::with_config(main_world).with_material(terrain_material))
            .insert_resource(Terrain::new())
            .insert_resource(DirectionalLightShadowMap { size: 4096 })
            .add_systems(Startup, setup_lighting);
    }
}

fn setup_lighting(mut commands: Commands) {
    info!("inf3d: left-click the ground to move the player (A* over the voxel surface).");

    let cascade_shadow_config = CascadeShadowConfigBuilder {
        maximum_distance: 240.0,
        num_cascades: 3,
        overlap_proportion: 0.35,
        ..default()
    }
    .build();

    commands.spawn((
        DirectionalLight {
            color: Color::srgb(1.0, 0.82, 0.58),
            illuminance: 12_000.0,
            shadows_enabled: true,
            ..default()
        },
        // Low warm late-afternoon sun: longer shadows and a cozier terrain read.
        // The previous y-heavy direction (-0.35, -0.75, 0.35) put the sun high in
        // the sky, so shadows were technically enabled but visually very short.
        Transform::from_xyz(0.0, 0.0, 0.0).looking_at(Vec3::new(-0.60, -0.35, 0.60), Vec3::Y),
        cascade_shadow_config,
    ));

    // Soft sky fill, kept below the sun so shadows still have readable contrast.
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.78, 0.82, 0.90),
        brightness: 230.0,
        affects_lightmapped_meshes: true,
    });
}

/// Per-chunk voxel lookup closure (runs on worker threads). Solidity and the
/// land/water split both derive from the shared [`inf3d_worldgen`] column
/// helpers ([`ColumnKind`]), so the *classification* (land vs water) a player
/// stands/pathfinds on agrees with the material picked for each meshed voxel.
///
/// Two honest caveats to that agreement:
///   - The meshed seafloor descends to `SEA_FLOOR_MIN`, which is below the
///     oracle's `surface_y.max(0)` for submerged columns. Harmless: those
///     columns are unwalkable water, so gameplay never reads that deep bottom.
///   - `lod` selects how many noise octaves feed the height field (coarser LODs
///     drop high-frequency octaves — cheaper, and they avoid baking detail the
///     downsampled mesh can't show). Because that changes the sampled height, a
///     far chunk's *visual* coastline can shift slightly from the full-res one.
///     The gameplay oracle ([`Terrain`]) always samples LOD-0 noise, so
///     navigation stays consistent with the finest (near-camera) geometry.
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
            // Classify via the same helper the `Terrain` oracle uses (off the
            // cached raw height, so no extra noise sample) — seafloor for
            // submerged columns, land otherwise — so coastlines stay consistent
            // with pathfinding. Emit the canonical material indices.
            let mat = if ColumnKind::from_height(surface).is_water {
                TerrainMaterialId::Seafloor
            } else {
                TerrainMaterialId::Grass
            };
            WorldVoxel::Solid(mat as u8)
        } else {
            WorldVoxel::Air
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PALETTE is the single source of truth, indexed by discriminant. This guards
    /// the invariant every consumer relies on: row `i` describes the variant whose
    /// discriminant is `i`, the table covers EVERY variant exactly once, and
    /// `from_index` / `label` round-trip through it. If someone adds a variant but
    /// forgets a row (or mis-orders them), this fails loudly here instead of
    /// silently mis-texturing terrain in-game (the desync this table exists to kill).
    #[test]
    fn palette_matches_enum() {
        use TerrainMaterialId::*;
        // Every variant, listed once. Adding a variant means updating this list,
        // the enum, AND PALETTE — and any mismatch trips an assertion below.
        let all = [Grass, Dirt, Stone, Seafloor];
        assert_eq!(
            PALETTE.len(),
            all.len(),
            "PALETTE must have exactly one row per TerrainMaterialId variant"
        );
        for (i, def) in PALETTE.iter().enumerate() {
            assert_eq!(def.id as usize, i, "PALETTE[{i}] must describe discriminant {i}");
            assert_eq!(
                TerrainMaterialId::from_index(i as u8),
                Some(def.id),
                "from_index({i}) must round-trip to PALETTE[{i}].id"
            );
            assert_eq!(def.id.label(), def.label, "label() must equal the table label");
        }
        // Out-of-range indices have no material.
        assert_eq!(TerrainMaterialId::from_index(PALETTE.len() as u8), None);
    }
}
