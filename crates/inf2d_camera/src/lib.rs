#![deny(unsafe_code)]
//! Camera rig (pan/zoom/picking) for the iso world.
//!
//! Add [`CameraPlugin`] after [`inf2d_core::CorePlugin`], [`inf2d_input::InputPlugin`],
//! and [`inf2d_world::WorldPlugin`]. The plugin spawns one [`CameraRigBundle`] at
//! `Startup`, wires drag-to-pan + scroll-zoom in [`inf2d_core::CoreSet`] (so the
//! camera is up-to-date before chunk streaming runs in `SimulationSet`), and runs
//! the cursor → tile picker in [`inf2d_core::RenderPrepSet`].

mod pan;
mod picking;
mod rig;
mod shake;
mod zoom;

use bevy::prelude::*;
use inf2d_core::{CoreSet, RenderPrepSet};

pub use picking::{update_cursor_pick, CursorPick};
pub use rig::{spawn_camera, CameraMode, CameraRig, CameraRigBundle, CameraTuning};
pub use shake::{drive_shake, process_shake_requests, ActiveShake, ShakeRequest};

/// Registers camera tunables, the cursor-pick resource, spawns the rig, and wires
/// the pan / zoom / picking systems into the inf2d system-set ordering.
pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraTuning>()
            .init_resource::<CursorPick>()
            .add_message::<ShakeRequest>()
            .register_type::<CameraRig>()
            .register_type::<CameraTuning>()
            .register_type::<CursorPick>()
            .add_systems(Startup, rig::spawn_camera)
            .add_systems(
                Update,
                (
                    // Shake first (consume requests, advance envelope → writes
                    // `rig.shake`), then pan / zoom read the updated offset and
                    // fold it into the camera transform. All in CoreSet so the
                    // camera is up-to-date before SimulationSet (chunk streaming
                    // reads the camera's transform there).
                    (
                        shake::process_shake_requests,
                        shake::drive_shake,
                        pan::pan_camera,
                        zoom::zoom_camera,
                    )
                        .chain()
                        .in_set(CoreSet),
                    // Picking last: after all camera updates this frame.
                    picking::update_cursor_pick.in_set(RenderPrepSet),
                ),
            );
    }
}
