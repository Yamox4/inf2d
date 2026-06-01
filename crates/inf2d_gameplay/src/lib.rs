#![deny(unsafe_code)]
//! Gameplay loop: player entity, click-to-move via A* pathfinding, and a
//! follow-camera driver.
//!
//! Wiring order: add [`GameplayPlugin`] after `inf2d_camera::CameraPlugin` and
//! [`inf2d_pathfinding::PathfindingPlugin`]. On startup the plugin spawns a
//! single [`Player`] entity at world origin and switches the camera rig into
//! [`inf2d_camera::CameraMode::Follow`] tracking that entity. Each left-click
//! turns into a [`PathRequest`]; the resolved path is consumed waypoint by
//! waypoint by [`walk_along_path`] and gizmo-drawn by [`draw_path_gizmo`].

mod mobs;
mod replan;
mod stats;

use avian2d::prelude::*;
use bevy::math::curve::EaseFunction;
use bevy::prelude::*;
use bevy_tweening::lens::TransformScaleLens;
use bevy_tweening::{Lens, Tween, TweenAnim};
use inf2d_camera::{CameraMode, CameraRig, CursorPick, ShakeRequest};
use inf2d_core::{
    tile_to_world_with_height, ChunkPos, CoreSet, LocalTilePos, SimulationSet, WorldTile,
    CHUNK_SIZE, TILE_WIDTH,
};
use inf2d_input::InputState;
use inf2d_pathfinding::{PathFound, PathRequest};
use inf2d_physics::GameLayer;
use inf2d_render::{
    DropShadow, Emitter, EmitterPreset, EmitterShape, IsoAnchor, Particle, ParticleAssets,
    RenderLayer, SpriteStack, SpriteStackSlice,
};
use inf2d_world::{ActiveGenerator, ChunkData, ChunkManager, Tree};
use std::f32::consts::TAU;
use std::time::Duration;

pub use mobs::{MobsPlugin, Slime};
pub use replan::{replan_paths_on_chunk_change, PendingReplan};
pub use stats::{DamageEvent, DeathEvent, Health, StatsPlugin};

/// Plugin: registers the player's reflected components, the startup spawn, and the
/// click-to-move / follow-camera / gizmo systems.
pub struct GameplayPlugin;

impl Plugin for GameplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((stats::StatsPlugin, mobs::MobsPlugin))
            .register_type::<MoveSpeed>()
            .register_type::<Player>()
            .add_systems(
                Startup,
                (spawn_player, set_follow_player_on_startup).chain(),
            );
        app.add_systems(
            Update,
            (
                handle_click_to_move,
                spawn_click_ripple,
                // Run BEFORE `apply_paths` so any replan requests fired this
                // frame are solved by the pathfinder (which also lives in
                // `SimulationSet`) and applied without a frame of lag.
                replan_paths_on_chunk_change,
                apply_paths,
                walk_along_path,
                bob_player_sprites,
                draw_path_gizmo,
                toggle_follow_camera_on_f,
            )
                .chain()
                .in_set(SimulationSet),
        );
        // `tick_despawn_after` is unordered relative to the simulation chain —
        // it just decrements a timer and reaps zero-or-below entities.
        app.add_systems(Update, tick_despawn_after);
        app.add_systems(Update, update_follow_camera.in_set(CoreSet));
        // Bridge: `inf2d_world::props` spawns trees with placeholder sprites because it
        // can't depend on `inf2d_render` (cycle). Here in gameplay we have access to
        // both crates, so we can attach the SpriteStack visual + drop shadow + iso
        // anchor when a Tree appears. Runs every frame; `Added<Tree>` filters cheaply.
        app.add_systems(Update, attach_tree_visuals);
    }
}

/// Marker for the single player-controlled entity. Slice 3 only ever spawns one.
///
/// `ground_offset` lifts the sprite up so its visual base sits on the tile surface
/// rather than centered through the tile. `current_tile` / `current_height` are the
/// authoritative logical position; gameplay reads these instead of reverse-projecting
/// the visual `Transform`, which would be off after `ground_offset` + elevation
/// stacking are applied.
#[derive(Component, Reflect, Debug, Clone, Copy)]
#[reflect(Component)]
pub struct Player {
    pub ground_offset: f32,
    pub current_tile: WorldTile,
    pub current_height: i32,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            ground_offset: 16.0,
            current_tile: WorldTile::new(0, 0),
            current_height: 0,
        }
    }
}

/// Remaining waypoints in reverse order, so `pop()` yields the next tile to walk
/// toward. Empty = idle (no current path).
///
/// `velocity` is the current world-space velocity used by [`walk_along_path`] for
/// frame-rate-independent easing. Stored here (rather than as a separate
/// `Velocity` component) so it lives and dies with the path it belongs to —
/// when `path` is emptied and the target is removed, momentum naturally resets.
///
/// `goal` is the **original** destination tile (and elevation) the path was
/// computed for, snapshotted from the latest [`PathFound`] in
/// [`apply_paths`]. It stays stable as waypoints are popped, so chunk-streaming
/// replans (see [`replan_paths_on_chunk_change`]) can always re-issue a
/// [`PathRequest`] toward the *true* destination even after most of the path
/// has already been consumed. `None` = idle / no outstanding travel intent.
#[derive(Component, Debug, Default)]
pub struct MoveTarget {
    /// Remaining waypoints, ordered so `pop()` yields the next one to walk
    /// toward.
    pub path: Vec<(WorldTile, i32)>,
    /// Current world-space velocity for [`walk_along_path`]'s easing.
    pub velocity: Vec2,
    /// Original `(tile, height)` destination of the active path, preserved
    /// across waypoint consumption so replans always target the true goal.
    pub goal: Option<(WorldTile, i32)>,
}

#[derive(Component, Reflect, Debug, Clone, Copy)]
#[reflect(Component)]
pub struct MoveSpeed {
    pub tiles_per_second: f32,
}

impl Default for MoveSpeed {
    fn default() -> Self {
        Self { tiles_per_second: 2.0 }
    }
}

/// One-shot lifetime: decremented by [`tick_despawn_after`] each `Update`; when
/// it falls below zero the entity (and its children, via Bevy's default
/// hierarchy semantics) are despawned.
///
/// Used by the click-ripple sparkle and footstep dust emitters so they vanish
/// after their burst has finished animating, without each VFX needing its own
/// bespoke teardown system.
#[derive(Component, Debug, Clone, Copy)]
pub struct DespawnAfter {
    /// Remaining seconds until despawn. Constructed with `DespawnAfter::secs(t)`.
    pub secs: f32,
}

impl DespawnAfter {
    /// Construct a [`DespawnAfter`] that fires after `t` seconds.
    pub fn secs(t: f32) -> Self {
        Self { secs: t }
    }
}

/// Spawn the player and a sprite-stacked "fake 3D" visual.
///
/// The spawn point is picked by synchronously generating chunk (0, 0) and scanning
/// for the non-solid tile closest to the chunk's center. Without this, deterministic
/// worldgen seeds where (0, 0) lands in water leave the player on top of the ocean.
pub fn spawn_player(mut commands: Commands, generator: Res<ActiveGenerator>) {
    let mut player = Player::default();

    let chunk_pos = ChunkPos::new(0, 0);
    let chunk_data = generator.generate(chunk_pos);
    let center = (CHUNK_SIZE as i32) / 2;
    let mut best: Option<(i32, WorldTile, i32)> = None;
    for ly in 0..CHUNK_SIZE as i32 {
        for lx in 0..CHUNK_SIZE as i32 {
            let local = LocalTilePos::new(lx as u32, ly as u32);
            let tile = chunk_data.get(local);
            if tile.kind.is_solid() {
                continue;
            }
            let dx = lx - center;
            let dy = ly - center;
            let d2 = dx * dx + dy * dy;
            let world_tile = WorldTile::new(
                chunk_pos.x * CHUNK_SIZE as i32 + lx,
                chunk_pos.y * CHUNK_SIZE as i32 + ly,
            );
            if best.map(|b| d2 < b.0).unwrap_or(true) {
                best = Some((d2, world_tile, tile.height as i32));
            }
        }
    }
    let (start_tile, start_height) = match best {
        Some((_, tile, h)) => (tile, h),
        None => {
            warn!(
                "no walkable tile in chunk (0,0); spawning at origin anyway — player will be stuck"
            );
            (WorldTile::new(0, 0), 0)
        }
    };
    player.current_tile = start_tile;
    player.current_height = start_height;

    let base = tile_to_world_with_height(start_tile, start_height);
    let transform = Transform::from_xyz(base.x, base.y + player.ground_offset, RenderLayer::ENTITY);

    // Sprite-stacked body. 12 slices, 1 px apart, darker at the base, brighter
    // at the top — same idea as Brigador / RimWorld iso props built from
    // vertical stacks. The actual slice children are spawned by
    // `inf2d_render::SpriteStackPlugin` on the next `Update`.
    commands.spawn((
        player,
        Health::full(100.0),
        MoveSpeed::default(),
        MoveTarget::default(),
        IsoAnchor::default(),
        DropShadow {
            radius: 11.0,
            squash: 0.5,
            color: Color::srgba(0.0, 0.0, 0.0, 0.55),
        },
        SpriteStack {
            slices: 12,
            slice_size: Vec2::new(18.0, 10.0),
            slice_spacing: 1.0,
            base_color: Color::srgb(0.4, 0.05, 0.02),
            top_color: Color::srgb(1.00, 0.42, 0.28),
        },
        // Kinematic body: gameplay code moves the `Transform` each frame (see
        // `walk_along_path`), and Avian's transform-to-position sync keeps the
        // physics position in lockstep. Kinematic pushes other bodies on contact
        // but is not pushed back, which matches the design — gameplay decides
        // where the player goes; physics only exists so projectiles, mobs, and
        // knockback have something to interact with. Radius 8 px matches the
        // visual width of the sprite-stack body.
        RigidBody::Kinematic,
        Collider::circle(8.0),
        LinearVelocity::default(),
        CollisionLayers::new(
            GameLayer::Player,
            [GameLayer::Terrain, GameLayer::Mob],
        ),
        transform,
        Visibility::default(),
        Name::new("Player"),
    ));
}

pub fn set_follow_player_on_startup(
    mut rigs: Query<&mut CameraRig>,
    players: Query<Entity, With<Player>>,
) {
    let Ok(player) = players.single() else { return; };
    let Ok(mut rig) = rigs.single_mut() else { return; };
    rig.mode = CameraMode::Follow { entity: player, lag: 8.0 };
}

/// Tween lens that snaps a [`CameraRig`]'s zoom back toward a target value.
///
/// Used by [`toggle_follow_camera_on_f`] to give the follow re-engage a tactile
/// "snap" feel. Writes both `zoom` and `zoom_target` so the camera's own
/// exponential-smoothing zoom system (see `inf2d_camera::zoom`) doesn't
/// immediately drag the value back toward whatever target the user left
/// behind.
struct CameraZoomLens {
    from: f32,
    to: f32,
}

impl Lens<CameraRig> for CameraZoomLens {
    fn lerp(&mut self, mut target: Mut<'_, CameraRig>, ratio: f32) {
        let z = self.from + (self.to - self.from) * ratio;
        target.zoom = z;
        target.zoom_target = z;
    }
}

/// F key re-engages follow mode after the user pan-drags loose. (Pan kicks the
/// camera into Free; F snaps it back to following the player.)
///
/// On the Free → Follow transition this also spawns a 0.4 s ease-out tween on
/// the camera rig that pulls `zoom` back toward `1.0`, giving the re-engage a
/// tactile "snap" feel. Going Follow → Free is unaffected.
pub fn toggle_follow_camera_on_f(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut rigs: Query<(Entity, &mut CameraRig)>,
    players: Query<Entity, With<Player>>,
) {
    if !keys.just_pressed(KeyCode::KeyF) {
        return;
    }
    let Ok(player) = players.single() else { return; };
    let Ok((rig_entity, mut rig)) = rigs.single_mut() else { return; };
    let was_free = matches!(rig.mode, CameraMode::Free);
    rig.mode = match rig.mode {
        CameraMode::Free => CameraMode::Follow { entity: player, lag: 8.0 },
        _ => CameraMode::Free,
    };

    // Only snap-zoom on the Free → Follow transition, and only when the user
    // has actually drifted from the default zoom; otherwise the tween is a
    // no-op that fights the camera's idle smoothing for one frame.
    if was_free && (rig.zoom - 1.0).abs() > f32::EPSILON {
        let tween = Tween::new(
            EaseFunction::QuadraticOut,
            Duration::from_millis(400),
            CameraZoomLens { from: rig.zoom, to: 1.0 },
        );
        commands.entity(rig_entity).insert(TweenAnim::new(tween));
    }
}

/// Translate left-click + a valid cursor pick into a [`PathRequest`] from the
/// player's *logical* current tile (not reverse-projected from Transform — that
/// math is wrong once `ground_offset` and per-tile elevation are factored in).
///
/// Before emitting the request this also runs a cheap walkability check against
/// the clicked tile (chunk loaded, biome not solid, height step from the
/// player's current tile ≤ 1). On success the request is emitted and the
/// existing [`spawn_click_ripple`] paints a yellow sparkle confirmation. On
/// failure the request is skipped and a red rejection puff is spawned at the
/// clicked tile so the user sees that the click was rejected. A* still runs
/// the authoritative reachability check on the request that *is* emitted —
/// this gate just filters obviously unreachable single-step clicks before
/// they hit the pathfinder.
pub fn handle_click_to_move(
    mut commands: Commands,
    input: Res<InputState>,
    pick: Res<CursorPick>,
    players: Query<(Entity, &Player)>,
    manager: Res<ChunkManager>,
    chunks: Query<&ChunkData>,
    assets: Option<Res<ParticleAssets>>,
    mut requests: MessageWriter<PathRequest>,
) {
    if !input.select_just_pressed {
        return;
    }
    let Some(goal) = pick.tile else { return; };
    let Ok((entity, player)) = players.single() else { return; };

    if is_walkable_for_player(goal, player.current_height, &manager, &chunks) {
        requests.write(PathRequest {
            requester: entity,
            start: player.current_tile,
            goal,
            max_iterations: 5000,
        });
        return;
    }

    // Rejection — skip the request entirely so the silent A*-empty-path branch
    // never fires, and paint a red sparkle puff at the clicked tile so the
    // user gets feedback for the bad click.
    let Some(assets) = assets else { return; };
    let pos = tile_to_world_with_height(goal, 0);
    spawn_rejection_puff(&mut commands, &assets, pos);
}

// Quick walkability gate matching `inf2d_pathfinding`'s first-step rules:
// the tile's chunk must be loaded, the tile's biome must be non-solid, and
// the height step from `player_height` must be at most one (cliffs taller
// than that are unwalkable in a single move). A* on the actual
// `PathRequest` still validates the full path — this is just enough to
// distinguish "obviously unreachable" clicks from candidate ones.
fn is_walkable_for_player(
    tile: WorldTile,
    player_height: i32,
    manager: &ChunkManager,
    chunks: &Query<&ChunkData>,
) -> bool {
    let chunk_pos = ChunkPos::from_tile(tile);
    let Some(entity) = manager.get(chunk_pos) else { return false; };
    let Ok(data) = chunks.get(entity) else { return false; };
    let local = chunk_pos.local_of(tile);
    let t = data.get(local);
    if t.kind.is_solid() {
        return false;
    }
    (player_height - t.height as i32).abs() <= 1
}

// Spawn a short-lived red sparkle puff at `world_pos`. Six `Particle`
// entities are emitted directly (no `Emitter`) using the shared soft-disc
// texture in `ParticleAssets`, evenly spaced around a ring with an
// outward velocity and a 0.4-second lifetime. Used by
// `handle_click_to_move` to give visible feedback for clicks on water,
// cliff-blocked goals, and other unreachable tiles, distinguishing them
// from the existing yellow accept-ripple.
fn spawn_rejection_puff(commands: &mut Commands, assets: &ParticleAssets, world_pos: Vec2) {
    // Number of particles in one rejection puff.
    const COUNT: u32 = 6;
    // Radial speed of each rejection particle, in world units per second.
    // Slightly faster than the sparkle preset so the puff reads as "kicked
    // back" rather than "drifting".
    const SPEED: f32 = 24.0;
    // Lifetime of each rejection particle, in seconds.
    const LIFETIME: f32 = 0.4;
    // Saturated red for the rejection tint.
    let start_color = Color::srgba(1.0, 0.2, 0.1, 1.0);
    let end_color = Color::srgba(1.0, 0.2, 0.1, 0.0);

    for i in 0..COUNT {
        // Even angular spacing with a quarter-step offset so the burst doesn't
        // align to axes. Deterministic (no RNG dep) — the puff is small enough
        // that randomization isn't visually necessary.
        let theta = (i as f32 + 0.25) / COUNT as f32 * TAU;
        let dir = Vec2::new(theta.cos(), theta.sin());
        let template = Particle {
            lifetime: LIFETIME,
            age: 0.0,
            velocity: dir * SPEED,
            angular_velocity: 0.0,
            start_color,
            end_color,
            start_size: 4.0,
            end_size: 1.0,
            gravity: Vec2::ZERO,
        };
        commands.spawn((
            Sprite {
                image: assets.texture.clone(),
                color: start_color,
                custom_size: Some(Vec2::splat(template.start_size)),
                ..default()
            },
            Transform::from_xyz(world_pos.x, world_pos.y, RenderLayer::ENTITY + 0.2),
            Visibility::default(),
            template,
            Name::new("RejectionPuffParticle"),
        ));
    }
}

/// Apply incoming [`PathFound`] messages to their requester's [`MoveTarget`].
///
/// The path is stored in reverse so that [`walk_along_path`] can `pop()` the
/// next waypoint in O(1). The first entry of `msg.path` is the start tile (the
/// player is already there) and is skipped.
///
/// Also snapshots the **goal** on [`MoveTarget::goal`] — sourced from the last
/// entry of `msg.path` so we retain its destination height as well as its
/// tile. This stable goal is what [`replan_paths_on_chunk_change`] re-targets
/// when streaming events invalidate the active path. An empty `msg.path`
/// (failed search) clears the goal too so we don't keep retrying a known-dead
/// route.
pub fn apply_paths(
    mut found: MessageReader<PathFound>,
    mut targets: Query<&mut MoveTarget>,
) {
    for msg in found.read() {
        let Ok(mut target) = targets.get_mut(msg.requester) else {
            continue;
        };
        if msg.path.is_empty() {
            target.path.clear();
            target.goal = None;
            continue;
        }
        // Last entry of the forward path is the goal (with its height).
        // Preserve the height instead of pulling it from `msg.goal` (which is
        // just a `WorldTile`) so a future replan request can be matched
        // against the same `(tile, height)` shape this target was originally
        // routed to.
        let goal = msg.path.last().copied();
        let mut path: Vec<(WorldTile, i32)> = msg.path.iter().copied().skip(1).collect();
        path.reverse();
        target.path = path;
        target.goal = goal;
    }
}

/// Walk the player toward each waypoint using **velocity-based easing** with
/// approach deceleration and a per-frame overshoot cap.
///
/// Each frame the system computes a desired velocity that ramps down as the
/// player approaches the goal (so we ease IN to the waypoint instead of
/// blowing past it), then exponentially lerps `target.velocity` toward it
/// with smoothing constant `SMOOTHING`. The integrated step is *capped at the
/// remaining distance* so a long frame can never overshoot. A waypoint is
/// consumed when the player is within `WAYPOINT_ARRIVE_RADIUS` of it, or
/// when the frame step would have carried us past the goal (in which case
/// we snap exactly to it). The cap-then-snap branch guarantees the player
/// reaches every waypoint even at low frame rates or with small residual
/// velocity that the approach ramp leaves behind.
///
/// Idle: when the path is empty, residual velocity decays exponentially and
/// is hard-zeroed once below a small epsilon so the player comes fully to
/// rest instead of asymptoting forever (which used to leave the bob system
/// twitching near the threshold).
///
/// Footstep dust: each time a waypoint is consumed, this system also queues a
/// one-shot [`Emitter`] (dust preset, 5-particle burst) at the player's
/// *previous* logical tile so the puff appears where the foot pushed off.
pub fn walk_along_path(
    mut commands: Commands,
    time: Res<Time>,
    mut shake_requests: MessageWriter<ShakeRequest>,
    mut movers: Query<(&mut Transform, &MoveSpeed, &mut MoveTarget, Option<&mut Player>)>,
) {
    /// Higher = snappier acceleration; lower = floatier glide. 12 lands near
    /// "responsive but not robotic" for click-to-move feel.
    const SMOOTHING: f32 = 12.0;
    /// Distance (world units) at which the desired velocity starts ramping
    /// down linearly to zero so we ease IN to the waypoint instead of
    /// overshooting. Calibrated so a player at full speed (≈128 px/s) covers
    /// this slice in ≈100 ms, matching the smoothing time constant.
    const APPROACH_RADIUS: f32 = 12.0;
    /// Pop the waypoint within this many world units of it. Tighter than the
    /// old 1.5 because the approach ramp brings desired velocity to ~10 px/s
    /// at this radius, and the per-frame overshoot cap snaps us exactly on
    /// the goal whenever we *would* have stepped past.
    const WAYPOINT_ARRIVE_RADIUS: f32 = 1.0;
    /// Below this magnitude residual velocity is zeroed when idle so the
    /// player comes fully to rest rather than asymptoting forever.
    const IDLE_VELOCITY_EPSILON_SQ: f32 = 0.01;

    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    // Exponential smoothing factor: `1 - exp(-rate * dt)` is the standard
    // frame-rate-independent lerp weight.
    let smooth_alpha = 1.0 - (-SMOOTHING * dt).exp();

    for (mut transform, speed, mut target, mut player) in &mut movers {
        let Some(&(next_tile, next_height)) = target.path.last() else {
            // Idle: bleed off any residual velocity, then snap to zero so the
            // player doesn't drift indefinitely (asymptotic lerp never reaches
            // 0 on its own).
            target.velocity *= 1.0 - smooth_alpha;
            if target.velocity.length_squared() < IDLE_VELOCITY_EPSILON_SQ {
                target.velocity = Vec2::ZERO;
            }
            continue;
        };
        let ground_offset = player.as_ref().map(|p| p.ground_offset).unwrap_or(0.0);
        let base = tile_to_world_with_height(next_tile, next_height);
        let goal = Vec2::new(base.x, base.y + ground_offset);
        let pos = transform.translation.truncate();
        let delta = goal - pos;
        let distance = delta.length();
        let max_speed = speed.tiles_per_second * TILE_WIDTH;

        // Desired velocity: unit-direction toward the goal × max speed,
        // attenuated by an approach factor so we decelerate INTO the waypoint
        // instead of blowing through it.
        let desired = if distance > f32::EPSILON {
            let approach = (distance / APPROACH_RADIUS).min(1.0);
            (delta / distance) * max_speed * approach
        } else {
            Vec2::ZERO
        };
        target.velocity = target.velocity.lerp(desired, smooth_alpha);

        // Integrate, capping the per-frame step to the remaining `delta` so a
        // long frame (or fast speed near a close waypoint) can never carry the
        // transform past the goal.
        let frame_step = target.velocity * dt;
        let step_len_sq = frame_step.length_squared();
        let overshoots = step_len_sq > distance * distance;
        let applied_step = if overshoots { delta } else { frame_step };
        transform.translation.x += applied_step.x;
        transform.translation.y += applied_step.y;

        // Arrival: either we landed within the snap radius (the approach
        // ramp brings desired velocity to ~10 px/s here, plenty slow for a
        // visually invisible snap) OR the frame step was clamped to `delta`
        // because it would have overshot the goal. Both branches consume the
        // waypoint and emit the footstep puff.
        let arrived = overshoots || distance <= WAYPOINT_ARRIVE_RADIUS;

        if arrived {
            transform.translation.x = goal.x;
            transform.translation.y = goal.y;
            target.path.pop();

            // Remember the previous logical tile so the dust puff appears
            // where the foot pushed off, not where it landed.
            let prev_world = player
                .as_ref()
                .map(|p| tile_to_world_with_height(p.current_tile, p.current_height))
                .unwrap_or(Vec2::new(base.x, base.y));

            if let Some(p) = player.as_mut() {
                p.current_tile = next_tile;
                p.current_height = next_height;
            }

            // Tiny dust puff at the previous tile center. The emitter despawns
            // itself after 0.6 s via DespawnAfter; particles outlive the
            // emitter since they're independent entities.
            commands.spawn((
                Emitter {
                    preset: EmitterPreset::Dust,
                    shape: EmitterShape::Disc { radius: 4.0 },
                    rate: 0.0,
                    burst: 5,
                    enabled: true,
                    _spawn_accum: 0.0,
                    _burst_done: false,
                },
                Transform::from_xyz(prev_world.x, prev_world.y, RenderLayer::ENTITY - 0.1),
                Visibility::default(),
                DespawnAfter::secs(0.6),
                Name::new("FootstepDust"),
            ));

            // Footstep "kick" — fire a subtle camera shake on every other
            // consumed waypoint. The checkerboard parity gate
            // (`(x + y) & 1 == 0`) skips half the steps so the resulting
            // micro-shake reads as a deliberate cadence instead of constant
            // jitter while the player is moving.
            let parity = next_tile.x.wrapping_add(next_tile.y) & 1;
            if parity == 0 {
                shake_requests.write(ShakeRequest::subtle());
            }
        }
    }
}

/// While the player is moving, apply a subtle vertical bob to each visual
/// slice child. Operates on the **child** transforms only — the parent
/// [`Player`]'s transform is the logical position used by gameplay; bobbing
/// that would cause every dependent system (camera follow, pathfinding,
/// click-to-move from the player's current tile) to chase the wobble.
///
/// Each slice carries a base local-Y of `i as f32` (set in [`spawn_player`]);
/// we recompute that base each frame and add the bob, so the system is
/// idempotent and stays robust against re-orderings.
///
/// Filters on [`SpriteStackSlice`] so non-slice children (e.g. the drop-shadow
/// visual) don't get their `y` overwritten — the player owns both stacked
/// slices and a shadow child, and only the stack should bob.
pub fn bob_player_sprites(
    time: Res<Time>,
    players: Query<(&Children, &MoveTarget), With<Player>>,
    mut slices: Query<&mut Transform, With<SpriteStackSlice>>,
) {
    /// Below this speed the player is considered stationary and the bob is
    /// faded out — keeps slices from jittering when easing in / out of motion.
    const MOVING_SPEED_THRESHOLD: f32 = 4.0;
    /// Peak bob amplitude in world units.
    const BOB_AMPLITUDE: f32 = 1.5;
    /// Bob cycle frequency in radians per second. 8 rad/s ≈ 1.27 Hz step rate,
    /// which reads as a brisk walking cadence.
    const BOB_RATE: f32 = 8.0;

    for (children, target) in &players {
        let speed = target.velocity.length();
        // Linear ramp from 0 at threshold to 1 at 2x threshold so the bob
        // smoothly fades in as the player picks up speed.
        let intensity = ((speed - MOVING_SPEED_THRESHOLD) / MOVING_SPEED_THRESHOLD).clamp(0.0, 1.0);
        let bob = (time.elapsed_secs() * BOB_RATE).sin() * BOB_AMPLITUDE * intensity;

        // Slice index *among slices only* — non-slice children are skipped by
        // the `With<SpriteStackSlice>` filter, so the enumeration matches the
        // slice's intended vertical position regardless of how other child
        // visuals (shadow, future attachments) are interleaved.
        let mut slice_index: u32 = 0;
        for child in children.iter() {
            let Ok(mut tf) = slices.get_mut(child) else {
                continue;
            };
            // Restore the slice's authored base-Y (`slice_index` px above the
            // parent) and overlay the bob. Done as an absolute assignment
            // rather than += so the offset doesn't compound across frames.
            tf.translation.y = slice_index as f32 + bob;
            slice_index += 1;
        }
    }
}

/// Spawn a one-shot yellow ripple at the clicked tile whenever the user
/// left-clicks on a *walkable* pick. The walkability gate mirrors the one in
/// [`handle_click_to_move`] so the yellow ripple and the [`PathRequest`]
/// always co-fire — and a rejected click instead gets the red puff spawned
/// by [`handle_click_to_move`], not a misleading yellow sparkle.
///
/// Two visuals are spawned at the same world position:
///
/// 1. A flat yellow disc sprite that **scales from 0 → 1** over 0.2 s with an
///    ease-out curve via [`bevy_tweening`], so the ripple visibly expands
///    instead of popping in fully formed. Despawns after 0.5 s.
/// 2. The original `Sparkle` [`Emitter`] burst (eight short-lived particles)
///    layered on top, kept for the existing sparkle flavor.
pub fn spawn_click_ripple(
    mut commands: Commands,
    input: Res<InputState>,
    pick: Res<CursorPick>,
    players: Query<&Player>,
    manager: Res<ChunkManager>,
    chunks: Query<&ChunkData>,
    assets: Option<Res<ParticleAssets>>,
) {
    if !input.select_just_pressed {
        return;
    }
    let Some(tile) = pick.tile else {
        return;
    };
    let Ok(player) = players.single() else { return; };
    if !is_walkable_for_player(tile, player.current_height, &manager, &chunks) {
        return;
    }
    let pos = tile_to_world_with_height(tile, 0);

    // Expanding-ripple disc: scaled by a tween from 0 → 1 over 0.2 s. We
    // animate `Transform::scale` (not `Sprite::custom_size`) because
    // `bevy_tweening` ships a built-in `TransformScaleLens`, and routing the
    // animation through `Transform` keeps the lens code one line instead of
    // a hand-rolled sprite-size lens.
    if let Some(assets) = assets {
        let ripple_tween = Tween::new(
            EaseFunction::QuadraticOut,
            Duration::from_millis(200),
            TransformScaleLens {
                start: Vec3::ZERO,
                end: Vec3::ONE,
            },
        );
        commands.spawn((
            Sprite {
                image: assets.texture.clone(),
                color: Color::srgba(1.0, 0.95, 0.6, 0.85),
                custom_size: Some(Vec2::splat(18.0)),
                ..default()
            },
            Transform {
                translation: Vec3::new(pos.x, pos.y, RenderLayer::ENTITY - 0.06),
                scale: Vec3::ZERO,
                ..default()
            },
            Visibility::default(),
            TweenAnim::new(ripple_tween),
            DespawnAfter::secs(0.5),
            Name::new("ClickRippleDisc"),
        ));
    }

    commands.spawn((
        Emitter {
            preset: EmitterPreset::Sparkle,
            shape: EmitterShape::Ring { radius: 6.0 },
            rate: 0.0,
            burst: 8,
            enabled: true,
            _spawn_accum: 0.0,
            _burst_done: false,
        },
        Transform::from_xyz(pos.x, pos.y, RenderLayer::ENTITY - 0.05),
        Visibility::default(),
        DespawnAfter::secs(0.5),
        Name::new("ClickRipple"),
    ));
}

/// `Update` system: decrement every [`DespawnAfter::secs`] by `dt` and despawn
/// entities whose timer has expired. Single pass, no allocations.
pub fn tick_despawn_after(
    mut commands: Commands,
    time: Res<Time>,
    mut q: Query<(Entity, &mut DespawnAfter)>,
) {
    let dt = time.delta_secs();
    for (entity, mut tag) in &mut q {
        tag.secs -= dt;
        if tag.secs <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

pub fn update_follow_camera(
    time: Res<Time>,
    mut rigs: Query<(&mut CameraRig, &mut Transform)>,
    targets: Query<&GlobalTransform, Without<CameraRig>>,
) {
    let dt = time.delta_secs();
    for (mut rig, mut transform) in &mut rigs {
        let CameraMode::Follow { entity, lag } = rig.mode else {
            continue;
        };
        let Ok(target_xform) = targets.get(entity) else {
            continue;
        };
        let goal = target_xform.translation().truncate();
        let alpha = 1.0 - (-lag * dt).exp();
        let new_target = rig.target + (goal - rig.target) * alpha;
        rig.target = new_target;
        // Fold in `rig.shake` so screen-shake from `ShakeRequest` is visible
        // while the camera is following the player. The shake driver writes
        // the offset earlier this frame; we add it here.
        transform.translation.x = new_target.x + rig.shake.x;
        transform.translation.y = new_target.y + rig.shake.y;
    }
}

/// Bridge system: when a [`Tree`] entity is freshly added (by `inf2d_world::props`),
/// remove the placeholder flat sprite and attach the proper iso-aware visuals —
/// a vertical sprite-stack, a drop shadow, and an `IsoAnchor`. This lives here
/// because `inf2d_world` can't depend on `inf2d_render` without cycling.
pub fn attach_tree_visuals(
    mut commands: Commands,
    new_trees: Query<Entity, Added<Tree>>,
) {
    for entity in &new_trees {
        commands
            .entity(entity)
            .remove::<Sprite>()
            .insert((
                SpriteStack {
                    slices: 22,
                    slice_size: Vec2::new(10.0, 5.0),
                    slice_spacing: 1.0,
                    base_color: Color::srgb(0.30, 0.18, 0.08),
                    top_color: Color::srgb(0.35, 0.55, 0.20),
                },
                IsoAnchor::default(),
                DropShadow {
                    radius: 9.0,
                    squash: 0.5,
                    color: Color::srgba(0.0, 0.0, 0.0, 0.5),
                },
            ));
    }
}

pub fn draw_path_gizmo(
    mut gizmos: Gizmos,
    movers: Query<(&Transform, &MoveTarget), With<Player>>,
) {
    let color = Color::srgba(1.0, 1.0, 0.4, 0.7);
    for (transform, target) in &movers {
        if target.path.is_empty() {
            continue;
        }
        let mut prev = transform.translation.truncate();
        for (tile, height) in target.path.iter().rev() {
            let p = tile_to_world_with_height(*tile, *height);
            gizmos.line_2d(prev, p, color);
            gizmos.circle_2d(p, 3.0, color);
            prev = p;
        }
    }
}
