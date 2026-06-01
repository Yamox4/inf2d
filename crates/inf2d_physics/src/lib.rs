#![deny(unsafe_code)]
//! Avian2D physics integration. Builds per-chunk compound colliders from solid tile data.
//! No gravity (top-down iso game).

mod layers;
mod tile_colliders;

use avian2d::prelude::*;
use bevy::prelude::*;
use inf2d_core::{SimulationSet, TILE_WIDTH};

pub use layers::GameLayer;
pub use tile_colliders::{attach_chunk_colliders, build_chunk_collider_components, ChunkCollider};

/// SystemSet inside [`SimulationSet`] where collider construction runs. Other crates
/// can order against this if they need to observe freshly attached colliders.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub struct PhysicsBuildSet;

/// Registers the avian2d physics plugins (configured for a top-down game: no gravity,
/// `TILE_WIDTH` length unit) and the per-chunk collider builder.
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsPlugins::default().with_length_unit(TILE_WIDTH))
            .insert_resource(Gravity(Vec2::ZERO))
            .configure_sets(Update, PhysicsBuildSet.in_set(SimulationSet))
            .add_systems(
                Update,
                tile_colliders::attach_chunk_colliders.in_set(PhysicsBuildSet),
            );
    }
}

/// Add alongside [`PhysicsPlugin`] to enable Avian's built-in debug renderer (collider
/// outlines, contacts, etc.).
pub struct PhysicsDebugPlugin;

impl Plugin for PhysicsDebugPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(avian2d::prelude::PhysicsDebugPlugin::default());
    }
}
