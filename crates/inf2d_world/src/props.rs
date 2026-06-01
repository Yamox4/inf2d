//! Procedural prop scatter for chunks. Currently spawns trees on grass/dirt
//! tiles via a deterministic per-chunk Poisson-disk sampler. Re-uses the world
//! seed + `stream::SCATTER` so scattering is reproducible — re-entering an area
//! always sees the same trees.
//!
//! ## Cycle-free wiring
//!
//! `inf2d_render` and `inf2d_worldgen` both depend on `inf2d_world`, so this
//! module cannot import either crate without creating a Cargo cycle. Two
//! consequences:
//!
//! 1. The world seed is read from a [`WorldSeed`] resource defined locally. The
//!    worldgen plugin should mirror `BiomeParams::world_seed` into this
//!    resource at startup; if it doesn't, a stable default is used so scatter
//!    stays deterministic across runs.
//! 2. Prop entities ship with only the components `inf2d_world` can supply
//!    (markers, [`Transform`], [`Visibility`], a coarse [`Sprite`] placeholder).
//!    The full sprite-stack tree visual — `SpriteStack` + `DropShadow` +
//!    `IsoAnchor` — is attached by a small render-side helper that watches for
//!    the [`Tree`] marker. Today, trees render as a flat brown rectangle until
//!    that helper is wired; the deterministic scatter is the load-bearing piece.

use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use inf2d_core::{
    chunk_origin_world, chunk_rng, tile_to_world_with_height, ChunkPos, LocalTilePos,
    WorldTile, CHUNK_SIZE,
};
use rand::Rng;

use crate::{ChunkData, ChunkLoaded, TileKind};

// Stream tag matching inf2d_core::rng::stream::SCATTER. The constant lives in
// a private module over there, so we re-declare it here with the same value
// to keep the per-chunk RNG sequence in lock-step with the documented stream
// layout (0=TERRAIN, 1=MOISTURE, 2=SCATTER, ...).
const SCATTER_STREAM: u32 = 2;

// Fallback world seed used when no WorldSeed resource has been inserted
// (e.g. tests, headless tools that skip the worldgen plugin). Picked so that
// flipping bits in either half visibly changes the scatter pattern.
const FALLBACK_WORLD_SEED: u64 = 0xCAFE_F00D;

// Minimum Chebyshev tile distance enforced between any two trees inside a
// chunk. Three tiles reads as "scattered woodland" rather than "dense forest"
// at the current CHUNK_SIZE = 32.
const MIN_TREE_SPACING_TILES: i32 = 3;

// Number of candidate samples taken per chunk before the sampler gives up.
// Fifty is plenty for sparse forests — at min-spacing 3 the chunk holds
// roughly twenty trees max, and the rejection rate keeps the actual count
// well below the candidate count.
const POISSON_CANDIDATES_PER_CHUNK: u32 = 50;

// Placeholder sprite size for the trunk rectangle. Pure aesthetic — keeps
// the rectangle visible against the iso diamond's left vertex.
const PLACEHOLDER_TRUNK_SIZE: Vec2 = Vec2::new(8.0, 18.0);

/// World seed surfaced to systems inside `inf2d_world`. The worldgen plugin
/// owns the canonical seed inside `BiomeParams`; the cycle prevents us from
/// importing it, so we mirror the value through this resource.
///
/// Insert this resource alongside `WorldgenPlugin` (or any custom generator)
/// to keep prop scatter aligned with terrain generation. If absent, scatter
/// falls back to a stable constant and still produces a deterministic — if
/// terrain-decoupled — pattern.
#[derive(Resource, Reflect, Debug, Clone, Copy)]
#[reflect(Resource)]
pub struct WorldSeed(pub u64);

impl Default for WorldSeed {
    fn default() -> Self {
        Self(FALLBACK_WORLD_SEED)
    }
}

/// Marker on every prop entity (currently only trees, eventually rocks/shrubs/...).
#[derive(Component, Reflect, Default, Debug)]
#[reflect(Component)]
pub struct Prop;

/// Marker on trees specifically. Future systems (chopping, axe-target picking)
/// can query for this.
#[derive(Component, Reflect, Default, Debug)]
#[reflect(Component)]
pub struct Tree;

/// `Update` system: for every freshly-loaded chunk, run the deterministic
/// scatter and spawn one tree entity per accepted sample.
pub fn spawn_chunk_props(
    mut commands: Commands,
    mut events: MessageReader<ChunkLoaded>,
    chunks: Query<&ChunkData>,
    seed: Option<Res<WorldSeed>>,
) {
    let world_seed = seed.map(|s| s.0).unwrap_or(FALLBACK_WORLD_SEED);
    for ev in events.read() {
        let Ok(data) = chunks.get(ev.entity) else {
            continue;
        };
        let positions = poisson_disk_tiles(world_seed, ev.pos, data);
        for (tile, height) in positions {
            spawn_tree(&mut commands, ev.entity, ev.pos, tile, height);
        }
    }
}

// Generate up to ~20 tile positions per chunk via a simple grid-based
// Poisson approximation. Tiles are accepted only if they're grass or dirt,
// at or above sea level (height >= 0), and at least
// MIN_TREE_SPACING_TILES away (Chebyshev) from every previously-accepted
// sample inside the same chunk.
fn poisson_disk_tiles(
    world_seed: u64,
    chunk_pos: ChunkPos,
    data: &ChunkData,
) -> Vec<(WorldTile, i32)> {
    let mut rng = chunk_rng(world_seed, chunk_pos, SCATTER_STREAM);
    let mut placed: HashSet<(i32, i32)> = HashSet::default();
    let mut out: Vec<(WorldTile, i32)> = Vec::new();

    for _ in 0..POISSON_CANDIDATES_PER_CHUNK {
        let lx = rng.random_range(0..CHUNK_SIZE as i32);
        let ly = rng.random_range(0..CHUNK_SIZE as i32);
        let local = LocalTilePos::new(lx as u32, ly as u32);
        let tile = data.get(local);

        // Trees only on grass / dirt, height >= 0 (no underwater placements).
        let kind_ok = matches!(tile.kind, TileKind::Grass | TileKind::Dirt);
        if !kind_ok || tile.height < 0 {
            continue;
        }

        // Spacing check against everything already placed in this chunk.
        let too_close = placed.iter().any(|&(px, py)| {
            let dx = (lx - px).abs();
            let dy = (ly - py).abs();
            dx.max(dy) < MIN_TREE_SPACING_TILES
        });
        if too_close {
            continue;
        }

        placed.insert((lx, ly));
        let world_tile = WorldTile::new(
            chunk_pos.x * CHUNK_SIZE as i32 + lx,
            chunk_pos.y * CHUNK_SIZE as i32 + ly,
        );
        out.push((world_tile, tile.height as i32));
    }

    out
}

// Spawn one tree entity as a child of the owning chunk. Position is computed
// in the chunk's local frame so transform inheritance keeps the prop pinned
// to its tile when the chunk entity moves (it doesn't today, but the
// hierarchy makes despawn-on-unload cascade automatically).
//
// The visual is a placeholder flat brown sprite. The intended look — a
// sprite-stack with brown trunk fading to green canopy plus a soft drop
// shadow — lives in `inf2d_render::{SpriteStack, DropShadow, IsoAnchor}`;
// attaching those here would create a Cargo cycle since `inf2d_render`
// already depends on `inf2d_world`. A render-side observer keyed off the
// `Tree` marker is the right home for the upgrade.
fn spawn_tree(
    commands: &mut Commands,
    chunk_entity: Entity,
    chunk_pos: ChunkPos,
    tile: WorldTile,
    height: i32,
) {
    let world = tile_to_world_with_height(tile, height);
    let origin = chunk_origin_world(chunk_pos);
    let local = world - origin;

    // Local Z of 2.0 matches `inf2d_render::RenderLayer::ENTITY` so the
    // placeholder sprite composites above ground tiles and decals. Kept as a
    // plain literal to avoid pulling `inf2d_render` into this crate.
    const ENTITY_LAYER_Z: f32 = 2.0;

    let tree = commands
        .spawn((
            Prop,
            Tree,
            tile,
            Sprite {
                color: Color::srgb(0.30, 0.18, 0.08),
                custom_size: Some(PLACEHOLDER_TRUNK_SIZE),
                ..default()
            },
            Transform::from_xyz(local.x, local.y + PLACEHOLDER_TRUNK_SIZE.y * 0.5, ENTITY_LAYER_Z),
            Visibility::default(),
            Name::new("Tree"),
        ))
        .id();
    commands.entity(chunk_entity).add_child(tree);
}
