#![deny(unsafe_code)]
//! Slime mob — first AI archetype. Wanders aimlessly inside its starting
//! chunk, hops via a simple sin-wave bob, can be left-clicked to take damage.
//!
//! ## Ownership / cleanup
//!
//! Slimes are **top-level entities** (not parented to the chunk they spawn
//! from). Parenting would force every wander system to do math in chunk-local
//! space, but the player and every other moving entity in the project use
//! world-space `Transform`. To keep both conventions consistent each slime
//! carries a [`ChunkOwner`] tag with the `ChunkPos` it spawned in; a small
//! [`despawn_orphan_slimes`] system tears slimes down when their owning chunk
//! streams out, mirroring the cascade-despawn parents would have given us.

use bevy::prelude::*;
use inf2d_core::rng::chunk_rng;
use inf2d_core::{
    tile_to_world_with_height, ChunkPos, LocalTilePos, WorldTile, CHUNK_SIZE,
};
use inf2d_world::{ChunkData, ChunkLoaded, ChunkUnloaded, TileKind, WorldSeed};
use inf2d_render::{DropShadow, IsoAnchor, RenderLayer, SpriteStack, SpriteStackSlice};

use crate::stats::{DamageEvent, Health};

/// Marker for a slime mob — the first AI archetype shipped by the gameplay
/// crate.
#[derive(Component, Debug)]
pub struct Slime;

/// Wander state attached to every slime. `goal` is the next tile the mob is
/// drifting toward; `picked_at` records the wall-clock second the goal was
/// chosen so [`wander_slimes`] can re-pick after a timeout even when the
/// slime hasn't quite arrived.
#[derive(Component, Debug)]
pub struct WanderTarget {
    /// Next tile the slime is drifting toward.
    pub goal: WorldTile,
    /// `time.elapsed_secs()` when this target was picked.
    pub picked_at: f32,
}

/// Tag carrying the chunk the slime was originally spawned in, so a small
/// cleanup pass can despawn it when that chunk unloads. See module docs.
#[derive(Component, Debug, Clone, Copy)]
pub struct ChunkOwner(pub ChunkPos);

/// Distinct from terrain (`0`), moisture (`1`), scatter (`2`), structure
/// (`3`), and loot (`4`) — keeps slime placement independent of every other
/// per-chunk random stream so a change to either side doesn't shift the
/// other.
const MOB_STREAM: u32 = 6;

/// Upper bound on slimes per chunk; the actual count is rolled per chunk in
/// `0..=MOBS_PER_CHUNK_MAX`.
const MOBS_PER_CHUNK_MAX: u32 = 2;

/// Fallback world seed when [`WorldSeed`] is absent (tests, headless tools).
/// Same shape as `props.rs` uses for tree scatter so the two streams stay
/// deterministic together.
const FALLBACK_WORLD_SEED: u64 = 0xDEAD_BEEF;

/// World-space vertical lift applied to the slime sprite anchor so its visual
/// base sits on the tile surface instead of being bisected by it.
const SLIME_GROUND_OFFSET: f32 = 8.0;

/// Click-pick radius (world units) used by [`damage_clicked_slimes`] to
/// associate a cursor pick with the nearest slime.
const CLICK_RADIUS: f32 = 16.0;

/// Damage applied per left-click on a slime. `Health::full(30.0)` over
/// `10.0`-per-click works out to three clicks to kill.
const CLICK_DAMAGE: f32 = 10.0;

/// Spawn a few slimes per loaded chunk on grass tiles. Placement is
/// deterministic from `(world_seed, chunk, MOB_STREAM)` so re-loading the
/// same chunk re-spawns the same population.
pub fn spawn_chunk_mobs(
    mut commands: Commands,
    mut events: MessageReader<ChunkLoaded>,
    chunks: Query<&ChunkData>,
    seed: Option<Res<WorldSeed>>,
) {
    use rand::Rng;
    let world_seed = seed.map(|s| s.0).unwrap_or(FALLBACK_WORLD_SEED);

    for ev in events.read() {
        let Ok(data) = chunks.get(ev.entity) else {
            continue;
        };
        let mut rng = chunk_rng(world_seed, ev.pos, MOB_STREAM);
        let count = rng.random_range(0..=MOBS_PER_CHUNK_MAX);
        for _ in 0..count {
            // Reject loop: roll random local tiles until we find grass at or
            // above sea level. Capped at 20 tries so a stone/water-only chunk
            // gives up instead of spinning.
            let mut spot: Option<(i32, i32, i32)> = None;
            for _ in 0..20 {
                let lx = rng.random_range(0..CHUNK_SIZE as i32);
                let ly = rng.random_range(0..CHUNK_SIZE as i32);
                let tile = data.get(LocalTilePos::new(lx as u32, ly as u32));
                if tile.kind == TileKind::Grass && tile.height >= 0 {
                    spot = Some((lx, ly, tile.height as i32));
                    break;
                }
            }
            let Some((lx, ly, height)) = spot else {
                continue;
            };
            let world_tile = WorldTile::new(
                ev.pos.x * CHUNK_SIZE as i32 + lx,
                ev.pos.y * CHUNK_SIZE as i32 + ly,
            );
            let pos = tile_to_world_with_height(world_tile, height);
            commands.spawn((
                Slime,
                Health::full(30.0),
                WanderTarget {
                    goal: world_tile,
                    picked_at: 0.0,
                },
                ChunkOwner(ev.pos),
                SpriteStack {
                    slices: 8,
                    slice_size: Vec2::new(14.0, 7.0),
                    slice_spacing: 1.0,
                    base_color: Color::srgb(0.35, 0.65, 0.40),
                    top_color: Color::srgb(0.55, 0.85, 0.50),
                },
                IsoAnchor::default(),
                DropShadow {
                    radius: 10.0,
                    squash: 0.5,
                    color: Color::srgba(0.0, 0.0, 0.0, 0.5),
                },
                Transform::from_xyz(pos.x, pos.y + SLIME_GROUND_OFFSET, RenderLayer::ENTITY),
                Visibility::default(),
                Name::new("Slime"),
            ));
        }
    }
}

/// Each slime drifts toward its [`WanderTarget`] at a slow speed; when close
/// (or the target has aged past its timeout) it picks a new tile within a few
/// cells of the current one. Cheap "wander in a region" behavior — no
/// pathfinding, no collision checks, just a velocity toward the goal.
pub fn wander_slimes(
    time: Res<Time>,
    mut q: Query<(&mut Transform, &mut WanderTarget), With<Slime>>,
) {
    use rand::{Rng, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    let dt = time.delta_secs();
    let now = time.elapsed_secs();
    // World units per second. Slow ooze — slimes don't sprint.
    let speed = 18.0;

    for (mut transform, mut target) in &mut q {
        // Height lookup is approximated to ground level — slimes only spawn
        // on `height >= 0` grass, and a few pixels of vertical mismatch
        // during wander reads as part of the bob.
        let goal_world = tile_to_world_with_height(target.goal, 0);
        let pos = transform.translation.truncate();
        let delta = goal_world - pos;
        let dist = delta.length();
        if dist < 4.0 || now - target.picked_at > 6.0 {
            // Re-pick: a tile within 3 cells of the current. Seed from the
            // wall clock + the slime's X so neighbors don't synchronize.
            let seed = (now * 1000.0) as u64
                ^ ((transform.translation.x * 13.0) as i32 as u64);
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
            let dx = rng.random_range(-3..=3);
            let dy = rng.random_range(-3..=3);
            target.goal = WorldTile::new(target.goal.x + dx, target.goal.y + dy);
            target.picked_at = now;
            continue;
        }
        let step = delta.normalize() * speed * dt;
        transform.translation.x += step.x;
        transform.translation.y += step.y;
    }
}

/// Sin-wave bob for every slime's sprite-stack children — the same idea as
/// [`crate::bob_player_sprites`] but unconditional (slimes always bob, even
/// at rest). The base-Y for each slice is restored from its index before the
/// bob is overlaid, so the offset doesn't compound across frames.
pub fn bob_slimes(
    time: Res<Time>,
    slimes: Query<&Children, With<Slime>>,
    mut slices: Query<&mut Transform, With<SpriteStackSlice>>,
) {
    /// Peak vertical bob, in world units.
    const BOB_AMPLITUDE: f32 = 0.5;
    /// Bob frequency in radians per second. 6 rad/s ≈ 0.95 Hz — a slow
    /// pulsing breath that reads as "this thing is alive".
    const BOB_RATE: f32 = 6.0;

    let bob = (time.elapsed_secs() * BOB_RATE).sin() * BOB_AMPLITUDE;
    for children in &slimes {
        let mut slice_index: u32 = 0;
        for child in children.iter() {
            let Ok(mut transform) = slices.get_mut(child) else {
                continue;
            };
            // Absolute assignment — restore base-Y first, then overlay bob,
            // so re-running this system doesn't accumulate offsets.
            transform.translation.y = slice_index as f32 + bob;
            slice_index += 1;
        }
    }
}

/// Dispatch a [`DamageEvent`] when the user left-clicks within
/// [`CLICK_RADIUS`] of a slime. Picks the *first* matching slime — there's no
/// expectation of two slimes overlapping the same pixel, and the early
/// return keeps each click from damaging an arbitrary cluster.
pub fn damage_clicked_slimes(
    input: Res<inf2d_input::InputState>,
    pick: Res<inf2d_camera::CursorPick>,
    slimes: Query<(Entity, &Transform), With<Slime>>,
    mut dmg: MessageWriter<DamageEvent>,
) {
    if !input.select_just_pressed {
        return;
    }
    let Some(cursor_world) = pick.world else {
        return;
    };
    for (entity, transform) in &slimes {
        let dist = (transform.translation.truncate() - cursor_world).length();
        if dist < CLICK_RADIUS {
            dmg.write(DamageEvent {
                victim: entity,
                amount: CLICK_DAMAGE,
            });
            return;
        }
    }
}

/// Despawn every slime whose owning chunk was just unloaded. Compensates for
/// not parenting slimes to chunks (see module docs) — without this they'd
/// linger in unloaded regions and accumulate unboundedly.
pub fn despawn_orphan_slimes(
    mut commands: Commands,
    mut events: MessageReader<ChunkUnloaded>,
    slimes: Query<(Entity, &ChunkOwner), With<Slime>>,
) {
    for ev in events.read() {
        for (entity, owner) in &slimes {
            if owner.0 == ev.pos {
                commands.entity(entity).try_despawn();
            }
        }
    }
}

/// Plugin: schedules the slime spawn, wander, bob, click-damage, and orphan
/// cleanup systems on `Update`.
pub struct MobsPlugin;

impl Plugin for MobsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                spawn_chunk_mobs,
                wander_slimes,
                bob_slimes,
                damage_clicked_slimes,
                despawn_orphan_slimes,
            ),
        );
    }
}
