//! inf3d — 3D voxel game. Plugin composition / binary entry point.

use bevy::prelude::*;

use inf3d_camera::IsoCameraPlugin;
use inf3d_gameplay::PlayerPlugin;
use inf3d_pathfinding::PathfindPlugin;
use inf3d_render::{DustPlugin, FogPlugin, GrassPlugin, HighlightPlugin, WaterPlugin};
use inf3d_ui::HudPlugin;
use inf3d_world::WorldPlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(WorldPlugin)
        .add_plugins(PlayerPlugin)
        .add_plugins(IsoCameraPlugin)
        .add_plugins(PathfindPlugin)
        .add_plugins(HighlightPlugin)
        .add_plugins(DustPlugin)
        .add_plugins(FogPlugin)
        .add_plugins(HudPlugin)
        .add_plugins(WaterPlugin)
        .add_plugins(GrassPlugin)
        .run();
}
