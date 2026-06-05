//! Feeds the terrain material's see-through ("x-ray") uniform each frame.
//!
//! Player-built faces (material index `>= inf3d_world::BUILT_MATERIAL_BASE`) that
//! sit near the player on screen are dither-discarded by the terrain shader, so the
//! walls of a structure you're inside turn see-through. Terrain is never touched
//! (its material index is below the threshold). This system just supplies the
//! player's on-screen position; the cutout itself lives in `terrain_material.wgsl`.

use bevy::prelude::*;
use bevy_voxel_world::rendering::VoxelWorldMaterialHandle;

use inf3d_camera::IsoCamera;
use inf3d_core::{FollowTarget, GameSet};
use inf3d_world::terrain_material::TerrainMaterial;

/// Screen-space radius (pixels) around the player within which built faces fade.
const FADE_RADIUS_PX: f32 = 115.0;

pub struct XrayPlugin;

impl Plugin for XrayPlugin {
    fn build(&self, app: &mut App) {
        // End-of-frame visual; `Fx` stays ungated so the uniform tracks the camera
        // even while paused (harmless — the player is frozen).
        app.add_systems(Update, update_xray_uniform.in_set(GameSet::Fx));
    }
}

fn update_xray_uniform(
    handle: Option<Res<VoxelWorldMaterialHandle<TerrainMaterial>>>,
    mut materials: ResMut<Assets<TerrainMaterial>>,
    cam: Query<(&Camera, &GlobalTransform), With<IsoCamera>>,
    player: Query<&Transform, With<FollowTarget>>,
) {
    let Some(handle) = handle else {
        return;
    };
    let Some(mat) = materials.get_mut(&handle.handle) else {
        return;
    };
    let (Ok((camera, cam_gtf)), Ok(player_tf)) = (cam.single(), player.single()) else {
        mat.extension.xray.screen.w = 0.0; // no camera/player → cutout off
        return;
    };
    // DISABLED (`w = 0`): the forward-only dither just punches holes showing what's
    // behind the wall (darkness), not the player — because the terrain depth prepass
    // still occludes everything behind. It looks broken, so it stays off until the
    // matching custom PREPASS discard lands (then flip `ENABLED` to 1.0). The CPU
    // feed is wired and ready; only the prepass shader is missing.
    const ENABLED: f32 = 0.0;
    let player_pos = player_tf.translation + Vec3::Y * 0.8;
    match camera.world_to_viewport(cam_gtf, player_pos) {
        Ok(px) => {
            mat.extension.xray.screen = Vec4::new(px.x, px.y, FADE_RADIUS_PX, ENABLED);
        }
        Err(_) => mat.extension.xray.screen.w = 0.0,
    }
}
