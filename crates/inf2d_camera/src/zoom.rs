use bevy::prelude::*;
use inf2d_input::InputState;

use crate::rig::{CameraRig, CameraTuning};

/// Smooth multiplicative zoom. Reads [`InputState::zoom_delta`] (positive on
/// scroll-up), pushes `zoom_target` via `zoom_step.powf(-delta)` so a single
/// notch up zooms *in* (smaller scale), then exponentially eases `zoom` toward
/// `zoom_target` and writes the result to the orthographic projection.
pub fn zoom_camera(
    time: Res<Time>,
    tuning: Res<CameraTuning>,
    input: Res<InputState>,
    mut q: Query<(&mut CameraRig, &mut Projection)>,
) {
    let Ok((mut rig, mut projection)) = q.single_mut() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection.as_mut() else {
        return;
    };

    if input.zoom_delta != 0.0 {
        rig.zoom_target = (rig.zoom_target * tuning.zoom_step.powf(-input.zoom_delta))
            .clamp(tuning.zoom_min, tuning.zoom_max);
    }

    // Exponential smoothing — frame-rate independent unlike a fixed-fraction lerp.
    let alpha = 1.0 - (-tuning.zoom_smoothing * time.delta_secs()).exp();
    rig.zoom = rig.zoom.lerp(rig.zoom_target, alpha);
    ortho.scale = rig.zoom;
}
