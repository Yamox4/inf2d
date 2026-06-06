//! Feeds the terrain material's see-through ("x-ray") uniform each frame.
//!
//! Player-built voxels (material index `>= inf3d_world::BUILT_MATERIAL_BASE`) that sit
//! between the camera and the player are cut away — whole blocks, computed in WORLD
//! space — so the structure you're inside opens up around your character. Terrain /
//! natural blocks are never touched. The per-voxel cutaway test lives in the shared
//! `inf3d::terrain_xray` shader module (used by the forward shader AND the prepass, so
//! both remove the identical voxels); this system supplies that test's uniform — the
//! player position + camera view direction — AND gates the whole effect on a real
//! **occlusion test** ([`player_build_occluded`]): the cut only engages when a build
//! actually hides the character from this camera, so walls never vanish while the
//! player is in plain sight.

use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, NormalPrepass};
use bevy::prelude::*;
use bevy_voxel_world::prelude::{VoxelWorld, WorldVoxel};
use bevy_voxel_world::rendering::VoxelWorldMaterialHandle;

use inf3d_camera::IsoCamera;
use inf3d_core::{FollowTarget, GameSet};
use inf3d_world::terrain_material::{
    TerrainMaterial, XRAY_CEILING_RADIUS, XRAY_CUT_RADIUS, XRAY_HEAD_CLEARANCE,
    XRAY_PLAYER_HALF_HEIGHT,
};
use inf3d_world::{MainWorld, BUILT_MATERIAL_BASE};

/// How many of the player-silhouette samples must be hidden by a player build
/// before the cutaway engages. Requiring `>= 2` (of the [`OCCLUSION_SAMPLES`]
/// rays) means a single grazing ray — a shoulder clipping a wall edge — never
/// toggles the whole effect, while a real wall in front of the camera hides
/// several samples at once. Raise to demand more occlusion before cutting; lower
/// (toward 1) to cut more eagerly.
const OCCLUSION_TRIGGER: usize = 2;
/// Number of points sampled across the character's silhouette for the occlusion
/// test (three up the body + two lateral). See [`player_build_occluded`].
const OCCLUSION_SAMPLES: usize = 5;

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
    // The voxel data, queried for the occlusion gate: we only cut walls when a build
    // actually hides the player from THIS camera.
    voxel_world: VoxelWorld<MainWorld>,
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

    // Player body center + the camera view direction. Orthographic → every view ray is
    // parallel to `forward`, so the shader can test each voxel against the single
    // camera→player line in world space.
    let player_center = player_tf.translation;
    let forward = *cam_gtf.forward(); // Dir3 derefs to the unit Vec3 view direction

    // OCCLUSION GATE — the "smart" half. The cutaway used to fire purely on geometry,
    // so it carved walls away even when the character was in plain sight (a wall merely
    // *near* the camera→player line, or a shallow pit the camera sees straight into).
    // Now we first ask: is a player BUILD actually hiding the character from this camera?
    // Only then do we enable the cut. When the player is visible — corridor walls beside
    // you, an open pit, nothing in front — the whole effect stays OFF and every block
    // remains solid. (`prepass_can_discard` is still required: without it the cut would
    // leave un-revealable depth holes; see below.)
    let occluded =
        prepass_can_discard && player_build_occluded(&voxel_world, player_center, forward);
    let enabled = if occluded { 1.0 } else { 0.0 };

    mat.extension.xray.player = player_center.extend(enabled);
    mat.extension.xray.view = forward.extend(XRAY_CUT_RADIUS);
    mat.extension.xray.extra = Vec4::new(
        XRAY_PLAYER_HALF_HEIGHT,
        XRAY_CEILING_RADIUS,
        XRAY_HEAD_CLEARANCE,
        // The cutout's build-material base, fed from the Rust source of truth so the
        // shader never hard-codes (and drifts from) it.
        BUILT_MATERIAL_BASE as f32,
    );
}

/// Whether a player **build** actually hides the character from the current camera
/// — the occlusion test that gates the whole cutaway.
///
/// The view is orthographic, so every sight-line is parallel to `forward` and
/// `-forward` points straight at the camera. We sample [`OCCLUSION_SAMPLES`] points
/// spanning the character's silhouette (three up the body + two lateral at the
/// torso) and cast each toward the camera, looking ONLY for player builds
/// (`material >= `[`BUILT_MATERIAL_BASE`]). If at least [`OCCLUSION_TRIGGER`] of them
/// are blocked, a meaningful part of the character is hidden behind a build → cut.
///
/// Why only builds: the cutaway removes only player builds, so a character hidden
/// behind *terrain* (a hill, a natural overhang) can't be revealed by cutting and
/// must NOT trigger the effect. Why a multi-sample threshold (not a single center
/// ray): it ignores a lone grazing ray (a shoulder clipping a wall corner) so the
/// effect doesn't flicker on/off at the edge of cover, while a real wall in front
/// blocks several samples at once. The test reads the VOXEL data (not the rendered
/// result), so once the cut reveals the player it keeps reporting "occluded" and
/// the effect stays stable rather than oscillating.
fn player_build_occluded(voxel_world: &VoxelWorld<MainWorld>, center: Vec3, forward: Vec3) -> bool {
    // `-forward` aims at the camera. A degenerate forward (never, for a live iso cam)
    // disables the gate rather than panicking.
    let Ok(toward_cam) = Dir3::new(-forward) else {
        return false;
    };
    let samples = silhouette_samples(center, forward);

    let mut hidden = 0usize;
    for s in samples {
        // Nudge the origin a hair toward the camera so the player's own cell can't be
        // the first hit, then look for any build between this point and the camera.
        let ray = Ray3d {
            origin: s - forward * 0.05,
            direction: toward_cam,
        };
        let blocked = voxel_world
            .raycast(ray, &|(_coords, voxel)| {
                matches!(voxel, WorldVoxel::Solid(m) if (m as u32) >= BUILT_MATERIAL_BASE)
            })
            .is_some();
        if blocked {
            hidden += 1;
            if hidden >= OCCLUSION_TRIGGER {
                return true;
            }
        }
    }
    false
}

/// The points across the character's silhouette the occlusion test casts from:
/// three up the body (head / torso / lower body) plus two lateral at the torso.
///
/// `center` is the player's capsule centre (the `FollowTarget` transform origin),
/// `forward` the camera view direction. The lateral axis is the camera's horizontal
/// right (`forward × up`), so the two shoulder samples straddle the body across the
/// screen. The **lower** sample is deliberately kept just ABOVE the feet (at
/// `0.7 * XRAY_PLAYER_HALF_HEIGHT` below centre, vs. the ~1.0 capsule-centre→feet
/// distance) so a ray never starts inside the floor block the player stands on,
/// which would self-report as occluded and pin the cutaway on.
fn silhouette_samples(center: Vec3, forward: Vec3) -> [Vec3; OCCLUSION_SAMPLES] {
    let up = Vec3::Y * XRAY_PLAYER_HALF_HEIGHT;
    let side = forward.cross(Vec3::Y).normalize_or_zero() * 0.35;
    [
        center + up * 0.95, // head
        center,             // torso
        center - up * 0.7,  // lower body (above the feet)
        center + side,      // one shoulder
        center - side,      // other shoulder
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // The capsule-centre → feet distance the controller uses (`PLAYER_HALF_HEIGHT +
    // PLAYER_RADIUS`). The lower occlusion sample MUST stay above this, or its ray
    // starts inside the floor the player stands on and the cutaway pins on. Pinned as
    // a literal so this test stays free of an `inf3d_physics` dependency.
    const CENTER_TO_FEET: f32 = 1.0;

    #[test]
    fn lower_sample_stays_above_the_feet() {
        // Regardless of view direction, the lowest silhouette sample must sit strictly
        // above the player's feet so the occlusion ray never starts in the floor block.
        for forward in [
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::NEG_Z,
            Vec3::new(-2.0, -3.0, 0.5),
        ] {
            let center = Vec3::new(4.0, 20.0, -7.0);
            let samples = silhouette_samples(center, forward.normalize());
            let lowest = samples.iter().map(|s| s.y).fold(f32::INFINITY, f32::min);
            assert!(
                lowest > center.y - CENTER_TO_FEET,
                "lowest sample {lowest} dipped to/below the feet ({})",
                center.y - CENTER_TO_FEET
            );
        }
    }

    #[test]
    fn samples_straddle_the_body_laterally() {
        // The two shoulder samples must sit on opposite sides of the body centre in the
        // horizontal plane (so a wall hiding only one side still registers), and the
        // central three must share the player's XZ column.
        let center = Vec3::new(0.0, 10.0, 0.0);
        let samples = silhouette_samples(center, Vec3::new(1.0, -1.2, 0.6).normalize());
        // Central column: head/torso/lower share the centre's XZ.
        for i in 0..3 {
            assert!(
                (samples[i].x - center.x).abs() < 1e-5 && (samples[i].z - center.z).abs() < 1e-5
            );
        }
        // Shoulders are mirror images about the centre in XZ.
        let l = samples[3] - center;
        let r = samples[4] - center;
        assert!(
            (l.x + r.x).abs() < 1e-5 && (l.z + r.z).abs() < 1e-5,
            "shoulders not mirrored"
        );
        assert!(
            Vec2::new(l.x, l.z).length() > 0.2,
            "lateral offset collapsed"
        );
    }
}
