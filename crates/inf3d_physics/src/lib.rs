//! Physics & collision for inf3d, built on **avian3d** (ECS-native Bevy
//! physics).
//!
//! This crate owns:
//!
//! * The [`GameLayer`] collision layers that separate **solid** props (which
//!   block the player) from grass (which the player walks through — grass
//!   simply gets *no collider* and no layer at all).
//! * The single [`PlayerDims`] source of truth for the player's capsule radius,
//!   half-height, and the visual-root offset (gameplay derives its character
//!   root offset from this — no hand-kept literal).
//! * Static colliders for solid props (trees → capsule trunk, rocks → cuboid),
//!   attached on spawn by the foliage crate via [`SolidPropCollider`].
//! * A **kinematic character controller** for the player: props block
//!   horizontally via `move_and_slide`; the ground is derived **analytically**
//!   from the deterministic [`Terrain`] oracle (the terrain is a pure
//!   heightfield, so the top face of the player's column is just
//!   `surface_y + 1`). That both lets the player glide over voxel steps and
//!   structurally prevents falling through, with no terrain collider to build,
//!   stream, or fall out of sync with chunk meshing.
//! * An [`InteractionTarget`] resource + camera raycast hook.
//!
//! The simulation is stepped by avian's `PhysicsPlugins` at its default fixed
//! timestep (`FixedPostUpdate`, added in `inf3d_app`); this crate only wires
//! components + systems. The controller runs in `FixedPostUpdate` after avian's
//! `Writeback`, and the player carries `TransformInterpolation` so the rendered
//! transform is smoothly eased between fixed ticks.

use avian3d::math::Vector;
use avian3d::prelude::*;
use bevy::prelude::*;

use inf3d_camera::IsoCamera;
use inf3d_core::GameSet;
use inf3d_worldgen::Terrain;

/// Collision membership layers.
///
/// The player's horizontal move query hits [`Solid`](GameLayer::Solid) props
/// only; the ground is resolved **analytically** from the [`Terrain`] oracle, so
/// there is no terrain collider and no terrain layer to query. **Grass is
/// intentionally absent** — grass carries no collider at all, so the player
/// walks straight through it.
#[derive(PhysicsLayer, Clone, Copy, Debug, Default)]
pub enum GameLayer {
    /// Catch-all default layer.
    #[default]
    Default,
    /// Solid blocking props: trees and rocks.
    Solid,
    /// The player's character-controller capsule.
    Player,
}

/// Marker the foliage crate attaches to a solid prop entity to request a static
/// collider sized to its footprint.
#[derive(Component, Clone, Copy, Debug)]
pub enum SolidPropCollider {
    /// A tree: vertical capsule trunk standing `height` tall from the base.
    Tree { radius: f32, height: f32 },
    /// A rock: axis-aligned box, base sitting at the entity origin.
    Rock { half: Vec3 },
}

/// Per-player desired **horizontal** velocity this frame, written by gameplay's
/// `follow_path` and consumed by [`player_controller`]. Routing intent through a
/// component (rather than mutating `Transform`) lets the controller resolve
/// collisions while keeping the click-to-move feel.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct DesiredMove {
    /// Desired horizontal (XZ) velocity in world units per second.
    pub velocity: Vec3,
}

/// Marker for the kinematic player capsule the controller drives.
#[derive(Component)]
pub struct CharacterController {
    pub radius: f32,
    pub half_height: f32,
    /// Vertical velocity, integrated only while airborne (off a real ledge).
    pub vertical_velocity: f32,
    /// Whether the controller found ground last frame.
    pub grounded: bool,
}

impl Default for CharacterController {
    fn default() -> Self {
        Self {
            radius: PLAYER_DIMS.radius,
            half_height: PLAYER_DIMS.half_height,
            vertical_velocity: 0.0,
            grounded: false,
        }
    }
}

/// The single source of truth for the player's body dimensions. Physics builds
/// the capsule from `radius`/`half_height`; gameplay derives its character-root
/// (visual) offset from `visual_root_offset` so the figure's feet land on the
/// capsule's feet — no more coincidental `1.0` literal kept in sync by hand.
///
/// `visual_root_offset` is the distance from the capsule *center* (== the player
/// entity origin) down to the feet, i.e. `half_height + radius` for a capsule.
/// Keeping it an explicit field lets the visual be re-tuned independently of the
/// collision shape if ever needed, while the default keeps them coincident.
#[derive(Clone, Copy, Debug)]
pub struct PlayerDims {
    /// Capsule radius.
    pub radius: f32,
    /// Capsule cylindrical half-height (full capsule ≈ 2*half + 2*radius).
    pub half_height: f32,
    /// Distance from the entity origin (capsule center) down to the feet; the
    /// visual root is placed this far below the origin so the figure stands on
    /// the ground the controller resolves the capsule feet onto.
    pub visual_root_offset: f32,
}

/// Canonical player dimensions. `visual_root_offset` equals the capsule
/// center→feet distance (`half_height + radius`) so the figure's feet sit on the
/// capsule's feet.
pub const PLAYER_DIMS: PlayerDims = PlayerDims {
    radius: 0.4,
    half_height: 0.6,
    visual_root_offset: 0.4 + 0.6,
};

/// The prop the interaction hook currently has targeted (camera ray pick).
#[derive(Resource, Default, Debug)]
pub struct InteractionTarget {
    pub entity: Option<Entity>,
    pub point: Option<Vec3>,
}

// ---------------------------------------------------------------------------
// Tunables. All world-space unless noted.
// ---------------------------------------------------------------------------

/// Player capsule radius. Re-exported convenience alias for [`PLAYER_DIMS`]'s
/// `radius` (the single source of truth); kept for callers that import it.
pub const PLAYER_RADIUS: f32 = PLAYER_DIMS.radius;
/// Player capsule cylindrical half-height (full capsule ≈ 2*half + 2*radius).
/// Alias for [`PLAYER_DIMS`]'s `half_height`.
pub const PLAYER_HALF_HEIGHT: f32 = PLAYER_DIMS.half_height;
/// Gravity acceleration (world units / s²), applied only while airborne.
pub const GRAVITY: f32 = 24.0;
/// Max height the player will step up onto in one go (voxel steps / low props).
/// Any rise up to this much (relative to the current feet) is climbed smoothly;
/// a bigger jump down reads as a real ledge and the player falls.
pub const STEP_HEIGHT: f32 = 1.2;
/// Extra reach below the feet within which the ground still "grabs" the player
/// (keeps the feet glued to the surface on downhill steps instead of briefly
/// going airborne). A drop larger than this leaves the player airborne.
pub const GROUND_SNAP_DISTANCE: f32 = 0.5;
/// Distance the camera interaction ray travels before giving up.
pub const INTERACT_RAY_LENGTH: f32 = 1000.0;
/// How fast the feet ease onto the followed ground (per second). High = snappy
/// but soft; lower = floatier. Smooths step-ups so climbing a voxel eases up
/// instead of snapping in one frame (which read as a hard jolt).
pub const GROUND_FOLLOW_RATE: f32 = 14.0;

pub struct PhysicsGamePlugin;

impl Plugin for PhysicsGamePlugin {
    fn build(&self, app: &mut App) {
        // `InteractionTarget` is owned by this crate (per-crate resource, not a
        // shared one) so it stays here. Prop-collider builds are streaming work;
        // the interaction pick is per-frame logic.
        app.init_resource::<InteractionTarget>()
            .add_systems(Update, build_prop_colliders.in_set(GameSet::Streaming))
            .add_systems(Update, update_interaction_target.in_set(GameSet::Logic));
        // The controller drives a KINEMATIC body by writing `Transform` in the
        // FIXED schedule. It runs AFTER avian's `Writeback` (position→transform)
        // so our write is the final word for the step, and avian's
        // `TransformInterpolation` (on the player) eases that fixed-step result
        // into a smooth per-frame `Transform` right after `FixedMain`. Using the
        // fixed delta + interpolation removes the variable-timestep jitter the
        // old `PostUpdate` controller fought. It still consumes the latest
        // `DesiredMove` written by gameplay's per-frame `follow_path` (Update).
        app.add_systems(
            FixedPostUpdate,
            player_controller.after(PhysicsSystems::Writeback),
        );
    }
}

/// Turn each [`SolidPropCollider`] request into a real static collider on the
/// [`GameLayer::Solid`] layer. Idempotent via the `Without<Collider>` guard.
fn build_prop_colliders(
    mut commands: Commands,
    props: Query<(Entity, &SolidPropCollider), Without<Collider>>,
) {
    for (entity, spec) in &props {
        // Props sit with their base at the entity origin, so lift the shape to
        // its mid-height via a single-child compound (keeps the mesh pivot).
        let (shape, centre_y) = match *spec {
            SolidPropCollider::Tree { radius, height } => {
                let cyl = (height - 2.0 * radius).max(0.05);
                (Collider::capsule(radius, cyl), height * 0.5)
            }
            SolidPropCollider::Rock { half } => (
                Collider::cuboid(half.x * 2.0, half.y * 2.0, half.z * 2.0),
                half.y,
            ),
        };
        let collider = Collider::compound(vec![(
            Position(Vector::new(0.0, centre_y, 0.0)),
            Quat::IDENTITY,
            shape,
        )]);
        commands.entity(entity).insert((
            RigidBody::Static,
            collider,
            CollisionLayers::new(GameLayer::Solid, LayerMask::ALL),
        ));
    }
}

/// Kinematic character controller, run in `FixedPostUpdate` after avian's
/// `Writeback` so our `Transform` write is the final word for the step; avian's
/// `TransformInterpolation` then eases it smoothly between steps.
///
/// Horizontal: the desired (pathfinding) velocity is run through `move_and_slide`
/// against **solid props only**, so trees/rocks block and the player slides
/// along them — terrain is deliberately not a horizontal wall, so the player is
/// never stopped dead by a 1-voxel step.
///
/// Vertical: the terrain is a pure heightfield, so the ground under the player is
/// read **analytically** from the [`Terrain`] oracle — the top face of the
/// player's column is `surface_y + 1`. No ray/shape-cast and no terrain collider
/// are involved. The same step-up / ground-snap / airborne-fall behavior is
/// preserved against that analytic surface: a surface from `STEP_HEIGHT` above
/// the capsule center down to `GROUND_SNAP_DISTANCE` below the feet "grabs" the
/// player and the feet ease onto it (climbing voxel steps up to `STEP_HEIGHT`,
/// following the surface downhill); a larger drop (a real ledge) leaves the
/// player airborne, where gravity takes over until the feet reach the surface.
fn player_controller(
    time: Res<Time>,
    terrain: Res<Terrain>,
    move_and_slide: MoveAndSlide,
    mut q: Query<(
        Entity,
        &mut Transform,
        &mut CharacterController,
        &DesiredMove,
        &Collider,
    )>,
) {
    let dt = time.delta();
    let dt_s = time.delta_secs();
    if dt_s <= 0.0 {
        return;
    }

    for (entity, mut transform, mut cc, desired, collider) in &mut q {
        // --- HORIZONTAL: blocked by solid props only ---
        let prop_filter =
            SpatialQueryFilter::from_mask([GameLayer::Solid]).with_excluded_entities([entity]);
        let h_velocity = Vec3::new(desired.velocity.x, 0.0, desired.velocity.z);
        if h_velocity.length_squared() > 1e-6 {
            let out = move_and_slide.move_and_slide(
                collider,
                transform.translation,
                transform.rotation,
                h_velocity,
                dt,
                &MoveAndSlideConfig::default(),
                &prop_filter,
                |_hit| MoveAndSlideHitResponse::Accept,
            );
            transform.translation = out.position;
        }

        // --- VERTICAL: analytic ground from the Terrain heightfield ---
        // The (possibly just-moved) column under the player; voxel `(x,y,z)`
        // spans world `[y, y+1]`, so the standable top face is `surface_y + 1`.
        let col = IVec2::new(
            transform.translation.x.floor() as i32,
            transform.translation.z.floor() as i32,
        );
        let surface_y = terrain.surface_y(col.x, col.y) as f32 + 1.0;

        let foot_offset = PLAYER_HALF_HEIGHT + PLAYER_RADIUS; // capsule centre → feet
        let feet_y = transform.translation.y - foot_offset;
        // The surface "grabs" the player only within the same band the old
        // downward probe covered: up to STEP_HEIGHT above the capsule center
        // (step-up cap, so cliffs aren't teleported up in one step) and down to
        // GROUND_SNAP_DISTANCE below the feet (snap range on descents).
        let within_step_up = surface_y <= transform.translation.y + STEP_HEIGHT;
        let within_snap = surface_y >= feet_y - GROUND_SNAP_DISTANCE;

        if within_step_up && within_snap {
            // Rest the capsule on the surface; ease (don't hard-set) so step-ups
            // are a smooth rise, not a jolt.
            let target_y = surface_y + foot_offset;
            let k = 1.0 - (-GROUND_FOLLOW_RATE * dt_s).exp();
            transform.translation.y += (target_y - transform.translation.y) * k;
            cc.grounded = true;
            cc.vertical_velocity = 0.0;
        } else {
            // Off a real ledge: fall, but never sink below the analytic surface
            // (clamp the feet onto it once gravity brings them down to it).
            cc.grounded = false;
            cc.vertical_velocity -= GRAVITY * dt_s;
            transform.translation.y += cc.vertical_velocity * dt_s;
            let min_y = surface_y + foot_offset;
            if transform.translation.y <= min_y {
                transform.translation.y = min_y;
                cc.vertical_velocity = 0.0;
                cc.grounded = true;
            }
        }
    }
}

/// Refresh [`InteractionTarget`] by raycasting from the camera through the
/// cursor (falls back to camera-forward) against the solid-prop layer only.
fn update_interaction_target(
    spatial: SpatialQuery,
    windows: Query<&Window>,
    cam_q: Query<(&Camera, &GlobalTransform), With<IsoCamera>>,
    mut target: ResMut<InteractionTarget>,
) {
    let Ok((camera, cam_tf)) = cam_q.single() else {
        return;
    };

    let ray = windows
        .iter()
        .find_map(|w| w.cursor_position())
        .and_then(|cursor| camera.viewport_to_world(cam_tf, cursor).ok())
        .unwrap_or_else(|| Ray3d {
            origin: cam_tf.translation(),
            direction: Dir3::new(cam_tf.forward().as_vec3()).unwrap_or(Dir3::NEG_Z),
        });

    let filter = SpatialQueryFilter::from_mask([GameLayer::Solid]);
    let hit = spatial.cast_ray(ray.origin, ray.direction, INTERACT_RAY_LENGTH, true, &filter);

    match hit {
        Some(h) => {
            target.entity = Some(h.entity);
            target.point = Some(ray.origin + ray.direction.as_vec3() * h.distance);
        }
        None => {
            target.entity = None;
            target.point = None;
        }
    }
}
