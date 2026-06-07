//! Voxel foliage scatter: trees, grass clumps, and rocks loaded from the
//! MagicaVoxel `.vox` files under `assets/foliage/vox/{Trees,Rocks,Grass}/`.
//!
//! We parse each `.vox` file with [`dot_vox`] directly and build a Bevy
//! [`Mesh`] via a small cull-face mesher: every solid voxel emits face quads
//! only where its neighbor is air. Per-face vertex colors come from the
//! MagicaVoxel palette, so a single shared white [`StandardMaterial`] +
//! vertex colors gives the full per-voxel look (and Bevy auto-batches every
//! instance of a variant into one draw call).
//!
//! This route replaced an earlier `bevy_vox_scene` attempt: that crate
//! panics on `.vox` files without `MATL` material chunks (our files don't
//! have them — they're palette-only exports), so we cut out the middleman
//! and use `dot_vox` ourselves.
//!
//! Each variant is loaded once at startup. Spawned props are scattered around
//! the player and parented to a per-tile entity so leaving a ring cascades the
//! despawn.
//!
//! ## Two independent streaming layers
//!
//! Foliage streams as **two genuinely independent layers**, each with its own
//! tile field, its own ring, and its own per-frame budgets:
//!
//! * **Solid layer** ([`SolidField`]) — trees + rocks. Streamed in a
//!   ring sized to the perspective horizon ([`stream::compute_ring`] clamped by
//!   [`QualitySettings::foliage_ring_max`](inf3d_core::QualitySettings)) so props
//!   fill the orbit view to its edges. Solid props get per-prop colliders
//!   ([`SolidPropCollider`](inf3d_physics::SolidPropCollider)) and record
//!   footprint-inflated [`BlockedCells`](inf3d_core::BlockedCells) that the
//!   character controller reads as walls.
//! * **Grass layer** ([`GrassField`]) — grass only. Streamed in a
//!   **player-centered, zoom-INDEPENDENT** ring whose radius is
//!   `ceil(grass_radius_world / TILE)` tiles, so the dense grass carpet is a
//!   fixed-size disc that simply follows the player. Grass gets NO collider and
//!   records NO blocked cells; a grass tile despawns the moment it leaves the
//!   grass ring.
//!
//! Splitting the layers is what removes the old whole-tile re-stream churn: the
//! solid ring (zoom-driven) and the grass disc (player-driven) move at different
//! rates, but neither ever touches the other's entities. A grass tile appearing
//! or disappearing never disturbs a tree/rock collider or its blocked cells, and
//! a solid tile never re-streams for a grass reason.
//!
//! ## Streaming performance (async scatter + budgeting + hysteresis)
//!
//! The heavy part of streaming a tile is the per-column terrain sampling:
//! `TILE*TILE` (256) five-octave Perlin lookups deciding, for each column,
//! whether it's land and what prop (if any) sits there. That used to run
//! synchronously inside the `Update` system and hitched the render thread
//! whenever the player crossed a tile boundary.
//!
//! Both layers run their scatter on a [`bevy::tasks::AsyncComputeTaskPool`]
//! worker: the streamer snapshots the
//! cheap-to-clone [`Terrain`](inf3d_worldgen::Terrain) oracle into a task, the
//! worker returns a plain `Vec<ScatterItem>` (no ECS), and the main thread only
//! spawns entities from that list. Determinism is preserved because each tile's
//! seed derives purely from the tile coordinate — and crucially the SAME seed
//! derivation is shared by both layers, so a tile's grass is stable whether or
//! not its solid props are currently streamed. Tile *task starts* and *despawns*
//! are budgeted per frame (nearest-to-center first), so a big zoom-out or a fast
//! walk fills/clears each ring over several frames instead of in one stall.
//!
//! ## Module layout
//!
//! * [`vox_mesh`] — parse `.vox` files and build cull-face meshes at load time.
//! * [`scatter`] — the off-thread per-tile scatter workers (pure data, no ECS):
//!   one for the solid layer, one for the grass layer.
//! * [`stream`] — the two streaming systems (solid + grass) that ring tiles in/out.
//! * [`spawn`] — main-thread replay of [`ScatterItem`]s into real entities.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::tasks::Task;

use inf3d_core::GameSet;
use inf3d_worldgen::Biome;

mod scatter;
mod spawn;
mod stream;
mod vox_mesh;

/// Per-tile parent marker (re-exported from the crate root) so downstream
/// telemetry can count foliage tiles by component instead of by name.
pub use spawn::FoliageTile;

/// Side length of one streaming tile, in voxel columns.
const TILE: i32 = 16;

// World-space target heights per category. Each variant is uniform-scaled so
// its tallest dimension hits this value, keeping props reasonably sized
// regardless of the source `.vox` model's voxel count.
const TREE_TARGET_HEIGHT: f32 = 4.2;
const GRASS_TARGET_HEIGHT: f32 = 0.45;
const ROCK_TARGET_HEIGHT: f32 = 1.8;

// Asset directories (paths relative to the app crate's `assets/`).
const TREES_DIR: &str = "foliage/vox/Trees";
const ROCKS_DIR: &str = "foliage/vox/Rocks";
const GRASS_DIR: &str = "foliage/vox/Grass";

/// A single loaded foliage variant: its file-stem NAME, its mesh, plus the
/// post-scale bounding-box size (width X, height Y, depth Z) used to size physics
/// colliders and the solid-prop overlap footprint.
///
/// The `name` is the source `.vox` file stem (e.g. `"tree_large"`, `"cactus"`,
/// `"pine_small"`). It exists so the per-biome scatter policy can SELECT a subset
/// of tree variants by matching name substrings (see [`biome_policy`]) — the
/// scatter worker filters trees whose `name` contains one of the biome's allowed
/// substrings, but still emits the variant's ORIGINAL index into the full trees
/// `Vec` so [`spawn`] can index `assets.trees[variant]` unchanged.
#[derive(Clone)]
struct FoliageVariant {
    name: String,
    mesh: Handle<Mesh>,
    size: Vec3,
}

/// Loaded foliage meshes, grouped by category. The vector index is the
/// "variant" the per-tile RNG picks.
#[derive(Resource)]
struct FoliageAssets {
    trees: Vec<FoliageVariant>,
    rocks: Vec<FoliageVariant>,
    grass: Vec<FoliageVariant>,
    /// One tinted material PER biome, indexed by `biome as usize` (see
    /// [`BIOME_COUNT`]). Vertex colors carry the per-voxel palette and the
    /// material's `base_color` (the biome's [`tint`](BiomePolicy::tint))
    /// MULTIPLIES it, so e.g. Snow foliage reads cool-blue and Desert foliage
    /// warm-sand while still showing each model's own palette. A prop selects its
    /// material by [`ScatterItem::biome`]; batching now splits per biome (an
    /// accepted cost — five tints is five small batches per mesh, not per prop).
    materials: [Handle<StandardMaterial>; BIOME_COUNT],
}

/// Which loaded-asset category a scattered item draws from. Carried as plain
/// data out of the worker so the main thread can index the right `Vec` in
/// [`FoliageAssets`]; the variant *index* within the category was already
/// chosen (deterministically) by the worker.
///
/// The solid scatter worker emits only [`Tree`](ScatterCategory::Tree) /
/// [`Rock`](ScatterCategory::Rock); the grass scatter worker emits only
/// [`Grass`](ScatterCategory::Grass). The enum stays unified so [`spawn`] can
/// replay either layer's items through one path.
#[derive(Clone, Copy)]
enum ScatterCategory {
    Tree,
    Rock,
    Grass,
}

/// One placement the scatter worker decided on: which category+variant to use,
/// where it sits (world space), its yaw, and the [`Biome`] of its column. Pure
/// data — no ECS, no asset handles — so it can cross the thread boundary and be
/// replayed deterministically.
///
/// `biome` is what [`spawn`] uses to pick the tinted material
/// ([`FoliageAssets::materials`]`[biome as usize]`); it's a pure function of
/// `(x, z)` ([`Terrain::biome_at`]), so carrying it changes nothing about
/// determinism — the same column always yields the same biome run to run.
#[derive(Clone, Copy)]
struct ScatterItem {
    category: ScatterCategory,
    variant: usize,
    pos: Vec3,
    yaw: f32,
    biome: Biome,
}

/// Snapshot of the per-variant footprint *sizes* (post-scale bounding boxes) the
/// SOLID worker needs to (a) pick a variant index and (b) run the SAME solid-prop
/// overlap test the synchronous spawner used — plus the tree variant NAMES so the
/// worker can filter trees to the column's biome by name substring (see
/// [`biome_policy`]). Cloned into the task alongside the
/// [`Terrain`](inf3d_worldgen::Terrain) so the worker touches no ECS / asset
/// state. Cheap: a few `Vec3`s and short strings, snapshotted once per frame and
/// shared across the few tasks started that frame.
///
/// `tree_names[i]` is the name of the tree variant at index `i` in `trees`, so
/// the two vecs are index-parallel: the worker filters by `tree_names` but emits
/// the original `i` so [`spawn`] indexes the full `assets.trees` Vec. Rocks need
/// no names — every rock is allowed in every biome (only the density multiplier
/// differs).
#[derive(Clone)]
struct SolidVariantSizes {
    trees: Vec<Vec3>,
    tree_names: Vec<String>,
    rocks: Vec<Vec3>,
}

/// Number of [`Biome`] variants — the length of the per-biome material array and
/// the valid range of `biome as usize`. Kept in lockstep with the FROZEN
/// `Biome` enum (`Plains=0 … Beach=4`); if that enum grows, bump this and the
/// [`BIOME_POLICIES`] table below (a missing entry would index out of bounds at
/// startup, caught immediately).
const BIOME_COUNT: usize = 5;

/// How foliage VARIES by biome: density multipliers per category, which TREE
/// variants are allowed (by name substring), and a material tint. This is the one
/// table the whole biome-foliage feature keys off — the scatter worker reads the
/// multipliers + name filter, and [`setup_foliage`] reads the tint.
///
/// Multipliers scale the base per-column densities in [`scatter`]
/// (`TREE_DENSITY` / `ROCK_DENSITY` / `GRASS_DENSITY`); `grass_mul == 0.0`
/// disables grass entirely for that biome (Desert/Snow/Beach). `tree_names` is a
/// set of case-sensitive substrings — a tree variant is eligible in this biome if
/// its [`FoliageVariant::name`] CONTAINS any of them (so `"tree_"` matches the
/// leafy `tree_*` family, `"pine"` both pines, `"palm"` both palms, etc.). An
/// empty match set, or a biome whose substrings match no loaded variant, simply
/// scatters no trees there (the worker skips the tree branch for that column).
#[derive(Clone, Copy)]
struct BiomePolicy {
    tree_mul: f32,
    rock_mul: f32,
    grass_mul: f32,
    /// Substrings a tree variant's name must contain (any-of) to be eligible.
    tree_names: &'static [&'static str],
    /// `base_color` of this biome's foliage material; vertex colors multiply it.
    tint: Color,
}

/// The per-biome policy table, indexed by `biome as usize` (so the order MUST
/// match the `Biome` discriminants: `Plains=0, Forest=1, Desert=2, Snow=3,
/// Beach=4`). Values are the tuned starting point from the feature spec:
///
/// * Plains — sparse leafy trees, normal rocks, full grass, neutral tint.
/// * Forest — dense leafy trees + pines, slightly thinned grass.
/// * Desert — sparse cacti + dead stumps, more rocks, NO grass, warm tint.
/// * Snow   — sparse pines only, NO grass, cool tint.
/// * Beach  — sparse palms only, fewer rocks, NO grass.
const BIOME_POLICIES: [BiomePolicy; BIOME_COUNT] = [
    // Plains
    BiomePolicy {
        tree_mul: 0.6,
        rock_mul: 1.0,
        grass_mul: 1.0,
        tree_names: &["tree_"],
        tint: Color::WHITE,
    },
    // Forest
    BiomePolicy {
        tree_mul: 2.5,
        rock_mul: 1.0,
        grass_mul: 0.8,
        tree_names: &["tree_", "pine"],
        tint: Color::WHITE,
    },
    // Desert
    BiomePolicy {
        tree_mul: 0.5,
        rock_mul: 1.5,
        grass_mul: 0.0,
        tree_names: &["cactus", "stump"],
        tint: Color::srgb(1.0, 0.93, 0.80),
    },
    // Snow
    BiomePolicy {
        tree_mul: 0.8,
        rock_mul: 1.0,
        grass_mul: 0.0,
        tree_names: &["pine"],
        tint: Color::srgb(0.85, 0.92, 1.0),
    },
    // Beach
    BiomePolicy {
        tree_mul: 0.4,
        rock_mul: 0.5,
        grass_mul: 0.0,
        tree_names: &["palm"],
        tint: Color::WHITE,
    },
];

/// The [`BiomePolicy`] for a biome. A total lookup over the FROZEN `Biome` enum:
/// `biome as usize` is always a valid [`BIOME_POLICIES`] index, so this never
/// panics for any real biome (and the array length is checked against
/// [`BIOME_COUNT`] at compile time).
fn biome_policy(biome: Biome) -> BiomePolicy {
    BIOME_POLICIES[biome as usize]
}

/// Per-tile seed derived purely from the tile coordinate. Shared by BOTH scatter
/// workers so a tile's grass is stable whether or not its solid layer is
/// currently streamed (and vice versa). The two workers then advance their own
/// RNG streams independently — the solid layer's tree/rock placement is never
/// perturbed by the grass layer, and grass is bit-identical run to run.
fn tile_seed(tile: IVec2) -> u64 {
    (tile.x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (tile.y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
}

/// An in-flight (or just-finished) per-tile scatter computation running on the
/// [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool). We hold the
/// [`Task`] handle and poll it once per frame with `block_on(poll_once(..))`; when
/// it resolves we spawn the entities. Used by both layers.
struct TileScatterTask {
    task: Task<Vec<ScatterItem>>,
}

/// State a SOLID tile occupies in [`SolidField`]. Tiles flow
/// `Pending` (scatter task in flight) → `Live` (entities spawned, parent held).
///
/// `Live` also carries the voxel cells its SOLID props occupy, so that when the
/// tile despawns we can release exactly those claims. The two cell lists mirror
/// the two routing destinations a prop can take (see [`is_low_prop`]):
///
/// * `cells` — TALL props' footprint-inflated claims in
///   [`BlockedCells`](inf3d_core::BlockedCells) (impassable, with a collider).
/// * `prop_cells` — LOW props' un-inflated claims in
///   [`PropSurfaces`](inf3d_core::PropSurfaces) (no collider, a walkable 1-voxel
///   step).
///
/// Both record EVERY claim (duplicates included), so releasing each list on
/// despawn decrements the shared refcount exactly as many times as this tile
/// incremented it — never freeing a cell a neighbouring tile's prop still holds.
enum SolidTileState {
    Pending(TileScatterTask),
    Live {
        entity: Entity,
        cells: Vec<IVec2>,
        prop_cells: Vec<IVec2>,
    },
}

/// State a GRASS tile occupies in [`GrassField`]. Like [`SolidTileState`] but
/// grass records NO blocked cells and gets NO collider, so `Live` carries only
/// the parent entity to cascade-despawn when the tile leaves the grass ring.
enum GrassTileState {
    Pending(TileScatterTask),
    Live { entity: Entity },
}

/// The SOLID streaming field (trees + rocks), ringed by camera zoom.
#[derive(Resource, Default)]
struct SolidField {
    tiles: HashMap<IVec2, SolidTileState>,
}

/// The GRASS streaming field, ringed in a fixed-size player-centered disc.
#[derive(Resource, Default)]
struct GrassField {
    tiles: HashMap<IVec2, GrassTileState>,
}

/// Horizontal footprint radius of a prop from its post-scale bounding box
/// (half the larger XZ extent), used for the solid-prop overlap test and to
/// size tree-trunk colliders. Shared by [`scatter`] (overlap rejection) and
/// [`spawn`] (collider sizing).
fn footprint_radius(size: Vec3) -> f32 {
    size.x.max(size.z) * 0.5
}

/// Post-scale bounding-box height (world units) at or below which a SOLID prop
/// is treated as **low** — a single climbable voxel step the player walks ONTO
/// rather than an obstacle. Matched to the physics `STEP_HEIGHT` (one voxel) so a
/// low prop is exactly one step up: a low prop gets NO horizontal collider and
/// claims [`PropSurfaces`](inf3d_core::PropSurfaces) instead of
/// [`BlockedCells`](inf3d_core::BlockedCells), while a taller prop keeps its
/// collider + blocked footprint. The gate is purely on height (per the decided
/// design): a short tree and a short rock are both climbable — category never
/// enters into it.
const LOW_PROP_MAX_HEIGHT: f32 = 1.1;

/// Whether a solid prop of post-scale bounding-box `size` is **low** (≤ one
/// climbable voxel step, see [`LOW_PROP_MAX_HEIGHT`]). Pure height test, shared by
/// [`stream`] (routes low props to [`PropSurfaces`](inf3d_core::PropSurfaces),
/// tall ones to [`BlockedCells`](inf3d_core::BlockedCells)) and [`spawn`] (a low
/// prop gets no [`SolidPropCollider`](inf3d_physics::SolidPropCollider) so it
/// can't block the player horizontally). Gates on height ONLY — a short tree and a
/// short rock alike read as climbable.
fn is_low_prop(size: Vec3) -> bool {
    size.y <= LOW_PROP_MAX_HEIGHT
}

pub struct FoliagePlugin;

impl Plugin for FoliagePlugin {
    fn build(&self, app: &mut App) {
        // `QualitySettings` and `BlockedCells` are owned (init) by `CorePlugin`;
        // foliage only reads them. `SolidField` / `GrassField` are foliage's own
        // state — one per layer.
        app.init_resource::<SolidField>()
            .init_resource::<GrassField>()
            .add_systems(Startup, setup_foliage)
            .add_systems(
                Update,
                (
                    // FIRST: if the world backend switched, wipe all foliage so the
                    // streamers below re-scatter it at the new world's heights this
                    // same frame (kills the "props carry over / float" bug).
                    stream::clear_foliage_on_world_change,
                    stream::stream_solid,
                    stream::stream_grass,
                    // Despawn grass blades on player-edited cells (order-independent
                    // — it only touches the edited cell's own blade entity).
                    stream::invalidate_grass_on_edit,
                )
                    .chain()
                    .in_set(GameSet::Streaming),
            );
    }
}

/// Load every `.vox` variant once and publish the shared [`FoliageAssets`].
fn setup_foliage(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let trees = vox_mesh::load_category(TREES_DIR, TREE_TARGET_HEIGHT, &mut meshes);
    let rocks = vox_mesh::load_category(ROCKS_DIR, ROCK_TARGET_HEIGHT, &mut meshes);
    let grass = vox_mesh::load_category(GRASS_DIR, GRASS_TARGET_HEIGHT, &mut meshes);

    // One tinted material per biome. Same params as the old single white material
    // (rough, non-metallic, vertex colors carry the palette), but `base_color` is
    // the biome's tint — which MULTIPLIES the per-voxel vertex colors, so Snow
    // reads cool, Desert warm, the rest neutral. Built by `Biome` index so
    // `materials[item.biome as usize]` selects the right one in `spawn`.
    //
    // `std::array::from_fn` runs the closure once per index (0..BIOME_COUNT), so
    // the array is fully initialised without an `unwrap` on a fallible collect —
    // and it stays in lockstep with `BIOME_POLICIES` (both indexed by biome).
    let biome_materials: [Handle<StandardMaterial>; BIOME_COUNT] = std::array::from_fn(|i| {
        materials.add(StandardMaterial {
            base_color: BIOME_POLICIES[i].tint,
            perceptual_roughness: 0.95,
            metallic: 0.0,
            reflectance: 0.0,
            ..default()
        })
    });

    info!(
        "foliage: loaded {} trees, {} rocks, {} grass",
        trees.len(),
        rocks.len(),
        grass.len()
    );

    commands.insert_resource(FoliageAssets {
        trees,
        rocks,
        grass,
        materials: biome_materials,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_low_prop_gates_on_height() {
        // Well under the threshold: a pebble / flat rock is climbable.
        assert!(is_low_prop(Vec3::new(2.0, 0.5, 2.0)));
        // Exactly one voxel tall is climbable (width never matters — gate is Y).
        assert!(is_low_prop(Vec3::new(0.3, 1.0, 0.3)));
    }

    #[test]
    fn is_low_prop_includes_the_boundary() {
        // `<=` so the boundary value itself is low (a prop exactly one step tall
        // is still a single climbable step).
        assert!(is_low_prop(Vec3::new(1.0, LOW_PROP_MAX_HEIGHT, 1.0)));
        assert!(is_low_prop(Vec3::new(1.0, 1.1, 1.0)));
    }

    #[test]
    fn is_low_prop_rejects_taller_props() {
        // Just over the threshold is already tall (gets a collider + blocked cells).
        assert!(!is_low_prop(Vec3::new(1.0, 1.2, 1.0)));
        // A full tree is firmly tall.
        assert!(!is_low_prop(Vec3::new(2.0, 3.0, 2.0)));
    }

    #[test]
    fn is_low_prop_ignores_category_only_height() {
        // The decided design: a SHORT prop is climbable whether it'd be a tree or a
        // rock — a wide-but-short footprint is low, a thin-but-tall one is tall.
        // Width/depth never flip the result; only Y does.
        assert!(is_low_prop(Vec3::new(5.0, 0.8, 5.0)));
        assert!(!is_low_prop(Vec3::new(0.2, 2.5, 0.2)));
    }

    #[test]
    fn biome_policy_table_is_indexable_for_every_biome() {
        // `biome as usize` must be a valid `BIOME_POLICIES` index for EVERY biome
        // (the lookup is total — no panic), and the table length tracks the enum.
        assert_eq!(BIOME_POLICIES.len(), BIOME_COUNT);
        for biome in [
            Biome::Plains,
            Biome::Forest,
            Biome::Desert,
            Biome::Snow,
            Biome::Beach,
        ] {
            // Must not panic; just exercise the index path.
            let _ = biome_policy(biome);
        }
    }

    #[test]
    fn dry_biomes_disable_grass() {
        // Desert / Snow / Beach must scatter NO grass: their grass multiplier is
        // exactly 0.0, which `scatter_grass` multiplies the base density by (so the
        // probability test can never fire). Plains/Forest keep grass.
        assert_eq!(biome_policy(Biome::Desert).grass_mul, 0.0);
        assert_eq!(biome_policy(Biome::Snow).grass_mul, 0.0);
        assert_eq!(biome_policy(Biome::Beach).grass_mul, 0.0);
        assert!(biome_policy(Biome::Plains).grass_mul > 0.0);
        assert!(biome_policy(Biome::Forest).grass_mul > 0.0);
    }

    #[test]
    fn biome_tree_name_substrings_select_the_intended_families() {
        // The name filter is "variant name CONTAINS any allowed substring". Spot
        // check a couple biomes against representative loaded-variant stems.
        let matches = |biome: Biome, name: &str| {
            biome_policy(biome)
                .tree_names
                .iter()
                .any(|sub| name.contains(sub))
        };

        // Desert allows cacti + dead stumps, but NOT leafy trees, pines, or palms.
        assert!(matches(Biome::Desert, "cactus"));
        assert!(matches(Biome::Desert, "cactus_small"));
        assert!(matches(Biome::Desert, "tree_stump"));
        assert!(!matches(Biome::Desert, "tree_large"));
        assert!(!matches(Biome::Desert, "pine_small"));
        assert!(!matches(Biome::Desert, "palm"));

        // Snow allows pines only.
        assert!(matches(Biome::Snow, "pine_small"));
        assert!(matches(Biome::Snow, "pine_large"));
        assert!(!matches(Biome::Snow, "tree_medium"));
        assert!(!matches(Biome::Snow, "cactus"));

        // Beach allows palms only.
        assert!(matches(Biome::Beach, "palm"));
        assert!(matches(Biome::Beach, "palm_small"));
        assert!(!matches(Biome::Beach, "tree_small"));

        // Forest allows BOTH the leafy `tree_*` family and pines.
        assert!(matches(Biome::Forest, "tree_XL"));
        assert!(matches(Biome::Forest, "pine_large"));
        assert!(!matches(Biome::Forest, "cactus"));

        // Plains allows the leafy `tree_*` family but not the biome specials.
        assert!(matches(Biome::Plains, "tree_large"));
        assert!(!matches(Biome::Plains, "pine_small"));
        assert!(!matches(Biome::Plains, "cactus"));
    }

    #[test]
    fn biome_density_multipliers_match_the_spec() {
        // Lock in the tuned starting values so an accidental edit is caught.
        assert_eq!(biome_policy(Biome::Plains).tree_mul, 0.6);
        assert_eq!(biome_policy(Biome::Forest).tree_mul, 2.5);
        assert_eq!(biome_policy(Biome::Desert).rock_mul, 1.5);
        assert_eq!(biome_policy(Biome::Beach).rock_mul, 0.5);
    }
}
