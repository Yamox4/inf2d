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
//! the player on a tile-ring (see [`stream_foliage`]) and parented to a
//! per-tile entity so leaving the ring cascades the despawn. The ring scales
//! with the camera's orthographic viewport so that zooming out shows props
//! all the way to the screen edges (clamped per quality preset).
//!
//! ## Streaming performance (async scatter + budgeting + hysteresis)
//!
//! The heavy part of streaming a tile is the per-column terrain sampling:
//! `TILE*TILE` (256) five-octave Perlin lookups deciding, for each column,
//! whether it's land and what prop (if any) sits there. That ran synchronously
//! inside the `Update` system and hitched the render thread whenever the player
//! crossed a tile boundary (and especially on first spawn / zoom-out, when the
//! whole ring streamed at once).
//!
//! Now the scatter runs on a [`bevy::tasks::AsyncComputeTaskPool`] worker
//! (mirroring `inf3d_pathfinding`): the streamer snapshots the cheap-to-clone
//! [`Terrain`] oracle into a [`TileScatterTask`], the worker returns a plain
//! `Vec<ScatterItem>` (no ECS), and the main thread only spawns entities from
//! that list. Determinism is preserved because the seed derives purely from the
//! tile coordinate. Tile *task starts* are budgeted to
//! [`MAX_TILE_TASKS_PER_FRAME`] per frame (nearest-to-center first), so a big
//! zoom-out fills the ring over several frames instead of in one stall.
//!
//! The despawn ring is a **hysteresis band** wider than the spawn ring
//! ([`DESPAWN_RING_MARGIN`] extra tiles), so on this wide orthographic-iso view
//! props don't pop out the moment the camera nudges or zooms — tiles only
//! unload well outside the visible area.
//!
//! Past [`QualitySettings::foliage_lod_distance`] from the camera, a tile is
//! streamed as a cheap LOD: grass (the densest, collider-free category) is
//! skipped entirely, leaving only the sparse solid props (trees/rocks). This
//! both cuts entity/draw-call count for far tiles and keeps the per-prop
//! physics colliders intact for the near tiles that matter.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::camera::{Projection, ScalingMode};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool, Task};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use inf3d_camera::IsoCamera;
use inf3d_core::{BlockedCells, FollowTarget, QualitySettings, Rock, Tree};
use inf3d_physics::SolidPropCollider;
use inf3d_worldgen::{Terrain, WATER_HEIGHT};

/// Side length of one streaming tile, in voxel columns.
const TILE: i32 = 16;
/// Minimum ring radius the streamer ever uses, regardless of zoom level.
const RING_MIN: i32 = 2;
/// Fallback ring radius used when the camera entity hasn't spawned yet (or
/// isn't orthographic).
const RING_FALLBACK: i32 = 3;
/// Multiplier from the camera's orthographic `viewport_height` to the
/// world-XZ radius the foliage ring needs to cover. Generous (> the literal
/// half-height) so the spawn ring already covers a margin around the viewport
/// — the wide iso view sees props well past the vertical screen edges.
const RING_ZOOM_COVERAGE: f32 = 1.1;

/// Extra tiles the *despawn* ring extends past the *spawn* ring. This
/// hysteresis band means a tile spawned at the spawn-ring edge has to drift
/// `DESPAWN_RING_MARGIN` whole tiles further out before it unloads, so small
/// camera nudges / zoom wobble near the edge don't pop props in and out. On a
/// wide orthographic-iso camera this is the fix for "trees pop out when I
/// zoom": tiles only unload comfortably outside the visible area.
const DESPAWN_RING_MARGIN: i32 = 2;

/// Maximum number of tile scatter tasks STARTED per frame. A single camera move
/// (or first spawn / big zoom-out) can leave the entire ring missing (a few
/// hundred tiles × 256 columns); starting them all at once would flood the task
/// pool and spawn everything in one frame. Bounding the starts spreads the ring
/// fill over several frames; combined with async scatter, the per-frame
/// main-thread cost is just spawning whatever tasks finished this frame.
const MAX_TILE_TASKS_PER_FRAME: usize = 3;

// Per-column probability of spawning each foliage category.
const TREE_DENSITY: f32 = 0.004;
const GRASS_DENSITY: f32 = 0.018;
const ROCK_DENSITY: f32 = 0.002;

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

/// Filesystem root the dev-mode `AssetPlugin` reads from. Bevy resolves
/// `<CARGO_MANIFEST_DIR>/assets/foo` in dev. The render crate's manifest dir
/// is `crates/inf3d_render/`, so we hop up one level and back into `inf3d_app`.
const APP_ASSETS_ROOT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../inf3d_app/assets");

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

#[derive(Component)]
struct FoliageTile;

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
/// [`Terrain`] so the worker touches no ECS / asset state. Cheap: a few `Vec3`s.
#[derive(Clone)]
struct VariantSizes {
    trees: Vec<Vec3>,
    rocks: Vec<Vec3>,
    grass: Vec<Vec3>,
}

/// An in-flight (or just-finished) per-tile scatter computation running on the
/// [`AsyncComputeTaskPool`]. Mirrors `inf3d_pathfinding::ActivePathTask`: we
/// hold the [`Task`] handle and poll it once per frame with
/// `block_on(poll_once(..))`; when it resolves we spawn the entities.
struct TileScatterTask {
    task: Task<Vec<ScatterItem>>,
}

/// State a tile occupies in the streaming field. Tiles flow
/// `Pending` (scatter task in flight) → `Live` (entities spawned, parent held).
///
/// `Live` also carries the voxel cells its SOLID props occupy, so that when the
/// tile despawns we can remove exactly those cells from [`BlockedCells`] (the
/// shared resource the pathfinder reads). Grass cells are never recorded.
enum TileState {
    Pending(TileScatterTask),
    Live(Entity, Vec<IVec2>),
}

#[derive(Resource, Default)]
struct FoliageField {
    tiles: HashMap<IVec2, TileState>,
}

pub struct FoliagePlugin;

impl Plugin for FoliagePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<QualitySettings>()
            .init_resource::<FoliageField>()
            .init_resource::<BlockedCells>()
            .add_systems(Startup, setup_foliage)
            .add_systems(Update, stream_foliage);
    }
}

fn setup_foliage(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let trees = load_category(TREES_DIR, TREE_TARGET_HEIGHT, &mut meshes);
    let rocks = load_category(ROCKS_DIR, ROCK_TARGET_HEIGHT, &mut meshes);
    let grass = load_category(GRASS_DIR, GRASS_TARGET_HEIGHT, &mut meshes);

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

/// Enumerate `.vox` files under `<APP_ASSETS_ROOT>/<rel_dir>`, parse each
/// with `dot_vox`, build a cull-face mesh per file, and return mesh handles.
fn load_category(
    rel_dir: &str,
    target_height: f32,
    meshes: &mut Assets<Mesh>,
) -> Vec<FoliageVariant> {
    let abs_dir = format!("{}/{}", APP_ASSETS_ROOT, rel_dir);
    let entries = match fs::read_dir(Path::new(&abs_dir)) {
        Ok(e) => e,
        Err(err) => {
            warn!("foliage: could not read {}: {}", abs_dir, err);
            return Vec::new();
        }
    };

    let mut handles = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("vox") {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) => {
                warn!("foliage: failed to read {}: {}", path.display(), err);
                continue;
            }
        };
        let data = match dot_vox::load_bytes(&bytes) {
            Ok(d) => d,
            Err(err) => {
                warn!("foliage: failed to parse {}: {}", path.display(), err);
                continue;
            }
        };
        // Rocks and dead tree stumps must fit inside a SINGLE voxel so their
        // texture never overlaps neighbouring voxels; trees/grass keep the
        // height-normalized scaling.
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let fit_unit = rel_dir == ROCKS_DIR || stem.contains("stump");
        let Some((mesh, size)) = build_voxel_mesh(&data, target_height, fit_unit) else {
            warn!(
                "foliage: empty/unsupported model in {}, skipping",
                path.display()
            );
            continue;
        };
        handles.push(FoliageVariant {
            mesh: meshes.add(mesh),
            size,
        });
    }
    handles
}

/// Build a Bevy [`Mesh`] from the first non-empty model in a [`DotVoxData`].
///
/// Algorithm: scan every voxel, emit face quads for each of the 6 faces whose
/// neighbor in that direction is empty. Per-vertex color is the voxel's
/// palette color converted from sRGB→linear (Bevy's pipeline does the
/// reverse on output, so this round-trips correctly).
///
/// MagicaVoxel uses **Z-up, right-handed**. Bevy uses **Y-up, right-handed**.
/// We apply `(x, y, z) -> (x, z, -y)` per vertex so the model's natural up
/// axis lines up with the world's. The mesh is then translated so the
/// **bottom-center** sits at `(0, 0, 0)` and uniform-scaled so its **vertical
/// (Y) extent** equals `target_height`.
///
/// Centering bug fix: the previous code scaled by `target_height / tallest`
/// where `tallest = max(extent.x, extent.y, extent.z)`. For a prop whose widest
/// dimension is horizontal (e.g. a squat, wide rock), that made the *width*
/// equal `target_height`, leaving the prop far too short — visually it read as
/// off / sunk into the ground relative to the tall props. Scaling by the
/// **vertical** extent makes "target height" mean what it says for every prop,
/// and the bottom-center pivot keeps it sitting exactly on the cell surface.
///
/// Returns the built mesh together with its **post-scale** bounding box (width
/// in X, height in Y, depth in Z), which the collider/footprint logic uses.
fn build_voxel_mesh(
    data: &dot_vox::DotVoxData,
    target_height: f32,
    fit_unit_voxel: bool,
) -> Option<(Mesh, Vec3)> {
    let model = data.models.iter().find(|m| !m.voxels.is_empty())?;
    let sx = model.size.x as usize;
    let sy = model.size.y as usize;
    let sz = model.size.z as usize;
    if sx == 0 || sy == 0 || sz == 0 {
        return None;
    }

    // 3D grid of palette indices; 0 means "air".
    // `voxel.i = 0` is treated as solid (palette[0] is a valid color); we use
    // an `Option<u8>` so we can distinguish "no voxel" from "voxel of palette
    // index 0".
    let mut grid: Vec<Option<u8>> = vec![None; sx * sy * sz];
    let idx = |x: usize, y: usize, z: usize| (z * sy + y) * sx + x;
    for v in &model.voxels {
        let (x, y, z) = (v.x as usize, v.y as usize, v.z as usize);
        if x < sx && y < sy && z < sz {
            grid[idx(x, y, z)] = Some(v.i);
        }
    }

    let solid = |x: i32, y: i32, z: i32| -> bool {
        if x < 0 || y < 0 || z < 0 {
            return false;
        }
        let (x, y, z) = (x as usize, y as usize, z as usize);
        if x >= sx || y >= sy || z >= sz {
            return false;
        }
        grid[idx(x, y, z)].is_some()
    };

    let palette = &data.palette;
    let color_of = |pal_idx: u8| -> [f32; 4] {
        let c = palette.get(pal_idx as usize).copied().unwrap_or(dot_vox::Color {
            r: 255,
            g: 0,
            b: 255,
            a: 255,
        });
        let lin = bevy::color::LinearRgba::from(Color::srgba_u8(c.r, c.g, c.b, c.a));
        [lin.red, lin.green, lin.blue, lin.alpha]
    };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Emit a face quad in MagicaVoxel-space (Z-up) coordinates. The 4 corners
    // are written CCW when viewed from outside the voxel so culling works.
    let emit_face = |corners: [[f32; 3]; 4],
                         normal: [f32; 3],
                         color: [f32; 4],
                         positions: &mut Vec<[f32; 3]>,
                         normals: &mut Vec<[f32; 3]>,
                         colors: &mut Vec<[f32; 4]>,
                         indices: &mut Vec<u32>| {
        let base = positions.len() as u32;
        for c in &corners {
            positions.push(*c);
            normals.push(normal);
            colors.push(color);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    for z in 0..sz {
        for y in 0..sy {
            for x in 0..sx {
                let Some(pal) = grid[idx(x, y, z)] else {
                    continue;
                };
                let color = color_of(pal);
                let fx = x as f32;
                let fy = y as f32;
                let fz = z as f32;

                // +X face: neighbor at x+1 is air → render
                if !solid(x as i32 + 1, y as i32, z as i32) {
                    emit_face(
                        [
                            [fx + 1.0, fy, fz],
                            [fx + 1.0, fy + 1.0, fz],
                            [fx + 1.0, fy + 1.0, fz + 1.0],
                            [fx + 1.0, fy, fz + 1.0],
                        ],
                        [1.0, 0.0, 0.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // -X face
                if !solid(x as i32 - 1, y as i32, z as i32) {
                    emit_face(
                        [
                            [fx, fy + 1.0, fz],
                            [fx, fy, fz],
                            [fx, fy, fz + 1.0],
                            [fx, fy + 1.0, fz + 1.0],
                        ],
                        [-1.0, 0.0, 0.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // +Y face
                if !solid(x as i32, y as i32 + 1, z as i32) {
                    emit_face(
                        [
                            [fx + 1.0, fy + 1.0, fz],
                            [fx, fy + 1.0, fz],
                            [fx, fy + 1.0, fz + 1.0],
                            [fx + 1.0, fy + 1.0, fz + 1.0],
                        ],
                        [0.0, 1.0, 0.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // -Y face
                if !solid(x as i32, y as i32 - 1, z as i32) {
                    emit_face(
                        [
                            [fx, fy, fz],
                            [fx + 1.0, fy, fz],
                            [fx + 1.0, fy, fz + 1.0],
                            [fx, fy, fz + 1.0],
                        ],
                        [0.0, -1.0, 0.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // +Z face (Z-up "top")
                if !solid(x as i32, y as i32, z as i32 + 1) {
                    emit_face(
                        [
                            [fx, fy, fz + 1.0],
                            [fx + 1.0, fy, fz + 1.0],
                            [fx + 1.0, fy + 1.0, fz + 1.0],
                            [fx, fy + 1.0, fz + 1.0],
                        ],
                        [0.0, 0.0, 1.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // -Z face (Z-up "bottom")
                if !solid(x as i32, y as i32, z as i32 - 1) {
                    emit_face(
                        [
                            [fx, fy + 1.0, fz],
                            [fx + 1.0, fy + 1.0, fz],
                            [fx + 1.0, fy, fz],
                            [fx, fy, fz],
                        ],
                        [0.0, 0.0, -1.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
            }
        }
    }

    if positions.is_empty() {
        return None;
    }

    // MagicaVoxel Z-up → Bevy Y-up: (x, y, z) → (x, z, -y).
    for p in &mut positions {
        let (px, py, pz) = (p[0], p[1], p[2]);
        *p = [px, pz, -py];
    }
    for n in &mut normals {
        let (nx, ny, nz) = (n[0], n[1], n[2]);
        *n = [nx, nz, -ny];
    }

    // Bbox in Bevy coords for normalization.
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for p in &positions {
        let v = Vec3::from_array(*p);
        min = min.min(v);
        max = max.max(v);
    }
    let extent = max - min;
    let scale = if fit_unit_voxel {
        // Fit the WHOLE model inside a single 1x1x1 voxel (rocks, stumps) so its
        // texture never spills onto neighbouring voxels: scale the largest extent
        // to exactly 1.0, the others fall within. `target_height` is ignored.
        1.0 / extent.max_element().max(1e-6)
    } else {
        // Normalize by the *vertical* (Y) extent so `target_height` is the prop's
        // real height, never its width. Bottom-center pivot → sits on the surface.
        target_height / extent.y.max(1e-6)
    };
    let pivot = Vec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5);
    for p in &mut positions {
        let v = (Vec3::from_array(*p) - pivot) * scale;
        *p = [v.x, v.y, v.z];
    }

    // Post-scale bounding box: width (X), height (Y, == target_height), depth (Z).
    let scaled_size = extent * scale;

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    Some((mesh, scaled_size))
}

/// Stream foliage tiles in/out as the player moves.
///
/// Three phases per frame:
/// 1. **Poll** in-flight scatter tasks; spawn entities for any that finished.
/// 2. **Despawn** tiles outside the (wider) despawn ring — recursively, which
///    cascades to every prop under the tile parent.
/// 3. **Start** up to [`MAX_TILE_TASKS_PER_FRAME`] new scatter tasks for the
///    nearest missing tiles inside the spawn ring.
fn stream_foliage(
    mut commands: Commands,
    assets: Option<Res<FoliageAssets>>,
    terrain: Res<Terrain>,
    mut field: ResMut<FoliageField>,
    mut blocked: ResMut<BlockedCells>,
    settings: Res<QualitySettings>,
    player_q: Query<&Transform, With<FollowTarget>>,
    camera_q: Query<(&Projection, &GlobalTransform), With<IsoCamera>>,
) {
    let Some(assets) = assets else {
        return;
    };
    if !settings.foliage_enabled {
        if !field.tiles.is_empty() {
            for (_, state) in field.tiles.drain() {
                // Pending tasks just drop (their handle abandons the future,
                // exactly like pathfinding's cancel). Live tiles despawn and
                // surrender their blocked cells back to the shared set.
                if let TileState::Live(entity, cells) = state {
                    commands.entity(entity).despawn();
                    for cell in cells {
                        blocked.0.remove(&cell);
                    }
                }
            }
        }
        return;
    }
    let Ok(player) = player_q.single() else {
        return;
    };
    let center = IVec2::new(
        (player.translation.x / TILE as f32).floor() as i32,
        (player.translation.z / TILE as f32).floor() as i32,
    );

    let camera = camera_q.single().ok();
    let spawn_ring = compute_ring(camera.map(|(p, _)| p), settings.foliage_ring_max);
    // Hysteresis: despawn well outside the spawn ring so props don't pop on the
    // wide iso view. (Capped to keep the resident set bounded.)
    let despawn_ring = spawn_ring + DESPAWN_RING_MARGIN;
    let cam_pos = camera.map(|(_, gt)| gt.translation());
    // Snapshot per-variant footprint sizes once per frame; cloned into each task
    // started this frame (bounded to MAX_TILE_TASKS_PER_FRAME).
    let sizes = VariantSizes {
        trees: assets.trees.iter().map(|v| v.size).collect(),
        rocks: assets.rocks.iter().map(|v| v.size).collect(),
        grass: assets.grass.iter().map(|v| v.size).collect(),
    };

    // --- Phase 1: poll in-flight scatter tasks; spawn the ones that finished.
    let mut ready: Vec<(IVec2, Vec<ScatterItem>)> = Vec::new();
    for (tile, state) in field.tiles.iter_mut() {
        if let TileState::Pending(pending) = state {
            if let Some(items) = block_on(poll_once(&mut pending.task)) {
                ready.push((*tile, items));
            }
        }
    }
    for (tile, items) in ready {
        let entity = spawn_tile_entities(&mut commands, &assets, tile, &items);
        // Record the voxel cells the tile's SOLID props occupy so the pathfinder
        // routes around them (and so we can release them on despawn).
        let mut cells: Vec<IVec2> = Vec::new();
        for item in &items {
            if matches!(item.category, ScatterCategory::Tree | ScatterCategory::Rock) {
                let cell = IVec2::new(item.pos.x.floor() as i32, item.pos.z.floor() as i32);
                if blocked.0.insert(cell) {
                    cells.push(cell);
                }
            }
        }
        field.tiles.insert(tile, TileState::Live(entity, cells));
    }

    // --- Phase 2: despawn tiles outside the wider despawn ring.
    field.tiles.retain(|tile, state| {
        let in_band = (tile.x - center.x).abs() <= despawn_ring
            && (tile.y - center.y).abs() <= despawn_ring;
        if !in_band {
            if let TileState::Live(entity, cells) = state {
                commands.entity(*entity).despawn();
                for cell in cells.iter() {
                    blocked.0.remove(cell);
                }
            }
            // Pending tasks outside the band just drop here.
        }
        in_band
    });

    // --- Phase 3: start up to MAX_TILE_TASKS_PER_FRAME new tasks, nearest first.
    let mut missing: Vec<IVec2> = Vec::new();
    for dx in -spawn_ring..=spawn_ring {
        for dz in -spawn_ring..=spawn_ring {
            let tile = center + IVec2::new(dx, dz);
            if !field.tiles.contains_key(&tile) {
                missing.push(tile);
            }
        }
    }
    // Sort by squared chebyshev-ish distance (use squared euclidean for a nice
    // round fill order). Allocation here is bounded to the ring and only happens
    // while the ring is filling — steady state has no missing tiles.
    missing.sort_by_key(|t| {
        let d = *t - center;
        d.x * d.x + d.y * d.y
    });

    let pool = AsyncComputeTaskPool::get();
    for tile in missing.into_iter().take(MAX_TILE_TASKS_PER_FRAME) {
        // LOD decision is per-tile and based on the tile center's distance from
        // the camera (a tile's props all live within ~TILE of its center, so a
        // per-tile cull is plenty granular for the iso view).
        let cheap_lod = match cam_pos {
            Some(cp) => {
                let tile_center = Vec2::new(
                    (tile.x * TILE) as f32 + TILE as f32 * 0.5,
                    (tile.y * TILE) as f32 + TILE as f32 * 0.5,
                );
                let dx = tile_center.x - cp.x;
                let dz = tile_center.y - cp.z;
                let lod = settings.foliage_lod_distance;
                lod > 0.0 && (dx * dx + dz * dz) > lod * lod
            }
            None => false,
        };

        // Cheap snapshot — `Terrain` is just noise parameters; `sizes` is a few
        // `Vec3`s per category. Both move into the worker.
        let terrain_snapshot: Terrain = terrain.clone();
        let sizes_snapshot = sizes.clone();
        let task = pool.spawn(async move {
            scatter_tile(&terrain_snapshot, tile, &sizes_snapshot, cheap_lod)
        });
        field
            .tiles
            .insert(tile, TileState::Pending(TileScatterTask { task }));
    }
}

fn compute_ring(projection: Option<&Projection>, quality_ring_max: i32) -> i32 {
    let max = quality_ring_max.max(RING_MIN);
    let raw = match projection {
        Some(Projection::Orthographic(ortho)) => match ortho.scaling_mode {
            ScalingMode::FixedVertical { viewport_height } => {
                let blocks = viewport_height * RING_ZOOM_COVERAGE;
                let tiles = (blocks / TILE as f32).ceil();
                if tiles.is_finite() {
                    tiles as i32
                } else {
                    RING_FALLBACK
                }
            }
            _ => RING_FALLBACK,
        },
        _ => RING_FALLBACK,
    };
    raw.clamp(RING_MIN, max)
}

/// Worker side (runs on the [`AsyncComputeTaskPool`]): decide, per column in the
/// tile, whether it's land and what prop variant + position + yaw goes there.
/// Returns plain [`ScatterItem`]s; touches no ECS or asset state — only the
/// cloned [`Terrain`] snapshot and the variant *sizes*.
///
/// Determinism: the RNG is seeded purely from the tile coordinate and consumed
/// in a fixed scan order, so the same tile always produces the same scatter
/// (same seed/derivation → same items), matching the old synchronous behavior.
///
/// `cheap_lod` (the tile is past `foliage_lod_distance`): skip grass entirely.
/// Grass is the densest, collider-free category, so dropping it for far tiles
/// is the cheap LOD — far tiles keep only their sparse solid props.
///
/// The returned vec is sorted by (category, variant) so the main thread spawns
/// all instances of one variant contiguously. Bevy auto-batches instances that
/// share a mesh handle + material; spawning grouped (instead of per-column
/// interleaved) keeps those batches from fragmenting. This sort changes only
/// spawn order, never which items exist or where they sit — determinism holds.
fn scatter_tile(
    terrain: &Terrain,
    tile: IVec2,
    sizes: &VariantSizes,
    cheap_lod: bool,
) -> Vec<ScatterItem> {
    let seed = (tile.x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (tile.y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    let mut rng = StdRng::seed_from_u64(seed);

    let base_x = tile.x * TILE;
    let base_z = tile.y * TILE;

    // Footprints (XZ center + radius) of solid props already placed in this
    // tile. Solid props (trees/rocks) must not inter-penetrate, so each
    // candidate is rejected if its footprint disc overlaps a placed one.
    // Grass is exempt and never recorded here — it may overlap freely. The
    // worker uses the SAME variant `size` (snapshotted into `sizes`) and the
    // SAME `try_place_solid` test the synchronous spawner used, so the set of
    // placed props is identical to the old behavior.
    let mut solid_footprints: Vec<(Vec2, f32)> = Vec::new();
    let mut items: Vec<ScatterItem> = Vec::new();

    for lx in 0..TILE {
        for lz in 0..TILE {
            let x = base_x + lx;
            let z = base_z + lz;
            // 1.3: single height sample per column (was is_land + stand_pos,
            // i.e. the 5-octave noise twice). Reuse `pos` for placement.
            let pos = terrain.stand_pos(x, z);
            if pos.y <= WATER_HEIGHT {
                continue;
            }
            let yaw = snap_yaw(&mut rng);
            let xz = Vec2::new(pos.x, pos.z);

            if !sizes.trees.is_empty() && rng.random::<f32>() < TREE_DENSITY {
                let variant = rng.random_range(0..sizes.trees.len());
                if try_place_solid(&mut solid_footprints, xz, sizes.trees[variant]) {
                    items.push(ScatterItem {
                        category: ScatterCategory::Tree,
                        variant,
                        pos,
                        yaw,
                    });
                }
                continue;
            }
            if !sizes.rocks.is_empty() && rng.random::<f32>() < ROCK_DENSITY {
                let variant = rng.random_range(0..sizes.rocks.len());
                if try_place_solid(&mut solid_footprints, xz, sizes.rocks[variant]) {
                    items.push(ScatterItem {
                        category: ScatterCategory::Rock,
                        variant,
                        pos,
                        yaw,
                    });
                }
                continue;
            }
            if !cheap_lod && !sizes.grass.is_empty() && rng.random::<f32>() < GRASS_DENSITY {
                let variant = rng.random_range(0..sizes.grass.len());
                items.push(ScatterItem {
                    category: ScatterCategory::Grass,
                    variant,
                    pos,
                    yaw,
                });
            }
        }
    }

    // Group by category+variant for batch-friendly spawn order (see doc above).
    items.sort_by_key(|it| (category_rank(it.category), it.variant));
    items
}

/// Stable ordering key for grouping spawns by category.
fn category_rank(category: ScatterCategory) -> u8 {
    match category {
        ScatterCategory::Tree => 0,
        ScatterCategory::Rock => 1,
        ScatterCategory::Grass => 2,
    }
}

/// Main-thread side: spawn the tile parent and replay the worker's
/// [`ScatterItem`]s into real entities (meshes/materials/colliders). All the
/// physics-agent code paths (`spawn_prop`'s centering + per-prop colliders)
/// are reused verbatim — only WHEN/WHERE the scatter is decided moved off-thread.
fn spawn_tile_entities(
    commands: &mut Commands,
    assets: &FoliageAssets,
    tile: IVec2,
    items: &[ScatterItem],
) -> Entity {
    let parent = commands
        .spawn((
            Transform::default(),
            Visibility::default(),
            Name::new(format!("FoliageTile {},{}", tile.x, tile.y)),
            FoliageTile,
        ))
        .id();

    for item in items {
        let (variant, kind) = match item.category {
            ScatterCategory::Tree => (&assets.trees[item.variant], Some(PropKind::Tree)),
            ScatterCategory::Rock => (&assets.rocks[item.variant], Some(PropKind::Rock)),
            ScatterCategory::Grass => (&assets.grass[item.variant], None),
        };
        spawn_prop(
            commands,
            parent,
            variant,
            assets.material.clone(),
            item.pos,
            item.yaw,
            kind,
        );
    }

    parent
}

/// Horizontal footprint radius of a prop from its post-scale bounding box
/// (half the larger XZ extent), used for the solid-prop overlap test.
fn footprint_radius(size: Vec3) -> f32 {
    size.x.max(size.z) * 0.5
}

/// Try to claim a footprint disc for a solid prop. Returns `true` (and records
/// the disc) if it doesn't overlap any previously placed solid prop in the tile;
/// returns `false` to reject the placement (props would inter-penetrate).
fn try_place_solid(placed: &mut Vec<(Vec2, f32)>, center: Vec2, size: Vec3) -> bool {
    let r = footprint_radius(size);
    for (c, pr) in placed.iter() {
        if center.distance_squared(*c) < (r + pr) * (r + pr) {
            return false;
        }
    }
    placed.push((center, r));
    true
}

#[derive(Clone, Copy)]
enum PropKind {
    Tree,
    Rock,
}

fn spawn_prop(
    commands: &mut Commands,
    parent: Entity,
    variant: &FoliageVariant,
    material: Handle<StandardMaterial>,
    pos: Vec3,
    yaw: f32,
    kind: Option<PropKind>,
) {
    let mut entity = commands.spawn((
        Mesh3d(variant.mesh.clone()),
        MeshMaterial3d(material),
        Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
        Visibility::default(),
        ChildOf(parent),
    ));
    // Solid props (trees, rocks) get a static collider sized to their footprint
    // so the player is blocked by them and can stand on rocks. The physics crate
    // turns `SolidPropCollider` into the real `Collider` + `RigidBody::Static`
    // on the `Solid` collision layer.
    //
    // GRASS gets NO collider — it's intentionally left out of the physics layers
    // so the player walks straight through it (see `inf3d_physics::GameLayer`).
    match kind {
        Some(PropKind::Tree) => {
            let height = variant.size.y;
            let radius = (footprint_radius(variant.size) * 0.35).clamp(0.12, 0.6);
            entity.insert((
                Tree,
                SolidPropCollider::Tree { radius, height },
            ));
        }
        Some(PropKind::Rock) => {
            entity.insert((
                Rock,
                SolidPropCollider::Rock {
                    half: variant.size * 0.5,
                },
            ));
        }
        None => {}
    }
}

fn snap_yaw(rng: &mut StdRng) -> f32 {
    let q: u32 = rng.random_range(0..4);
    q as f32 * std::f32::consts::FRAC_PI_2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ortho_proj(vh: f32) -> Projection {
        Projection::Orthographic(bevy::camera::OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: vh,
            },
            ..bevy::camera::OrthographicProjection::default_3d()
        })
    }

    #[test]
    fn compute_ring_falls_back_when_no_camera() {
        assert_eq!(compute_ring(None, 8), RING_FALLBACK);
    }

    #[test]
    fn compute_ring_respects_minimum() {
        let proj = ortho_proj(4.0);
        assert_eq!(compute_ring(Some(&proj), 8), RING_MIN);
    }

    #[test]
    fn compute_ring_respects_quality_cap() {
        let proj = ortho_proj(400.0);
        assert_eq!(compute_ring(Some(&proj), 4), 4);
    }

    #[test]
    fn compute_ring_scales_with_zoom() {
        // blocks = viewport_height * RING_ZOOM_COVERAGE (1.1); tiles = ceil(blocks / TILE).
        // 90 * 1.1 = 99 → ceil(99/16) = 7.
        let proj = ortho_proj(90.0);
        assert_eq!(compute_ring(Some(&proj), 8), 7);
        // 44 * 1.1 = 48.4 → ceil(48.4/16) = 4.
        let proj = ortho_proj(44.0);
        assert_eq!(compute_ring(Some(&proj), 8), 4);
    }

    #[test]
    fn snap_yaw_returns_cardinal_only() {
        let mut rng = StdRng::seed_from_u64(0xCAFEBABE);
        let valid = [
            0.0,
            std::f32::consts::FRAC_PI_2,
            std::f32::consts::PI,
            std::f32::consts::FRAC_PI_2 * 3.0,
        ];
        for _ in 0..256 {
            let y = snap_yaw(&mut rng);
            assert!(
                valid.iter().any(|v| (y - v).abs() < 1e-5),
                "non-cardinal yaw {y}"
            );
        }
    }

    #[test]
    fn build_voxel_mesh_handles_single_voxel() {
        let data = dot_vox::DotVoxData {
            version: 150,
            index_map: vec![],
            models: vec![dot_vox::Model {
                size: dot_vox::Size { x: 1, y: 1, z: 1 },
                voxels: vec![dot_vox::Voxel {
                    x: 0,
                    y: 0,
                    z: 0,
                    i: 0,
                }],
            }],
            palette: vec![dot_vox::Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }],
            materials: vec![],
            scenes: vec![],
            layers: vec![],
        };
        let (mesh, size) = build_voxel_mesh(&data, 1.0, false).expect("mesh");
        // A single 1×1×1 voxel scaled so its height (Y) == target 1.0 → unit box.
        assert!((size.y - 1.0).abs() < 1e-5, "height should equal target");
        // 6 visible faces × 4 verts = 24, × 6 indices/face = 36.
        let pos = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| match a {
                bevy::mesh::VertexAttributeValues::Float32x3(v) => Some(v),
                _ => None,
            })
            .expect("positions");
        assert_eq!(pos.len(), 24);
        match mesh.indices().expect("indices") {
            Indices::U32(v) => assert_eq!(v.len(), 36),
            Indices::U16(v) => assert_eq!(v.len(), 36),
        }
    }

    #[test]
    fn build_voxel_mesh_culls_interior_faces() {
        // 2×1×1 of solid voxels — interior +X/-X faces between them are culled.
        let data = dot_vox::DotVoxData {
            version: 150,
            index_map: vec![],
            models: vec![dot_vox::Model {
                size: dot_vox::Size { x: 2, y: 1, z: 1 },
                voxels: vec![
                    dot_vox::Voxel { x: 0, y: 0, z: 0, i: 0 },
                    dot_vox::Voxel { x: 1, y: 0, z: 0, i: 0 },
                ],
            }],
            palette: vec![dot_vox::Color { r: 0, g: 255, b: 0, a: 255 }],
            materials: vec![],
            scenes: vec![],
            layers: vec![],
        };
        let (mesh, size) = build_voxel_mesh(&data, 1.0, false).expect("mesh");
        // 2-wide, 1-tall, 1-deep voxels: scaling normalizes the *height* to 1.0,
        // so the width (X) stays at twice the height (2.0) — the fix's whole
        // point (previously it would have been squashed to make width == 1.0).
        assert!((size.y - 1.0).abs() < 1e-5, "height should equal target");
        assert!((size.x - 2.0).abs() < 1e-5, "width preserved relative to height");
        let pos = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| match a {
                bevy::mesh::VertexAttributeValues::Float32x3(v) => Some(v),
                _ => None,
            })
            .expect("positions");
        // Each voxel has 6 faces, but 1 face per voxel is shared interior →
        // culled. So 2 × 5 = 10 faces × 4 verts = 40.
        assert_eq!(pos.len(), 40);
    }
}
