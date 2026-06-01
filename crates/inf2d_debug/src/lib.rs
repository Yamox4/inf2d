#![deny(unsafe_code)]
//! Debug overlays.
//!
//! - F3 toggles the bevy-inspector-egui world inspector.
//! - F4 should toggle Avian's physics debug renderer — wired by `inf2d_physics`,
//!   listed here for discoverability.
//! - F5 toggles chunk-border gizmos.

mod gizmos;
mod inspector;

use bevy::prelude::*;
use inf2d_core::CoreSet;

pub use gizmos::ChunkGizmosEnabled;
pub use inspector::InspectorState;

/// Plugin: world inspector (F3) + chunk-border gizmos (F5).
pub struct DebugPlugin;

impl Plugin for DebugPlugin {
    fn build(&self, app: &mut App) {
        inspector::add_world_inspector(app);
        app.init_resource::<ChunkGizmosEnabled>().add_systems(
            Update,
            (gizmos::toggle_chunk_gizmos, gizmos::draw_chunk_gizmos).in_set(CoreSet),
        );
    }
}
