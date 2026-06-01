//! Animated ocean via the `bevy_water` crate — Gerstner waves with lit, shaded
//! water. It tiles a global plane at [`WATER_HEIGHT`]; terrain taller than that
//! occludes it, so water only shows on the low seafloor flats (exactly where the
//! seafloor/"water" voxels are exposed).
//!
//! Note: `bevy_water` loads its WGSL from `assets/shaders/` (shipped in this
//! crate's `assets/`). Without those files the water silently fails to render.
//!
//! `bevy_water` provides moving Gerstner waves, normal-based lighting/specular
//! (sun glints), and built-in environment (sky) reflections; the water also
//! receives directional-light shadows via the `bevy_pbr` import. True
//! screen-space reflections of the *player/terrain* are NOT available because
//! the voxel terrain opts out of the depth prepass (the `ssr` feature broke
//! rendering), so we rely on the built-in reflections + specular instead.

use bevy::prelude::*;
use bevy_water::{WaterPlugin as BevyWaterPlugin, WaterSettings};

use inf3d_worldgen::WATER_HEIGHT;

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(WaterSettings {
            height: WATER_HEIGHT,
            // Triple-A ocean tuning: bigger moving Gerstner swell, richer depth
            // tint, deep-ocean blue to teal shallows, brighter shore foam, and a
            // clear directional swell. water_quality stays at its default (Ultra).
            amplitude: 1.0,
            clarity: 0.4,
            deep_color: Color::srgba(0.02, 0.12, 0.22, 1.0),
            shallow_color: Color::srgba(0.10, 0.42, 0.55, 1.0),
            edge_color: Color::srgba(0.85, 0.95, 1.0, 1.0),
            edge_scale: 0.25,
            wave_direction: Vec2::new(1.0, 0.6),
            ..default()
        })
        .add_plugins(BevyWaterPlugin);
    }
}
