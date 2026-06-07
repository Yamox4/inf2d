//! Player character: spawning, camera-relative WASD movement, and a walk
//! animation. The character is a smooth multi-part figure — a teardrop head
//! (sphere + cone tip), a rounded-cone body, two floating hand spheres at the
//! sides and two floating foot spheres at the front-bottom. While walking it
//! bobs in a hop arc, the feet step (swinging fore/aft and lifting), and the
//! arms swing counter to the legs, all emitting dust.

use avian3d::prelude::*;
use bevy::prelude::*;

use inf3d_core::{AppState, FollowTarget, GameSet, MoveIntent, Pause};
use inf3d_physics::{CharacterController, DesiredMove, GameLayer, PLAYER_DIMS};
use inf3d_worldgen::Terrain;

/// The controllable player. `cell` is the current voxel column `(x, z)` the
/// player occupies, resynced from the transform each step.
#[derive(Component)]
pub struct Player {
    pub speed: f32,
    pub cell: IVec2,
    /// Yaw (radians) of the travel direction, for facing the visual.
    pub facing: f32,
}

/// Emitted once each time the walking character lands a hop (its feet touch the
/// ground), at the same instant the visual dust burst fires. A downstream audio
/// sink ([`inf3d_audio`]) plays a footstep per message, decoupled from the dust —
/// audio never piggybacks on dust particle counts. `pos` is the feet position at
/// the step, for future spatial audio / per-surface clip selection.
#[derive(Message)]
pub struct Footstep {
    pub pos: Vec3,
}

/// The animated root node holding all the character body parts. Kept separate
/// from the logical player transform so animation never feeds back into the
/// movement integration. The whole figure bobs and yaws via this node.
#[derive(Component)]
struct CharacterRoot;

/// Identifies an individually animated body part (hands and feet swing/step,
/// head counter-bobs). The body cone carries no `Part` marker — it's static.
#[derive(Component, Clone, Copy, PartialEq)]
enum Part {
    Head,
    HandL,
    HandR,
    FootL,
    FootR,
}

/// A part's neutral local translation, the rest pose it eases back toward.
#[derive(Component)]
struct RestPos(Vec3);

/// Distance from the player entity origin (capsule center) down to the feet.
/// Derived from the single [`PLAYER_DIMS`] source of truth so the visual figure
/// always stands on the capsule's feet — no hand-kept literal. The character
/// visual root is placed at local Y = `-VISUAL_ROOT_OFFSET`.
const VISUAL_ROOT_OFFSET: f32 = PLAYER_DIMS.visual_root_offset;

// Walk-animation tuning.
const HOP_RATE: f32 = 4.5; // hops per second while moving
const HOP_HEIGHT: f32 = 0.32; // peak hop height (world units)
const WALK_DUST_INTERVAL: f32 = 0.18; // seconds between trailing dirt puffs
const ANIM_EASE: f32 = 12.0; // ease-to-rest / smoothing rate
const STEP_SWING: f32 = 0.18; // fore/aft foot swing amplitude
const STEP_LIFT: f32 = 0.12; // foot lift on the forward swing
const ARM_SWING: f32 = 0.14; // hand fore/aft swing amplitude
/// Sprint speed multiplier applied to the player's base speed while Shift is held.
const SPRINT_MULT: f32 = 1.75;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Footstep>()
            .add_systems(Startup, spawn_player)
            // `apply_move` translates the orbit camera's `MoveIntent` into the
            // movement INTENT (`DesiredMove`) and must run at the FIXED rate, in
            // lockstep with the `inf3d_physics` character controller
            // (`FixedPostUpdate`). At low fps the fixed loop runs several steps per
            // frame; running it once per frame would let the controller take ALL of
            // those steps on a single stale direction. It only *reads* `Transform`,
            // so it cannot corrupt the interpolated physics state. Gated to
            // un-paused play: `apply_move` is in `FixedUpdate` (not a `GameSet`), so
            // the one core gating lever does NOT cover it. Without this it would keep
            // writing `DesiredMove` while the pause/main-menu is up. (The physics
            // controller that consumes `DesiredMove` is gated identically in
            // `inf3d_physics`.)
            .add_systems(
                FixedUpdate,
                apply_move.run_if(in_state(AppState::InGame).and(in_state(Pause::Running))),
            )
            // `animate_player` is per-frame VISUAL only (hop/feet/dust; reads the
            // interpolated transform), so it stays in the render-rate Logic set.
            .add_systems(Update, animate_player.in_set(GameSet::Logic));
    }
}

/// Spawn the player at column (0, 0): a logical parent entity holding the
/// gameplay transform, with the animated character figure as its child tree.
fn spawn_player(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    terrain: Res<Terrain>,
) {
    // Spawn on the nearest land so the player never starts submerged in water.
    let spawn = terrain.nearest_land(IVec2::ZERO);
    let center = terrain.stand_pos(spawn.x, spawn.y) + Vec3::Y * VISUAL_ROOT_OFFSET;

    // Smooth meshes. Bevy's Sphere/Cone are already smooth-shaded.
    let body_mesh = meshes.add(Cone {
        radius: 0.5,
        height: 1.0,
    });
    let head_mesh = meshes.add(Sphere::new(0.32));
    let tip_mesh = meshes.add(Cone {
        radius: 0.30,
        height: 0.5,
    });
    let hand_mesh = meshes.add(Sphere::new(0.14));
    let foot_mesh = meshes.add(Sphere::new(0.16));

    // Shared materials (handles cloned across parts).
    let body_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.85, 0.22, 0.22),
        ..default()
    });
    let skin_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.96, 0.86, 0.78),
        ..default()
    });
    let hand_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.20, 0.20, 0.25),
        ..default()
    });
    let foot_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.18, 0.18, 0.22),
        ..default()
    });

    // Rest translations.
    let head_rest = Vec3::new(0.0, 0.62, 0.0);
    let hand_l_rest = Vec3::new(-0.6, 0.0, 0.0);
    let hand_r_rest = Vec3::new(0.6, 0.0, 0.0);
    let foot_l_rest = Vec3::new(-0.22, -0.95, 0.12);
    let foot_r_rest = Vec3::new(0.22, -0.95, 0.12);

    commands
        .spawn((
            Transform::from_translation(center),
            Visibility::default(),
            Player {
                speed: 8.0,
                cell: spawn,
                facing: 0.0,
            },
            FollowTarget,
            // Kinematic character controller: avian moves it only via our
            // `move_and_slide` in `inf3d_physics`, never auto-integrated. The
            // capsule is built from the single `PLAYER_DIMS` source of truth. The
            // Player layer collides with Solid props only — the ground is derived
            // analytically from the Terrain heightfield, so there is no terrain
            // collider/layer to hit.
            RigidBody::Kinematic,
            Collider::capsule(PLAYER_DIMS.radius, PLAYER_DIMS.half_height * 2.0),
            CollisionLayers::new(GameLayer::Player, [GameLayer::Solid]),
            CharacterController::default(),
            DesiredMove::default(),
            // The controller writes this `Transform` in `FixedPostUpdate`;
            // avian's `TransformInterpolation` eases it between fixed ticks (right
            // after `FixedMain`, before `Update`) so the rendered figure — and
            // the camera following it — stay smooth at any frame rate / zoom.
            TransformInterpolation,
        ))
        .with_children(|parent| {
            parent
                .spawn((CharacterRoot, Transform::default(), Visibility::default()))
                .with_children(|root| {
                    // Body: rounded cone, wide bottom tapering up (apex +Y).
                    root.spawn((
                        Mesh3d(body_mesh.clone()),
                        MeshMaterial3d(body_mat.clone()),
                        Transform::from_translation(Vec3::new(0.0, -0.2, 0.0)),
                    ));

                    // Head sphere with a child cone tip (teardrop). The cone
                    // rides the head so it bobs together.
                    root.spawn((
                        Mesh3d(head_mesh.clone()),
                        MeshMaterial3d(skin_mat.clone()),
                        Transform::from_translation(head_rest),
                        Part::Head,
                        RestPos(head_rest),
                    ))
                    .with_children(|head| {
                        head.spawn((
                            Mesh3d(tip_mesh.clone()),
                            MeshMaterial3d(skin_mat.clone()),
                            Transform::from_translation(Vec3::new(0.0, 0.30, 0.0)),
                        ));
                    });

                    // Floating hands at the sides.
                    root.spawn((
                        Mesh3d(hand_mesh.clone()),
                        MeshMaterial3d(hand_mat.clone()),
                        Transform::from_translation(hand_l_rest),
                        Part::HandL,
                        RestPos(hand_l_rest),
                    ));
                    root.spawn((
                        Mesh3d(hand_mesh.clone()),
                        MeshMaterial3d(hand_mat.clone()),
                        Transform::from_translation(hand_r_rest),
                        Part::HandR,
                        RestPos(hand_r_rest),
                    ));

                    // Floating feet at the front-bottom.
                    root.spawn((
                        Mesh3d(foot_mesh.clone()),
                        MeshMaterial3d(foot_mat.clone()),
                        Transform::from_translation(foot_l_rest),
                        Part::FootL,
                        RestPos(foot_l_rest),
                    ));
                    root.spawn((
                        Mesh3d(foot_mesh.clone()),
                        MeshMaterial3d(foot_mat.clone()),
                        Transform::from_translation(foot_r_rest),
                        Part::FootR,
                        RestPos(foot_r_rest),
                    ));
                });
        });
}

/// Translate the orbit camera's [`MoveIntent`] (camera-relative WASD) into a
/// desired **horizontal** velocity on [`DesiredMove`]; the physics character
/// controller in `inf3d_physics` consumes it in the SAME fixed step
/// (`FixedPostUpdate`), runs it through `move_and_slide` against solid props, and
/// handles gravity / ground-snap. Runs in `FixedUpdate` so the velocity is fresh
/// for every physics step (not once per render frame). The character rotates to
/// face its travel direction. The controller owns Y — this only writes the
/// horizontal velocity + the jump request.
fn apply_move(
    intent: Res<MoveIntent>,
    mut query: Query<(&Transform, &mut Player, &mut DesiredMove)>,
) {
    let Ok((transform, mut player, mut desired)) = query.single_mut() else {
        return;
    };
    // No active intent (free-fly, or the cursor is freed for UI with Alt) → STOP the
    // player by zeroing the desired horizontal velocity, instead of coasting on the
    // last value (the controller would otherwise keep integrating that stale velocity).
    if !intent.active {
        desired.velocity = Vec3::ZERO;
        desired.jump = false;
        return;
    }

    player.cell = cell_of(transform.translation);

    let speed = if intent.sprint {
        player.speed * SPRINT_MULT
    } else {
        player.speed
    };
    desired.velocity = Vec3::new(intent.direction.x * speed, 0.0, intent.direction.z * speed);
    desired.jump = intent.jump;
    if intent.direction.length_squared() > 1e-4 {
        player.facing = intent.direction.x.atan2(intent.direction.z);
    }
}

/// Animate the character: the root hops in a smooth arc while walking, the feet
/// step (fore/aft swing + lift), the hands swing counter to the legs and the
/// head subtly counter-bobs — emitting a dust burst on each landing plus
/// trailing dirt puffs. Idle eases everything back to rest. The root yaws to
/// face the travel direction (no tilt).
fn animate_player(
    time: Res<Time>,
    mut dust: MessageWriter<inf3d_render::DustBurst>,
    mut footstep: MessageWriter<Footstep>,
    state_q: Query<
        (&Transform, &Player, &CharacterController),
        (Without<CharacterRoot>, Without<Part>),
    >,
    mut root_q: Query<&mut Transform, (With<CharacterRoot>, Without<Part>, Without<Player>)>,
    mut part_q: Query<(&mut Transform, &Part, &RestPos), (Without<CharacterRoot>, Without<Player>)>,
    mut phase: Local<f32>,
    mut walk_accum: Local<f32>,
) {
    let Ok((p_tf, player, cc)) = state_q.single() else {
        return;
    };
    let Ok(mut root) = root_q.single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    // Drive the walk anim off the ACTUAL (ramped) horizontal speed, not the raw input, so
    // the feet keep stepping through the brief decel slide after a key release.
    let moving = cc.horizontal_velocity.length_squared() > 0.04;
    let feet = p_tf.translation - Vec3::Y * VISUAL_ROOT_OFFSET;

    // Face travel direction (yaw only, no tilt).
    root.rotation = root
        .rotation
        .slerp(Quat::from_rotation_y(player.facing), ANIM_EASE * dt);

    if moving {
        let prev = *phase;
        *phase += dt * HOP_RATE;
        // 0→1→0 over one hop: a smooth jump arc.
        let arch = (phase.fract() * std::f32::consts::PI).sin();
        root.translation.y = arch * HOP_HEIGHT;

        let stride = *phase * std::f32::consts::TAU;
        let s = stride.sin();

        // Landing: crossed an integer hop boundary → kick up a burst.
        if phase.floor() > prev.floor() {
            dust.write(inf3d_render::DustBurst {
                pos: feet,
                amount: 12,
                speed: 2.2,
            });
            // One footstep sound per hop landing (inf3d_audio plays it with a
            // slight random pitch/volume). Separate from the dust above so audio
            // and particles can evolve independently.
            footstep.write(Footstep { pos: feet });
        }
        // Trailing dirt puffs at a steady cadence.
        *walk_accum += dt;
        if *walk_accum >= WALK_DUST_INTERVAL {
            *walk_accum = 0.0;
            dust.write(inf3d_render::DustBurst {
                pos: feet,
                amount: 2,
                speed: 1.0,
            });
        }

        for (mut t, part, rest) in &mut part_q {
            let target = match part {
                Part::FootL => rest.0 + Vec3::new(0.0, s.max(0.0) * STEP_LIFT, s * STEP_SWING),
                Part::FootR => rest.0 + Vec3::new(0.0, (-s).max(0.0) * STEP_LIFT, -s * STEP_SWING),
                Part::HandL => rest.0 + Vec3::new(0.0, s.abs() * 0.04, -s * ARM_SWING),
                Part::HandR => rest.0 + Vec3::new(0.0, s.abs() * 0.04, s * ARM_SWING),
                Part::Head => rest.0 + Vec3::new(0.0, (stride * 2.0).sin() * -0.03, 0.0),
            };
            t.translation = t.translation.lerp(target, ANIM_EASE * dt);
        }
    } else {
        root.translation.y = lerp(root.translation.y, 0.0, ANIM_EASE * dt);
        // Reset so the next hop starts fresh and a puff fires immediately.
        *phase = phase.ceil();
        *walk_accum = WALK_DUST_INTERVAL;

        for (mut t, _part, rest) in &mut part_q {
            t.translation = t.translation.lerp(rest.0, ANIM_EASE * dt);
        }
    }
}

/// Derive the voxel column `(x, z)` the player currently occupies.
fn cell_of(translation: Vec3) -> IVec2 {
    IVec2::new(translation.x.floor() as i32, translation.z.floor() as i32)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
