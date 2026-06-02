//! inf3d — 3D voxel game. Plugin composition / binary entry point.

use bevy::prelude::*;
use bevy::window::{PresentMode, Window, WindowPlugin};

use inf3d_camera::IsoCameraPlugin;
use inf3d_core::CorePlugin;
use inf3d_gameplay::PlayerPlugin;
use inf3d_pathfinding::PathfindPlugin;
use inf3d_render::{DustPlugin, FogPlugin, FoliagePlugin, HighlightPlugin, WaterPlugin};
use inf3d_ui::HudPlugin;
use inf3d_world::WorldPlugin;

fn main() {
    App::new()
        // Vsync OFF (`PresentMode::Immediate`) — required to exceed monitor
        // refresh. Trades a tiny chance of tearing for an uncapped FPS readout.
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                present_mode: PresentMode::Immediate,
                ..default()
            }),
            ..default()
        }))
        // CorePlugin must come first: it installs QualitySettings / GrassStats /
        // FrameStats so every downstream plugin can read them at build time.
        .add_plugins(CorePlugin)
        .add_plugins(WorldPlugin)
        .add_plugins(PlayerPlugin)
        .add_plugins(IsoCameraPlugin)
        .add_plugins(PathfindPlugin)
        .add_plugins(HighlightPlugin)
        .add_plugins(DustPlugin)
        .add_plugins(FogPlugin)
        .add_plugins(HudPlugin)
        .add_plugins(WaterPlugin)
        .add_plugins(FoliagePlugin)
        .run();
}
