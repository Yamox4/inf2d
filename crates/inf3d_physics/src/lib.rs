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
use inf3d_core::{AppState, GameSet, Pause};
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
    /// Request a jump this fixed step. The controller only honors it while grounded.
    pub jump: bool,
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

/// Canonical player dimensions: a **1 voxel wide × 2 voxels tall** capsule
/// (diameter `2*radius = 1.0`, height `2*(half_height + radius) = 2.0`), so the
/// body fully occupies a voxel column and can't slip between/through blocks.
/// `visual_root_offset` equals the capsule center→feet distance (`half_height +
/// radius`) so the figure's feet sit on the capsule's feet — kept at `1.0` (same
/// as before) so the visual + spawn alignment is unchanged by the width change.
pub const PLAYER_DIMS: PlayerDims = PlayerDims {
    radius: 0.5,
    half_height: 0.5,
    visual_root_offset: 0.5 + 0.5,
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
/// Initial upward velocity for FPS/manual jumps.
pub const JUMP_SPEED: f32 = 9.0;
/// Max height (above the **feet**) the player will step up onto in one go. Set
/// just above 1.0 so a single 1-unit voxel step is climbed smoothly while a
/// 2-voxel cliff is rejected — this mirrors pathfinding's `MAX_STEP = 1` voxel
/// (a path only routes over ≤1-voxel rises, so the controller must climb the
/// same). Any rise up to this much (relative to the current feet) is climbed; a
/// bigger jump down reads as a real ledge and the player falls.
pub const STEP_HEIGHT: f32 = 1.1;
/// Extra reach below the feet within which the ground still "grabs" the player
/// (keeps the feet glued to the surface on downhill steps instead of briefly
/// going airborne). A drop larger than this leaves the player airborne.
///
/// Sized to **mirror the climb cap on the way DOWN**: it is just above one voxel
/// (matching [`STEP_HEIGHT`] / pathfinding's `MAX_STEP = 1`) so a single 1-voxel
/// step down — the most common descent, and every tread of a built staircase —
/// stays grounded and *eases* down instead of flinging the player airborne for a
/// frame. At `0.5` (the old value) a 1.0-voxel step sat OUTSIDE the snap band, so
/// the controller went ballistic on every downward step: walking downhill or down
/// stairs read as a constant series of little "jumps off ledges". A genuine ledge
/// (a ≥2-voxel drop) is still beyond this and correctly goes airborne. Keeping the
/// up cap ([`STEP_HEIGHT`]) and this down cap equal is what makes the locomotion
/// able to execute, smoothly, exactly the ≤1-step routes the planner produces.
pub const GROUND_SNAP_DISTANCE: f32 = 1.1;
/// Distance the camera interaction ray travels before giving up.
pub const INTERACT_RAY_LENGTH: f32 = 1000.0;
/// How fast the feet ease onto the followed ground (per second). High = snappy
/// but soft; lower = floatier. Smooths step-ups so climbing a voxel eases up
/// instead of snapping in one frame (which read as a hard jolt).
pub const GROUND_FOLLOW_RATE: f32 = 14.0;
/// Collision "skin": how far the capsule's horizontal half-width is inset when
/// testing terrain / placed-block walls. The visual + avian capsule stays a full
/// voxel wide (`2 * PLAYER_RADIUS`); only the analytic wall test uses the inset.
///
/// Both the "phase through a wall's side" and the "can't walk a 2×1 corridor"
/// bugs trace to using the exact `PLAYER_RADIUS` (0.5) here. A capsule centred in
/// a cell (which the pathfinder always aims for — waypoints are cell centres) has
/// its edge land *exactly* on the neighbour cell boundary, and `floor()` then:
/// (a) reports the body as spanning TWO cells, so a 1-wide gap reads as blocked
/// even though a 1-wide body fits; and (b) places the leading edge already inside
/// the wall cell at the *start* of a step, so the `new_edge != old_edge` crossing
/// test never fires and the player slides into the wall face. Insetting by a small
/// skin fixes both: a centred body occupies exactly its own cell, and a wall
/// crossing is detected at the face. Kept tiny so the body still stops flush
/// against walls (≈5 cm of give).
pub const WALL_SKIN: f32 = 0.05;
/// Effective horizontal half-width for terrain / placed-block wall tests: the
/// capsule radius inset by [`WALL_SKIN`] (see there for the why). Used only by the
/// analytic wall math — vertical ground support still samples the full
/// [`PLAYER_RADIUS`] footprint so standing is never narrowed.
pub const WALL_HALF_WIDTH: f32 = PLAYER_RADIUS - WALL_SKIN;

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
        // Gated to un-paused play: the controller runs in `FixedPostUpdate` (not a
        // `GameSet`), so the core gating lever does NOT cover it. Without this it
        // would keep integrating the player (gravity, ground-follow, the last
        // `DesiredMove`) while the menu/pause is up. The menu also pauses
        // `Time<Physics>` so avian's own solver freezes; this stops OUR integrator.
        app.add_systems(
            FixedPostUpdate,
            player_controller
                .after(PhysicsSystems::Writeback)
                .run_if(in_state(AppState::InGame).and(in_state(Pause::Running))),
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
    // Short props (low rocks) carry NO physics collider, so the analytic ground is
    // the only thing that can put the player on top of one. `PropSurfaces::step`
    // contributes a +1 voxel rise on a cell occupied by a short prop; folded into
    // the effective walkable surface below it reads as a single climbable step (its
    // top is `terrain_top + 1`, ≤ `feet + STEP_HEIGHT` from adjacent flat ground, so
    // it's climbed — not flagged a wall).
    props: Res<inf3d_core::PropSurfaces>,
    move_and_slide: MoveAndSlide,
    mut q: Query<(
        Entity,
        &mut Transform,
        &mut CharacterController,
        &DesiredMove,
        &Collider,
    )>,
) {
    // The player body is a pure `RigidBody::Kinematic`, which avian does NOT
    // solver-integrate; this system is the SOLE position integrator for the
    // player — every `transform.translation` write below is the authoritative
    // motion for the step. `CustomPositionIntegration` only opts a body out of
    // avian's `integrate_positions` (its query is `Without<CustomPositionIntegration>`
    // over `SolverBody`), and that integrator advances *solver-integrated*
    // bodies; a pure `Kinematic` body is never position-integrated there in the
    // first place, so the marker is neither needed nor wanted here.
    let dt = time.delta();
    let dt_s = time.delta_secs();
    if dt_s <= 0.0 {
        return;
    }

    for (entity, mut transform, mut cc, desired, collider) in &mut q {
        // The player's `y` is untouched by the horizontal move, so compute the
        // foot level once here and reuse it for both the horizontal wall-block and
        // the vertical resolution below.
        let foot_offset = PLAYER_HALF_HEIGHT + PLAYER_RADIUS; // capsule centre → feet
        let feet_y = transform.translation.y - foot_offset;
        // Resolve ground at the player's CURRENT level: in a tunnel / under a built
        // overhang the player must rest on the floor nearest their feet, not the
        // topmost voxel. On normal terrain / single-layer columns this == surface_y.
        let ref_y = feet_y.round() as i32;
        // A terrain cell is a WALL the player can't enter when its top face is above
        // the step cap (feet + STEP_HEIGHT). A 1-voxel rise is at/below the cap and
        // stays walkable (you climb it); a 2-high wall is above it and blocks. Water
        // columns are never walls here.
        let wall_cap = feet_y + STEP_HEIGHT;
        // Whether terrain cell `(cx, cz)` is a WALL: dry land whose top face is above
        // the step cap (a 2-high wall blocks; a 1-voxel rise stays walkable). The
        // surface is the EFFECTIVE walkable height — terrain plus any short-prop step
        // — so a low rock adds +1 and its top (`terrain_top + 1`) stays ≤ the step cap
        // for a player on adjacent flat ground: it reads as a climbable step, NOT a
        // wall (a real 2-high terrain wall is still above the cap and blocks).
        let is_wall = |cx: i32, cz: i32| {
            terrain.is_land(cx, cz)
                && (terrain.surface_y_near(cx, cz, ref_y) + props.step(IVec2::new(cx, cz))) as f32
                    + 1.0
                    > wall_cap
        };
        // Integer cell range the capsule spans about `v`, using the inset
        // `WALL_HALF_WIDTH` so a body centred in a cell occupies exactly that cell
        // (1-wide corridors stay passable; see `WALL_SKIN`).
        let cell_span = |v: f32| cells_spanned(v, WALL_HALF_WIDTH);

        // --- HORIZONTAL: blocked by solid props (move_and_slide) AND by terrain
        // walls taller than the step cap. Without the terrain block the player walks
        // straight INTO a 2-high wall and phases into the terrain (terrain is not a
        // physics collider). The terrain block keeps the capsule CENTRE — the cell
        // that would swallow the player — out of wall cells, axis-separated so you
        // still SLIDE along a wall and step smoothly up 1-voxel rises. While
        // airborne (e.g. mid-jump) `feet_y` rises, so a wall the feet have cleared
        // stops counting as one and you can move over it. ---
        let prop_filter =
            SpatialQueryFilter::from_mask([GameLayer::Solid]).with_excluded_entities([entity]);
        let h_velocity = Vec3::new(desired.velocity.x, 0.0, desired.velocity.z);
        let old_xz = transform.translation;
        let mut new_pos = old_xz;
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
            new_pos = out.position;
        }
        // Block each axis when the capsule's LEADING EDGE crosses into a wall cell
        // that wasn't a wall at the old edge — so the body stops at the wall FACE
        // (the edge uses the inset `WALL_HALF_WIDTH`, so a centred body stops flush
        // with ≈`WALL_SKIN` of give and a 1-wide corridor stays passable) yet still
        // slides along a wall it's already beside.
        let dx = new_pos.x - old_xz.x;
        if dx.abs() > 1e-6 {
            let new_edge = leading_cell(new_pos.x, WALL_HALF_WIDTH, dx.signum());
            let old_edge = leading_cell(old_xz.x, WALL_HALF_WIDTH, dx.signum());
            let (z0, z1) = cell_span(old_xz.z);
            let into_wall = new_edge != old_edge
                && (z0..=z1).any(|cz| is_wall(new_edge, cz))
                && !(z0..=z1).any(|cz| is_wall(old_edge, cz));
            if into_wall {
                new_pos.x = old_xz.x;
            }
        }
        let dz = new_pos.z - old_xz.z;
        if dz.abs() > 1e-6 {
            let new_edge = leading_cell(new_pos.z, WALL_HALF_WIDTH, dz.signum());
            let old_edge = leading_cell(old_xz.z, WALL_HALF_WIDTH, dz.signum());
            let (x0, x1) = cell_span(new_pos.x);
            let into_wall = new_edge != old_edge
                && (x0..=x1).any(|cx| is_wall(cx, new_edge))
                && !(x0..=x1).any(|cx| is_wall(cx, old_edge));
            if into_wall {
                new_pos.z = old_xz.z;
            }
        }
        transform.translation = new_pos;

        // --- VERTICAL: analytic ground from the Terrain heightfield ---
        // Sample the analytic top face over every cell the footprint disc overlaps
        // and rest on the highest CLIMBABLE one (top within STEP_HEIGHT of the feet);
        // a taller neighbour is a wall, excluded from support. Voxel `(x,y,z)` spans
        // world `[y, y+1]`, so its top face is `surface_y + 1`.
        let support_surface_y = footprint_surface(
            transform.translation.x,
            transform.translation.z,
            PLAYER_RADIUS,
            feet_y,
            feet_y + STEP_HEIGHT,
            // EFFECTIVE walkable surface: terrain height plus any short-prop step on
            // the cell, so the feet rest on the prop's top (terrain_top + 1) when the
            // player stands over a low rock. `is_land` stays terrain-only — short props
            // only ever sit on land, so they never gate standability themselves.
            |cx, cz| terrain.surface_y_near(cx, cz, ref_y) + props.step(IVec2::new(cx, cz)),
            |cx, cz| terrain.is_land(cx, cz),
        );

        if desired.jump && cc.grounded {
            cc.grounded = false;
            cc.vertical_velocity = JUMP_SPEED - GRAVITY * dt_s;
            transform.translation.y += JUMP_SPEED * dt_s;
        } else {
            let (new_y, grounded, new_vv) = resolve_ground(
                feet_y,
                transform.translation.y,
                support_surface_y,
                foot_offset,
                cc.vertical_velocity,
                dt_s,
            );
            transform.translation.y = new_y;
            cc.grounded = grounded;
            cc.vertical_velocity = new_vv;
        }
    }
}

/// Inclusive integer cell range `[lo, hi]` an axis-aligned span of half-width
/// `half` about `center` touches — the cells under the capsule to test for walls.
/// With the inset [`WALL_HALF_WIDTH`] a capsule centred in a cell spans exactly
/// that one cell, so a 1-wide corridor is passable; with the full [`PLAYER_RADIUS`]
/// a centred body straddles into the neighbour cell (the corridor-blocking bug).
fn cells_spanned(center: f32, half: f32) -> (i32, i32) {
    (
        (center - half).floor() as i32,
        (center + half).floor() as i32,
    )
}

/// Integer cell index the capsule's **leading edge** lies in when moving along an
/// axis: `center` pushed out by `half` in the travel direction (`dir_sign` is
/// `±1`). The controller blocks a step when this index crosses from a non-wall
/// cell into a wall cell. With the inset [`WALL_HALF_WIDTH`] a centred body's
/// leading edge stays inside its own cell, so the crossing into an adjacent wall
/// is actually detected; the full [`PLAYER_RADIUS`] would start it on the boundary
/// (already "in" the wall cell) and miss the crossing (the wall-phasing bug).
fn leading_cell(center: f32, half: f32, dir_sign: f32) -> i32 {
    (center + half * dir_sign).floor() as i32
}

/// Sample the analytic terrain top face under the capsule footprint and return
/// the highest **climbable** standable surface it overlaps.
///
/// The capsule is a circle of `radius` in plan. The footprint is the set of
/// cells the AABB `[x-radius, x+radius] × [z-radius, z+radius]` touches: we
/// compute the integer cell range each axis spans (flooring the lo/hi extents)
/// and read [`Terrain::surface_y`] over that small grid — at most a 2×2 (or 3×3
/// for a radius spanning a cell) block, no allocation. We take the max of the
/// tops that are **climbable** (`<= max_climb_y`, i.e. `feet + STEP_HEIGHT`), so
/// the feet rest on the tallest cell beneath the body they can actually stand on
/// — resting on cliff edges and stepping smoothly up 1-voxel rises — while a
/// taller neighbour beyond step height is treated as a WALL, not support.
///
/// Why the climbable filter matters: if an unreachable rise (a 2-voxel cliff in
/// the footprint) were returned as "support", the ground resolver — handed a rest
/// height *above* the player — could neither ease them up onto it (out of step
/// range) nor clamp them down (the rest is above their centre), so a player
/// carrying any downward velocity would fall through the floor forever. Excluding
/// walls keeps `support` to ground the player can rest on.
///
/// BOXED-IN FALLBACK: if every overlapped cell is a wall above the step cap there
/// is nothing here to stand on. This is reachable when the player digs a niche
/// into a wall and caps their own headroom — `surface_y_near` then disqualifies
/// the floor under their feet (no standing headroom) and reports the wall TOP far
/// above as the column's nearest standable floor. Returning that unclimbable top
/// would let [`resolve_ground`]'s landing clamp **teleport the player up onto the
/// wall** (the 10-voxel-wall climb bug). Instead we report support at the player's
/// `feet_y`, so a squished/boxed-in player stays grounded in place and is never
/// lifted — upholding the "support is always climbable" contract `resolve_ground`
/// relies on.
///
/// This samples the cells the disc *actually overlaps* regardless of sub-cell
/// position: a capsule at a cell centre still picks up a taller (but climbable)
/// cell across the boundary. Each voxel `(x,y,z)` spans world `[y, y+1]`, so its
/// top face is `surface_y + 1`. `surface_y` maps a column to its topmost-solid
/// index — production passes [`Terrain::surface_y_near`]; tests pass a synthetic
/// field.
fn footprint_surface(
    x: f32,
    z: f32,
    radius: f32,
    feet_y: f32,
    max_climb_y: f32,
    surface_y: impl Fn(i32, i32) -> i32,
    is_land: impl Fn(i32, i32) -> bool,
) -> f32 {
    let x_min = (x - radius).floor() as i32;
    let x_max = (x + radius).floor() as i32;
    let z_min = (z - radius).floor() as i32;
    let z_max = (z + radius).floor() as i32;

    let mut best = f32::NEG_INFINITY;
    for cx in x_min..=x_max {
        for cz in z_min..=z_max {
            // Submerged water columns (seafloor) are NOT standable support — only
            // dry land counts, so the controller is never seated on a seafloor that
            // a footprint happened to overlap near a shoreline.
            if !is_land(cx, cz) {
                continue;
            }
            let top = surface_y(cx, cz) as f32 + 1.0;
            // Climbable cells only — a wall above the step cap is not support.
            if top <= max_climb_y {
                best = best.max(top);
            }
        }
    }
    if best == f32::NEG_INFINITY {
        // Boxed in — nothing climbable under the footprint. Hold at the feet so the
        // player is never launched onto an unclimbable wall (see BOXED-IN FALLBACK).
        feet_y
    } else {
        best
    }
}

/// Pure vertical-resolution decision: given the current feet/centre heights and
/// the analytic `support_surface_y` (footprint-max top face), decide the new
/// capsule-centre Y, grounded flag, and vertical velocity for this step.
///
/// Behavior:
/// * **Eased follow (grounded):** when the player is *not* already falling
///   (`vertical_velocity >= 0`) and the surface sits within the grab band — up
///   to [`STEP_HEIGHT`] above the **feet** (step-up cap, so a 2-voxel cliff
///   isn't teleported up) and down to [`GROUND_SNAP_DISTANCE`] below the feet
///   (snap range on descents) — the feet ease onto it at [`GROUND_FOLLOW_RATE`],
///   grounded, zero vertical velocity. Gating on the feet (not the centre) keeps
///   the climb cap at ~1 voxel, matching pathfinding's `MAX_STEP = 1`.
/// * **Ballistic (airborne / out of band):** otherwise apply [`GRAVITY`] and
///   integrate, then LAND (clamp onto the surface, re-ground) the moment the
///   step reaches the support's rest height (`new_y <= min_y`). The caller only
///   ever passes a CLIMBABLE `support_surface_y` (its footprint sampler excludes
///   walls above `feet + STEP_HEIGHT`), so `min_y` is always real ground —
///   clamping up to it catches both a normal fall and a player who sank slightly
///   below the surface, with no risk of being lifted onto an unreachable wall
///   (those are filtered out before they ever reach this function).
///
/// Kept free of ECS/`Time` so it can be unit-tested directly; `foot_offset` is
/// the capsule centre→feet distance the caller already computes.
fn resolve_ground(
    feet_y: f32,
    current_center_y: f32,
    support_surface_y: f32,
    foot_offset: f32,
    vertical_velocity: f32,
    dt_s: f32,
) -> (f32, bool, f32) {
    let within_step_up = support_surface_y <= feet_y + STEP_HEIGHT;
    let within_snap = support_surface_y >= feet_y - GROUND_SNAP_DISTANCE;
    let min_y = support_surface_y + foot_offset;

    // Only ease-follow when stationary/rising against an in-band surface; a body
    // already carrying downward momentum is mid-fall and must integrate + land
    // (so it clamps exactly onto the surface rather than easing toward it).
    if vertical_velocity >= 0.0 && within_step_up && within_snap {
        // Rest the capsule on the surface; ease (don't hard-set) so step-ups
        // are a smooth rise, not a jolt.
        let k = 1.0 - (-GROUND_FOLLOW_RATE * dt_s).exp();
        let new_y = current_center_y + (min_y - current_center_y) * k;
        (new_y, true, 0.0)
    } else {
        // Off a real ledge (or mid-fall): apply gravity and integrate, then land
        // the instant the step reaches the support's rest height. `support` is
        // guaranteed climbable by the caller (walls are filtered out of the
        // footprint), so `min_y` is always ground the player belongs on — landing
        // when `new_y <= min_y` catches a normal fall AND re-seats a player who
        // sank just below the surface, instead of (the old `min_y <= centre`
        // guard's bug) letting a sunk, downward-moving player fall through forever.
        let vv = vertical_velocity - GRAVITY * dt_s;
        let new_y = current_center_y + vv * dt_s;
        if new_y <= min_y {
            (min_y, true, 0.0)
        } else {
            (new_y, false, vv)
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
    let hit = spatial.cast_ray(
        ray.origin,
        ray.direction,
        INTERACT_RAY_LENGTH,
        true,
        &filter,
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Capsule centre→feet distance used in the controller.
    const FOOT_OFFSET: f32 = PLAYER_HALF_HEIGHT + PLAYER_RADIUS;
    /// A representative fixed-step delta (avian's default 64 Hz).
    const DT: f32 = 1.0 / 64.0;

    /// Build a centre/feet pair resting exactly on `surface` (feet on the top
    /// face), the steady state the controller converges to.
    fn resting_on(surface: f32) -> (f32, f32) {
        let center = surface + FOOT_OFFSET;
        (center - FOOT_OFFSET, center)
    }

    // --- horizontal wall math: the 2×1 corridor + wall-phasing fix (WALL_SKIN) ---

    // REGRESSION (can't walk a 2×1 corridor): a capsule centred in a cell must
    // span exactly that one cell with the inset half-width, so the perpendicular
    // wall test never sees the flanking wall of a 1-wide corridor. The full radius
    // straddles into the neighbour — the bug that made a 1-wide corridor read as
    // blocked even though a 1-wide body fits.
    #[test]
    fn centred_capsule_spans_single_cell_with_skin() {
        // Centre of cell 5 is x = 5.5.
        assert_eq!(
            cells_spanned(5.5, WALL_HALF_WIDTH),
            (5, 5),
            "a centred body must occupy exactly its own cell (corridor passable)"
        );
        // The unfixed full-radius span straddles into cell 6 — the corridor block.
        assert_eq!(
            cells_spanned(5.5, PLAYER_RADIUS),
            (5, 6),
            "full radius straddles into the neighbour cell (the corridor bug)"
        );
    }

    // The skin must be small enough that a body NOT centred — pushed up against a
    // wall — still only reaches the wall it's beside, and large enough that the
    // span never reports three cells for a sub-voxel body.
    #[test]
    fn cell_span_is_one_cell_across_a_voxel() {
        // Anywhere inside cell 5, the inset span stays within [4, 6] and is at most
        // two cells wide; centred it is exactly one. Sample several offsets.
        for &x in &[5.05_f32, 5.25, 5.5, 5.75, 5.95] {
            let (lo, hi) = cells_spanned(x, WALL_HALF_WIDTH);
            assert!(hi - lo <= 1, "span {lo}..={hi} for x={x} exceeds two cells");
            assert!(
                lo >= 4 && hi <= 6,
                "span {lo}..={hi} for x={x} escaped the voxel"
            );
        }
    }

    // REGRESSION (phasing through a wall's side): moving from a cell-centred
    // position toward a wall in the next cell, the OLD leading edge must still be
    // inside the current cell and the NEW edge must cross into the wall, so the
    // crossing is detectable. The full radius starts the edge already in the wall
    // cell, so old == new and the block never fires (the phase-through bug).
    #[test]
    fn leading_edge_crossing_into_wall_is_detected() {
        let center = 5.5_f32; // centred in cell 5; wall is cell 6
        let old_edge = leading_cell(center, WALL_HALF_WIDTH, 1.0);
        assert_eq!(
            old_edge, 5,
            "inset leading edge starts inside the current cell"
        );
        let moved = center + 0.1; // one step toward the wall
        let new_edge = leading_cell(moved, WALL_HALF_WIDTH, 1.0);
        assert_eq!(
            new_edge, 6,
            "the step crosses the leading edge into the wall cell"
        );
        assert_ne!(old_edge, new_edge, "a crossing the controller can block");
        // The unfixed full radius puts the edge on the wall cell from the start, so
        // there is no crossing to detect → the player slides straight in.
        assert_eq!(
            leading_cell(center, PLAYER_RADIUS, 1.0),
            6,
            "full radius starts the edge already in the wall cell (the phasing bug)"
        );
    }

    // Symmetry: the same holds for negative-direction travel (the leading edge is
    // on the low side), so walls block identically whichever way you walk into them.
    #[test]
    fn leading_edge_crossing_is_symmetric() {
        let center = 5.5_f32; // centred in cell 5; wall is cell 4
        assert_eq!(leading_cell(center, WALL_HALF_WIDTH, -1.0), 5);
        assert_eq!(leading_cell(center - 0.1, WALL_HALF_WIDTH, -1.0), 4);
    }

    // (a) A surface exactly 1 voxel above the feet is within STEP_HEIGHT (1.1):
    // the player eases up toward it and stays grounded.
    #[test]
    fn step_up_one_voxel_accepted() {
        let (feet, center) = resting_on(0.0);
        let surface = 1.0; // one voxel rise
        let (new_y, grounded, vv) = resolve_ground(feet, center, surface, FOOT_OFFSET, 0.0, DT);
        assert!(grounded, "a 1-voxel step must keep the player grounded");
        assert_eq!(vv, 0.0, "grounded resolution zeroes vertical velocity");
        // Eased (not snapped) toward the new resting centre, and strictly rising.
        let target = surface + FOOT_OFFSET;
        assert!(new_y > center, "should rise toward the step");
        assert!(new_y < target, "rise eases (does not snap) in one step");
    }

    // (b) A neighbour beyond STEP_HEIGHT is a WALL: `footprint_surface` EXCLUDES
    // it and falls back to climbable ground, so the resolver is never handed a
    // rest height above the player. (Feeding a 2-voxel rise straight to
    // `resolve_ground` is no longer a reachable state — the sampler filters it —
    // so we assert the filtering here instead.)
    #[test]
    fn footprint_excludes_unclimbable_wall() {
        // Cell 1 is a 5-voxel wall (top 11); cell 0 is the flat ground (top 6).
        let field = |cx: i32, _cz: i32| if cx >= 1 { 10 } else { 5 };
        // Player on cell 0 (feet at top face 6); ceiling = feet + STEP_HEIGHT.
        let feet = 6.0;
        let ceiling = feet + STEP_HEIGHT;
        // Centre near the +x boundary so the footprint AABB reaches the wall cell.
        let s = footprint_surface(0.7, 0.5, PLAYER_RADIUS, feet, ceiling, field, |_, _| true);
        assert_eq!(
            s, 6.0,
            "an unclimbable wall in the footprint is excluded; support is the ground"
        );
    }

    // (b2) REGRESSION (the first-move fall-through): a player BELOW a climbable
    // support's rest height while carrying downward velocity must LAND on it, not
    // fall through. The old `min_y <= centre` ballistic guard skipped landing in
    // exactly this state (rest above the sunk, falling player) → infinite fall.
    #[test]
    fn falling_player_lands_on_climbable_support() {
        let surface = 1.0; // top face of a climbable rise
        let min_y = surface + FOOT_OFFSET;
        let center = min_y - 0.5; // player is BELOW the rest height
        let feet = center - FOOT_OFFSET;
        // Precondition: the support is genuinely climbable from the feet.
        assert!(surface <= feet + STEP_HEIGHT && surface >= feet - GROUND_SNAP_DISTANCE);
        let (new_y, grounded, vv) = resolve_ground(feet, center, surface, FOOT_OFFSET, -4.0, DT);
        assert!(
            grounded,
            "a falling player must land on climbable support, not fall through"
        );
        assert_eq!(new_y, min_y, "the centre is seated exactly on the support");
        assert_eq!(vv, 0.0, "landing zeroes the vertical velocity");
    }

    // (c) Walking off a ledge: the surface drops far below the feet (beyond
    // GROUND_SNAP_DISTANCE), so the player goes airborne and begins to fall.
    #[test]
    fn ledge_goes_airborne_and_falls() {
        let (feet, center) = resting_on(5.0);
        let surface = 0.0; // floor fell away well beyond the snap range
        let (new_y, grounded, vv) = resolve_ground(feet, center, surface, FOOT_OFFSET, 0.0, DT);
        assert!(!grounded, "stepping off a ledge must go airborne");
        assert!(vv < 0.0, "gravity must pull the velocity negative");
        assert!(new_y < center, "the player must start descending");
    }

    // (d) A small drop within GROUND_SNAP_DISTANCE keeps the player glued to the
    // surface (grounded, eased down) instead of briefly going airborne.
    #[test]
    fn snap_down_within_distance_stays_grounded() {
        let (feet, center) = resting_on(2.0);
        // Surface a little below the feet but inside the snap band.
        let surface = 2.0 - (GROUND_SNAP_DISTANCE * 0.5);
        let (new_y, grounded, vv) = resolve_ground(feet, center, surface, FOOT_OFFSET, 0.0, DT);
        assert!(grounded, "a drop within snap distance stays grounded");
        assert_eq!(vv, 0.0, "snapped resolution zeroes vertical velocity");
        assert!(new_y < center, "the feet ease down onto the lower surface");
        assert!(
            new_y > surface + FOOT_OFFSET,
            "the descent eases (does not snap) in one step"
        );
    }

    // (f) REGRESSION (the "jumps off ledges" / downhill hopping bug): a normal
    // 1-voxel step DOWN must stay grounded and ease down, NOT go airborne. With the
    // old GROUND_SNAP_DISTANCE = 0.5 a 1.0-voxel drop sat outside the snap band, so
    // every downward step flung the player ballistic for a frame — descending stairs
    // / hills read as a constant series of little jumps. The snap band now mirrors
    // the climb cap (≈1 voxel), so a single step down is glued to the surface.
    #[test]
    fn step_down_one_voxel_stays_grounded() {
        let (feet, center) = resting_on(5.0);
        let surface = 4.0; // exactly one voxel below the current surface
        let (new_y, grounded, vv) = resolve_ground(feet, center, surface, FOOT_OFFSET, 0.0, DT);
        assert!(
            grounded,
            "a 1-voxel step down must stay grounded, not go airborne (the hopping bug)"
        );
        assert_eq!(vv, 0.0, "a grounded step-down carries no falling velocity");
        // Eased (not snapped) down toward the lower resting centre, strictly descending.
        let target = surface + FOOT_OFFSET;
        assert!(new_y < center, "the feet ease down onto the lower step");
        assert!(
            new_y > target,
            "the descent eases (does not snap) in one step: {new_y} vs target {target}"
        );
    }

    // (f2) BOUNDARY: a 2-voxel drop is a genuine ledge — beyond the (≈1-voxel) snap
    // band — so it still goes airborne and falls. This pins GROUND_SNAP_DISTANCE
    // between one voxel (grabbed, above) and two (released, here) so a future tweak
    // can't silently start gluing the player to ledges they should fall off.
    #[test]
    fn step_down_two_voxels_goes_airborne() {
        let (feet, center) = resting_on(5.0);
        let surface = 3.0; // two voxels below — a real ledge
        let (new_y, grounded, vv) = resolve_ground(feet, center, surface, FOOT_OFFSET, 0.0, DT);
        assert!(
            !grounded,
            "a 2-voxel drop is a ledge and must go airborne, not stick"
        );
        assert!(vv < 0.0, "gravity pulls the velocity negative off the ledge");
        assert!(new_y < center, "the player starts descending");
    }

    // --- footprint_surface: multi-cell support sampling ---

    // A capsule resting exactly at a cell centre (integer + 0.5) next to a
    // taller cell directly across the boundary must pick up that taller cell's
    // top face — the precise "sinking into a higher adjacent cell" case the
    // footprint sampling targets. With radius 0.4 and centre at x = 0.5, the
    // disc spans [0.1, 0.9] which floors to cell 0 only on x, but the original
    // point-rim samples (0.1 / 0.9) both floored to 0 and missed cell 1. Place
    // the centre near the +x boundary so the disc reaches into cell 1.
    #[test]
    fn footprint_picks_up_taller_adjacent_cell_when_centred() {
        // Cell 1 (in x) is one voxel taller than cell 0; everything else flat.
        let field = |cx: i32, _cz: i32| if cx >= 1 { 6 } else { 5 };
        // Centre at x = 0.7: disc spans [0.3, 1.1] → cells 0 and 1 in x.
        // INFINITY ceiling: this test exercises AABB sampling, not the climb cap
        // (so `feet` is irrelevant here — a finite placeholder).
        let s = footprint_surface(
            0.7,
            0.5,
            PLAYER_RADIUS,
            5.0,
            f32::INFINITY,
            field,
            |_, _| true,
        );
        assert_eq!(
            s, 7.0,
            "footprint must rest on the taller adjacent cell's top face (6+1)"
        );
    }

    // When the whole footprint sits over uniform terrain, the result is just
    // that uniform top face (no spurious lift from neighbour sampling).
    #[test]
    fn footprint_uniform_terrain_is_flat_top_face() {
        let field = |_cx: i32, _cz: i32| 5;
        let s = footprint_surface(
            10.5,
            -3.5,
            PLAYER_RADIUS,
            5.0,
            f32::INFINITY,
            field,
            |_, _| true,
        );
        assert_eq!(s, 6.0, "uniform terrain yields surface_y + 1");
    }

    // A taller cell reachable only on a DIAGONAL corner of the footprint must
    // still be picked up — the AABB sampling covers diagonal neighbours, which
    // the old 4-cardinal point sampling never checked.
    #[test]
    fn footprint_picks_up_taller_diagonal_cell() {
        // Only the diagonal cell (1, 1) is taller.
        let field = |cx: i32, cz: i32| if cx >= 1 && cz >= 1 { 6 } else { 5 };
        // Centre near the +x/+z corner so the disc's AABB reaches cell (1, 1).
        let s = footprint_surface(
            0.7,
            0.7,
            PLAYER_RADIUS,
            5.0,
            f32::INFINITY,
            field,
            |_, _| true,
        );
        assert_eq!(s, 7.0, "diagonal corner of the footprint must be sampled");
    }

    // --- short-prop step: PropSurfaces::step folded into the walkable surface ---

    // A short prop (low rock) carries NO collider, so the analytic ground is the
    // only thing that can stand the player on it. The controller folds
    // `PropSurfaces::step` (+1 on a prop cell) into the EFFECTIVE surface passed to
    // `footprint_surface`. Standing over the prop, the feet must rise onto the
    // prop's top (terrain top + 1), and that rise must be CLIMBABLE — within
    // `STEP_HEIGHT` of the feet — so the support is accepted, not filtered as a wall.
    #[test]
    fn footprint_climbs_onto_short_prop_step() {
        // Flat terrain at surface_y = 5 (top face 6); the prop sits on the cell the
        // player stands over (here x == prop_x == 0 at centre 0.5). The effective
        // field is `base + step`, i.e. terrain 5 plus the +1 short-prop step.
        let base = 5;
        let prop_x = 0;
        let effective = |cx: i32, _cz: i32| base + if cx == prop_x { 1 } else { 0 };
        // Player feet on the flat ground top face (6); the prop top is 7 = feet + 1,
        // which is ≤ feet + STEP_HEIGHT (7.1), so it's a climbable step.
        let feet = 6.0;
        let s = footprint_surface(
            0.5,
            0.5,
            PLAYER_RADIUS,
            feet,
            feet + STEP_HEIGHT,
            effective,
            |_, _| true,
        );
        assert_eq!(
            s, 7.0,
            "feet must rest on the short prop's top (terrain top 6 + the +1 prop step)"
        );
        // And the rise is genuinely climbable from the feet (mirrors the controller's
        // step cap), so the resolver eases the player up and keeps them grounded.
        let (feet_flat, center_flat) = resting_on(6.0);
        let (new_y, grounded, vv) = resolve_ground(feet_flat, center_flat, s, FOOT_OFFSET, 0.0, DT);
        assert!(
            grounded,
            "stepping onto a short prop keeps the player grounded"
        );
        assert_eq!(vv, 0.0, "grounded resolution zeroes vertical velocity");
        assert!(new_y > center_flat, "the feet rise toward the prop top");
    }

    // The +1 short-prop step must read as a climbable STEP, never a wall. The
    // controller's `is_wall` flags a cell only when its effective top face exceeds
    // the step cap (`feet + STEP_HEIGHT`). For a player on adjacent flat ground
    // (`feet = base + 1` voxels), a short prop's effective top is `base + 1 + 1`,
    // which is ≤ the cap — so it is NOT a wall. (A real 2-high terrain wall would be
    // `base + 2 + 1`, above the cap, and would block.) This mirrors the exact
    // comparison `is_wall` makes, on a synthetic effective field.
    #[test]
    fn short_prop_step_is_not_a_wall() {
        // Player stands on flat ground whose top face is `base + 1` (surface_y base,
        // top = base + 1). Express the feet in those terms.
        let base = 5_i32;
        let feet = (base + 1) as f32; // feet on the flat-ground top face
        let wall_cap = feet + STEP_HEIGHT;
        // Effective surface_y over the prop cell = terrain `base` + the +1 prop step.
        let prop_surface_y = base + 1;
        let prop_top = prop_surface_y as f32 + 1.0; // == base + 1 + 1
        assert!(
            prop_top <= wall_cap,
            "a +1 short-prop step (top {prop_top}) is within the step cap {wall_cap} — a climbable step, not a wall"
        );
        // Contrast: a genuine 2-high terrain wall on the same cell IS above the cap.
        let wall_top = (base + 2) as f32 + 1.0; // == base + 2 + 1
        assert!(
            wall_top > wall_cap,
            "a real 2-high wall (top {wall_top}) still exceeds the cap {wall_cap} and blocks"
        );
    }

    // REGRESSION (the 10-voxel wall climb): a player boxed in by walls all taller
    // than the step cap — e.g. they dug a niche into a wall and capped their own
    // headroom, so even their own column resolves to the wall TOP far above — must
    // NOT be lifted. The old fallback returned the (unclimbable) center-column
    // surface, which `resolve_ground`'s landing clamp then teleported the player up
    // onto. The fallback must instead report support at the feet (hold in place).
    #[test]
    fn footprint_boxed_in_holds_at_feet_not_wall_top() {
        // Every cell is a 10-high wall (top 11); the player's feet are at 2, so the
        // step cap (3.1) excludes all of them — the boxed-in fallback path.
        let field = |_cx: i32, _cz: i32| 10;
        let feet = 2.0;
        let s = footprint_surface(
            0.5,
            0.5,
            PLAYER_RADIUS,
            feet,
            feet + STEP_HEIGHT,
            field,
            |_, _| true,
        );
        assert_eq!(
            s, feet,
            "a boxed-in player must hold at their feet, never be lifted onto the wall top"
        );
        // Feeding that support to the resolver keeps them exactly in place
        // (grounded, no upward teleport) instead of clamping up to the wall.
        let center = feet + FOOT_OFFSET;
        let (new_y, grounded, vv) = resolve_ground(feet, center, s, FOOT_OFFSET, 0.0, DT);
        assert!(grounded, "the squished player stays grounded");
        assert_eq!(vv, 0.0);
        assert!(
            (new_y - center).abs() < 1e-3,
            "the player must not be teleported up the wall (new_y {new_y}, center {center})"
        );
    }

    // (e) While falling, the gravity integration never sinks the feet below the
    // support surface — once the integrated step would cross it, the centre is
    // clamped exactly onto it and the player re-grounds.
    #[test]
    fn gravity_clamp_never_sinks_below_surface() {
        let surface = 0.0;
        let min_y = surface + FOOT_OFFSET;
        // Just above the surface with a large downward velocity: one step would
        // overshoot below `min_y` without the clamp.
        let center = min_y + 0.01;
        let feet = center - FOOT_OFFSET;
        let (new_y, grounded, vv) = resolve_ground(feet, center, surface, FOOT_OFFSET, -50.0, DT);
        assert_eq!(
            new_y, min_y,
            "the centre is clamped exactly onto the surface"
        );
        assert!(grounded, "landing re-grounds the player");
        assert_eq!(vv, 0.0, "landing zeroes the vertical velocity");
        assert!(
            new_y - FOOT_OFFSET >= surface - 1e-6,
            "the feet never sink below the support surface"
        );
    }
}
