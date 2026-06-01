#![deny(unsafe_code)]
//! Deterministic procedural worldgen plugin.
//!
//! Inserts a [`BiomeParams`] resource and an [`inf2d_world::ActiveGenerator`] backed by
//! [`BiomeGenerator`]. If a user inserts a custom [`BiomeParams`] before adding this plugin,
//! that one is honored; otherwise the documented defaults are used.

pub mod biome;
pub mod params;

use bevy::prelude::*;
use inf2d_world::{ActiveGenerator, WorldSeed};

pub use biome::BiomeGenerator;
pub use params::{BiomeParams, DEFAULT_WORLD_SEED};

/// Bevy plugin: register types, seat the params resource, and install the active generator.
///
/// Must be added after `inf2d_world::WorldPlugin` so the fallback `FlatGenerator` is
/// replaced rather than overwriting our generator with the fallback.
pub struct WorldgenPlugin;

impl Plugin for WorldgenPlugin {
    fn build(&self, app: &mut App) {
        let params = app
            .world()
            .get_resource::<BiomeParams>()
            .cloned()
            .unwrap_or_default();

        if let Err(msg) = params.validate() {
            tracing::warn!(
                target: "inf2d_worldgen",
                "BiomeParams failed validation: {msg}; proceeding anyway"
            );
        }

        app.register_type::<BiomeParams>()
            .insert_resource(params.clone())
            // Propagate the world seed into `inf2d_world` so the prop scatter
            // (which can't depend on inf2d_worldgen without cycling) shares the
            // same deterministic seed as terrain generation.
            .insert_resource(WorldSeed(params.world_seed))
            .insert_resource(ActiveGenerator::new(BiomeGenerator::new(params)));
    }
}
