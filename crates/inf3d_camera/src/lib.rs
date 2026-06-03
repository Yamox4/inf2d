//! Orthographic isometric follow camera (Diablo-style 3/4 view) with springy
//! follow, scroll-wheel zoom, and Q/E orbit.

use bevy::camera::{OrthographicProjection, Projection, ScalingMode};
use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, NormalPrepass};
use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel};
use bevy::post_process::bloom::Bloom;
use bevy::post_process::dof::DepthOfField;
use bevy::post_process::motion_blur::MotionBlur;
use bevy::prelude::*;
use bevy::render::view::{Hdr, Msaa};
use bevy_voxel_world::prelude::*;

use inf3d_core::{FollowTarget, GameSet, QualitySettings};
use inf3d_world::MainWorld;

/// Horizontal (XZ-plane) distance from the player to the camera.
const ORBIT_RADIUS: f32 = 39.6;
/// Camera height above the player.
const ORBIT_HEIGHT: f32 = 36.0;
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

#[derive(Component)]
pub struct IsoCamera;

/// Smoothed camera state: orbit yaw around the player and orthographic zoom,
/// each easing toward a target the input system sets.
#[derive(Component)]
pub struct CameraRig {
    yaw: f32,
    yaw_target: f32,
    zoom: f32,
    zoom_target: f32,
}

pub struct IsoCameraPlugin;

impl Plugin for IsoCameraPlugin {
    fn build(&self, app: &mut App) {
        // `QualitySettings` is a shared resource owned solely by `CorePlugin`; we
        // only read it here. `camera_input` is raw-input (Input phase);
        // `apply_quality_to_camera` reconfigures post-FX (Fx phase).
        app.add_systems(Startup, spawn_camera)
            .add_systems(Update, camera_input.in_set(GameSet::Input))
            .add_systems(Update, apply_quality_to_camera.in_set(GameSet::Fx))
            // Follow in PostUpdate, AFTER avian's `TransformInterpolation` easing
            // (which runs in `RunFixedMainLoop`, before `Update`) has written the
            // smoothed player `Transform`, and BEFORE Bevy's transform
            // propagation so the camera's `GlobalTransform` is up to date this
            // frame. Reading the interpolated (not mid-step) transform is what
            // keeps the camera smooth at any zoom.
            .add_systems(PostUpdate, follow_player.before(TransformSystems::Propagate));
    }
}

/// Camera offset from the player for a given orbit yaw.
fn orbit_offset(yaw: f32) -> Vec3 {
    Vec3::new(yaw.sin() * ORBIT_RADIUS, ORBIT_HEIGHT, yaw.cos() * ORBIT_RADIUS)
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

fn spawn_camera(mut commands: Commands, quality: Res<QualitySettings>) {
    let yaw = std::f32::consts::FRAC_PI_4; // classic 45° iso

    // Orthographic projection gives the flat, parallel-line iso look. FixedVertical
    // keeps a constant world-height slice on screen regardless of aspect ratio.
    let projection = Projection::Orthographic(OrthographicProjection {
        scaling_mode: ScalingMode::FixedVertical {
            viewport_height: ZOOM_DEFAULT,
        },
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
        },
        // HDR is the color pipeline contract regardless of post-FX quality.
        Hdr,
    ));

    if quality.bloom_enabled {
        entity.insert(bloom_component());
    }
    // SSAO + motion blur are AAA post-FX gated to the higher-quality presets via
    // their own real `QualitySettings` flags (Medium/High on, Potato/Low off).
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

/// Re-apply the post-FX component set whenever `QualitySettings` changes,
/// adding or stripping `Bloom`, `DepthPrepass`, `DepthOfField`, SSAO (+ its
/// `NormalPrepass`/`Msaa::Off`), and motion blur (+ its `MotionVectorPrepass`)
/// to match the new preset. Skips re-inserting when the component is already
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

    // SSAO + motion blur use their own real quality flags (see spawn_camera) so
    // Potato/Low stay cheap.
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
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mut scroll: MessageReader<MouseWheel>,
    mut motion: MessageReader<MouseMotion>,
    mut rig_q: Query<&mut CameraRig>,
) {
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

/// Smoothly chase the player at the orbit offset, applying smoothed zoom/orbit.
fn follow_player(
    time: Res<Time>,
    player_q: Query<&Transform, (With<FollowTarget>, Without<IsoCamera>)>,
    mut cam_q: Query<(&mut Transform, &mut Projection, &mut CameraRig), With<IsoCamera>>,
) {
    let Ok(player) = player_q.single() else {
        return;
    };
    let Ok((mut cam_t, mut proj, mut rig)) = cam_q.single_mut() else {
        return;
    };

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
