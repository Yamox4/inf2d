//! A MagicaVoxel / Blender-style orbit camera for viewing the model.
//!
//! The control scheme deliberately keeps **left mouse free for painting**
//! ([`crate::paint`]):
//! - **Orbit** — hold the **right** *or* **middle** mouse button and drag.
//! - **Pan** — hold **Shift** with the right *or* middle button and drag (moves
//!   the focus point in the camera's screen plane).
//! - **Zoom** — the **scroll wheel** (dollies the boom in/out).
//!
//! The camera never grabs the cursor — it stays free for the egui panels and for
//! click-to-paint. The focus point starts at the center of the multi-block build
//! volume so rotating keeps the model framed; it re-centers whenever the block
//! extent changes (so the model stays framed as the slider moves) and the user
//! can nudge it by panning.
//!
//! Input is gated by [`PointerOverUi`](crate::paint::PointerOverUi): a drag that
//! *starts* over an egui panel never moves the camera, so clicking the UI can't
//! orbit/pan the view. Once a drag begins in the viewport it keeps tracking even
//! if the pointer crosses a panel (standard DCC behavior), and the drag state is
//! cleared when the button releases so buffered motion never leaks.

use bevy::camera::{PerspectiveProjection, Projection};
use bevy::input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::prelude::*;

use crate::paint::PointerOverUi;
use crate::state::EditorState;

/// Vertical field of view (radians) — a natural ~50° modeling lens.
const FOV: f32 = 0.9;
/// Default boom distance from the focus to the eye (world units; 1 unit = 1
/// reference block).
const DISTANCE_DEFAULT: f32 = 3.5;
/// Closest the boom can zoom in.
const DISTANCE_MIN: f32 = 0.6;
/// Furthest the boom can zoom out.
const DISTANCE_MAX: f32 = 30.0;
/// Boom-distance change per scroll line (mouse wheel).
const ZOOM_SPEED_LINE: f32 = 0.6;
/// Boom-distance change per scroll pixel (trackpad), much smaller per unit.
const ZOOM_SPEED_PIXEL: f32 = 0.02;
/// Orbit sensitivity (radians per pixel of drag).
const LOOK_SENS: f32 = 0.006;
/// Pan sensitivity (world units per pixel of drag, scaled by boom distance so the
/// model tracks the cursor at any zoom).
const PAN_SENS: f32 = 0.0015;
/// Pitch clamp (radians) just shy of the poles so the look-at never degenerates.
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.05;
/// Default yaw — a comfortable 3/4 front view of the platform.
const YAW_DEFAULT: f32 = 0.7;
/// Default pitch — looking slightly down onto the build volume.
const PITCH_DEFAULT: f32 = 0.5;

/// The editor's single camera. Stores its own orbit state so the controller is
/// self-contained (no shared resource needed).
#[derive(Component)]
pub struct EditorCamera {
    /// Yaw about world up (radians).
    yaw: f32,
    /// Pitch above the horizon (radians).
    pitch: f32,
    /// Boom length from focus to eye (world units).
    distance: f32,
    /// The look-at point the camera orbits and frames. Re-centered to the build
    /// volume's center when the extent changes; nudged by panning.
    focus: Vec3,
    /// The build-volume extent (`blocks`) the focus was last centered for, so the
    /// controller can detect an extent change and re-frame the model.
    framed_blocks: u32,
    /// Whether a button-drag is currently active (started in the viewport). Held
    /// across frames so a drag that wanders over a panel keeps tracking, and so a
    /// drag that started on a panel never moves the camera.
    dragging: bool,
}

impl Default for EditorCamera {
    fn default() -> Self {
        Self {
            yaw: YAW_DEFAULT,
            pitch: PITCH_DEFAULT,
            distance: DISTANCE_DEFAULT,
            focus: Vec3::ZERO,
            framed_blocks: 0,
            dragging: false,
        }
    }
}

/// Plugin: spawns the camera + a key light, and runs the orbit/pan/zoom controller.
pub struct EditorCameraPlugin;

impl Plugin for EditorCameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_camera)
            .add_systems(Update, control_camera);
    }
}

/// Spawn the orbit camera plus a sun + ambient fill so the model reads with
/// clear form from any angle.
fn spawn_camera(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: FOV,
            near: 0.01,
            far: 1000.0,
            ..default()
        }),
        Transform::default(),
        EditorCamera::default(),
    ));

    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 8.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Bevy 0.18: the scene-wide ambient fill is the `GlobalAmbientLight`
    // resource (per-camera `AmbientLight` is the component form). A soft fill so
    // faces away from the sun still read.
    commands.insert_resource(GlobalAmbientLight {
        color: Color::WHITE,
        brightness: 600.0,
        affects_lightmapped_meshes: false,
    });
}

/// Orbit / pan on a right- or middle-button drag, zoom on scroll. Left mouse is
/// never read here, so it stays free for painting. UI is respected: a drag that
/// starts over a panel is ignored, and the scroll wheel is consumed by egui first
/// (only viewport-leftover wheel events reach this system).
fn control_camera(
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    over_ui: Res<PointerOverUi>,
    state: Res<EditorState>,
    mut cam: Query<(&mut EditorCamera, &mut Transform)>,
) {
    let Ok((mut orbit, mut transform)) = cam.single_mut() else {
        return;
    };

    // Re-center the focus on the build volume whenever the extent changes, so the
    // model stays framed as the block slider moves. Panning then nudges it.
    let blocks = state.model.blocks();
    if blocks != orbit.framed_blocks {
        let half = blocks as f32 * 0.5;
        orbit.focus = Vec3::new(half, half, half);
        orbit.framed_blocks = blocks;
    }

    // A right- or middle-button drag drives the camera; Shift switches it from
    // orbit to pan. Begin a drag only when the press lands in the viewport (not
    // over a panel) so clicking the UI never moves the camera; keep tracking once
    // started even if the pointer wanders over a panel.
    let nav_button = buttons.pressed(MouseButton::Right) || buttons.pressed(MouseButton::Middle);
    let nav_just_pressed =
        buttons.just_pressed(MouseButton::Right) || buttons.just_pressed(MouseButton::Middle);
    if nav_just_pressed && !over_ui.0 {
        orbit.dragging = true;
    }
    if !nav_button {
        orbit.dragging = false;
    }

    if orbit.dragging {
        let mut delta = Vec2::ZERO;
        for ev in motion.read() {
            delta += ev.delta;
        }
        let panning = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        if panning {
            // Pan the focus in the camera's screen plane. Right/up are the
            // camera's local axes; scale by distance so the model tracks the
            // cursor at any zoom. Screen-down (+delta.y) moves the focus up.
            let right = *transform.right();
            let up = *transform.up();
            let scale = PAN_SENS * orbit.distance;
            orbit.focus += (-delta.x * right + delta.y * up) * scale;
        } else {
            orbit.yaw -= delta.x * LOOK_SENS;
            orbit.pitch = (orbit.pitch + delta.y * LOOK_SENS).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        }
    } else {
        // Drain so buffered motion doesn't jump the next time a drag starts.
        motion.clear();
    }

    // Zoom: only when the pointer is over the viewport, so scrolling a panel's
    // scroll-area doesn't also dolly the camera (`over_ui` is the unioned panel
    // rects from the UI layer). Honor both line (mouse wheel) and pixel
    // (trackpad) units so the feel is consistent across devices.
    let mut scroll = 0.0;
    for ev in wheel.read() {
        scroll += match ev.unit {
            MouseScrollUnit::Line => ev.y * ZOOM_SPEED_LINE,
            MouseScrollUnit::Pixel => ev.y * ZOOM_SPEED_PIXEL,
        };
    }
    if scroll != 0.0 && !over_ui.0 {
        orbit.distance = (orbit.distance - scroll).clamp(DISTANCE_MIN, DISTANCE_MAX);
    }

    // Compose the eye from the orbit angles + boom and look at the focus.
    let dir = Vec3::new(
        orbit.yaw.cos() * orbit.pitch.cos(),
        orbit.pitch.sin(),
        orbit.yaw.sin() * orbit.pitch.cos(),
    );
    let eye = orbit.focus + dir * orbit.distance;
    *transform = Transform::from_translation(eye).looking_at(orbit.focus, Vec3::Y);
}
