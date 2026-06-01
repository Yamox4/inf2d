//! Shared core types for the inf3d engine.

use bevy::prelude::*;

/// Marks the entity that camera, fog, and grass should follow/center on (the
/// player). Lives in `inf3d_core` so render/camera can depend on it without
/// depending on `inf3d_gameplay` — this breaks the otherwise-cyclic dependency
/// (gameplay → render → camera → gameplay).
#[derive(Component)]
pub struct FollowTarget;
