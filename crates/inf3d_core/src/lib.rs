//! Shared core types for the inf3d engine.
//!
//! Everything here is data + lightweight glue (resources, components, enums) so
//! that any other crate — render, world, camera, gameplay, ui — can depend on
//! it without dragging in a heavy module. `CorePlugin` registers the global
//! quality / stats resources; register it **first** in the app so subsequent
//! plugins observe fixed high-quality `QualitySettings` at their own `build` time.

use bevy::platform::collections::HashMap;
use bevy::prelude::*;

/// Explicit ordering backbone for the `Update` schedule. Every `Update` system
/// across the workspace gets `.in_set(GameSet::X)`; `CorePlugin` chains the four
/// variants once so the phase order is fixed regardless of plugin registration
/// order. Fixed-step and `PostUpdate` systems keep their avian-relative ordering
/// (the scheduling spine) instead.
///
/// Order is `Input -> Logic -> Streaming -> Fx`:
/// - [`Input`](GameSet::Input): raw-input reads (camera input, clicks).
/// - [`Logic`](GameSet::Logic): pathfinding, follow-path, animation, interaction.
/// - [`Streaming`](GameSet::Streaming): foliage streaming, prop collider builds.
/// - [`Fx`](GameSet::Fx): dust, highlights, quality application, diagnostics, HUD.
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GameSet {
    Input,
    Logic,
    Streaming,
    Fx,
}

/// Voxel columns `(x, z)` occupied by SOLID props (trees & rocks — never
/// grass). Populated by the foliage scatter system in `inf3d_render` as props
/// spawn, and consumed by the A* pathfinder in `inf3d_pathfinding` so routes
/// detour around props instead of walking into their physics colliders.
///
/// Lives in `inf3d_core` because pathfinding is *upstream* of render and so
/// cannot depend on it — the data crosses the dependency direction through this
/// shared resource (the same pattern as [`FollowTarget`]).
///
/// **Refcounted.** The map value is the number of distinct prop discs (across
/// any number of foliage tiles) currently claiming that cell. A cell is
/// impassable iff its count is `> 0`. Refcounting is mandatory because one cell
/// can legitimately sit inside two props' inflated footprints — both within a
/// single tile AND across a tile boundary (a prop in an edge column inflates its
/// disc by `PLAYER_RADIUS` and spills into the neighbouring tile). Without the
/// count, the first tile to despawn would clear a cell the surviving neighbour
/// still occupies, routing the pathfinder straight into a still-present trunk.
/// Claim with [`BlockedCells::claim`], release with [`BlockedCells::release`].
#[derive(Resource, Default)]
pub struct BlockedCells(pub HashMap<IVec2, u32>);

impl BlockedCells {
    /// Claim `cell` for one prop disc, incrementing its refcount. Returns `true`
    /// the first time the cell transitions from unclaimed → claimed (count went
    /// `0 → 1`), so the caller can record it once per *tile* for later release.
    pub fn claim(&mut self, cell: IVec2) -> bool {
        let count = self.0.entry(cell).or_insert(0);
        *count += 1;
        *count == 1
    }

    /// Release one claim on `cell`, decrementing its refcount and removing the
    /// entry only when it reaches zero (the last claimant left). A release with
    /// no matching claim is ignored.
    pub fn release(&mut self, cell: IVec2) {
        if let Some(count) = self.0.get_mut(&cell) {
            *count -= 1;
            if *count == 0 {
                self.0.remove(&cell);
            }
        }
    }

    /// Whether `cell` is currently claimed by at least one prop disc (impassable).
    pub fn contains(&self, cell: IVec2) -> bool {
        self.0.contains_key(&cell)
    }

    /// Iterate the currently-claimed (impassable) cells.
    pub fn iter(&self) -> impl Iterator<Item = IVec2> + '_ {
        self.0.keys().copied()
    }
}

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

/// Fixed high-quality visual / streaming knobs.
///
/// Runtime presets were removed for now; a real settings module can reintroduce
/// user-facing quality tiers later. Until then this resource is deliberately a
/// small, honest set of fields that are actually consumed by downstream systems.
/// `render_distance_chunks` is read once at world-plugin build, while
/// `terrain_lod_distance` is raised dynamically by the camera so LOD transitions
/// stay outside the current orthographic footprint.
#[derive(Resource, Clone, Debug)]
pub struct QualitySettings {
    pub render_distance_chunks: u32,
    /// World-space radius around the player within which dense grass spawns,
    /// regardless of camera zoom. Caps the zoom-out cost: sparse trees/rocks
    /// still fill the iso view to the edges via the foliage ring, but the
    /// expensive grass carpet is bounded to this circle. `0.0` disables grass.
    pub grass_radius_world: f32,
    pub foliage_enabled: bool,
    pub dof_enabled: bool,
    pub bloom_enabled: bool,
    /// Screen-space ambient occlusion toggle for the camera.
    pub ssao_enabled: bool,
    /// Per-object / camera motion blur toggle for the camera.
    pub motion_blur_enabled: bool,
    pub water_enabled: bool,
    pub water_amplitude: f32,
    /// Maximum foliage tile-ring radius, in tiles. Clamps the dynamic
    /// camera-zoom-driven ring computed in the foliage streamer.
    pub foliage_ring_max: i32,
    /// World-space distance (in voxels/world units) that sets the width of
    /// each terrain LOD band. Chunk LOD level `n` begins at
    /// `n * terrain_lod_distance` from the LOD focus. Consumed by `inf3d_world`'s
    /// `MainWorld::chunk_lod`, which feeds `chunk_data_shape`/`chunk_meshing_shape`
    /// (coarser voxels) and the octave count in the voxel lookup delegate.
    pub terrain_lod_distance: f32,
}

impl Default for QualitySettings {
    fn default() -> Self {
        Self {
            render_distance_chunks: 10,
            grass_radius_world: 60.0,
            foliage_enabled: true,
            dof_enabled: true,
            bloom_enabled: true,
            ssao_enabled: true,
            motion_blur_enabled: true,
            water_enabled: true,
            water_amplitude: 0.45,
            foliage_ring_max: 9,
            // Startup fallback only. The camera raises this from the current zoom
            // footprint before streaming/LOD settles, so LOD 0 covers the view.
            terrain_lod_distance: 165.0,
        }
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
/// own `build` time.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            Update,
            (
                GameSet::Input,
                GameSet::Logic,
                GameSet::Streaming,
                GameSet::Fx,
            )
                .chain(),
        )
        .init_resource::<QualitySettings>()
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
    fn blocked_cells_refcount_survives_cross_tile_release() {
        // Two tiles both claim the same boundary cell (a prop in each tile's edge
        // column inflates into the shared cell). Releasing one tile's claim must
        // NOT free the cell while the other tile still occupies it.
        let cell = IVec2::new(7, 3);
        let mut blocked = BlockedCells::default();

        // Tile A's prop claims it first (0 -> 1, first-claim true).
        assert!(blocked.claim(cell));
        // Tile B's prop claims the same cell (1 -> 2, not first).
        assert!(!blocked.claim(cell));
        assert!(blocked.contains(cell));

        // Tile A despawns / re-streams and releases its one claim.
        blocked.release(cell);
        // Tile B's prop is still physically there → cell stays blocked.
        assert!(
            blocked.contains(cell),
            "cell freed while a neighbour tile's prop still occupies it"
        );

        // Tile B finally releases → now the cell is free.
        blocked.release(cell);
        assert!(!blocked.contains(cell));
        assert!(
            blocked.0.is_empty(),
            "fully-released cell must drop its entry"
        );
    }

    #[test]
    fn blocked_cells_release_without_claim_is_ignored() {
        let mut blocked = BlockedCells::default();
        blocked.release(IVec2::new(1, 1));
        assert!(blocked.0.is_empty());
        assert!(!blocked.contains(IVec2::new(1, 1)));
    }

    #[test]
    fn blocked_cells_iter_yields_claimed_cells_once() {
        let mut blocked = BlockedCells::default();
        let a = IVec2::new(0, 0);
        let b = IVec2::new(2, 5);
        blocked.claim(a);
        blocked.claim(a); // double-claimed, still one logical cell
        blocked.claim(b);
        let mut got: Vec<IVec2> = blocked.iter().collect();
        got.sort_by_key(|c| (c.x, c.y));
        assert_eq!(got, vec![a, b]);
    }

    #[test]
    fn default_quality_settings_are_fixed_high() {
        let s = QualitySettings::default();
        assert_eq!(s.render_distance_chunks, 10);
        assert!(s.foliage_enabled);
        assert!(s.dof_enabled);
        assert!(s.bloom_enabled);
        assert!(s.ssao_enabled);
        assert!(s.motion_blur_enabled);
        assert!(s.water_enabled);
        assert!(s.grass_radius_world > 0.0);
        assert!(s.foliage_ring_max > 0);
        assert!(s.terrain_lod_distance > 0.0);
    }
}
