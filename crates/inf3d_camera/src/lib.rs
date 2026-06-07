//! Perspective third-person **orbit** camera (Cube World-style) with WASD play.
//!
//! The camera floats on a boom behind the player: the mouse orbits it (yaw +
//! pitch), the scroll wheel zooms the boom distance, and the rig looks at a focus
//! point just above the player. The OS cursor is captured/hidden in play so
//! mouse-look has unlimited travel. WASD is always-on and **camera-relative**
//! (W drives away from the camera, into the screen); the player faces its travel
//! direction. `F` toggles a debug free-fly camera. The rig raycasts the voxel
//! world along the boom so it never clips through terrain or player builds.

use bevy::camera::{PerspectiveProjection, Projection};
use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, NormalPrepass};
use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel};
use bevy::post_process::bloom::Bloom;
use bevy::post_process::dof::DepthOfField;
use bevy::post_process::motion_blur::MotionBlur;
use bevy::prelude::*;
use bevy::render::view::{ColorGrading, Hdr, Msaa};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use bevy_voxel_world::prelude::*;
// Only the read-only spatial query — used to keep the boom from clipping through prop
// colliders (trees/rocks), which the voxel raycast can't see.
use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};

use inf3d_core::{AppState, FollowTarget, GameSet, MoveIntent, Pause, QualitySettings};
use inf3d_world::MainWorld;

// ── Projection ───────────────────────────────────────────────────────────────
/// Vertical field of view (radians). 60° reads as a natural third-person lens.
const FOV: f32 = std::f32::consts::FRAC_PI_3;
/// Near clip plane (world units). Small so the boom can pull in close without the
/// player clipping out of view.
const NEAR: f32 = 0.1;
/// Far clip plane (world units). Large enough for the orbit/free-fly view to reach
/// the horizon over the streamed terrain.
const FAR: f32 = 1500.0;

// ── Boom distance (zoom) ───────────────────────────────────────────────────────
/// Default boom distance from the focus to the eye (world units).
const DISTANCE_DEFAULT: f32 = 12.0;
/// Closest the scroll wheel can pull the boom in.
const DISTANCE_MIN: f32 = 4.0;
/// Furthest the scroll wheel can push the boom out.
const DISTANCE_MAX: f32 = 40.0;
/// Boom-distance change per scroll notch (world units).
const ZOOM_SPEED: f32 = 2.0;

// ── Orbit angles ───────────────────────────────────────────────────────────────
/// Lowest pitch (radians): dips the camera below the focus to look UP, so you can aim
/// up at tall walls / overhead blocks when building. Not so low it flips under the world.
const PITCH_MIN: f32 = -0.5;
/// Highest pitch (radians): a steep near-top-down look, short of straight down so
/// the look-at never degenerates against world up.
const PITCH_MAX: f32 = 1.4;
/// Default pitch (radians): a gentle 3/4 down-angle, the Cube World resting view.
const PITCH_DEFAULT: f32 = 0.5;
/// Mouse-look sensitivity (radians per pixel of motion).
const LOOK_SENS: f32 = 0.005;

// ── Focus + smoothing ──────────────────────────────────────────────────────────
/// Height of the focus point above the player's transform origin (world units). A bit
/// above the capsule centre (≈ head height) so the character frames in the lower third
/// and the screen-centre crosshair points AHEAD of it (into the build space), not at
/// its head. The camera pivots on the PLAYER — there is no forward pivot offset, which
/// read as a disorienting "orbit around a point in front of you".
const FOCUS_HEIGHT: f32 = 1.2;
/// Exponential rate the boom DISTANCE eases toward its scroll target (zoom glide). The
/// camera POSITION is otherwise rigid — recomputed from the (already interpolated)
/// player every frame with NO horizontal lerp — so the follow has no lag and never
/// feels floaty. Only zoom + collision are smoothed.
const DISTANCE_SMOOTH: f32 = 12.0;
/// Radius (world units) of the boom's collision "bulk": the spread of the parallel-ray
/// bundle that approximates a sphere-cast, so the camera keeps this much clearance and a
/// single thin ray can't slip through a voxel edge.
const CAMERA_RADIUS: f32 = 0.3;
/// Gap (world units) kept between the camera and a voxel the boom would otherwise clip
/// into — the boom stops this far short of terrain/builds.
const BOOM_PADDING: f32 = 0.3;
/// Floor for the boom length after a collision clamp, so the camera never collapses
/// fully onto the focus when pressed against a wall (kept just outside the body —
/// near-first-person in a tight cave, which is the indoor "zoom in" behaviour).
const BOOM_MIN: f32 = 1.0;
/// Exponential rate the boom snaps IN when an obstacle appears — fast, so the camera
/// never lingers inside a wall.
const BOOM_IN_SMOOTH: f32 = 35.0;
/// Exponential rate the boom eases back OUT when the obstacle clears — slow, so rounding
/// a corner doesn't whip the camera back and reveal the world jarringly.
const BOOM_OUT_SMOOTH: f32 = 6.0;

// ── Free-fly debug camera ────────────────────────────────────────────────────
const FREE_FLY_SPEED: f32 = 34.0;
const FREE_FLY_FAST_MULT: f32 = 3.0;
const FREE_FLY_LOOK_SENS: f32 = 0.003;

// ── Voxel streaming / terrain LOD reach ──────────────────────────────────────
/// Voxel edge length of a chunk's interior, matching `bevy_voxel_world` (32³). The
/// streamed disc radius in world units is `render_distance_chunks * CHUNK_VOXELS`.
const CHUNK_VOXELS: u32 = 32;
/// Upper bound on the terrain-LOD band as a fraction of the streamed disc radius.
/// LOD level `n` begins at `n * band`, so the band MUST be smaller than the disc or
/// no chunk inside the disc ever coarsens (LOD silently dead). Capping the per-preset
/// `terrain_lod_distance` here guarantees the LOD-0→1 ring falls inside the disc even
/// if the render distance is set small. At the default render distance every preset
/// value is well under this cap, so it passes through unchanged — and the low presets
/// keep their intended earlier coarsening (the old fixed 150-unit floor overrode
/// Potato 90 / Low 120 up to 150, defeating their perf tuning).
const LOD_BAND_MAX_DISC_FRACTION: f32 = 0.9;

/// Marker for the single gameplay camera entity (orbit / free-fly).
#[derive(Component)]
pub struct OrbitCamera;

/// Runtime camera mode. `F` toggles free-fly in every world. Menus/debug flows can
/// also force free-fly off (e.g. on load) when needed.
#[derive(Resource, Default)]
pub struct CameraMode {
    mode: CameraViewMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CameraViewMode {
    /// Third-person orbit following the player (normal play).
    #[default]
    Orbit,
    /// Debug free-fly: the camera flies freely, the player idles.
    FreeFly,
}

impl CameraMode {
    fn is_orbit(&self) -> bool {
        self.mode == CameraViewMode::Orbit
    }

    pub fn is_free_fly(&self) -> bool {
        self.mode == CameraViewMode::FreeFly
    }

    pub fn set_free_fly(&mut self, enabled: bool) {
        self.mode = if enabled {
            CameraViewMode::FreeFly
        } else {
            CameraViewMode::Orbit
        };
    }

    fn toggle_free_fly(&mut self) {
        self.mode = if self.is_free_fly() {
            CameraViewMode::Orbit
        } else {
            CameraViewMode::FreeFly
        };
    }

    fn label(&self) -> &'static str {
        match self.mode {
            CameraViewMode::Orbit => "orbit",
            CameraViewMode::FreeFly => "free-fly",
        }
    }
}

/// Orbit-rig state: the yaw/pitch the mouse drives directly, plus the boom
/// distance that eases toward a scroll-set target.
///
/// `yaw`/`pitch` are written DIRECTLY from the mouse delta (no smoothing) so the
/// view feels responsive; only the boom `distance` eases toward `distance_target`
/// so the zoom glides instead of snapping.
#[derive(Component)]
pub struct CameraRig {
    yaw: f32,
    pitch: f32,
    /// Eased zoom distance (the user's scroll target glides into this).
    distance: f32,
    distance_target: f32,
    /// The ACTUAL boom length after collision (fast-in / slow-out), distinct from the
    /// eased zoom `distance`. Transient — not persisted; reset to `distance` on snap.
    boom: f32,
}

impl CameraRig {
    /// Current orbit yaw (radians) — read by save/load to persist the view.
    pub fn yaw(&self) -> f32 {
        self.yaw
    }

    /// Current orbit pitch (radians) — read by save/load to persist the view.
    pub fn pitch(&self) -> f32 {
        self.pitch
    }

    /// Current boom distance (world units) — read by save/load to persist the view.
    pub fn distance(&self) -> f32 {
        self.distance
    }

    /// Snap the whole rig to `yaw`/`pitch`/`distance` (and the easing target), so
    /// Load restores a saved view instantly rather than easing from the old one.
    /// `follow_player` repositions the eye from these on the next frame.
    pub fn snap_to(&mut self, yaw: f32, pitch: f32, distance: f32) {
        self.yaw = yaw;
        self.pitch = pitch.clamp(PITCH_MIN, PITCH_MAX);
        self.distance = distance.clamp(DISTANCE_MIN, DISTANCE_MAX);
        self.distance_target = self.distance;
        self.boom = self.distance;
    }
}

pub struct OrbitCameraPlugin;

impl Plugin for OrbitCameraPlugin {
    fn build(&self, app: &mut App) {
        // `QualitySettings` is a shared resource owned solely by `CorePlugin`; we
        // only read it here. The input chain is raw-input (Input phase);
        // `apply_quality_to_camera` reconfigures post-FX (Fx phase).
        app.init_resource::<CameraMode>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
                    toggle_camera_mode,
                    apply_camera_mode,
                    manage_cursor_grab,
                    orbit_input,
                    free_fly_input,
                    write_move_intent,
                )
                    .chain()
                    .in_set(GameSet::Input),
            )
            // Drive the voxel chunk render distance + terrain LOD (Input phase,
            // before streaming) so enough terrain loads to fill the view.
            .add_systems(Update, scale_voxel_render_distance.in_set(GameSet::Input))
            .add_systems(Update, apply_quality_to_camera.in_set(GameSet::Fx))
            // Follow in PostUpdate, AFTER avian's `TransformInterpolation` easing
            // (which runs in `RunFixedMainLoop`, before `Update`) has written the
            // smoothed player `Transform`, and BEFORE Bevy's transform
            // propagation so the camera's `GlobalTransform` is up to date this
            // frame. Reading the interpolated (not mid-step) transform is what
            // keeps the camera smooth at any frame rate.
            .add_systems(
                PostUpdate,
                follow_player.before(TransformSystems::Propagate),
            );
    }
}

/// Unit direction from the focus toward the eye for a given `yaw`/`pitch`. The
/// boom eye is `focus + orbit_dir(yaw, pitch) * distance`. Yaw 0 / pitch 0 places
/// the eye on +Z behind the focus, matching the WASD basis in [`write_move_intent`]
/// (forward = away from the camera = world -Z at yaw 0).
fn orbit_dir(yaw: f32, pitch: f32) -> Vec3 {
    let cp = pitch.cos();
    Vec3::new(yaw.sin() * cp, pitch.sin(), yaw.cos() * cp)
}

/// The point the camera looks at (and the screen-centre crosshair aims at): just above
/// the player (≈ head height) so the figure frames low and the reticle points ahead of
/// it. The camera pivots on the player here — NO forward offset — so orbiting feels
/// anchored to the character. Shared by `apply_camera_mode` (snap) and `follow_player`.
fn focus_point(player_translation: Vec3) -> Vec3 {
    player_translation + Vec3::Y * FOCUS_HEIGHT
}

/// Nearest distance the boom may extend along `dir` from `focus` before a solid voxel
/// OR a prop collider would clip the camera, capped at `desired`. Casts a small bundle
/// of parallel rays (centre + four offset by [`CAMERA_RADIUS`] perpendicular to the
/// boom) to approximate a sphere-cast — a single ray is too thin and threads through
/// voxel edges — against both the voxel terrain/builds and the physics world (props
/// are avian colliders, invisible to the voxel raycast), taking the nearest hit minus
/// [`BOOM_PADDING`], floored at [`BOOM_MIN`]. `exclude` is the player entity, whose
/// capsule sits at the focus and must not stop the boom.
fn boom_collision_distance(
    voxel_world: &VoxelWorld<MainWorld>,
    spatial: &SpatialQuery,
    exclude: Entity,
    focus: Vec3,
    dir: Vec3,
    desired: f32,
) -> f32 {
    let Ok(axis) = Dir3::new(dir) else {
        return desired;
    };
    // Two axes perpendicular to the boom to spread the bundle (the cheap sphere-cast).
    let right = dir.cross(Vec3::Y).normalize_or_zero();
    let up = if right == Vec3::ZERO {
        Vec3::X
    } else {
        dir.cross(right).normalize_or_zero()
    };
    let offsets = [
        Vec3::ZERO,
        right * CAMERA_RADIUS,
        -right * CAMERA_RADIUS,
        up * CAMERA_RADIUS,
        -up * CAMERA_RADIUS,
    ];
    // Props are the only colliders besides the player; excluding the player by entity
    // gives us "props only" without referencing `inf3d_physics::GameLayer` (the camera
    // is upstream of that crate).
    let prop_filter = SpatialQueryFilter::default().with_excluded_entities([exclude]);
    let mut limit = desired;
    for off in offsets {
        let origin = focus + off;
        let ray = Ray3d {
            origin,
            direction: axis,
        };
        if let Some(hit) =
            voxel_world.raycast(ray, &|(_coords, voxel)| matches!(voxel, WorldVoxel::Solid(_)))
        {
            limit = limit.min((hit.position - origin).length() - BOOM_PADDING);
        }
        if let Some(hit) = spatial.cast_ray(origin, axis, desired, true, &prop_filter) {
            limit = limit.min(hit.distance - BOOM_PADDING);
        }
    }
    limit.max(BOOM_MIN)
}

/// The perspective projection used for both orbit and free-fly.
fn perspective_projection() -> Projection {
    Projection::Perspective(PerspectiveProjection {
        fov: FOV,
        near: NEAR,
        far: FAR,
        ..default()
    })
}

/// Drive camera-dependent voxel streaming and terrain LOD from fixed perspective
/// reach values (the old orthographic zoom model is gone).
///
/// Terrain LOD is player/focus-centered and must keep LOD 0 over the visible
/// footprint so the first coarse ring stays off-screen; streaming keeps the
/// global render distance (the single `render_distance_chunks`; we never shrink it
/// and the presets never change it). Both stay conservative for perf — the
/// perspective view is shallower than the old iso rig.
fn scale_voxel_render_distance(
    mode: Res<CameraMode>,
    cam_q: Query<&Transform, With<OrbitCamera>>,
    player_q: Query<&Transform, (With<FollowTarget>, Without<OrbitCamera>)>,
    quality: Res<QualitySettings>,
    mut world: ResMut<MainWorld>,
) {
    // LOD focus tracks the camera while free-flying (so the detailed band follows
    // where you're looking), else the player.
    if mode.is_free_fly() {
        if let Ok(cam) = cam_q.single() {
            world.lod_focus_xz = Some(Vec2::new(cam.translation.x, cam.translation.z));
        }
    } else if let Ok(player) = player_q.single() {
        world.lod_focus_xz = Some(Vec2::new(player.translation.x, player.translation.z));
    }

    // LOD band width, capped to the streamed disc so a coarse ring always exists
    // inside it (a fixed floor could exceed a small disc → LOD never fires).
    let band = lod_band(quality.render_distance_chunks, quality.terrain_lod_distance);
    if (world.terrain_lod_distance - band).abs() >= 1.0 {
        world.terrain_lod_distance = band;
    }

    // Keep the streamed disc at the configured global radius — never shrink below it.
    if world.render_distance_chunks != quality.render_distance_chunks {
        world.render_distance_chunks = quality.render_distance_chunks;
    }
}

/// Width (world units) of each terrain-LOD band: the per-preset `terrain_lod_distance`
/// capped so the first coarse ring always falls inside the streamed disc (see
/// [`LOD_BAND_MAX_DISC_FRACTION`]). LOD level `n` begins at `n * band`.
fn lod_band(render_distance_chunks: u32, terrain_lod_distance: f32) -> f32 {
    let disc = (render_distance_chunks * CHUNK_VOXELS) as f32;
    terrain_lod_distance
        .min(disc * LOD_BAND_MAX_DISC_FRACTION)
        .max(1.0)
}

/// Bloom config used everywhere it's enabled (Startup + runtime re-apply).
fn bloom_component() -> Bloom {
    Bloom {
        intensity: 0.15,
        ..default()
    }
}

/// Depth-of-field config used everywhere it's enabled.
fn dof_component() -> DepthOfField {
    DepthOfField {
        focal_distance: 55.0,
        aperture_f_stops: 8.0,
        ..default()
    }
}

/// SSAO config used everywhere it's enabled. `ScreenSpaceAmbientOcclusion`
/// `#[require(DepthPrepass, NormalPrepass)]`, so both prepasses must also be on
/// the camera, and Bevy enforces `Msaa::Off` for SSAO (it logs an error and
/// skips otherwise — see bevy_pbr ssao/mod.rs). `Medium` is a sensible
/// quality/perf trade for the blocky terrain (High/Ultra cost more GPU).
fn ssao_component() -> ScreenSpaceAmbientOcclusion {
    ScreenSpaceAmbientOcclusion {
        quality_level: ScreenSpaceAmbientOcclusionQualityLevel::Medium,
        ..default()
    }
}

/// Motion-blur config used everywhere it's enabled. `MotionBlur`
/// `#[require(DepthPrepass, MotionVectorPrepass)]`, so both prepasses must also
/// be on the camera. Deliberately subtle: a short `shutter_angle` (the smear
/// length — 0 = off, 1 = a full 360° shutter) plus a single sample give a hint
/// of AAA smear, not a heavy blur. Lower `shutter_angle` further to soften more.
fn motion_blur_component() -> MotionBlur {
    MotionBlur {
        shutter_angle: 0.05,
        samples: 1,
    }
}

/// Filmic color grading on the camera's tonemapped output. The default tonemapper
/// is neutral, so the scene reads flat; a touch of midtone contrast + post
/// saturation gives it the graded, "AAA" pop without touching gameplay. Always on
/// (it folds into the existing tonemapping pass — effectively free). Deliberately
/// subtle — bump `post_saturation` for richer color, `midtones.contrast` for more
/// punch.
fn color_grading_component() -> ColorGrading {
    let mut grading = ColorGrading::default();
    // 1.0 = neutral. Post-tonemap saturation lift for richer color.
    grading.global.post_saturation = 1.08;
    // A little midtone contrast so the image isn't flat (1.0 = no change).
    grading.midtones.contrast = 1.06;
    grading
}

fn spawn_camera(mut commands: Commands, quality: Res<QualitySettings>) {
    let yaw = 0.0;
    let pitch = PITCH_DEFAULT;
    let distance = DISTANCE_DEFAULT;

    // Place the camera looking at the origin from the default boom; `follow_player`
    // re-snaps it to the real focus on the first frame.
    let eye = orbit_dir(yaw, pitch) * distance;

    let mut entity = commands.spawn((
        Camera3d::default(),
        perspective_projection(),
        Transform::from_translation(eye).looking_at(Vec3::ZERO, Vec3::Y),
        // Required: bevy_voxel_world streams chunks around this marked camera.
        VoxelWorldCamera::<MainWorld>::default(),
        OrbitCamera,
        CameraRig {
            yaw,
            pitch,
            distance,
            distance_target: distance,
            boom: distance,
        },
        // HDR is the color pipeline contract regardless of post-FX quality.
        Hdr,
        // Filmic grade on the tonemapped output (subtle contrast + saturation).
        color_grading_component(),
    ));

    if quality.bloom_enabled {
        entity.insert(bloom_component());
    }
    // SSAO + motion blur are controlled by real `QualitySettings` flags, even
    // though the project currently runs fixed-high until a settings UI returns.
    // The custom terrain material writes the depth/normal/motion-vector prepass,
    // so the voxel terrain participates in both.
    let ssao_enabled = quality.ssao_enabled;
    let motion_blur_enabled = quality.motion_blur_enabled;

    // DepthPrepass feeds Depth-of-Field, the water's depth-based deep/shallow
    // color blend + shoreline foam (bevy_water samples the depth texture to know
    // how deep the water is at each pixel), AND SSAO + motion blur. The custom
    // terrain material writes the prepass, so the voxel terrain participates —
    // enable it whenever ANY of those features wants it (OR of all consumers, so
    // it's never missing while one still needs it).
    if quality.dof_enabled || quality.water_enabled || ssao_enabled || motion_blur_enabled {
        entity.insert(DepthPrepass);
    }
    if quality.dof_enabled {
        // Subtle depth-of-field (cozy tilt-shift).
        entity.insert(dof_component());
    }
    if ssao_enabled {
        // SSAO additionally needs the normal prepass, and Bevy requires MSAA off
        // for SSAO (it errors and skips the effect otherwise). We add both here.
        // (`ScreenSpaceAmbientOcclusion` also `#[require]`s these, but we insert
        // them explicitly so `apply_quality_to_camera` can track/remove them.)
        entity.insert((ssao_component(), NormalPrepass, Msaa::Off));
    }
    if motion_blur_enabled {
        // Motion blur additionally needs the motion-vector prepass.
        entity.insert((motion_blur_component(), MotionVectorPrepass));
    }
}

/// `F` toggles the debug free-fly camera.
fn toggle_camera_mode(keys: Res<ButtonInput<KeyCode>>, mut mode: ResMut<CameraMode>) {
    if keys.just_pressed(KeyCode::KeyF) {
        mode.toggle_free_fly();
        info!("inf3d_camera: {} camera", mode.label());
    }
}

/// React to a mode change. The projection is always perspective now; on entering
/// Orbit we place the camera from the rig so it doesn't ease from the free-fly pose.
fn apply_camera_mode(
    mode: Res<CameraMode>,
    player_q: Query<&Transform, (With<FollowTarget>, Without<OrbitCamera>)>,
    mut cam_q: Query<(&mut Transform, &mut Projection, &mut CameraRig), With<OrbitCamera>>,
) {
    if !mode.is_changed() {
        return;
    }
    let Ok((mut cam_t, mut projection, mut rig)) = cam_q.single_mut() else {
        return;
    };
    // Keep the projection perspective in both modes (it may have been replaced).
    if !matches!(&*projection, Projection::Perspective(_)) {
        *projection = perspective_projection();
    }
    // On entering Orbit, snap the eye onto the boom so the view doesn't lerp from
    // wherever free-fly left the camera.
    if mode.is_orbit() {
        if let Ok(player) = player_q.single() {
            // Reset the collision boom to the full zoom so it doesn't ease in from a
            // stale (free-fly) length, then snap the eye onto it.
            rig.boom = rig.distance;
            let focus = focus_point(player.translation);
            cam_t.translation = focus + orbit_dir(rig.yaw, rig.pitch) * rig.boom;
            *cam_t = cam_t.looking_at(focus, Vec3::Y);
        }
    }
}

/// Re-apply the post-FX component set whenever `QualitySettings` changes,
/// adding or stripping `Bloom`, `DepthPrepass`, `DepthOfField`, SSAO (+ its
/// `NormalPrepass`/`Msaa::Off`), and motion blur (+ its `MotionVectorPrepass`)
/// to match the new settings. Skips re-inserting when the component is already
/// present (avoids GPU churn). There is no fog component: atmospheric fog was
/// removed (see inf3d_render::fog, now just the horizon clear color).
fn apply_quality_to_camera(
    mut commands: Commands,
    quality: Res<QualitySettings>,
    cam_q: Query<
        (
            Entity,
            Has<Bloom>,
            Has<DepthPrepass>,
            Has<DepthOfField>,
            Has<ScreenSpaceAmbientOcclusion>,
            Has<NormalPrepass>,
            Has<MotionBlur>,
            Has<MotionVectorPrepass>,
        ),
        With<OrbitCamera>,
    >,
) {
    if !quality.is_changed() {
        return;
    }
    // Empty query is fine — settings may flip before the camera spawns.
    let Ok((entity, has_bloom, has_depth, has_dof, has_ssao, has_normal, has_mb, has_motion_vec)) =
        cam_q.single()
    else {
        return;
    };

    // SSAO + motion blur use their own real settings flags (see spawn_camera).
    let ssao_enabled = quality.ssao_enabled;
    let motion_blur_enabled = quality.motion_blur_enabled;

    let mut e = commands.entity(entity);

    if quality.bloom_enabled {
        if !has_bloom {
            e.insert(bloom_component());
        }
    } else if has_bloom {
        e.remove::<Bloom>();
    }

    // DepthPrepass is shared by Depth-of-Field, the water's depth-based color
    // blend + shoreline foam, SSAO, and motion blur (the terrain writes the
    // prepass now). Compute "needs depth" as the OR of every consumer so we
    // never strip it while one of them still needs it.
    let need_depth =
        quality.dof_enabled || quality.water_enabled || ssao_enabled || motion_blur_enabled;
    if need_depth {
        if !has_depth {
            e.insert(DepthPrepass);
        }
    } else if has_depth {
        e.remove::<DepthPrepass>();
    }

    if quality.dof_enabled {
        if !has_dof {
            e.insert(dof_component());
        }
    } else if has_dof {
        e.remove::<DepthOfField>();
    }

    // SSAO: needs NormalPrepass + Msaa::Off. We track NormalPrepass separately so
    // it's only stripped when SSAO is off (nothing else here uses it). For MSAA,
    // the simplest correct approach is to flip the camera's `Msaa` component to
    // match SSAO: `Off` while SSAO is on (Bevy requires it), and back to the
    // default (Sample4) when SSAO is off so we regain antialiasing. Tradeoff:
    // toggling presets re-creates MSAA targets, but only on the preset change
    // (from the settings menu), not per frame.
    if ssao_enabled {
        if !has_ssao {
            e.insert(ssao_component());
        }
        if !has_normal {
            e.insert(NormalPrepass);
        }
        e.insert(Msaa::Off);
    } else {
        if has_ssao {
            e.remove::<ScreenSpaceAmbientOcclusion>();
        }
        if has_normal {
            e.remove::<NormalPrepass>();
        }
        // Restore antialiasing now that SSAO no longer forbids MSAA.
        e.insert(Msaa::default());
    }

    // Motion blur: needs MotionVectorPrepass. Track it separately so it's only
    // stripped when motion blur is off (nothing else here uses it).
    if motion_blur_enabled {
        if !has_mb {
            e.insert(motion_blur_component());
        }
        if !has_motion_vec {
            e.insert(MotionVectorPrepass);
        }
    } else {
        if has_mb {
            e.remove::<MotionBlur>();
        }
        if has_motion_vec {
            e.remove::<MotionVectorPrepass>();
        }
    }
}

/// Orbit-mode mouse-look + scroll-zoom. Mouse motion drives yaw/pitch directly;
/// the scroll wheel retargets the boom distance (eased in `follow_player`).
fn orbit_input(
    mode: Res<CameraMode>,
    keys: Res<ButtonInput<KeyCode>>,
    mut scroll: MessageReader<MouseWheel>,
    mut motion: MessageReader<MouseMotion>,
    mut rig_q: Query<&mut CameraRig>,
) {
    if !mode.is_orbit() {
        // Drain the scroll reader so stale wheel events don't apply when toggling
        // back to orbit, but leave mouse motion for the free-fly reader below.
        for _ in scroll.read() {}
        return;
    }
    // While the cursor is freed for UI (Alt held), suspend mouse-look + zoom and drain
    // the buffered events so moving the pointer to a button never spins the camera.
    if keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight) {
        motion.clear();
        scroll.clear();
        return;
    }
    let Ok(mut rig) = rig_q.single_mut() else {
        return;
    };

    let mut delta = Vec2::ZERO;
    for ev in motion.read() {
        delta += ev.delta;
    }
    if delta.length_squared() > 0.0 {
        rig.yaw -= delta.x * LOOK_SENS;
        // `pitch` is the camera's ELEVATION above the focus (higher pitch = the eye
        // rises and looks DOWN), which is geometrically opposite a first-person view
        // pitch. So ADD `delta.y` here: mouse up (`delta.y < 0`) LOWERS the camera to
        // look up — the standard, non-inverted feel.
        rig.pitch = (rig.pitch + delta.y * LOOK_SENS).clamp(PITCH_MIN, PITCH_MAX);
    }

    for ev in scroll.read() {
        // Scroll up (positive y) zooms in -> shorter boom.
        rig.distance_target =
            (rig.distance_target - ev.y * ZOOM_SPEED).clamp(DISTANCE_MIN, DISTANCE_MAX);
    }
}

/// Debug free-fly: WASD + Space/Ctrl flies the camera, mouse-look applies directly
/// (the cursor is captured in play, so no mouse-button gate is needed).
fn free_fly_input(
    mode: Res<CameraMode>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut motion: MessageReader<MouseMotion>,
    mut cam_q: Query<&mut Transform, With<OrbitCamera>>,
) {
    if !mode.is_free_fly() {
        return;
    }
    let Ok(mut t) = cam_q.single_mut() else {
        return;
    };

    // Hold Alt to free the cursor for UI — suspend free-fly mouse-look while it's held.
    let ui_cursor = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);

    let mut delta = Vec2::ZERO;
    for ev in motion.read() {
        delta += ev.delta;
    }
    if !ui_cursor && delta.length_squared() > 0.0 {
        let (mut yaw, mut pitch, _) = t.rotation.to_euler(EulerRot::YXZ);
        yaw -= delta.x * FREE_FLY_LOOK_SENS;
        pitch = (pitch - delta.y * FREE_FLY_LOOK_SENS).clamp(-1.5, 1.5);
        t.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
    }

    let mut dir = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        dir += t.forward().as_vec3();
    }
    if keys.pressed(KeyCode::KeyS) {
        dir -= t.forward().as_vec3();
    }
    if keys.pressed(KeyCode::KeyD) {
        dir += t.right().as_vec3();
    }
    if keys.pressed(KeyCode::KeyA) {
        dir -= t.right().as_vec3();
    }
    if keys.pressed(KeyCode::Space) {
        dir += Vec3::Y;
    }
    if keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight) {
        dir -= Vec3::Y;
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
        FREE_FLY_SPEED * FREE_FLY_FAST_MULT
    } else {
        FREE_FLY_SPEED
    };
    t.translation += dir.normalize_or_zero() * speed * time.delta_secs();
}

/// Lock + hide the OS cursor while in play so mouse-look (orbit AND free-fly) has
/// unlimited travel and the pointer never drifts off-window or onto UI. Releases it
/// whenever the game is paused / not in play, so the menus stay clickable. Runs
/// every frame but only writes the window on an actual change, so it's effectively
/// free.
fn manage_cursor_grab(
    app_state: Res<State<AppState>>,
    pause: Res<State<Pause>>,
    keys: Res<ButtonInput<KeyCode>>,
    // In Bevy 0.18 cursor state is its own component on the primary window entity,
    // not a field on `Window`.
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    // Hold Left/Right Alt to temporarily FREE the cursor (to click the Walk/Build
    // buttons, the material picker, etc.) without leaving play. Mouse-look + WASD are
    // suspended while it's held (see `orbit_input` / `write_move_intent`), so moving the
    // pointer to a button never spins the camera or walks the player.
    let ui_cursor = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let lock =
        *app_state.get() == AppState::InGame && *pause.get() == Pause::Running && !ui_cursor;
    let Ok(mut cursor) = cursor.single_mut() else {
        return;
    };
    let (want_grab, want_visible) = if lock {
        (CursorGrabMode::Locked, false)
    } else {
        (CursorGrabMode::None, true)
    };
    if cursor.grab_mode != want_grab {
        cursor.grab_mode = want_grab;
    }
    if cursor.visible != want_visible {
        cursor.visible = want_visible;
    }
}

/// Translate WASD into a camera-relative [`MoveIntent`] in Orbit mode (the player
/// idles in free-fly).
fn write_move_intent(
    mode: Res<CameraMode>,
    keys: Res<ButtonInput<KeyCode>>,
    rig_q: Query<&CameraRig, With<OrbitCamera>>,
    mut intent: ResMut<MoveIntent>,
) {
    // Not driving the player when free-flying, OR while the cursor is freed for UI
    // (Alt held) so you don't walk into the scene while clicking a button.
    if !mode.is_orbit() || keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight) {
        if intent.active {
            *intent = MoveIntent::default();
        }
        return;
    }
    let Ok(rig) = rig_q.single() else {
        *intent = MoveIntent::default();
        return;
    };

    // Horizontal camera-relative basis keyed off the orbit yaw. `forward` points
    // AWAY from the camera (into the screen): at yaw 0 the eye sits on +Z behind the
    // focus (see `orbit_dir`), so forward is world -Z — matching Bevy's default
    // camera-looks-down-(-Z) convention. `right` is the in-plane perpendicular.
    let forward = Vec3::new(-rig.yaw.sin(), 0.0, -rig.yaw.cos());
    let right = Vec3::new(rig.yaw.cos(), 0.0, -rig.yaw.sin());
    let mut direction = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        direction += forward;
    }
    if keys.pressed(KeyCode::KeyS) {
        direction -= forward;
    }
    if keys.pressed(KeyCode::KeyD) {
        direction += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        direction -= right;
    }

    intent.active = true;
    intent.direction = direction.normalize_or_zero();
    intent.jump = keys.pressed(KeyCode::Space);
    intent.sprint = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
}

/// Chase the player on the orbit boom: ease the zoom distance, sphere-cast the boom for
/// terrain/builds (fast-in / slow-out so the camera never clips in or whips back out),
/// then RIGIDLY place the eye from the interpolated player (no position lag) and look at
/// the focus. Skipped entirely in free-fly (the camera is flown manually there).
fn follow_player(
    mode: Res<CameraMode>,
    time: Res<Time>,
    player_q: Query<(Entity, &Transform), (With<FollowTarget>, Without<OrbitCamera>)>,
    mut cam_q: Query<(&mut Transform, &mut CameraRig), With<OrbitCamera>>,
    voxel_world: VoxelWorld<MainWorld>,
    spatial: SpatialQuery,
) {
    if mode.is_free_fly() {
        return;
    }
    let Ok((player_entity, player)) = player_q.single() else {
        return;
    };
    let Ok((mut cam_t, mut rig)) = cam_q.single_mut() else {
        return;
    };

    let dt = time.delta_secs();

    // Zoom: ease the user's scroll target into the live distance. This is the only
    // thing smoothed besides collision — the camera POSITION is rigid below.
    let k_zoom = 1.0 - (-DISTANCE_SMOOTH * dt).exp();
    rig.distance = lerp(rig.distance, rig.distance_target, k_zoom);

    let focus = focus_point(player.translation);
    let dir = orbit_dir(rig.yaw, rig.pitch);

    // Boom collision (sphere-cast-ish): how far the camera may sit before a wall clips
    // it, capped at the eased zoom. Fast-IN (snap toward a closer limit so the camera
    // never sits inside geometry), slow-OUT (ease back when it clears so rounding a
    // corner doesn't whip the view).
    let limit =
        boom_collision_distance(&voxel_world, &spatial, player_entity, focus, dir, rig.distance);
    let rate = if limit < rig.boom {
        BOOM_IN_SMOOTH
    } else {
        BOOM_OUT_SMOOTH
    };
    let k = 1.0 - (-rate * dt).exp();
    rig.boom = lerp(rig.boom, limit, k);

    // RIGID horizontal follow: recompute the eye from the (already interpolated, smooth)
    // player every frame — no position lerp — so the follow has zero lag.
    cam_t.translation = focus + dir * rig.boom;
    *cam_t = cam_t.looking_at(focus, Vec3::Y);
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Terrain LOD must actually fire: for every preset the LOD band has to be
    /// strictly smaller than the streamed disc radius, or LOD level 1 starts beyond
    /// the disc edge and no chunk ever coarsens. Guards the regression where a fixed
    /// reach floor exceeded the disc (it had silently killed LOD on the low presets
    /// if the render distance was ever lowered).
    #[test]
    fn lod_band_fires_inside_disc_for_every_preset() {
        use inf3d_core::{QualityPreset, QualitySettings};
        for preset in QualityPreset::ALL {
            let mut q = QualitySettings::default();
            preset.apply(&mut q);
            let disc = (q.render_distance_chunks * CHUNK_VOXELS) as f32;
            let band = lod_band(q.render_distance_chunks, q.terrain_lod_distance);
            assert!(
                band < disc,
                "{:?}: LOD band {band} must be < disc radius {disc} so LOD fires",
                preset,
            );
        }
    }

    /// `orbit_dir` is a UNIT vector for any yaw/pitch (it parameterizes a sphere),
    /// so `eye = focus + dir * distance` is always exactly `distance` from the focus.
    #[test]
    fn orbit_dir_is_unit_length() {
        for &(yaw, pitch) in &[
            (0.0_f32, 0.0_f32),
            (1.3, 0.5),
            (-2.1, PITCH_MAX),
            (3.0, PITCH_MIN),
            (std::f32::consts::PI, PITCH_DEFAULT),
        ] {
            let d = orbit_dir(yaw, pitch);
            assert!(
                (d.length() - 1.0).abs() < 1e-5,
                "orbit_dir({yaw},{pitch}) length {} != 1",
                d.length()
            );
        }
    }

    /// At yaw 0 / pitch 0 the eye sits straight behind the focus on +Z, and the
    /// WASD `forward` points the opposite way (into the screen, world -Z). This pins
    /// the "W goes away from the camera / into the screen" convention.
    #[test]
    fn yaw_zero_eye_behind_focus_and_forward_into_screen() {
        let dir = orbit_dir(0.0, 0.0);
        assert!(
            (dir - Vec3::Z).length() < 1e-5,
            "eye should sit on +Z behind the focus at yaw 0, got {dir:?}"
        );
        // The WASD forward (from `write_move_intent`) at yaw 0.
        let forward = Vec3::new(-0.0_f32.sin(), 0.0, -0.0_f32.cos());
        assert!(
            (forward - Vec3::NEG_Z).length() < 1e-5,
            "W should drive into the screen (world -Z) at yaw 0, got {forward:?}"
        );
        // Forward is the horizontal projection of pointing FROM the eye TOWARD the
        // focus (i.e. -dir flattened), so moving W walks away from the camera.
        let toward_focus = Vec3::new(-dir.x, 0.0, -dir.z);
        assert!((forward - toward_focus).length() < 1e-5);
    }

    /// The WASD basis is orthonormal in the XZ plane for any yaw, so diagonal input
    /// has consistent magnitude and W/D never skew. (Y is always 0 — horizontal move.)
    #[test]
    fn wasd_basis_is_orthonormal() {
        for &yaw in &[0.0_f32, 0.7, -1.9, 3.1, std::f32::consts::FRAC_PI_2] {
            let forward = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
            let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
            assert!((forward.length() - 1.0).abs() < 1e-5, "forward not unit");
            assert!((right.length() - 1.0).abs() < 1e-5, "right not unit");
            assert!(forward.dot(right).abs() < 1e-5, "forward·right != 0");
            assert!(forward.y.abs() < 1e-6 && right.y.abs() < 1e-6, "basis not horizontal");
        }
    }

    /// `right` is the camera-relative strafe axis: at yaw 0 pressing D moves +X
    /// (screen-right for a camera looking down -Z), and forward × right points down
    /// (consistent right-handedness) so A/D aren't mirrored.
    #[test]
    fn right_axis_points_screen_right_at_yaw_zero() {
        let yaw = 0.0_f32;
        let forward = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
        let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
        assert!((right - Vec3::X).length() < 1e-5, "D should move +X at yaw 0");
        // forward (-Z) × right (+X) = +Z×... check handedness: (-Z)×(+X) = -(Z×X)=-Y.
        let cross = forward.cross(right);
        assert!(cross.y < 0.0, "forward × right should point down (right-handed)");
    }
}
