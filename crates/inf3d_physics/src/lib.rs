//! Physics & collision for inf3d, built on **avian3d** (ECS-native Bevy
//! physics).
//!
//! This crate owns:
//!
//! * The [`GameLayer`] collision layers that separate **solid** props + terrain
//!   (which block the player) from **grass** (which the player walks through —
//!   grass simply gets *no collider*; the [`GameLayer::Grass`] membership only
//!   documents that intent).
//! * **Solid voxel terrain colliders** — a player-following patch of avian's
//!   `Collider::voxels` (parry's *solid* `Voxels` shape) generated from the
//!   deterministic [`Terrain`] oracle (see [`build_voxel_ground`]). Solid +
//!   always present, so the player can never fall through it (a hollow trimesh
//!   or one-sided heightfield can be sunk below; a solid voxel set cannot).
//! * Static colliders for solid props (trees → capsule trunk, rocks → cuboid),
//!   attached on spawn by the foliage crate via [`SolidPropCollider`].
//! * A **kinematic character controller** for the player: props block
//!   horizontally via `move_and_slide`; the ground is followed by a shape-cast
//!   from ABOVE the feet down onto the solid terrain, which both lets the player
//!   glide over voxel steps and structurally prevents falling through.
//! * An [`InteractionTarget`] resource + camera raycast hook.
//!
//! The simulation is stepped by avian's `PhysicsPlugins` (added in `inf3d_app`);
//! this crate only wires components + systems.

use avian3d::math::Vector;
use avian3d::prelude::*;
use bevy::prelude::*;

use inf3d_camera::IsoCamera;
use inf3d_worldgen::Terrain;

/// Collision membership layers.
///
/// The player's ground-follow + move queries hit [`Terrain`](GameLayer::Terrain)
/// and [`Solid`](GameLayer::Solid). **Grass is intentionally absent** — grass
/// carries no collider at all, so the player walks straight through it.
#[derive(PhysicsLayer, Clone, Copy, Debug, Default)]
pub enum GameLayer {
    /// Catch-all default layer.
    #[default]
    Default,
    /// The voxel terrain ground (solid `Collider::voxels` patch).
    Terrain,
    /// Solid blocking props: trees and rocks.
    Solid,
    /// The player's character-controller capsule.
    Player,
    /// Grass tufts. **No collider is ever attached to grass** — this layer only
    /// documents the "grass is non-colliding" decision.
    Grass,
}

/// Marker for the single player-following solid-voxel ground collider; `center`
/// is the world column its voxel set is currently built around.
#[derive(Component)]
struct VoxelGround {
    center: IVec2,
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
            radius: PLAYER_RADIUS,
            half_height: PLAYER_HALF_HEIGHT,
            vertical_velocity: 0.0,
            grounded: false,
        }
    }
}

/// The prop the interaction hook currently has targeted (camera ray pick).
#[derive(Resource, Default, Debug)]
pub struct InteractionTarget {
    pub entity: Option<Entity>,
    pub point: Option<Vec3>,
}

// ---------------------------------------------------------------------------
// Tunables. All world-space unless noted.
// ---------------------------------------------------------------------------

/// Player capsule radius.
pub const PLAYER_RADIUS: f32 = 0.4;
/// Player capsule cylindrical half-height (full capsule ≈ 2*half + 2*radius).
pub const PLAYER_HALF_HEIGHT: f32 = 0.6;
/// Gravity acceleration (world units / s²), applied only while airborne.
pub const GRAVITY: f32 = 24.0;
/// Max height the player will step up onto in one go (voxel steps / low props).
/// The ground probe starts this far ABOVE the feet, so any rise up to this much
/// is climbed smoothly; bigger jumps read as a real ledge and the player falls.
pub const STEP_HEIGHT: f32 = 1.2;
/// Extra reach below the feet for the ground probe (keeps the player glued to
/// the surface on downhill steps instead of briefly going airborne).
pub const GROUND_SNAP_DISTANCE: f32 = 0.5;
/// Distance the camera interaction ray travels before giving up.
pub const INTERACT_RAY_LENGTH: f32 = 1000.0;
/// How fast the feet ease onto the followed ground (per second). High = snappy
/// but soft; lower = floatier. Smooths step-ups so climbing a voxel eases up
/// instead of snapping in one frame (which read as a hard jolt).
pub const GROUND_FOLLOW_RATE: f32 = 14.0;

/// Half-width (in voxel columns) of the player-following solid-voxel ground
/// patch; it spans `2*GROUND_PATCH_HALF + 1` columns per axis.
const GROUND_PATCH_HALF: i32 = 24;
/// Rebuild + recenter the ground patch once the player drifts this many columns
/// from its center. Must stay well under `GROUND_PATCH_HALF`.
const GROUND_RECENTER_DIST: i32 = 12;
/// Solid voxel layers included below each column's surface. The player rests on
/// the top layer; a few layers give the collider real thickness.
const GROUND_PATCH_DEPTH: i32 = 3;

pub struct PhysicsGamePlugin;

impl Plugin for PhysicsGamePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InteractionTarget>()
            .add_systems(Startup, spawn_voxel_ground)
            .add_systems(
                Update,
                (
                    recenter_voxel_ground,
                    build_prop_colliders,
                    update_interaction_target,
                ),
            );
        // Runs after gameplay's `follow_path` (Update) writes `DesiredMove`, and
        // before the camera reads the player (PostUpdate).
        app.add_systems(PostUpdate, player_controller);
    }
}

/// Build a **solid** voxel collider for the terrain around `center`, sampled
/// from the deterministic [`Terrain`] oracle.
///
/// Each solid voxel center is at `(x + 0.5, y + 0.5, z + 0.5)`; parry bins each
/// point into the unit voxel that contains it, so the collider lines up exactly
/// with `bevy_voxel_world`'s mesh (voxel `(x,y,z)` spans world `[x, x+1]`, top
/// face at `y + 1` == `Terrain::stand_pos().y`). Unlike a trimesh (hollow) or a
/// heightfield (one-sided surface), this is a solid volume that cannot be fallen
/// through, and it is built instantly from the oracle so it never lags chunk
/// streaming.
fn build_voxel_ground(terrain: &Terrain, center: IVec2) -> Collider {
    let mut centers: Vec<Vector> = Vec::new();
    for dx in -GROUND_PATCH_HALF..=GROUND_PATCH_HALF {
        for dz in -GROUND_PATCH_HALF..=GROUND_PATCH_HALF {
            let x = center.x + dx;
            let z = center.y + dz;
            // Top solid voxel index in this column (solid fills `y < sample`).
            let top = terrain.surface_y(x, z);
            let bottom = (top - GROUND_PATCH_DEPTH + 1).max(0);
            for y in bottom..=top {
                centers.push(Vector::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5));
            }
        }
    }
    Collider::voxels_from_points(Vector::ONE, &centers)
}

/// Spawn the solid ground collider at startup, centered on the player's spawn
/// column so solid terrain exists under the player from frame one.
fn spawn_voxel_ground(mut commands: Commands, terrain: Res<Terrain>) {
    let center = terrain.nearest_land(IVec2::ZERO);
    commands.spawn((
        RigidBody::Static,
        build_voxel_ground(&terrain, center),
        CollisionLayers::new(GameLayer::Terrain, LayerMask::ALL),
        // Voxel centers are absolute world coords, so the entity sits at origin.
        Transform::default(),
        VoxelGround { center },
    ));
}

/// Rebuild the ground patch around the player when they drift far enough from
/// its center. Rebuilt only on a recenter, never per frame.
fn recenter_voxel_ground(
    terrain: Res<Terrain>,
    player_q: Query<&Transform, (With<CharacterController>, Without<VoxelGround>)>,
    mut patch_q: Query<(&mut Collider, &mut VoxelGround), Without<CharacterController>>,
) {
    let Ok(player) = player_q.single() else {
        return;
    };
    let Ok((mut collider, mut patch)) = patch_q.single_mut() else {
        return;
    };
    let cell = IVec2::new(
        player.translation.x.floor() as i32,
        player.translation.z.floor() as i32,
    );
    if (cell - patch.center).abs().max_element() >= GROUND_RECENTER_DIST {
        patch.center = cell;
        *collider = build_voxel_ground(&terrain, cell);
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

/// Kinematic character controller.
///
/// Horizontal: the desired (pathfinding) velocity is run through `move_and_slide`
/// against **solid props only**, so trees/rocks block and the player slides
/// along them — terrain is deliberately not a horizontal wall, so the player is
/// never stopped dead by a 1-voxel step.
///
/// Vertical: a capsule shape-cast from `STEP_HEIGHT` ABOVE the feet straight down
/// onto terrain + props. Whatever surface it lands on becomes the new foot
/// height. Casting from *above* means it climbs steps up to `STEP_HEIGHT`,
/// follows the surface downhill, lets the player stand on rocks, and — because
/// it always resolves the feet *onto* the solid surface from above — the player
/// can never end up below the ground. Only a drop larger than the probe range
/// (a real ledge) leaves the player airborne, where gravity takes over.
fn player_controller(
    time: Res<Time>,
    move_and_slide: MoveAndSlide,
    spatial: SpatialQuery,
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

        // --- VERTICAL: ground-follow via a downward RAY from the player center ---
        // A thin ray (not a capsule shape-cast) samples only the column directly
        // under the player, so pressing the capsule against a prop/cliff can't
        // register a side-contact at distance 0 and snap the player upward — that
        // was the "snapping like crazy when colliding" jitter. Casting from
        // STEP_HEIGHT above the center lets it still climb voxel steps, and it
        // lands on terrain or a prop top the player is centred over.
        let ground_filter = SpatialQueryFilter::from_mask([GameLayer::Terrain, GameLayer::Solid])
            .with_excluded_entities([entity]);
        let foot_offset = PLAYER_HALF_HEIGHT + PLAYER_RADIUS; // capsule centre → feet
        let probe_origin = transform.translation + Vec3::Y * STEP_HEIGHT;
        let probe_dist = STEP_HEIGHT + foot_offset + GROUND_SNAP_DISTANCE;
        let ground = spatial.cast_ray(probe_origin, Dir3::NEG_Y, probe_dist, true, &ground_filter);

        if let Some(hit) = ground {
            // Surface height directly under the player; rest the capsule on it.
            let surface_y = probe_origin.y - hit.distance;
            let target_y = surface_y + foot_offset;
            // Ease (don't hard-set) so step-ups are a smooth rise, not a jolt.
            let k = 1.0 - (-GROUND_FOLLOW_RATE * dt_s).exp();
            transform.translation.y += (target_y - transform.translation.y) * k;
            cc.grounded = true;
            cc.vertical_velocity = 0.0;
        } else {
            // Off a real ledge: fall.
            cc.grounded = false;
            cc.vertical_velocity -= GRAVITY * dt_s;
            transform.translation.y += cc.vertical_velocity * dt_s;
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
