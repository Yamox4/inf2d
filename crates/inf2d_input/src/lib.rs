#![deny(unsafe_code)]
//! Input adapter. Bevy's `ButtonInput<KeyCode>` / `ButtonInput<MouseButton>` and the
//! `MouseWheel` event stream are the source of truth; this crate distills them into a
//! single [`InputState`] resource that every consumer reads.
//!
//! Two reasons for the indirection over scattered `Res<ButtonInput<…>>` lookups:
//! 1. **One place to rebind.** Any future "remap controls" UI edits this plugin's table.
//! 2. **Consumers don't import `KeyCode` / `MouseButton`.** A camera test doesn't need to
//!    know that pan is bound to middle-mouse — it just reads `state.pan_drag`.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use inf2d_core::CoreSet;

/// Per-frame distilled input. Cleared and rewritten by [`read_inputs`] at the top of
/// `Update`'s [`CoreSet`]; consumers in later sets read it like any other resource.
#[derive(Resource, Reflect, Default, Debug, Clone, Copy)]
#[reflect(Resource)]
pub struct InputState {
    /// True while either pan-drag mouse button (middle or right) is held.
    pub pan_drag: bool,
    /// Accumulated mouse-wheel Y delta this frame (positive = scroll up).
    pub zoom_delta: f32,
    /// One-shot: left mouse button went down this frame.
    pub select_just_pressed: bool,
    /// One-shot: F3.
    pub toggle_inspector: bool,
    /// One-shot: F4.
    pub toggle_physics_debug: bool,
    /// One-shot: F5.
    pub toggle_chunk_gizmos: bool,
    /// One-shot: F10.
    pub toggle_perf_ui: bool,
    /// One-shot: Esc.
    pub toggle_pause: bool,
}

/// Plugin: registers the `InputState` resource and the per-frame `read_inputs` system.
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputState>()
            .register_type::<InputState>()
            .add_systems(Update, read_inputs.in_set(CoreSet));
    }
}

/// Drain Bevy's native input state into [`InputState`]. Runs in [`CoreSet`] before camera
/// and gameplay see anything — by the time `SimulationSet` runs, `state` reflects this frame.
pub fn read_inputs(
    mut state: ResMut<InputState>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut scroll: MessageReader<MouseWheel>,
) {
    state.pan_drag =
        mouse.pressed(MouseButton::Middle) || mouse.pressed(MouseButton::Right);
    state.zoom_delta = scroll.read().map(|w| w.y).sum();
    state.select_just_pressed = mouse.just_pressed(MouseButton::Left);
    state.toggle_inspector = keys.just_pressed(KeyCode::F3);
    state.toggle_physics_debug = keys.just_pressed(KeyCode::F4);
    state.toggle_chunk_gizmos = keys.just_pressed(KeyCode::F5);
    state.toggle_perf_ui = keys.just_pressed(KeyCode::F10);
    state.toggle_pause = keys.just_pressed(KeyCode::Escape);
}
