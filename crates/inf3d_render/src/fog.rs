//! Background clear color only.
//!
//! This used to spawn a screen-space "cover-fog" band at the bottom of the
//! screen to mask the foreground terrain reveal. It was removed at the user's
//! request — opaque enough to hide the reveal, it read as a black bar. The
//! foreground-occlusion problem is better solved at the camera/terrain level
//! (see notes in chat) than by painting over the bottom of the frame.

use bevy::prelude::*;

/// Cool horizon tone used as the clear color, so the world edge dissolves into a
/// consistent backdrop instead of pure black where no chunk is drawn.
const HORIZON: Color = Color::srgb(0.60, 0.67, 0.71);

pub struct FogPlugin;

impl Plugin for FogPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(HORIZON));
    }
}
