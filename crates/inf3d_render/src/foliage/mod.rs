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
//!   **zoom-driven** ring ([`stream::compute_ring`] clamped by
//!   [`QualitySettings::foliage_ring_max`](inf3d_core::QualitySettings)) so
//!   zooming out shows props all the way to the iso-view edges. Solid props get
//!   per-prop colliders ([`SolidPropCollider`](inf3d_physics::SolidPropCollider))
//!   and record footprint-inflated [`BlockedCells`](inf3d_core::BlockedCells) for
//!   the pathfinder.
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
//! worker (mirroring `inf3d_pathfinding`): the streamer snapshots the
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

/// A single loaded foliage variant: its mesh plus the post-scale bounding-box
/// size (width X, height Y, depth Z) used to size physics colliders and the
/// solid-prop overlap footprint.
#[derive(Clone)]
struct FoliageVariant {
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
    /// One shared material — vertex colors carry the per-voxel palette, so a
    /// white base lets every instance share the same draw call.
    material: Handle<StandardMaterial>,
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
/// where it sits (world space), and its yaw. Pure data — no ECS, no asset
/// handles — so it can cross the thread boundary and be replayed deterministically.
#[derive(Clone, Copy)]
struct ScatterItem {
    category: ScatterCategory,
    variant: usize,
    pos: Vec3,
    yaw: f32,
}

/// Snapshot of the per-variant footprint *sizes* (post-scale bounding boxes)
/// the SOLID worker needs to (a) pick a variant index and (b) run the SAME
/// solid-prop overlap test the synchronous spawner used. Cloned into the task
/// alongside the [`Terrain`](inf3d_worldgen::Terrain) so the worker touches no
/// ECS / asset state. Cheap: a few `Vec3`s.
#[derive(Clone)]
struct SolidVariantSizes {
    trees: Vec<Vec3>,
    rocks: Vec<Vec3>,
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
/// [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool). Mirrors
/// `inf3d_pathfinding::ActivePathTask`: we hold the [`Task`] handle and poll it
/// once per frame with `block_on(poll_once(..))`; when it resolves we spawn the
/// entities. Used by both layers.
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

    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.95,
        metallic: 0.0,
        reflectance: 0.0,
        ..default()
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
        material,
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
}
