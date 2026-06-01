//! inf3d_proto — 3D voxel exploration prototype (bevy_voxel_world 0.16, Bevy 0.18).
//!
//! Evolving from a fly-camera tech demo into a Diablo-style slice: orthographic
//! isometric follow camera, a player capsule, and click-to-move A* pathfinding
//! over the voxel surface. Lives in its own crate so the 2.5D iso engine stays
//! untouched and this experiment is trivial to delete.

mod camera;
mod dust;
mod fog;
mod grass;
mod highlight;
mod hud;
mod pathfind;
mod player;
mod water;
mod world;

use bevy::prelude::*;

use camera::IsoCameraPlugin;
use dust::DustPlugin;
use fog::FogPlugin;
use grass::GrassPlugin;
use highlight::HighlightPlugin;
use hud::HudPlugin;
use pathfind::PathfindPlugin;
use player::PlayerPlugin;
use water::WaterPlugin;
use world::WorldPlugin;

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
