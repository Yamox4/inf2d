//! Shared core types for the inf3d engine.
//!
//! Everything here is data + lightweight glue (resources, components, enums) so
//! that any other crate — render, world, camera, gameplay, ui — can depend on
//! it without dragging in a heavy module. `CorePlugin` registers the global
//! quality / stats resources; register it **first** in the app so subsequent
//! plugins observe non-default `QualitySettings` at their own `build` time.

use bevy::platform::collections::HashSet;
use bevy::prelude::*;

/// Voxel columns `(x, z)` occupied by SOLID props (trees & rocks — never
/// grass). Populated by the foliage scatter system in `inf3d_render` as props
/// spawn, and consumed by the A* pathfinder in `inf3d_pathfinding` so routes
/// detour around props instead of walking into their physics colliders.
///
/// Lives in `inf3d_core` because pathfinding is *upstream* of render and so
/// cannot depend on it — the data crosses the dependency direction through this
/// shared resource (the same pattern as [`FollowTarget`]).
#[derive(Resource, Default)]
pub struct BlockedCells(pub HashSet<IVec2>);

/// The current click-to-move destination cell `(x, z)`, or `None` when the
/// player is idle / has arrived. Set by `inf3d_pathfinding` when a click
/// produces a path, cleared by `inf3d_gameplay` when the player reaches it, and
/// read by `inf3d_render` to draw a persistent destination highlight.
#[derive(Resource, Default)]
pub struct PathTarget(pub Option<IVec2>);

/// Marks the entity that camera, fog, and grass should follow/center on (the
/// player). Lives in `inf3d_core` so render/camera can depend on it without
/// depending on `inf3d_gameplay` — this breaks the otherwise-cyclic dependency
/// (gameplay → render → camera → gameplay).
#[derive(Component)]
pub struct FollowTarget;

/// Marker for a harvestable wood resource (a scattered tree). Gameplay systems
/// (chop-down, drop-loot, etc.) can find trees with `Query<&Tree>`. The visual
/// is provided by the foliage scatter system in `inf3d_render`.
#[derive(Component, Clone, Copy, Debug)]
pub struct Tree;

/// Marker for a harvestable stone resource (a scattered rock). Same pattern as
/// [`Tree`] — gameplay finds rocks via `Query<&Rock>`.
#[derive(Component, Clone, Copy, Debug)]
pub struct Rock;

/// Discrete visual-quality tiers. Each preset maps deterministically to a
/// [`QualitySettings`] via [`QualitySettings::from_preset`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualityPreset {
    Potato,
    Low,
    Medium,
    High,
}

impl QualityPreset {
    /// Cycles forward through the presets: Potato → Low → Medium → High →
    /// Potato. Used by the F2 keybinding in the HUD plugin.
    pub fn cycle(self) -> Self {
        match self {
            QualityPreset::Potato => QualityPreset::Low,
            QualityPreset::Low => QualityPreset::Medium,
            QualityPreset::Medium => QualityPreset::High,
            QualityPreset::High => QualityPreset::Potato,
        }
    }

    /// Human-readable name for HUD display.
    pub fn name(self) -> &'static str {
        match self {
            QualityPreset::Potato => "Potato",
            QualityPreset::Low => "Low",
            QualityPreset::Medium => "Medium",
            QualityPreset::High => "High",
        }
    }
}

/// Global visual / streaming knobs. Read by world (render distance), render
/// (grass density / dof / bloom / water), and ui (HUD readout +
/// F2 cycle). Most fields take effect each frame when `is_changed()` fires;
/// `render_distance_chunks` is read **once** at plugin build time (the voxel
/// world plugin doesn't re-register).
#[derive(Resource, Clone, Debug)]
pub struct QualitySettings {
    pub preset: QualityPreset,
    pub render_distance_chunks: u32,
    pub grass_enabled: bool,
    pub grass_radius_tiles: i32,
    pub grass_density: f32,
    pub grass_lod_enabled: bool,
    pub foliage_enabled: bool,
    pub dof_enabled: bool,
    pub bloom_enabled: bool,
    pub water_enabled: bool,
    pub water_amplitude: f32,
    /// Maximum foliage tile-ring radius, in tiles. Clamps the dynamic
    /// camera-zoom-driven ring computed in the foliage streamer.
    pub foliage_ring_max: i32,
    /// World-space distance past which foliage variants render at lower
    /// detail (meshlet pipelines pick coarser clusters; non-meshlet meshes
    /// can swap to a cheaper representation if available).
    pub foliage_lod_distance: f32,
    /// World-space distance (in voxels/world units) that sets the width of
    /// each terrain LOD band. Chunk LOD level `n` begins at
    /// `n * terrain_lod_distance` from the camera. Consumed by `inf3d_world`'s
    /// `MainWorld::chunk_lod`, which feeds `chunk_data_shape`/`chunk_meshing_shape`
    /// (coarser voxels) and the octave count in the voxel lookup delegate.
    pub terrain_lod_distance: f32,
}

impl QualitySettings {
    /// Canonical mapping from preset → concrete settings. Keep this in sync
    /// with the unit test's round-trip assertion.
    pub fn from_preset(p: QualityPreset) -> Self {
        match p {
            QualityPreset::Potato => Self {
                preset: p,
                render_distance_chunks: 5,
                grass_enabled: false,
                grass_radius_tiles: 0,
                grass_density: 0.0,
                grass_lod_enabled: false,
                foliage_enabled: false,
                dof_enabled: false,
                bloom_enabled: false,
                water_enabled: false,
                water_amplitude: 0.0,
                foliage_ring_max: 2,
                foliage_lod_distance: 40.0,
                terrain_lod_distance: 150.0,
            },
            QualityPreset::Low => Self {
                preset: p,
                render_distance_chunks: 8,
                grass_enabled: true,
                grass_radius_tiles: 2,
                grass_density: 0.22,
                grass_lod_enabled: true,
                foliage_enabled: true,
                dof_enabled: false,
                bloom_enabled: false,
                water_enabled: true,
                water_amplitude: 0.20,
                foliage_ring_max: 3,
                foliage_lod_distance: 70.0,
                terrain_lod_distance: 220.0,
            },
            QualityPreset::Medium => Self {
                preset: p,
                render_distance_chunks: 12,
                grass_enabled: true,
                grass_radius_tiles: 3,
                grass_density: 0.35,
                grass_lod_enabled: true,
                foliage_enabled: true,
                dof_enabled: false,
                bloom_enabled: true,
                water_enabled: true,
                water_amplitude: 0.35,
                foliage_ring_max: 4,
                foliage_lod_distance: 100.0,
                terrain_lod_distance: 300.0,
            },
            QualityPreset::High => Self {
                preset: p,
                render_distance_chunks: 16,
                grass_enabled: true,
                grass_radius_tiles: 4,
                grass_density: 0.5,
                grass_lod_enabled: true,
                foliage_enabled: true,
                dof_enabled: true,
                bloom_enabled: true,
                water_enabled: true,
                water_amplitude: 0.45,
                foliage_ring_max: 6,
                foliage_lod_distance: 160.0,
                terrain_lod_distance: 450.0,
            },
        }
    }
}

impl Default for QualitySettings {
    fn default() -> Self {
        Self::from_preset(QualityPreset::Medium)
    }
}

/// Live grass-system metrics surfaced in the HUD. Written by the grass plugin
/// (in `inf3d_render`) and read by the HUD; kept here so neither crate has to
/// depend on the other.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct GrassStats {
    pub active_tiles: usize,
    pub vertex_count: usize,
    pub mesh_asset_count: usize,
}

/// Smoothed frame-time stats. The HUD owns the rolling-window computation and
/// writes the p95 here so other systems / debug overlays can read it cheaply.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct FrameStats {
    pub ms_p95: f32,
}

/// Registers all engine-wide resources. **Add this plugin first** so other
/// plugins (`WorldPlugin`, `GrassPlugin`, …) see `QualitySettings` at their
/// own `build` time and can read the preset.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<QualitySettings>()
            .init_resource::<GrassStats>()
            .init_resource::<FrameStats>()
            .init_resource::<BlockedCells>()
            .init_resource::<PathTarget>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_visits_every_preset_and_returns_to_start() {
        let start = QualityPreset::Potato;
        let mut p = start;
        let mut seen = vec![p];
        for _ in 0..3 {
            p = p.cycle();
            seen.push(p);
        }
        assert_eq!(
            seen,
            vec![
                QualityPreset::Potato,
                QualityPreset::Low,
                QualityPreset::Medium,
                QualityPreset::High,
            ]
        );
        assert_eq!(p.cycle(), start);
    }

    #[test]
    fn from_preset_round_trips_preset_field() {
        for p in [
            QualityPreset::Potato,
            QualityPreset::Low,
            QualityPreset::Medium,
            QualityPreset::High,
        ] {
            let s = QualitySettings::from_preset(p);
            assert_eq!(s.preset, p, "preset field must echo input");
        }
    }

    #[test]
    fn default_quality_settings_is_medium() {
        assert_eq!(QualitySettings::default().preset, QualityPreset::Medium);
    }

    #[test]
    fn render_distance_monotonically_non_decreasing_with_tier() {
        let order = [
            QualityPreset::Potato,
            QualityPreset::Low,
            QualityPreset::Medium,
            QualityPreset::High,
        ];
        let settings: Vec<QualitySettings> = order
            .iter()
            .map(|p| QualitySettings::from_preset(*p))
            .collect();

        let dists: Vec<u32> = settings.iter().map(|s| s.render_distance_chunks).collect();
        for w in dists.windows(2) {
            assert!(w[0] <= w[1], "render distance must not decrease: {:?}", dists);
        }

        let ring_max: Vec<i32> = settings.iter().map(|s| s.foliage_ring_max).collect();
        for w in ring_max.windows(2) {
            assert!(
                w[0] <= w[1],
                "foliage_ring_max must not decrease: {:?}",
                ring_max
            );
        }

        let foliage_lod: Vec<f32> = settings.iter().map(|s| s.foliage_lod_distance).collect();
        for w in foliage_lod.windows(2) {
            assert!(
                w[0] <= w[1],
                "foliage_lod_distance must not decrease: {:?}",
                foliage_lod
            );
        }

        let terrain_lod: Vec<f32> = settings.iter().map(|s| s.terrain_lod_distance).collect();
        for w in terrain_lod.windows(2) {
            assert!(
                w[0] <= w[1],
                "terrain_lod_distance must not decrease: {:?}",
                terrain_lod
            );
        }
    }
}
