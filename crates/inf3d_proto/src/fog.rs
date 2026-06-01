//! Fog-of-war edge haze.
//!
//! A large, camera-following [`FogVolume`] with uniform density and enough
//! vertical extent to cover all terrain — so it reads as a distance haze that
//! fades the streamed-world edge into the horizon (chunks blend in instead of
//! hard-cutting), NOT as a low ground/height layer.
//!
//! Why volumetric and not `DistanceFog`: the voxel terrain uses a custom
//! material shader that contains no fog code, so `DistanceFog` never touches the
//! green terrain. Volumetric fog renders in its own pass and does. It needs
//! `VolumetricFog` on the camera (see camera.rs) and `VolumetricLight` on the
//! sun (see world.rs).

use bevy::light::FogVolume;
use bevy::prelude::*;

use crate::player::Player;

/// Cool horizon tone; also the clear color so the edge dissolves into it.
const HORIZON: Color = Color::srgb(0.60, 0.67, 0.71);
/// Horizontal coverage (the volume follows the player).
const FOG_EXTENT: f32 = 700.0;
/// Vertical coverage — tall enough to engulf all terrain so there's no visible
/// height gradient (uniform fog of war, not ground fog).
const FOG_TALL: f32 = 240.0;
/// Uniform density. Higher = the fog-of-war edge closes in nearer the player.
const FOG_DENSITY: f32 = 0.015;

#[derive(Component)]
struct FogOfWar;

pub struct FogPlugin;

impl Plugin for FogPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(HORIZON))
            .add_systems(Startup, spawn_fog)
            .add_systems(Update, follow_fog);
    }
}

fn spawn_fog(mut commands: Commands) {
    commands.spawn((
        FogVolume {
            fog_color: HORIZON,
            density_factor: FOG_DENSITY,
            scattering: 0.3,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 0.0).with_scale(Vec3::new(FOG_EXTENT, FOG_TALL, FOG_EXTENT)),
        Visibility::default(),
        FogOfWar,
    ));
}

/// Keep the fog volume centered on the player (XZ) so the endless world keeps a
/// consistent fog-of-war edge. Y stays at 0 with a tall extent — uniform, no layer.
fn follow_fog(
    player_q: Query<&Transform, (With<Player>, Without<FogOfWar>)>,
    mut fog_q: Query<&mut Transform, With<FogOfWar>>,
) {
    let Ok(mut fog) = fog_q.single_mut() else {
        return;
    };
    if let Ok(player) = player_q.single() {
        fog.translation.x = player.translation.x;
        fog.translation.z = player.translation.z;
    }
}
