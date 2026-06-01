//! Orthographic isometric follow camera (Diablo-style 3/4 view) with springy
//! follow, scroll-wheel zoom, and Q/E orbit.

use bevy::camera::{OrthographicProjection, Projection, ScalingMode};
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::light::VolumetricFog;
use bevy::post_process::bloom::Bloom;
use bevy::post_process::dof::DepthOfField;
use bevy::prelude::*;
use bevy::render::view::Hdr;
use bevy_voxel_world::prelude::*;

use inf3d_core::FollowTarget;
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
        app.add_systems(Startup, spawn_camera)
            .add_systems(Update, camera_input)
            // PostUpdate so the camera reads the player's final position this frame.
            .add_systems(PostUpdate, follow_player);
    }
}

/// Camera offset from the player for a given orbit yaw.
fn orbit_offset(yaw: f32) -> Vec3 {
    Vec3::new(yaw.sin() * ORBIT_RADIUS, ORBIT_HEIGHT, yaw.cos() * ORBIT_RADIUS)
}

fn spawn_camera(mut commands: Commands) {
    let yaw = std::f32::consts::FRAC_PI_4; // classic 45° iso

    // Orthographic projection gives the flat, parallel-line iso look. FixedVertical
    // keeps a constant world-height slice on screen regardless of aspect ratio.
    let projection = Projection::Orthographic(OrthographicProjection {
        scaling_mode: ScalingMode::FixedVertical {
            viewport_height: ZOOM_DEFAULT,
        },
        ..OrthographicProjection::default_3d()
    });

    commands.spawn((
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
    ))
    // Split into a second tuple: a single spawn tuple can't hold this many.
    .insert((
        Hdr,
        Bloom {
            intensity: 0.15,
            ..default()
        },
        // Volumetric fog renders in its own pass (independent of the voxel
        // terrain's custom material). Needs a FogVolume (fog.rs) + VolumetricLight (sun).
        VolumetricFog {
            ambient_intensity: 0.5,
            step_count: 48,
            ..default()
        },
        // Depth prepass feeds SSR (water) and DoF.
        DepthPrepass,
        // Subtle depth-of-field (cozy tilt-shift). SSAO + motion blur were dropped:
        // the voxel terrain skips the prepass so they can't shade it, and SSAO
        // forces MSAA off (worse on the blocky terrain).
        DepthOfField {
            focal_distance: 55.0,
            aperture_f_stops: 8.0,
            ..default()
        },
    ));
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
