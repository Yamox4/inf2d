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
//! the player on a tile-ring (see [`stream`]) and parented to a per-tile entity
//! so leaving the ring cascades the despawn. The ring scales with the camera's
//! orthographic viewport so that zooming out shows props all the way to the
//! screen edges (clamped per quality preset).
//!
//! ## Streaming performance (async scatter + budgeting + hysteresis)
//!
//! The heavy part of streaming a tile is the per-column terrain sampling:
//! `TILE*TILE` (256) five-octave Perlin lookups deciding, for each column,
//! whether it's land and what prop (if any) sits there. That used to run
//! synchronously inside the `Update` system and hitched the render thread
//! whenever the player crossed a tile boundary (and especially on first spawn /
//! zoom-out, when the whole ring streamed at once).
//!
//! Now the scatter runs on a [`bevy::tasks::AsyncComputeTaskPool`] worker
//! (mirroring `inf3d_pathfinding`): the streamer snapshots the cheap-to-clone
//! [`Terrain`](inf3d_worldgen::Terrain) oracle into a task, the worker returns a
//! plain `Vec<ScatterItem>` (no ECS), and the main thread only spawns entities
//! from that list. Determinism is preserved because the seed derives purely from
//! the tile coordinate. Tile *task starts* are budgeted per frame
//! (nearest-to-center first), so a big zoom-out fills the ring over several
//! frames instead of in one stall.
//!
//! ## Module layout
//!
//! * [`vox_mesh`] — parse `.vox` files and build cull-face meshes at load time.
//! * [`scatter`] — the off-thread per-tile scatter worker (pure data, no ECS).
//! * [`stream`] — the streaming system that rings tiles in/out around the camera.
//! * [`spawn`] — main-thread replay of [`ScatterItem`]s into real entities.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::tasks::Task;

use inf3d_core::GameSet;

mod scatter;
mod spawn;
mod stream;
mod vox_mesh;

/// Side length of one streaming tile, in voxel columns.
const TILE: i32 = 16;

// World-space target heights per category. Each variant is uniform-scaled so
// its tallest dimension hits this value, keeping props reasonably sized
// regardless of the source `.vox` model's voxel count.
const TREE_TARGET_HEIGHT: f32 = 3.0;
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
/// the worker needs to (a) pick a variant index and (b) run the SAME solid-prop
/// overlap test the synchronous spawner used. Cloned into the task alongside the
/// [`Terrain`](inf3d_worldgen::Terrain) so the worker touches no ECS / asset
/// state. Cheap: a few `Vec3`s.
#[derive(Clone)]
struct VariantSizes {
    trees: Vec<Vec3>,
    rocks: Vec<Vec3>,
    grass: Vec<Vec3>,
}

/// An in-flight (or just-finished) per-tile scatter computation running on the
/// [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool). Mirrors
/// `inf3d_pathfinding::ActivePathTask`: we hold the [`Task`] handle and poll it
/// once per frame with `block_on(poll_once(..))`; when it resolves we spawn the
/// entities.
struct TileScatterTask {
    task: Task<Vec<ScatterItem>>,
}

/// State a tile occupies in the streaming field. Tiles flow
/// `Pending` (scatter task in flight) → `Live` (entities spawned, parent held).
///
/// `Live` also carries the voxel cells its SOLID props occupy, so that when the
/// tile despawns we can remove exactly those cells from
/// [`BlockedCells`](inf3d_core::BlockedCells) (the shared resource the
/// pathfinder reads). Grass cells are never recorded.
enum TileState {
    Pending(TileScatterTask),
    Live(Entity, Vec<IVec2>),
}

#[derive(Resource, Default)]
struct FoliageField {
    tiles: HashMap<IVec2, TileState>,
}

/// Horizontal footprint radius of a prop from its post-scale bounding box
/// (half the larger XZ extent), used for the solid-prop overlap test and to
/// size tree-trunk colliders. Shared by [`scatter`] (overlap rejection) and
/// [`spawn`] (collider sizing).
fn footprint_radius(size: Vec3) -> f32 {
    size.x.max(size.z) * 0.5
}

pub struct FoliagePlugin;

impl Plugin for FoliagePlugin {
    fn build(&self, app: &mut App) {
        // `QualitySettings` and `BlockedCells` are owned (init) by `CorePlugin`;
        // foliage only reads them. `FoliageField` is foliage's own state.
        app.init_resource::<FoliageField>()
            .add_systems(Startup, setup_foliage)
            .add_systems(Update, stream::stream_foliage.in_set(GameSet::Streaming));
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
