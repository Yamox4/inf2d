//! Animated ocean via the `bevy_water` crate — Gerstner waves with lit, shaded
//! water. It tiles a global plane at [`WATER_HEIGHT`]; terrain taller than that
//! occludes it, so water only shows on the low seafloor flats (exactly where the
//! seafloor/"water" voxels are exposed).
//!
//! Note: `bevy_water` loads its WGSL from `assets/shaders/` (shipped in this
//! crate's `assets/`). Without those files the water silently fails to render.

use bevy::prelude::*;
use bevy_water::{WaterPlugin as BevyWaterPlugin, WaterSettings};

use crate::world::WATER_HEIGHT;

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(WaterSettings {
            height: WATER_HEIGHT,
            // Livelier moving swell that laps against the shore blocks, with a
            // soft tropical teal palette. SSR (enabled via the crate feature)
            // adds shiny reflections of the player/sky.
            amplitude: 0.8,
            clarity: 0.25,
            deep_color: Color::srgba(0.07, 0.27, 0.41, 1.0),
            shallow_color: Color::srgba(0.24, 0.60, 0.68, 1.0),
            edge_color: Color::srgba(0.78, 0.93, 0.97, 1.0),
            edge_scale: 0.18,
            ..default()
        })
        .add_plugins(BevyWaterPlugin);
    }
}
