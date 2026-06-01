#![deny(unsafe_code)]
//! Procedural cliff-face rendering for terraced terrain — **one merged mesh
//! per chunk**.
//!
//! For each tile T whose neighbor on any of its four iso edges sits at a lower
//! height, a parallelogram is appended to the chunk's cliff mesh, filling the
//! visible "side wall" between them. Per-vertex colors carry the biome × side
//! tint so a single material can render every biome × side combination in one
//! draw call.
//!
//! Without this pass, a grass plateau at height 2 next to a sand tile at
//! height 0 leaves a 32-pixel gap that shows the background color through it —
//! terraces look like floating tiles. With this pass, the gap is filled by a
//! darker parallelogram whose top/bottom edges are co-linear with T's diamond
//! edge and the lower neighbor's diamond edge, so the terrain reads as a solid
//! 3D block — the classic Minecraft-iso stacked-cube silhouette.
//!
//! ## Consolidation: one entity per chunk
//!
//! The previous implementation spawned one `Mesh2d` + one `ColorMaterial` per
//! `(tile, side)` cliff drop — up to ~30 entities and materials per chunk.
//! This version merges every cliff in a chunk into a single mesh whose vertex
//! buffer holds every parallelogram's four corners and an interleaved
//! [`Mesh::ATTRIBUTE_COLOR`] giving each vertex its biome × side tint. One
//! shared [`ChunkCliffMaterial`] handle (a custom [`Material2d`] that does
//! `out = vertex_color * uniform_tint`) is used by every chunk. Net cost
//! drops from "tens of entities + materials per chunk" to **one entity per
//! chunk** plus a single material asset for the whole world.
//!
//! ## Hierarchy & lifecycle
//!
//! The cliff mesh is spawned as a direct child of the chunk entity, so it
//! inherits the chunk's `Transform` for world placement and rides the
//! `ChildOf` cascade despawn when the world streamer drops the chunk. No
//! bespoke teardown system is needed.
//!
//! When a new chunk loads, the cliff systems also **rebuild** the cliff mesh
//! of each of its four already-loaded chunk neighbors. The previous version's
//! `EmittedCliffs` dedupe set is gone — rebuilding the neighbor's whole mesh
//! is cheap (it visits `CHUNK_SIZE^2 * 4` neighbor checks once) and removes
//! an entire cross-frame state-tracking dimension. A rebuild despawns the
//! neighbor's existing cliff entity (if any) and spawns a fresh one.
//!
//! ## Geometry — proper parallelograms
//!
//! `inf2d_core::tile_to_world` anchors each tile at its **bottom vertex** B.
//! Diamond vertices relative to B (at `height = 0`):
//!
//! * Bottom B = `( 0,    0     )`
//! * Left   L = `(-W/2, +H/2  )`
//! * Top    T = `( 0,   +H    )`
//! * Right  R = `(+W/2, +H/2  )`
//!
//! where `W = TILE_WIDTH` and `H = TILE_HEIGHT`. Raising a tile by `h` steps
//! shifts every vertex up by `h * HEIGHT_STEP_PX`. Each cliff face is a
//! parallelogram whose top edge runs along T's diamond edge at `h_t` and whose
//! bottom edge runs along the lower neighbor's matching edge at `h_n`:
//!
//! | Side       | Top-left vertex  | Top-right vertex | Neighbor offset |
//! |------------|------------------|------------------|-----------------|
//! | FrontLeft  | L + h_t          | B + h_t          | `(-1,  0)`      |
//! | FrontRight | B + h_t          | R + h_t          | `( 0, -1)`      |
//! | BackLeft   | L + h_t          | T + h_t          | `( 0,  1)`      |
//! | BackRight  | T + h_t          | R + h_t          | `( 1,  0)`      |
//!
//! ## Skip cliffs facing water
//!
//! A grass tile next to a water tile (height -1) used to draw a cliff face
//! dropping into the water — but the water surface sits *above* the recessed
//! water-tile floor, so that wall hung visibly behind the water diamond and
//! read as "grass extending over water." The new builder checks the lower
//! neighbor's [`TileKind`]: if the neighbor is [`TileKind::Water`] we skip
//! the cliff entirely. The screen-Y gap between grass and water plus the
//! water shader's surface reads as a coastline on its own.
//!
//! ## Cross-chunk edges
//!
//! When a neighbor's local coords fall outside the current chunk we resolve
//! the neighbor chunk through [`ChunkManager`] and read its [`ChunkData`]
//! component directly. If the neighbor chunk is **not yet loaded** we skip
//! the cliff entirely — drawing it against an assumed `height = 0` would
//! flash a phantom cliff that disappears once the real neighbor arrives.

use bevy::asset::{embedded_asset, Asset, RenderAssetUsages};
use bevy::mesh::Indices;
use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, PrimitiveTopology};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dPlugin, MeshMaterial2d};
use inf2d_core::{
    chunk_origin_world, tile_to_world, ChunkPos, LocalTilePos, WorldTile, CHUNK_SIZE,
    HEIGHT_STEP_PX, TILE_HEIGHT, TILE_WIDTH,
};
use inf2d_world::{ChunkData, ChunkLoaded, ChunkManager, TileKind};

use crate::atlas::BASE_COLOR;
use crate::layers::RenderLayer;

/// Plugin: registers the [`ChunkCliffMaterial`] pipeline, embeds the WGSL,
/// builds the shared material at `Startup`, and schedules the per-chunk
/// cliff-mesh build system in `Update`.
pub struct CliffsPlugin;

impl Plugin for CliffsPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "chunk_cliff.wgsl");

        app.add_plugins(Material2dPlugin::<ChunkCliffMaterial>::default())
            .init_resource::<ChunkCliffAssets>()
            .add_systems(Startup, build_chunk_cliff_assets)
            .add_systems(Update, spawn_chunk_cliffs);
    }
}

/// Marker on the per-chunk merged cliff-mesh entity. Lets debug overlays
/// filter cliffs out of the inspector and lets the rebuild path find and
/// despawn the existing cliff child before respawning a fresh one.
#[derive(Component, Debug)]
pub struct ChunkCliff;

/// **Legacy marker** kept as a re-export so any debug tooling that referenced
/// the per-face entity tag still compiles. The new builder never spawns this
/// component; it exists purely so external consumers don't break on the API
/// rename.
#[derive(Component, Debug)]
pub struct CliffFace;

// Per-height Z separation between tilemap layers — must match the constant in
// `tilemap.rs`. The merged cliff mesh anchors at `RenderLayer::GROUND` and
// each parallelogram vertex carries its own Z offset built into the position
// attribute (see `append_cliff_into_mesh`), so a single Transform z works
// for every cliff in the chunk.
const HEIGHT_LAYER_Z_STEP: f32 = 0.01;

/// Identifies which of T's four diamond edges the cliff is filling. Each
/// variant picks both the diamond edge whose top/bottom vertices the mesh
/// spans and the neighbor tile coordinate sharing that edge with T.
///
/// Naming convention: "front" = screen-down edges (B↔L and B↔R, visible to
/// the camera under a higher tile); "back" = screen-up edges (L↔T and T↔R,
/// hidden behind higher tiles but needed for visual continuity on peaks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CliffSide {
    /// Front-left edge of T's diamond (between bottom vertex B and left
    /// vertex L). Fills the gap down to neighbor `(x-1, y)` —
    /// screen-down-left.
    FrontLeft,
    /// Front-right edge of T's diamond (between B and R). Fills the gap
    /// down to neighbor `(x, y-1)` — screen-down-right.
    FrontRight,
    /// Back-left edge of T's diamond (between left vertex L and top vertex
    /// T). Fills the gap down to neighbor `(x, y+1)` — screen-up-left.
    BackLeft,
    /// Back-right edge of T's diamond (between T and R). Fills the gap down
    /// to neighbor `(x+1, y)` — screen-up-right.
    BackRight,
}

impl CliffSide {
    // Tile-coord offset `(dx, dy)` from T to the neighbor sharing this edge.
    #[inline]
    fn neighbor_offset(self) -> (i32, i32) {
        match self {
            CliffSide::FrontLeft => (-1, 0),
            CliffSide::FrontRight => (0, -1),
            CliffSide::BackLeft => (0, 1),
            CliffSide::BackRight => (1, 0),
        }
    }

    // Diamond edge endpoints (left then right, when looking at the face from
    // outside the prism) relative to T's bottom-vertex anchor B, at
    // `height = 0`. Vertices used: L = (-W/2, +H/2), T = ( 0,   +H  ),
    // R = (+W/2, +H/2), B = ( 0,    0  ). "Left" / "right" are chosen so the
    // resulting quad is wound counter-clockwise in screen space (positive-y
    // up), matching the index order `[0, 1, 2, 0, 2, 3]`.
    #[inline]
    fn edge_endpoints(self) -> (f32, f32, f32, f32) {
        match self {
            CliffSide::FrontLeft => (-TILE_WIDTH * 0.5, TILE_HEIGHT * 0.5, 0.0, 0.0),
            CliffSide::FrontRight => (0.0, 0.0, TILE_WIDTH * 0.5, TILE_HEIGHT * 0.5),
            CliffSide::BackLeft => (-TILE_WIDTH * 0.5, TILE_HEIGHT * 0.5, 0.0, TILE_HEIGHT),
            CliffSide::BackRight => (0.0, TILE_HEIGHT, TILE_WIDTH * 0.5, TILE_HEIGHT * 0.5),
        }
    }

    // Per-side multiplier on the biome base color, simulating cheap ambient
    // occlusion: brighter on the front (camera-facing) faces, darker on the
    // back. Identical to the original per-material brightness.
    #[inline]
    fn brightness(self) -> f32 {
        match self {
            CliffSide::FrontLeft => 0.75,
            CliffSide::FrontRight => 0.65,
            CliffSide::BackLeft | CliffSide::BackRight => 0.55,
        }
    }
}

// All four sides — iterated per-tile in the cliff-mesh builder.
const ALL_SIDES: [CliffSide; 4] = [
    CliffSide::FrontLeft,
    CliffSide::FrontRight,
    CliffSide::BackLeft,
    CliffSide::BackRight,
];

/// Custom [`Material2d`] for the merged per-chunk cliff mesh. The fragment
/// shader does `out = vertex_color * tint` so every biome × side variation is
/// driven by the mesh's [`Mesh::ATTRIBUTE_COLOR`] data — one material handle
/// is reused for every chunk in the world.
///
/// The `tint` uniform is currently always white; it's retained as a knob for
/// future debug overlays (e.g. flash all cliffs red on a damage event) without
/// requiring a new pipeline.
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct ChunkCliffMaterial {
    /// Multiplicative tint applied on top of the vertex color. Default white
    /// preserves the per-vertex biome × AO color exactly.
    #[uniform(0)]
    pub tint: LinearRgba,
}

impl Default for ChunkCliffMaterial {
    fn default() -> Self {
        Self {
            tint: LinearRgba::WHITE,
        }
    }
}

impl Material2d for ChunkCliffMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://inf2d_render/chunk_cliff.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        // Cliffs are fully opaque (their per-vertex alpha is 1.0); the opaque
        // phase is the cheapest path and also matches the original per-face
        // `ColorMaterial` default.
        AlphaMode2d::Opaque
    }
}

/// Shared GPU handle for the single [`ChunkCliffMaterial`] every chunk's
/// merged cliff mesh references. Built once in [`build_chunk_cliff_assets`];
/// dropping all chunks releases the per-chunk meshes (via `ChildOf` cascade)
/// but leaves this handle alive for the next chunk to spawn.
#[derive(Resource, Default, Clone)]
pub struct ChunkCliffAssets {
    /// Shared material handle for every per-chunk cliff mesh. `Option` because
    /// the resource is initialized via `init_resource` (default) and populated
    /// in `Startup`; consumers must check before use.
    pub material: Option<Handle<ChunkCliffMaterial>>,
}

/// `Startup` system: cache the shared material in [`ChunkCliffAssets`].
fn build_chunk_cliff_assets(
    mut assets: ResMut<ChunkCliffAssets>,
    mut materials: ResMut<Assets<ChunkCliffMaterial>>,
) {
    let handle = materials.add(ChunkCliffMaterial::default());
    assets.material = Some(handle);
}

/// `Update` system: react to [`ChunkLoaded`] by building a single merged
/// cliff mesh for the just-loaded chunk and respawning the cliff meshes of
/// each of its four already-loaded chunk neighbors (so their cross-edge
/// cliffs facing into the new chunk get drawn now that we know the new
/// chunk's heights). Runs in plain `Update`; each `ChunkLoaded` is read
/// exactly once via its `MessageReader` cursor.
pub fn spawn_chunk_cliffs(
    mut commands: Commands,
    mut events: MessageReader<ChunkLoaded>,
    chunks: Query<&ChunkData>,
    manager: Res<ChunkManager>,
    cliffs: Query<(Entity, &ChildOf), With<ChunkCliff>>,
    assets: Res<ChunkCliffAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Some(material) = assets.material.as_ref() else {
        // Startup hasn't completed yet; the streamer normally fires
        // `ChunkLoaded` after Startup, but in headless / test paths it can
        // arrive first. Drop silently — the next chunk load after the
        // material is built will pick the missing cliffs up via the
        // neighbor-rebuild pass.
        return;
    };

    // Snapshot events first so we can iterate `new_chunks` twice: once to
    // build the just-loaded chunks' own cliff meshes, then again to rebuild
    // each new chunk's loaded neighbors. Both passes mutate `commands` and
    // `meshes`, but the snapshot keeps the data flow obviously acyclic.
    let new_chunks: Vec<(Entity, ChunkPos)> =
        events.read().map(|ev| (ev.entity, ev.pos)).collect();

    // Track which chunk entities we've already rebuilt this frame, so a
    // chunk that's a neighbor of two new chunks doesn't get its mesh
    // despawned + respawned twice. The set is keyed on `Entity` because
    // that's what `cliffs.iter().filter(|(_, c)| c.parent() == ...)` looks
    // at; chunk positions don't appear in the iteration.
    let mut rebuilt: HashSet<Entity> = HashSet::default();

    for (entity, pos) in &new_chunks {
        if !rebuilt.insert(*entity) {
            continue;
        }
        rebuild_chunk_cliffs(
            &mut commands,
            *entity,
            *pos,
            &chunks,
            &manager,
            &cliffs,
            &mut meshes,
            material,
        );
    }

    // Rebuild every already-loaded neighbor that wasn't itself in `new_chunks`.
    // A neighbor that was already rebuilt above (because it was new this
    // frame) is skipped via the `rebuilt` guard.
    for (_, pos) in &new_chunks {
        for nb in chunk_neighbors(*pos) {
            let Some(nb_entity) = manager.get(nb) else {
                continue;
            };
            if !rebuilt.insert(nb_entity) {
                continue;
            }
            rebuild_chunk_cliffs(
                &mut commands,
                nb_entity,
                nb,
                &chunks,
                &manager,
                &cliffs,
                &mut meshes,
                material,
            );
        }
    }
}

/// Backwards-compatibility stub.
///
/// The previous implementation tracked emitted `(chunk, tile, side)` keys in
/// an `EmittedCliffs` resource and freed them on `ChunkUnloaded`. The merged
/// per-chunk mesh approach rebuilds the whole chunk's cliff mesh on every
/// `ChunkLoaded` (and neighbor-load), so no such dedupe set exists today.
/// This function is exported so existing callers compile cleanly; it does
/// nothing.
pub fn release_cliff_keys_on_unload() {}

// The four chunk-neighbors that share an edge with the given chunk.
#[inline]
fn chunk_neighbors(pos: ChunkPos) -> [ChunkPos; 4] {
    [
        ChunkPos::new(pos.x - 1, pos.y),
        ChunkPos::new(pos.x + 1, pos.y),
        ChunkPos::new(pos.x, pos.y - 1),
        ChunkPos::new(pos.x, pos.y + 1),
    ]
}

// Despawn the chunk's existing `ChunkCliff` child (if any) and spawn a fresh
// merged mesh built from the chunk's current `ChunkData` plus its loaded
// neighbors. If the chunk has no cliffs (all neighbors equal-or-higher, or
// the only drops face water tiles), nothing is spawned and the chunk simply
// has no `ChunkCliff` child this frame.
fn rebuild_chunk_cliffs(
    commands: &mut Commands,
    chunk_entity: Entity,
    chunk_pos: ChunkPos,
    chunks: &Query<&ChunkData>,
    manager: &ChunkManager,
    cliffs: &Query<(Entity, &ChildOf), With<ChunkCliff>>,
    meshes: &mut Assets<Mesh>,
    material: &Handle<ChunkCliffMaterial>,
) {
    // Despawn any existing cliff child so we can replace it with a fresh
    // merged mesh. A chunk has at most one `ChunkCliff` child (this system
    // is the only spawner), but the linear scan is harmless and avoids
    // having to thread a `Children` query into this function.
    for (cliff_entity, parent) in cliffs.iter() {
        if parent.parent() == chunk_entity {
            commands.entity(cliff_entity).try_despawn();
        }
    }

    let Ok(data) = chunks.get(chunk_entity) else {
        tracing::warn!(
            "rebuild_chunk_cliffs: ChunkData missing for entity {:?} ({:?}) — skipping rebuild",
            chunk_entity,
            chunk_pos,
        );
        return;
    };

    let Some(mesh) = build_chunk_cliff_mesh(data, chunk_pos, chunks, manager) else {
        // Empty mesh — no cliff drops in this chunk this frame.
        return;
    };

    let mesh_handle = meshes.add(mesh);

    commands.entity(chunk_entity).with_children(|parent| {
        parent.spawn((
            ChunkCliff,
            Mesh2d(mesh_handle),
            MeshMaterial2d(material.clone()),
            // The merged mesh bakes per-cliff Z offsets into its position
            // attribute (Z = `(h_n + 0.5) * HEIGHT_LAYER_Z_STEP`), so a flat
            // Transform Z of `RenderLayer::GROUND` suffices here.
            Transform::from_xyz(0.0, 0.0, RenderLayer::GROUND),
            Visibility::default(),
            Name::new(format!("ChunkCliff({}, {})", chunk_pos.x, chunk_pos.y)),
        ));
    });
}

// Build a single merged mesh containing every cliff parallelogram in
// `chunk_pos`. Returns `None` if there are no cliff drops (e.g. a perfectly
// flat chunk, or one whose only drops all face water). Each vertex carries
// its biome × side tint via `Mesh::ATTRIBUTE_COLOR` so a single material can
// render the whole mesh without needing per-face materials.
fn build_chunk_cliff_mesh(
    data: &ChunkData,
    chunk_pos: ChunkPos,
    chunks: &Query<&ChunkData>,
    manager: &ChunkManager,
) -> Option<Mesh> {
    let chunk_origin_world_xy = chunk_origin_world(chunk_pos);
    let chunk_size_i = CHUNK_SIZE as i32;

    // Pre-allocate generously: at most `CHUNK_SIZE^2 * 4` cliffs per chunk,
    // each contributing 4 vertices and 6 indices. The actual count is
    // typically a small fraction of that, but the over-allocation cost is
    // a handful of KB transient.
    let max_cliffs = (CHUNK_SIZE as usize * CHUNK_SIZE as usize) * 4;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(max_cliffs * 4);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(max_cliffs * 4);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(max_cliffs * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(max_cliffs * 6);

    for (local, tile) in data.iter() {
        let h_t = tile.height;

        for &side in &ALL_SIDES {
            let (dx, dy) = side.neighbor_offset();
            let nx = local.x as i32 + dx;
            let ny = local.y as i32 + dy;

            let nb_lookup: Option<(i8, TileKind)> =
                if nx >= 0 && ny >= 0 && nx < chunk_size_i && ny < chunk_size_i {
                    // Fast path: neighbor is in the same chunk.
                    let nb_tile = data.get(LocalTilePos::new(nx as u32, ny as u32));
                    Some((nb_tile.height, nb_tile.kind))
                } else {
                    // Slow path: neighbor lives in an adjacent chunk. If
                    // that chunk isn't loaded yet, skip the cliff —
                    // drawing it against an assumed height of 0 would
                    // flash a phantom cliff that disappears once the real
                    // neighbor arrives. The neighbor's own load will
                    // trigger a rebuild of *this* chunk (see
                    // `spawn_chunk_cliffs::rebuild_chunk_cliffs` for new
                    // chunks' neighbors), so the cliff will appear then.
                    let world_tile = WorldTile::new(
                        chunk_pos.x * chunk_size_i + nx,
                        chunk_pos.y * chunk_size_i + ny,
                    );
                    let nb_chunk_pos = ChunkPos::from_tile(world_tile);
                    match manager.get(nb_chunk_pos) {
                        None => None,
                        Some(nb_entity) => match chunks.get(nb_entity) {
                            Ok(nb_data) => {
                                let nb_local = nb_chunk_pos.local_of(world_tile);
                                let nb_tile = nb_data.get(nb_local);
                                Some((nb_tile.height, nb_tile.kind))
                            }
                            Err(_) => {
                                tracing::warn!(
                                    "build_chunk_cliff_mesh: ChunkManager points at entity {:?} for {:?} but ChunkData query failed — treating as unloaded",
                                    nb_entity,
                                    nb_chunk_pos,
                                );
                                None
                            }
                        },
                    }
                };

            let Some((h_n, nb_kind)) = nb_lookup else {
                continue;
            };

            if h_t <= h_n {
                // No drop on this side → no exposed cliff face.
                continue;
            }

            // **Skip cliffs facing water.** A grass tile next to a recessed
            // water tile used to draw a cliff that hung visibly behind the
            // water surface, reading as "grass extending over water". The
            // shoreline reads better without the wall — the screen-Y gap
            // plus the water shimmer alone communicates the coast.
            if nb_kind == TileKind::Water {
                continue;
            }

            // **Skip cliffs on either side of a stair landing.** A stair
            // tile IS the bridging surface between two height bands — its
            // job is to let the player walk straight through the elevation
            // gap. Drawing a wall between the stair and its lower neighbor
            // (or between a higher tile and a stair sitting below it) would
            // visually contradict the pathfinder's "this edge is walkable"
            // promise. Skip in both directions:
            //   * owning tile T is a stair → no downhill wall on its skirts;
            //   * neighbor is a stair → don't bury the landing behind a wall
            //     dropping onto it from above.
            if tile.kind == TileKind::Stairs || nb_kind == TileKind::Stairs {
                continue;
            }

            // World tile → chunk-local XY anchor (the diamond's bottom vertex
            // B at `height = 0`). The chunk's own `Transform` supplies the
            // chunk-origin world offset; we work in chunk-local space here.
            let world_tile = local.to_world(chunk_pos);
            let world_xy = tile_to_world(world_tile);
            let local_xy = world_xy - chunk_origin_world_xy;

            append_cliff_into_mesh(
                &mut positions,
                &mut uvs,
                &mut colors,
                &mut indices,
                local_xy,
                tile.kind as u8,
                h_t,
                h_n,
                side,
            );
        }
    }

    if indices.is_empty() {
        return None;
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    // Bevy's mesh2d pipeline detects `Mesh::ATTRIBUTE_COLOR` and injects the
    // `VERTEX_COLORS` shader-def, which the cliff shader uses to choose the
    // `mesh.color * tint` path.
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

// Append a single cliff parallelogram (4 vertices, 2 triangles, 6 indices)
// onto the in-progress per-chunk mesh buffers. `local_xy` is the higher tile
// T's chunk-local XY anchor (its diamond's bottom vertex at `height = 0`);
// `h_t` and `h_n` are the higher and lower tile heights in step units;
// `side` picks the diamond edge being filled.
//
// Per-vertex Z is baked into the position attribute as
// `(h_n + 0.5) * HEIGHT_LAYER_Z_STEP` so the parallelogram sits between the
// lower neighbor's tilemap (at `h_n * step`) and the higher tile's tilemap
// (at `h_t * step`). All four vertices share the same Z because the cliff
// is a flat polygon in screen space; the height-banding is purely a sort
// key relative to neighboring per-height tilemaps.
//
// Vertex order — counter-clockwise looking at the +Z face of the quad — is:
//   0 = top-left, 1 = top-right, 2 = bottom-right, 3 = bottom-left
// Indices `[0, 1, 2, 0, 2, 3]` tessellate this into two triangles.
fn append_cliff_into_mesh(
    positions: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    local_xy: Vec2,
    kind_index: u8,
    h_t: i8,
    h_n: i8,
    side: CliffSide,
) {
    let (lx, ly, rx, ry) = side.edge_endpoints();
    let cliff_z = (h_n as f32 + 0.5) * HEIGHT_LAYER_Z_STEP;
    let top_y = h_t as f32 * HEIGHT_STEP_PX;
    let bot_y = h_n as f32 * HEIGHT_STEP_PX;

    let top_left = [local_xy.x + lx, local_xy.y + ly + top_y, cliff_z];
    let top_right = [local_xy.x + rx, local_xy.y + ry + top_y, cliff_z];
    let bot_right = [local_xy.x + rx, local_xy.y + ry + bot_y, cliff_z];
    let bot_left = [local_xy.x + lx, local_xy.y + ly + bot_y, cliff_z];

    let base_index = positions.len() as u32;
    positions.push(top_left);
    positions.push(top_right);
    positions.push(bot_right);
    positions.push(bot_left);

    // Standard rectangular UVs: top edge u increases along the diamond edge,
    // v goes from 0 (top) to 1 (bottom) down the wall. Kept identical to
    // the previous per-face mesh so any future shader that wants striated
    // rock textures sees the same parameterization.
    uvs.push([0.0, 0.0]);
    uvs.push([1.0, 0.0]);
    uvs.push([1.0, 1.0]);
    uvs.push([0.0, 1.0]);

    // Per-vertex color = biome base × per-side AO brightness. Stored linear
    // because Bevy's vertex pipeline expects linear-space color attributes;
    // the `BASE_COLOR` table is sRGB-tagged so we round-trip through `Color`
    // / `LinearRgba`. All four corners of one cliff share the same color so
    // the interpolation is flat across the parallelogram.
    let color = cliff_vertex_color_linear(kind_index, side);
    colors.push(color);
    colors.push(color);
    colors.push(color);
    colors.push(color);

    indices.push(base_index);
    indices.push(base_index + 1);
    indices.push(base_index + 2);
    indices.push(base_index);
    indices.push(base_index + 2);
    indices.push(base_index + 3);
}

// Compute the per-vertex linear-space color for one cliff face. Pulls the
// biome's sRGB base from `BASE_COLOR`, applies the per-side AO brightness,
// then converts to linear space (Bevy's pipeline samples vertex colors in
// linear space). Falls back to a mid-grey if `kind_index` lands outside
// the table.
fn cliff_vertex_color_linear(kind_index: u8, side: CliffSide) -> [f32; 4] {
    let base = BASE_COLOR
        .get(kind_index as usize)
        .copied()
        .unwrap_or([128, 128, 128, 255]);
    let scale = side.brightness();
    let r = (base[0] as f32 * scale).clamp(0.0, 255.0) / 255.0;
    let g = (base[1] as f32 * scale).clamp(0.0, 255.0) / 255.0;
    let b = (base[2] as f32 * scale).clamp(0.0, 255.0) / 255.0;
    let a = base[3] as f32 / 255.0;
    let linear = Color::srgba(r, g, b, a).to_linear();
    [linear.red, linear.green, linear.blue, linear.alpha]
}

/// Compute the cliff-face tint for a given biome × side combination. Each
/// face gets a per-side brightness multiplier on the biome's
/// [`crate::atlas::BASE_COLOR`]. Retained as a public function for tests
/// and any debug overlays that want the same color the GPU sees.
///
/// Falls back to a neutral mid-grey if `kind`'s discriminant lands outside
/// the `BASE_COLOR` table (defensive — should be unreachable today).
pub fn cliff_color(kind: inf2d_world::TileKind, side: CliffSide) -> Color {
    cliff_color_u8(kind as u8, side)
}

// Internal flavor that takes a raw u8 index — kept for tests and as the
// sRGB-space mirror of `cliff_vertex_color_linear`.
fn cliff_color_u8(kind_index: u8, side: CliffSide) -> Color {
    let base = BASE_COLOR
        .get(kind_index as usize)
        .copied()
        .unwrap_or([128, 128, 128, 255]);
    let scale = side.brightness();
    let r = (base[0] as f32 * scale).clamp(0.0, 255.0) / 255.0;
    let g = (base[1] as f32 * scale).clamp(0.0, 255.0) / 255.0;
    let b = (base[2] as f32 * scale).clamp(0.0, 255.0) / 255.0;
    let a = base[3] as f32 / 255.0;
    Color::srgba(r, g, b, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use inf2d_world::TileKind;

    #[test]
    fn cliff_color_darkens_base_per_side() {
        // Grass base is (96, 156, 84). After per-side darken, R/G/B should
        // all drop below the base — and FrontLeft (× 0.75) should be
        // strictly brighter than BackLeft (× 0.55).
        let fl = cliff_color(TileKind::Grass, CliffSide::FrontLeft).to_srgba();
        let bl = cliff_color(TileKind::Grass, CliffSide::BackLeft).to_srgba();
        assert!(fl.red < 96.0 / 255.0);
        assert!(fl.green < 156.0 / 255.0);
        assert!(fl.blue < 84.0 / 255.0);
        assert!(fl.red > bl.red);
        assert!(fl.green > bl.green);
        assert!(fl.blue > bl.blue);
        assert!((fl.alpha - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cliff_color_defaults_on_out_of_range_index() {
        // Out-of-range index falls back to mid-grey × per-side brightness.
        let c = cliff_color_u8(99, CliffSide::FrontRight).to_srgba();
        let expected = 128.0 * CliffSide::FrontRight.brightness() / 255.0;
        assert!((c.red - expected).abs() < 1e-3);
        assert!((c.green - expected).abs() < 1e-3);
        assert!((c.blue - expected).abs() < 1e-3);
    }

    #[test]
    fn cliff_brightness_ranks_match_camera_facing() {
        // FrontLeft is the brightest (most camera-facing); FrontRight a
        // little darker; both back sides dimmest. The ordering is what
        // makes stacked cubes read as 3D — flipping it would invert the
        // fake AO and the cliffs would look concave instead of convex.
        let fl = CliffSide::FrontLeft.brightness();
        let fr = CliffSide::FrontRight.brightness();
        let bl = CliffSide::BackLeft.brightness();
        let br = CliffSide::BackRight.brightness();
        assert!(fl > fr);
        assert!(fr > bl);
        assert!((bl - br).abs() < f32::EPSILON);
    }

    #[test]
    fn edge_endpoints_lie_on_diamond() {
        // Verify each side's edge endpoints land on the diamond's L/T/R/B
        // vertices, never any other point.
        let eps = 1e-4;

        let (lx, ly, rx, ry) = CliffSide::FrontLeft.edge_endpoints();
        assert!((lx - (-TILE_WIDTH * 0.5)).abs() < eps);
        assert!((ly - TILE_HEIGHT * 0.5).abs() < eps);
        assert!(rx.abs() < eps);
        assert!(ry.abs() < eps);

        let (lx, ly, rx, ry) = CliffSide::FrontRight.edge_endpoints();
        assert!(lx.abs() < eps);
        assert!(ly.abs() < eps);
        assert!((rx - TILE_WIDTH * 0.5).abs() < eps);
        assert!((ry - TILE_HEIGHT * 0.5).abs() < eps);

        let (lx, ly, rx, ry) = CliffSide::BackLeft.edge_endpoints();
        assert!((lx - (-TILE_WIDTH * 0.5)).abs() < eps);
        assert!((ly - TILE_HEIGHT * 0.5).abs() < eps);
        assert!(rx.abs() < eps);
        assert!((ry - TILE_HEIGHT).abs() < eps);

        let (lx, ly, rx, ry) = CliffSide::BackRight.edge_endpoints();
        assert!(lx.abs() < eps);
        assert!((ly - TILE_HEIGHT).abs() < eps);
        assert!((rx - TILE_WIDTH * 0.5).abs() < eps);
        assert!((ry - TILE_HEIGHT * 0.5).abs() < eps);
    }

    #[test]
    fn neighbor_offsets_are_axis_aligned() {
        // Neighbor offsets: front sides hit screen-down (decreasing y/x),
        // back sides hit screen-up (increasing y/x).
        assert_eq!(CliffSide::FrontLeft.neighbor_offset(), (-1, 0));
        assert_eq!(CliffSide::FrontRight.neighbor_offset(), (0, -1));
        assert_eq!(CliffSide::BackLeft.neighbor_offset(), (0, 1));
        assert_eq!(CliffSide::BackRight.neighbor_offset(), (1, 0));
    }

    #[test]
    fn append_cliff_pushes_four_vertices_two_triangles() {
        // One call appends exactly 4 positions/uvs/colors and 6 indices, so
        // a chunk with N cliffs ends up with a single 4N-vertex mesh.
        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut uvs: Vec<[f32; 2]> = Vec::new();
        let mut colors: Vec<[f32; 4]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        append_cliff_into_mesh(
            &mut positions,
            &mut uvs,
            &mut colors,
            &mut indices,
            Vec2::ZERO,
            TileKind::Grass as u8,
            2,
            0,
            CliffSide::FrontLeft,
        );
        assert_eq!(positions.len(), 4);
        assert_eq!(uvs.len(), 4);
        assert_eq!(colors.len(), 4);
        assert_eq!(indices, vec![0, 1, 2, 0, 2, 3]);
    }

    #[test]
    fn append_cliff_offsets_top_and_bottom_by_step() {
        // For a 2-step drop, the top edge sits `2 * HEIGHT_STEP_PX` above
        // the bottom edge. The Z value is `(h_n + 0.5) * 0.01 = 0.005`
        // for `h_n = 0`.
        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut uvs: Vec<[f32; 2]> = Vec::new();
        let mut colors: Vec<[f32; 4]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        append_cliff_into_mesh(
            &mut positions,
            &mut uvs,
            &mut colors,
            &mut indices,
            Vec2::ZERO,
            TileKind::Stone as u8,
            2,
            0,
            CliffSide::FrontLeft,
        );
        // Top y - bottom y == 2 * HEIGHT_STEP_PX for the right corners
        // (left corners share the same offset).
        let top_y = positions[1][1];
        let bot_y = positions[2][1];
        assert!((top_y - bot_y - 2.0 * HEIGHT_STEP_PX).abs() < 1e-4);
        assert!((positions[0][2] - 0.005).abs() < 1e-6);
    }

    #[test]
    fn chunk_neighbors_are_four_orthogonal_chunks() {
        let pos = ChunkPos::new(3, -2);
        let nbs = chunk_neighbors(pos);
        assert!(nbs.contains(&ChunkPos::new(2, -2)));
        assert!(nbs.contains(&ChunkPos::new(4, -2)));
        assert!(nbs.contains(&ChunkPos::new(3, -3)));
        assert!(nbs.contains(&ChunkPos::new(3, -1)));
        assert_eq!(nbs.len(), 4);
    }
}
