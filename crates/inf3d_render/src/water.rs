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
//! `QualitySettings::water_enabled` at app build time. Runtime toggling of
//! plugin registration is not supported, but runtime amplitude tuning is still
//! supported for the future settings UI via `apply_water_quality`.

use bevy::prelude::*;
use bevy_water::material::StandardWaterMaterial;
use bevy_water::{
    WaterPlugin as BevyWaterPlugin, WaterSettings, WaterTile, WaterTiles, WATER_SIZE,
};

use inf3d_core::{FollowTarget, GameSet, QualitySettings};
use inf3d_worldgen::WATER_HEIGHT;

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        // Snapshot the (possibly-overridden) settings at build time to decide
        // whether to register `BevyWaterPlugin`. `QualitySettings` is owned by
        // `CorePlugin`; if `CorePlugin` built first the resource is present, and
        // `unwrap_or_default()` covers the case where it hasn't yet. Plugin
        // registration can't be toggled after the fact, so this is the one
        // chance to honour `water_enabled = false` if future settings disable it.
        let settings = app
            .world()
            .get_resource::<QualitySettings>()
            .cloned()
            .unwrap_or_default();

        app.add_systems(Startup, init_water_settings);

        if settings.water_enabled {
            app.add_plugins(BevyWaterPlugin).add_systems(
                Update,
                (apply_water_quality, follow_water).in_set(GameSet::Fx),
            );
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
        // Fixed high-quality swell size. The voxel scale is 1x1x1 so the default
        // bevy_water amplitude of 1.0 is huge; 0.45 keeps it readable.
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

/// Make the ocean follow the player so it never runs out in the infinite world.
///
/// `bevy_water` spawns a fixed 6×6 grid of 256-unit tiles around the ORIGIN, so
/// past ~768 blocks you walk off it and the water vanishes. This re-centers the
/// whole [`WaterTiles`] grid on the player, snapped to whole 256-unit tiles (so it
/// only jumps a tile at a time — no per-frame swim). Each tile's Gerstner waves are
/// anchored to its `coord_offset` (its world corner), INDEPENDENT of the transform,
/// so we also update that to the tile's new world corner — keeping the water
/// surface continuous in world space as the grid recycles around you.
fn follow_water(
    player: Query<&Transform, (With<FollowTarget>, Without<WaterTiles>)>,
    mut grid: Query<&mut Transform, With<WaterTiles>>,
    tiles: Query<(&WaterTile, &MeshMaterial3d<StandardWaterMaterial>)>,
    mut materials: ResMut<Assets<StandardWaterMaterial>>,
) {
    let Ok(player_tf) = player.single() else {
        return;
    };
    let Ok(mut grid_tf) = grid.single_mut() else {
        return;
    };
    let size = WATER_SIZE as f32;
    let snapped = Vec2::new(
        (player_tf.translation.x / size).round() * size,
        (player_tf.translation.z / size).round() * size,
    );
    // Already centered on the player's tile → nothing to do (and no needless
    // material re-upload).
    if (grid_tf.translation.x - snapped.x).abs() < 0.5
        && (grid_tf.translation.z - snapped.y).abs() < 0.5
    {
        return;
    }
    grid_tf.translation.x = snapped.x;
    grid_tf.translation.z = snapped.y;
    for (tile, mat) in &tiles {
        if let Some(material) = materials.get_mut(&mat.0) {
            // The tile's new world corner = grid origin + its fixed local offset.
            material.extension.coord_offset = snapped + tile.offset;
        }
    }
}

/// Mirror runtime settings changes onto `WaterSettings::amplitude` so the
/// pause-menu / settings UI can resize swell without a restart later. Only runs on
/// frames where `QualitySettings` changed and the bevy_water plugin is
/// registered (`WaterSettings` only exists then).
fn apply_water_quality(quality: Res<QualitySettings>, water: Option<ResMut<WaterSettings>>) {
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
