use bevy::prelude::*;

/// Runs first: input sampling, time-of-day updates, anything that produces the frame's
/// authoritative input/state for downstream systems.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub struct CoreSet;

/// Gameplay simulation: chunk streaming, generation, AI ticks, gameplay mutations.
/// Most game logic lives here.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub struct SimulationSet;

/// Last: camera follows the simulation, render-side data is extracted for the GPU. Anything
/// that needs to see the final per-frame world state but must run before Bevy's render stage.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub struct RenderPrepSet;
