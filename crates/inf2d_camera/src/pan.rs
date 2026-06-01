use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use inf2d_input::InputState;

use crate::rig::{CameraMode, CameraRig, CameraTuning};

/// Drag-to-pan: while [`InputState::pan_drag`] is true, accumulate this frame's
/// mouse motion and translate the rig's `target` so the world point under the
/// cursor stays under the cursor (world-delta = screen-delta * projection.scale).
///
/// Sign convention: dragging the mouse right (+screen-x) should pan the *view*
/// right, meaning the camera target moves *left* (-world-x). Bevy's screen-y is
/// down-positive and world-y is up-positive, so the y axis is intentionally
/// passed through unflipped: `delta.y > 0` (cursor moving down) shifts the
/// target by `+y` (world-up), which scrolls the view down.
pub fn pan_camera(
    input: Res<InputState>,
    tuning: Res<CameraTuning>,
    mut mouse_motion: MessageReader<MouseMotion>,
    mut q: Query<(&mut CameraRig, &Projection, &mut Transform)>,
) {
    let mut total = Vec2::ZERO;
    for ev in mouse_motion.read() {
        total += ev.delta;
    }

    let Ok((mut rig, projection, mut transform)) = q.single_mut() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };

    if input.pan_drag && total != Vec2::ZERO {
        // Pan-drag wins over follow: as soon as the user grabs the camera, kick out of
        // Follow/Cinematic back into Free. Re-engaging follow is a separate input.
        if !matches!(rig.mode, CameraMode::Free) {
            rig.mode = CameraMode::Free;
        }

        let scale = ortho.scale * tuning.pan_speed;
        let world_delta = Vec2::new(-total.x, total.y) * scale;
        rig.target += world_delta;
    }

    // Reconcile the transform every frame so shake (and any other transient
    // offset on `rig.shake`) is visible even when there's no pan input and
    // we're not in Follow mode. The follow-camera driver in `inf2d_gameplay`
    // does the equivalent reconciliation for Follow mode.
    if matches!(rig.mode, CameraMode::Free) {
        transform.translation.x = rig.target.x + rig.shake.x;
        transform.translation.y = rig.target.y + rig.shake.y;
    }
}
