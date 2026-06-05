//! Feeds the terrain material's see-through ("x-ray") uniform each frame.
//!
//! Player-built voxels (material index `>= inf3d_world::BUILT_MATERIAL_BASE`) that sit
//! between the camera and the player are cut away — whole blocks, computed in WORLD
//! space — so the structure you're inside opens up around your character. Terrain /
//! natural blocks are never touched. This system only supplies the player's world
//! position + the camera's view direction; the per-voxel cutaway test lives in the
//! shared `inf3d::terrain_xray` shader module (used by the forward shader AND the
//! prepass, so both remove the identical voxels).

use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, NormalPrepass};
use bevy::prelude::*;
use bevy_voxel_world::rendering::VoxelWorldMaterialHandle;

use inf3d_camera::IsoCamera;
use inf3d_core::{FollowTarget, GameSet};
use inf3d_world::terrain_material::{
    TerrainMaterial, XRAY_CEILING_RADIUS, XRAY_CUT_RADIUS, XRAY_HEAD_CLEARANCE,
    XRAY_PLAYER_HALF_HEIGHT,
};

pub struct XrayPlugin;

impl Plugin for XrayPlugin {
    fn build(&self, app: &mut App) {
        // End-of-frame visual; `Fx` stays ungated so the uniform tracks the camera
        // even while paused (harmless — the player is frozen).
        app.add_systems(Update, update_xray_uniform.in_set(GameSet::Fx));
    }
}

#[allow(clippy::type_complexity)]
fn update_xray_uniform(
    handle: Option<Res<VoxelWorldMaterialHandle<TerrainMaterial>>>,
    mut materials: ResMut<Assets<TerrainMaterial>>,
    cam: Query<
        (
            &GlobalTransform,
            Has<DepthPrepass>,
            Has<NormalPrepass>,
            Has<MotionVectorPrepass>,
        ),
        With<IsoCamera>,
    >,
    player: Query<&Transform, With<FollowTarget>>,
) {
    let Some(handle) = handle else {
        return;
    };
    let Some(mat) = materials.get_mut(&handle.handle) else {
        return;
    };
    let (Ok((cam_gtf, has_depth, has_normal, has_motion)), Ok(player_tf)) =
        (cam.single(), player.single())
    else {
        mat.extension.xray.player.w = 0.0; // no camera/player → cutaway off
        return;
    };

    // The cutaway only looks right when the PREPASS can also remove the voxels
    // (otherwise the wall's prepass depth re-occludes the player). The prepass discard
    // lives in its FRAGMENT, which Bevy compiles only when a NORMAL or MOTION_VECTOR
    // prepass is active. So:
    //   - depth prepass + (normal or motion) → prepass fragment runs the cut → ON
    //   - a depth-ONLY prepass (e.g. water on, SSAO + motion off) → no prepass
    //     fragment → would leave un-revealable gaps → keep it OFF
    //   - no depth prepass at all → the forward discard alone already reveals the
    //     player → ON
    // The forward shader gates on this same `player.w`, so both halves stay in sync.
    let prepass_can_discard = !has_depth || has_normal || has_motion;
    let enabled = if prepass_can_discard { 1.0 } else { 0.0 };

    // Player body center + the camera view direction. Orthographic → every view ray is
    // parallel to `forward`, so the shader can test each voxel against the single
    // camera→player line in world space.
    let player_center = player_tf.translation;
    let forward = *cam_gtf.forward(); // Dir3 derefs to the unit Vec3 view direction

    mat.extension.xray.player = player_center.extend(enabled);
    mat.extension.xray.view = forward.extend(XRAY_CUT_RADIUS);
    mat.extension.xray.extra = Vec4::new(
        XRAY_PLAYER_HALF_HEIGHT,
        XRAY_CEILING_RADIUS,
        XRAY_HEAD_CLEARANCE,
        0.0,
    );
}
