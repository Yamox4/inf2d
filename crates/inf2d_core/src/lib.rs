#![deny(unsafe_code)]
#![doc = include_str!("../README.md")]
#![cfg_attr(not(doctest), doc = "")]

mod coords;
mod iso;
pub mod rng;
mod sets;
mod states;

use bevy::prelude::*;

pub use coords::{ChunkPos, LocalTilePos, WorldTile, CHUNK_SIZE, CHUNK_TILES};
pub use iso::{
    chunk_center_world, chunk_origin_world, tile_center_world, tile_to_world,
    tile_to_world_with_height, world_to_tile, HEIGHT_STEP_PX, TILE_HEIGHT, TILE_WIDTH,
};
pub use rng::{chunk_rng, mix_seed, splitmix64};
pub use sets::{CoreSet, RenderPrepSet, SimulationSet};
pub use states::{AppState, GameState};

/// Registers shared `Reflect` types, `SystemSet` ordering, and app-level states.
///
/// Add this once at the top of the plugin graph; every other inf2d crate
/// assumes these resources/types are already registered.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AppState>()
            .add_sub_state::<GameState>()
            .register_type::<WorldTile>()
            .register_type::<ChunkPos>()
            .register_type::<LocalTilePos>();

        // Cross-crate ordering: Core → Simulation → RenderPrep, applied in `Update`.
        // `SimulationSet` is additionally gated on `GameState::Playing` so a
        // paused game freezes gameplay (chunk streaming, AI, walk) while the
        // camera + UI continue to tick.
        app.configure_sets(
            Update,
            (
                CoreSet,
                SimulationSet.run_if(in_state(GameState::Playing)),
                RenderPrepSet,
            )
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
    }
}
