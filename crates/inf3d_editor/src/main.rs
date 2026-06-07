//! inf3d voxel model editor — standalone "Displayer + Creator" (Phase 1).
//!
//! A self-contained Bevy app (binary `inf3d-editor`) for building voxel models
//! the game and the future Animator can consume. It is **not** wired into the
//! game: it owns its own minimal plugin set + `bevy_egui` UI, paints sub-voxels
//! across an `N×N×N` reference-block volume, tags each voxel with a rig body
//! part, and saves to `.vox` (geometry) + `.rig.ron` (the rig sidecar).
//!
//! Plugin composition (single responsibility per plugin):
//! - [`EditorCameraPlugin`](camera::EditorCameraPlugin) — orbit camera + lights.
//! - [`EditorRenderPlugin`](render::EditorRenderPlugin) — voxel mesh + grid /
//!   reference / pivot gizmos.
//! - [`PaintPlugin`](paint::PaintPlugin) — click-to-add/erase ray interaction.
//! - [`EditorUiPlugin`](ui::EditorUiPlugin) — the egui panels.
//!
//! The single [`EditorState`](state::EditorState) resource is the source of
//! truth; every plugin reads/writes it. Run with `cargo run -p inf3d_editor`.

mod camera;
mod io;
mod paint;
mod palette;
mod parts;
mod render;
mod state;
mod ui;
mod volume;

use bevy::prelude::*;
use bevy::window::{Window, WindowPlugin};
use bevy_egui::EguiPlugin;

use camera::EditorCameraPlugin;
use paint::PaintPlugin;
use render::EditorRenderPlugin;
use state::EditorState;
use ui::EditorUiPlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "inf3d — voxel model editor".to_string(),
                ..default()
            }),
            ..default()
        }))
        // bevy_egui 0.39: add the plugin with defaults; UI systems run in the
        // `EguiPrimaryContextPass` schedule (see `ui::EditorUiPlugin`).
        .add_plugins(EguiPlugin::default())
        // The single owned editor state, installed once here.
        .init_resource::<EditorState>()
        .add_plugins((
            EditorCameraPlugin,
            EditorRenderPlugin,
            PaintPlugin,
            EditorUiPlugin,
        ))
        .run();
}
