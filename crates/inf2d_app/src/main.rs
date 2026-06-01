#![deny(unsafe_code)]
//! inf2d — isometric infinite-world game binary.
//!
//! Composes the plugin graph: bevy DefaultPlugins → core → input → world+worldgen
//! → render → physics → camera → ui → debug. Each crate is responsible for its own
//! schedule wiring; this file only declares the order in which they are added.

use bevy::prelude::*;
use bevy::window::{
    MonitorSelection, PresentMode, Window, WindowPlugin, WindowPosition, WindowResolution,
};
use bevy_tweening::TweeningPlugin;
use inf2d_core::CorePlugin;
use inf2d_world::WorldPlugin;
use inf2d_worldgen::WorldgenPlugin;
use inf2d_render::RenderPlugin;
use inf2d_physics::{PhysicsDebugPlugin, PhysicsPlugin};
use inf2d_camera::CameraPlugin;
use inf2d_input::InputPlugin;
use inf2d_pathfinding::PathfindingPlugin;
use inf2d_gameplay::GameplayPlugin;
use inf2d_audio::AudioPlugin;
use inf2d_save::SavePlugin;
use inf2d_ui::UiPlugin;
use inf2d_debug::DebugPlugin;

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "inf2d — isometric infinite world".into(),
                        resolution: WindowResolution::new(1280, 720),
                        // 720p, centered on the second monitor. Falls back to the
                        // primary monitor automatically if a second one isn't connected
                        // (winit gracefully degrades MonitorSelection::Index when the
                        // index is out of range).
                        position: WindowPosition::Centered(MonitorSelection::Index(1)),
                        present_mode: PresentMode::AutoVsync,
                        resizable: true,
                        ..default()
                    }),
                    ..default()
                })
                .set(ImagePlugin::default_nearest()),
        )
        .add_plugins(TweeningPlugin)
        .add_plugins((
            CorePlugin,
            InputPlugin,
            WorldPlugin,
            WorldgenPlugin,
            RenderPlugin,
            PhysicsPlugin,
            #[cfg(feature = "dev")]
            PhysicsDebugPlugin,
            CameraPlugin,
            PathfindingPlugin,
            GameplayPlugin,
            AudioPlugin,
            SavePlugin,
            UiPlugin,
            #[cfg(feature = "dev")]
            DebugPlugin,
        ))
        .run();
}
