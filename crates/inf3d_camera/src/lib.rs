//! Orthographic isometric follow camera (Diablo-style 3/4 view) with springy
//! follow, scroll-wheel zoom, and Q/E orbit.

use bevy::camera::{OrthographicProjection, PerspectiveProjection, Projection, ScalingMode};
use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, NormalPrepass};
use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel};
use bevy::post_process::bloom::Bloom;
use bevy::post_process::dof::DepthOfField;
use bevy::post_process::motion_blur::MotionBlur;
use bevy::prelude::*;
use bevy::render::view::{ColorGrading, Hdr, Msaa};
use bevy_voxel_world::prelude::*;

use inf3d_core::{FollowTarget, FpsMoveIntent, GameSet, QualitySettings};
use inf3d_world::MainWorld;

/// Horizontal (XZ-plane) distance from the player to the camera.
///
/// Keep this paired with [`ORBIT_HEIGHT`] to preserve the same ~42° iso pitch.
/// The old rig was only ~36 units above the player, but terrain can rise far
/// higher than that in the visible area, putting mountains between/through the
/// camera at max zoom. A taller rig prevents high terrain from clipping through
/// the camera while keeping the same isometric look.
const ORBIT_RADIUS: f32 = 110.0;
/// Camera height above the player.
const ORBIT_HEIGHT: f32 = 100.0;
/// Orthographic near plane. Negative on purpose: with a tall heightfield and an
/// overhead iso camera, nearby/high terrain can sit very close to or slightly
/// behind the eye plane during max zoom/orbit. A negative near plane prevents the
/// camera from slicing chunks when peaks get close.
const ORTHO_NEAR: f32 = -500.0;
/// Orthographic far plane, large enough for the raised camera + max zoom footprint.
const ORTHO_FAR: f32 = 1500.0;
/// Default vertical view size (smaller = more zoomed in).
const ZOOM_DEFAULT: f32 = 44.0;
const ZOOM_MIN: f32 = 14.0;
const ZOOM_MAX: f32 = 90.0;
/// World-units of zoom change per scroll notch.
const ZOOM_SPEED: f32 = 4.0;
/// Orbit speed (radians/sec) for Q/E.
const ORBIT_SPEED: f32 = 1.8;
/// Middle-mouse drag orbit sensitivity (radians per pixel of horizontal motion).
const DRAG_SENS: f32 = 0.008;
/// Exponential smoothing rate for follow/zoom/orbit. Higher = snappier.
const SMOOTH: f32 = 12.0;
const FREE_FLY_SPEED: f32 = 34.0;
const FREE_FLY_FAST_MULT: f32 = 3.0;
const FREE_FLY_LOOK_SENS: f32 = 0.003;
const FPS_LOOK_SENS: f32 = 0.0025;
const FPS_EYE_HEIGHT: f32 = 0.75;

// ── Zoom-scaled chunk render distance (the "half terrain culled" fix) ────────
// Voxel terrain only streams within `MainWorld::render_distance_chunks` of the
// (camera-centered) disc. When the view zooms out past it, the far water plane
// shows through where terrain isn't loaded and reads as a hard "culled" ocean
// edge — worse on one side because the disc is centered on the camera, which sits
// `ORBIT_RADIUS` off the player. The vendored voxel spawner re-reads the distance
// every frame, so `scale_voxel_render_distance` drives it from zoom: the preset
// value stays the FLOOR (normal/zoomed-in play unchanged), and zooming out grows
// the disc to keep the view filled, capped to bound the cost.
/// Extra chunk rings beyond the fixed base distance. This is a safety cap, not
/// the main max-zoom knob; with the fixed high base of 10, a cap of 5 still allows
/// `rd_dyn=15` if the reach formula asks for it.
const RD_ZOOM_HEADROOM: u32 = 5;
/// Ground reach (world units) per unit of orthographic `viewport_height` along
/// the camera's far (toward-horizon) direction, where iso foreshortening makes the
/// view reach furthest and the disc edge shows first. Empirical. The previous 4.0
/// pushed max zoom to `rd_dyn=15` (~3400 chunks in the monitor log). With the
/// player-centered LOD0 footprint fix below, 3.0 still covers max zoom while
/// keeping the loaded disc closer to `rd_dyn=12`.
const RD_ZOOM_REACH: f32 = 3.0;
/// Chunk edge length in world units (matches inf3d_world's 32-voxel chunks).
const CHUNK_WORLD_SIZE: f32 = 32.0;
/// Min zoom change (world units) before the disc is re-evaluated — hysteresis so
/// steady / micro-easing zoom doesn't thrash chunk spawn/despawn. Keep this small:
/// an 8-unit band could miss a one-chunk threshold near max zoom and leave the
/// far edge unloaded until another large zoom delta happened.
const RD_ZOOM_DEADBAND: f32 = 2.0;
/// Conservative world-XZ reach per orthographic vertical unit for this fixed iso
/// pitch. It intentionally overestimates the true footprint so LOD 1 begins just
/// off-screen even on wide windows and uneven heightfields.
const TERRAIN_LOD_VIEW_REACH: f32 = 2.0;
/// Extra full-detail world units outside the visible footprint. This hides chunk
/// center quantization, remesh latency, height variation, and edge scrolling.
const TERRAIN_LOD_SCREEN_MARGIN: f32 = 64.0;

#[derive(Component)]
pub struct IsoCamera;

/// Runtime camera mode. `F` toggles free-fly in every world; `G` toggles a
/// player-mounted FPS camera. The menu can also force free-fly for visual/debug
/// worlds like Cyberpunk City.
#[derive(Resource, Default)]
pub struct CameraMode {
    mode: CameraViewMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CameraViewMode {
    #[default]
    Iso,
    FreeFly,
    Fps,
}

impl CameraMode {
    fn is_iso(&self) -> bool {
        self.mode == CameraViewMode::Iso
    }

    pub fn is_free_fly(&self) -> bool {
        self.mode == CameraViewMode::FreeFly
    }

    fn is_fps(&self) -> bool {
        self.mode == CameraViewMode::Fps
    }

    pub fn set_free_fly(&mut self, enabled: bool) {
        self.mode = if enabled {
            CameraViewMode::FreeFly
        } else {
            CameraViewMode::Iso
        };
    }

    fn toggle_free_fly(&mut self) {
        self.mode = if self.is_free_fly() {
            CameraViewMode::Iso
        } else {
            CameraViewMode::FreeFly
        };
    }

    fn toggle_fps(&mut self) {
        self.mode = if self.is_fps() {
            CameraViewMode::Iso
        } else {
            CameraViewMode::Fps
        };
    }

    fn label(&self) -> &'static str {
        match self.mode {
            CameraViewMode::Iso => "isometric",
            CameraViewMode::FreeFly => "free-fly",
            CameraViewMode::Fps => "FPS",
        }
    }
}

/// Smoothed camera state: orbit yaw around the player and orthographic zoom,
/// each easing toward a target the input system sets.
#[derive(Component)]
pub struct CameraRig {
    yaw: f32,
    yaw_target: f32,
    zoom: f32,
    zoom_target: f32,
    fps_yaw: f32,
    fps_pitch: f32,
}

impl CameraRig {
    /// Current orbit yaw (radians) — read by save/load to persist the view.
    pub fn yaw(&self) -> f32 {
        self.yaw
    }

    /// Current orthographic zoom (vertical view height) — read by save/load.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Snap BOTH the live value and its easing target to `yaw`/`zoom`, so Load
    /// restores a saved camera instantly rather than easing from the old view.
    /// `camera_input` re-applies `zoom` to the projection on the next in-game
    /// frame; `follow_player` (PostUpdate, ungated) repositions from `yaw`.
    pub fn snap_to(&mut self, yaw: f32, zoom: f32) {
        self.yaw = yaw;
        self.yaw_target = yaw;
        self.zoom = zoom;
        self.zoom_target = zoom;
    }
}

pub struct IsoCameraPlugin;

impl Plugin for IsoCameraPlugin {
    fn build(&self, app: &mut App) {
        // `QualitySettings` is a shared resource owned solely by `CorePlugin`; we
        // only read it here. `camera_input` is raw-input (Input phase);
        // `apply_quality_to_camera` reconfigures post-FX (Fx phase).
        app.init_resource::<CameraMode>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
                    toggle_camera_mode,
                    apply_camera_mode,
                    camera_input,
                    free_fly_input,
                    fps_input,
                    write_fps_move_intent,
                )
                    .chain()
                    .in_set(GameSet::Input),
            )
            // Drive the voxel chunk render distance from zoom (Input phase, before
            // streaming) so a zoomed-out view loads enough terrain to fill it.
            .add_systems(Update, scale_voxel_render_distance.in_set(GameSet::Input))
            .add_systems(Update, apply_quality_to_camera.in_set(GameSet::Fx))
            // Follow in PostUpdate, AFTER avian's `TransformInterpolation` easing
            // (which runs in `RunFixedMainLoop`, before `Update`) has written the
            // smoothed player `Transform`, and BEFORE Bevy's transform
            // propagation so the camera's `GlobalTransform` is up to date this
            // frame. Reading the interpolated (not mid-step) transform is what
            // keeps the camera smooth at any zoom.
            .add_systems(
                PostUpdate,
                follow_player.before(TransformSystems::Propagate),
            );
    }
}

/// Camera offset from the player for a given orbit yaw.
fn orbit_offset(yaw: f32) -> Vec3 {
    Vec3::new(
        yaw.sin() * ORBIT_RADIUS,
        ORBIT_HEIGHT,
        yaw.cos() * ORBIT_RADIUS,
    )
}

fn fps_eye(player_translation: Vec3) -> Vec3 {
    player_translation + Vec3::Y * FPS_EYE_HEIGHT
}

/// Drive camera-dependent voxel streaming and terrain LOD.
///
/// Streaming still needs a camera-centered disc large enough to cover max zoom.
/// Terrain LOD, however, is player/focus-centered and must keep LOD 0 over the
/// whole visible orthographic footprint; otherwise the first coarse ring appears
/// at the screen edge. Settings provide floors only — the current camera footprint
/// is the source of truth.
fn scale_voxel_render_distance(
    mode: Res<CameraMode>,
    rig: Query<&CameraRig, With<IsoCamera>>,
    cam_q: Query<&Transform, With<IsoCamera>>,
    player_q: Query<&Transform, (With<FollowTarget>, Without<IsoCamera>)>,
    quality: Res<QualitySettings>,
    mut world: ResMut<MainWorld>,
    mut last_zoom: Local<f32>,
) {
    let Ok(rig) = rig.single() else {
        return;
    };
    if mode.is_free_fly() {
        if let Ok(cam) = cam_q.single() {
            world.lod_focus_xz = Some(Vec2::new(cam.translation.x, cam.translation.z));
        }
    } else if let Ok(player) = player_q.single() {
        world.lod_focus_xz = Some(Vec2::new(player.translation.x, player.translation.z));
    }

    // Use the larger of current/target zoom so scrolling out immediately requests
    // enough coverage before the smoothed camera visually arrives there.
    let effective_zoom = rig.zoom.max(rig.zoom_target);

    let lod_band = (effective_zoom * TERRAIN_LOD_VIEW_REACH + TERRAIN_LOD_SCREEN_MARGIN)
        .max(quality.terrain_lod_distance);
    if (world.terrain_lod_distance - lod_band).abs() >= 1.0 {
        world.terrain_lod_distance = lod_band;
    }

    // Re-evaluate streaming only after a meaningful zoom change, so the zoom's
    // asymptotic easing settling on a value doesn't churn chunk spawn/despawn every frame.
    if (effective_zoom - *last_zoom).abs() < RD_ZOOM_DEADBAND {
        return;
    }
    *last_zoom = effective_zoom;

    // The fixed high distance is the floor; grow only as much as the current zoom
    // needs. Reach is the far-direction ground span plus the camera's horizontal
    // offset from the player (the disc is camera-centered), converted to chunks.
    let floor = quality.render_distance_chunks;
    let ceil = floor + RD_ZOOM_HEADROOM;
    let reach = effective_zoom * RD_ZOOM_REACH + ORBIT_RADIUS;
    let target = ((reach / CHUNK_WORLD_SIZE).ceil() as u32).clamp(floor, ceil);
    if target != world.render_distance_chunks {
        world.render_distance_chunks = target;
    }
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
    let yaw = std::f32::consts::FRAC_PI_4; // classic 45° iso

    // Orthographic projection gives the flat, parallel-line iso look. FixedVertical
    // keeps a constant world-height slice on screen regardless of aspect ratio.
    let projection = Projection::Orthographic(OrthographicProjection {
        scaling_mode: ScalingMode::FixedVertical {
            viewport_height: ZOOM_DEFAULT,
        },
        near: ORTHO_NEAR,
        far: ORTHO_FAR,
        ..OrthographicProjection::default_3d()
    });

    let mut entity = commands.spawn((
        Camera3d::default(),
        projection,
        Transform::from_translation(orbit_offset(yaw)).looking_at(Vec3::ZERO, Vec3::Y),
        // Required: bevy_voxel_world streams chunks around this marked camera.
        VoxelWorldCamera::<MainWorld>::default(),
        IsoCamera,
        CameraRig {
            yaw,
            yaw_target: yaw,
            zoom: ZOOM_DEFAULT,
            zoom_target: ZOOM_DEFAULT,
            fps_yaw: 0.0,
            fps_pitch: 0.0,
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

fn toggle_camera_mode(keys: Res<ButtonInput<KeyCode>>, mut mode: ResMut<CameraMode>) {
    if keys.just_pressed(KeyCode::KeyF) {
        mode.toggle_free_fly();
        info!("inf3d_camera: {} camera", mode.label());
    }
    if keys.just_pressed(KeyCode::KeyG) {
        mode.toggle_fps();
        info!("inf3d_camera: {} camera", mode.label());
    }
}

fn apply_camera_mode(
    mode: Res<CameraMode>,
    player_q: Query<&Transform, (With<FollowTarget>, Without<IsoCamera>)>,
    mut cam_q: Query<(&mut Transform, &mut Projection, &mut CameraRig), With<IsoCamera>>,
) {
    if !mode.is_changed() {
        return;
    }
    let Ok((mut cam_t, mut projection, mut rig)) = cam_q.single_mut() else {
        return;
    };
    if mode.is_free_fly() || mode.is_fps() {
        *projection = Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_3,
            near: 0.1,
            far: ORTHO_FAR,
            ..default()
        });
        if mode.is_fps() {
            if let Ok(player) = player_q.single() {
                let (yaw, _, _) = player.rotation.to_euler(EulerRot::YXZ);
                rig.fps_yaw = yaw;
                rig.fps_pitch = 0.0;
                cam_t.translation = fps_eye(player.translation);
                cam_t.rotation = Quat::from_euler(EulerRot::YXZ, rig.fps_yaw, rig.fps_pitch, 0.0);
            }
        }
    } else {
        *projection = Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: rig.zoom,
            },
            near: ORTHO_NEAR,
            far: ORTHO_FAR,
            ..OrthographicProjection::default_3d()
        });
        if let Ok(player) = player_q.single() {
            cam_t.translation = player.translation + orbit_offset(rig.yaw);
            *cam_t = cam_t.looking_at(player.translation, Vec3::Y);
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
        With<IsoCamera>,
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
    // toggling presets re-creates MSAA targets, but only on the F2 change, not
    // per frame.
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

/// Scroll wheel zooms; Q/E orbit the view around the player.
fn camera_input(
    mode: Res<CameraMode>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mut scroll: MessageReader<MouseWheel>,
    mut motion: MessageReader<MouseMotion>,
    mut rig_q: Query<&mut CameraRig>,
) {
    if !mode.is_iso() {
        // Drain scroll for the iso zoom reader so stale wheel events do not apply
        // when toggling back, but leave mouse motion for debug/FPS readers below.
        for _ in scroll.read() {}
        return;
    }
    let Ok(mut rig) = rig_q.single_mut() else {
        return;
    };

    for ev in scroll.read() {
        // Scroll up (positive y) zooms in -> smaller viewport height.
        rig.zoom_target = (rig.zoom_target - ev.y * ZOOM_SPEED).clamp(ZOOM_MIN, ZOOM_MAX);
    }

    let dt = time.delta_secs();
    if keys.pressed(KeyCode::KeyQ) {
        rig.yaw_target -= ORBIT_SPEED * dt;
    }
    if keys.pressed(KeyCode::KeyE) {
        rig.yaw_target += ORBIT_SPEED * dt;
    }

    // Middle-mouse drag orbits horizontally only (pitch/height stay fixed to
    // preserve the iso view).
    if mouse_buttons.pressed(MouseButton::Middle) {
        let mut dx = 0.0;
        for ev in motion.read() {
            dx += ev.delta.x;
        }
        rig.yaw_target -= dx * DRAG_SENS;
    } else {
        // Drop buffered motion so the next drag doesn't jump.
        motion.clear();
    }
}

fn free_fly_input(
    mode: Res<CameraMode>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut cam_q: Query<&mut Transform, With<IsoCamera>>,
) {
    if !mode.is_free_fly() {
        return;
    }
    let Ok(mut t) = cam_q.single_mut() else {
        return;
    };

    if mouse_buttons.pressed(MouseButton::Right) {
        let mut delta = Vec2::ZERO;
        for ev in motion.read() {
            delta += ev.delta;
        }
        if delta.length_squared() > 0.0 {
            let (mut yaw, mut pitch, _) = t.rotation.to_euler(EulerRot::YXZ);
            yaw -= delta.x * FREE_FLY_LOOK_SENS;
            pitch = (pitch - delta.y * FREE_FLY_LOOK_SENS).clamp(-1.5, 1.5);
            t.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
        }
    } else {
        motion.clear();
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

fn fps_input(
    mode: Res<CameraMode>,
    mut motion: MessageReader<MouseMotion>,
    mut rig_q: Query<&mut CameraRig, With<IsoCamera>>,
) {
    if !mode.is_fps() {
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
        rig.fps_yaw -= delta.x * FPS_LOOK_SENS;
        rig.fps_pitch = (rig.fps_pitch - delta.y * FPS_LOOK_SENS).clamp(-1.5, 1.5);
    }
}

fn write_fps_move_intent(
    mode: Res<CameraMode>,
    keys: Res<ButtonInput<KeyCode>>,
    rig_q: Query<&CameraRig, With<IsoCamera>>,
    mut intent: ResMut<FpsMoveIntent>,
) {
    if !mode.is_fps() {
        if intent.active {
            *intent = FpsMoveIntent::default();
        }
        return;
    }
    let Ok(rig) = rig_q.single() else {
        *intent = FpsMoveIntent::default();
        return;
    };

    // Horizontal camera-relative movement. With Bevy cameras looking down -Z, the
    // forward vector for yaw 0 is world -Z.
    let forward = Vec3::new(-rig.fps_yaw.sin(), 0.0, -rig.fps_yaw.cos());
    let right = Vec3::new(rig.fps_yaw.cos(), 0.0, -rig.fps_yaw.sin());
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

/// Smoothly chase the player at the orbit offset, applying smoothed zoom/orbit.
fn follow_player(
    mode: Res<CameraMode>,
    time: Res<Time>,
    player_q: Query<&Transform, (With<FollowTarget>, Without<IsoCamera>)>,
    mut cam_q: Query<(&mut Transform, &mut Projection, &mut CameraRig), With<IsoCamera>>,
) {
    if mode.is_free_fly() {
        return;
    }
    let Ok(player) = player_q.single() else {
        return;
    };
    let Ok((mut cam_t, mut proj, mut rig)) = cam_q.single_mut() else {
        return;
    };

    if mode.is_fps() {
        cam_t.translation = fps_eye(player.translation);
        cam_t.rotation = Quat::from_euler(EulerRot::YXZ, rig.fps_yaw, rig.fps_pitch, 0.0);
        if !matches!(&*proj, Projection::Perspective(_)) {
            *proj = Projection::Perspective(PerspectiveProjection {
                fov: std::f32::consts::FRAC_PI_3,
                near: 0.1,
                far: ORTHO_FAR,
                ..default()
            });
        }
        return;
    }

    // Frame-rate-independent exponential smoothing factor.
    let k = 1.0 - (-SMOOTH * time.delta_secs()).exp();
    rig.yaw = lerp(rig.yaw, rig.yaw_target, k);
    rig.zoom = lerp(rig.zoom, rig.zoom_target, k);

    let target = player.translation + orbit_offset(rig.yaw);
    cam_t.translation = cam_t.translation.lerp(target, k);
    // Re-aim after moving: looking_at preserves translation, recomputes rotation.
    *cam_t = cam_t.looking_at(player.translation, Vec3::Y);

    if let Projection::Orthographic(ortho) = proj.as_mut() {
        ortho.scaling_mode = ScalingMode::FixedVertical {
            viewport_height: rig.zoom,
        };
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
