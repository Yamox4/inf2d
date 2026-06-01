#![deny(unsafe_code)]
//! HUD overlay: FPS, camera, world, cursor.
//!
//! Adds an egui-backed top-left HUD that surfaces frame timings, camera pose,
//! streaming load count, and a live cursor → tile/biome readout. Also exposes
//! a [`LoadingScreenPlugin`] for the `AppState::Loading` → `AppState::InGame`
//! transition.

mod hud;
mod loading;
mod pause_menu;

use bevy::diagnostic::{EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};
use inf2d_core::AppState;

pub use hud::hud_panel;
pub use loading::LoadingScreenPlugin;
pub use pause_menu::{PauseMenuPlugin, PauseUiState};

/// Registers diagnostics plugins, `EguiPlugin`, and the HUD draw system.
///
/// All dependencies are added idempotently — installing this plugin twice, or
/// alongside another plugin that already added [`EguiPlugin`] /
/// [`FrameTimeDiagnosticsPlugin`] / [`EntityCountDiagnosticsPlugin`], is safe.
///
/// The HUD is gated to [`AppState::InGame`] so it does not draw alongside the
/// loading screen.
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<FrameTimeDiagnosticsPlugin>() {
            app.add_plugins(FrameTimeDiagnosticsPlugin::default());
        }
        if !app.is_plugin_added::<EntityCountDiagnosticsPlugin>() {
            app.add_plugins(EntityCountDiagnosticsPlugin::default());
        }
        if !app.is_plugin_added::<EguiPlugin>() {
            app.add_plugins(EguiPlugin::default());
        }
        app.add_systems(
            EguiPrimaryContextPass,
            hud::hud_panel.run_if(in_state(AppState::InGame)),
        );
        app.add_plugins(LoadingScreenPlugin);
        app.add_plugins(PauseMenuPlugin);
    }
}
