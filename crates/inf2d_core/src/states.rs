use bevy::prelude::*;

/// Top-level lifecycle of the application. The plugin graph gates feature-set startup
/// against these states using `OnEnter`/`OnExit`/`run_if(in_state(...))`.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AppState {
    /// Window/asset/physics bootstrapping. No gameplay systems run.
    #[default]
    Loading,
    /// World loaded, gameplay loop active.
    InGame,
}

/// Sub-state nested under `AppState::InGame`. Gates per-frame gameplay vs paused UI screens.
#[derive(SubStates, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[source(AppState = AppState::InGame)]
pub enum GameState {
    #[default]
    Playing,
    Paused,
}
