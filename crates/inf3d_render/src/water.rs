//! Animated ocean via the `bevy_water` crate — Gerstner waves with lit, shaded
//! water. It tiles a global plane at [`WATER_HEIGHT`]; terrain taller than that
//! occludes it, so water only shows on the low seafloor flats (exactly where the
//! seafloor/"water" voxels are exposed).
//!
//! Note: `bevy_water` loads its WGSL from `assets/shaders/` (shipped in this
//! crate's `assets/`). Without those files the water silently fails to render.
//!
//! `bevy_water` provides moving Gerstner waves, normal-based lighting/specular
//! (sun glints), built-in environment (sky) reflections, and — now that the
//! custom terrain material writes the depth prepass and the camera carries
//! `DepthPrepass` whenever water is enabled — a depth-based deep/shallow color
//! blend with shoreline foam. Full screen-space reflections (the `ssr` feature)
//! are still off: bevy_water's SSR uses the deferred render path, which the
//! forward-only voxel terrain material doesn't feed.
//!
//! Wave `amplitude` is driven by `inf3d_core::QualitySettings`. Visual colors
//! and direction stay hard-coded — they tune the look, not the perf cost.
//! Whether `BevyWaterPlugin` is registered at all depends on
//! `QualitySettings::water_enabled` at app build time; runtime toggling of
//! plugin registration is not supported (Bevy plugin registration cannot be
//! cleanly unwound mid-run), but runtime amplitude tuning IS supported via
//! `apply_water_quality`.

use bevy::prelude::*;
use bevy_water::{WaterPlugin as BevyWaterPlugin, WaterSettings};

use inf3d_core::QualitySettings;
use inf3d_worldgen::WATER_HEIGHT;

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        // Defensively ensure QualitySettings exists; another plugin may have
        // inserted it with non-default values before us, in which case
        // `init_resource` is a no-op.
        app.init_resource::<QualitySettings>();

        // Snapshot the (possibly-overridden) settings at build time to decide
        // whether to register `BevyWaterPlugin`. Plugin registration can't be
        // toggled after the fact, so this is the one chance to honour
        // `water_enabled = false` (Potato preset).
        let settings = app
            .world()
            .get_resource::<QualitySettings>()
            .cloned()
            .unwrap_or_default();

        app.add_systems(Startup, init_water_settings);

        if settings.water_enabled {
            app.add_plugins(BevyWaterPlugin)
                .add_systems(Update, apply_water_quality);
        }
    }
}

/// Insert (or overwrite) `WaterSettings` once at startup, sourcing the
/// `amplitude` from the live `QualitySettings`. All other fields are tuned for
/// the project's triple-A ocean look and are deliberately not exposed to the
/// quality presets.
fn init_water_settings(mut commands: Commands, quality: Res<QualitySettings>) {
    commands.insert_resource(WaterSettings {
        height: WATER_HEIGHT,
        // Quality-driven swell size. The voxel scale is 1x1x1 so the default
        // bevy_water amplitude of 1.0 is huge — presets keep it in the
        // 0.06..0.15 range.
        amplitude: quality.water_amplitude,
        // BRIGHT base tint so the water always reads "lit up" rather than going
        // dark at angles where the sun glint doesn't hit — the user wants the
        // bright, reflective look from every viewing angle. The wave specular
        // (sun glint) then just adds extra shimmer on top of this bright base.
        base_color: Color::srgba(0.32, 0.58, 0.70, 1.0),
        // LOW clarity so the water's own (bright) color dominates over the bottom.
        clarity: 0.18,
        // Brightened deep/shallow so even un-glinted water stays a bright blue
        // instead of dark navy. Raise these further for an even more luminous look.
        deep_color: Color::srgba(0.18, 0.45, 0.60, 1.0),
        shallow_color: Color::srgba(0.40, 0.70, 0.78, 1.0),
        // Bright, wide shoreline foam where the water meets land (depth-driven).
        edge_color: Color::srgba(0.95, 1.0, 1.0, 1.0),
        edge_scale: 0.4,
        wave_direction: Vec2::new(1.0, 0.6),
        // `water_quality` defaults to `WaterQuality::Ultra` (max wave/normal
        // detail) and `spawn_tiles` to a large grid — both kept via `..default()`.
        ..default()
    });
}

/// Mirror runtime preset changes onto `WaterSettings::amplitude` so the
/// pause-menu / settings UI can resize swell without a restart. Only runs on
/// frames where `QualitySettings` changed and the bevy_water plugin is
/// registered (`WaterSettings` only exists then).
fn apply_water_quality(
    quality: Res<QualitySettings>,
    water: Option<ResMut<WaterSettings>>,
) {
    if !quality.is_changed() {
        return;
    }
    let Some(mut water) = water else {
        return;
    };
    if water.amplitude != quality.water_amplitude {
        water.amplitude = quality.water_amplitude;
    }
}
