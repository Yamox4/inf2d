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

/// Player movement intent produced by the FPS camera mode. Kept in `core` so the
/// camera crate can write it and gameplay/physics-facing systems can consume it
/// without introducing dependency cycles. `direction` is horizontal world-space
/// input, normalized by the consumer; `jump` is an input request, not physics state.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct FpsMoveIntent {
    pub active: bool,
    pub direction: Vec3,
    pub jump: bool,
    pub sprint: bool,
}

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
#[derive(Resource, Clone, Debug, serde::Serialize, serde::Deserialize)]
// `#[serde(default)]` makes every field optional in the RON: the file overrides
// only the knobs it lists, and any field omitted (or a future field added after
// the file was written) falls back to `Default`. So an old/partial `quality.ron`
// never breaks the load, and tuning one value means writing one line.
#[serde(default)]
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

/// What the mouse does, chosen by the player via the HUD mode buttons. The
/// block-edit system and the pathfinder both read this so exactly one of them
/// acts on a click: editing in `Build`, click-to-move in `Walk`.
#[derive(
    Resource, Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize,
)]
pub enum EditMode {
    /// Normal play — left-click pathfinds (click-to-move). The default.
    #[default]
    Walk,
    /// Editing — left-click places a block on the hovered face, right-click
    /// removes the hovered voxel.
    Build,
}

/// Default placed material — the first entry of `inf3d_world::BUILDABLE`
/// (`TerrainMaterialId::BuiltStone`, raw index 10). It lives here, not in
/// `inf3d_world`, because [`SelectedMaterial`]'s `Default` needs it and `inf3d_core`
/// must not depend on `inf3d_world` (which depends on core). The
/// `inf3d_world::buildable_defaults_align` test asserts this stays equal to
/// `BUILDABLE[0] as u8`, so the literal here can never silently desync.
pub const DEFAULT_BUILD_MATERIAL: u8 = 10;

/// The voxel material the player places in [`EditMode::Build`], chosen via the
/// in-game material picker (`inf3d_ui`) or the number keys. Stored as the raw
/// `MainWorld::MaterialIndex` (`u8`) so `inf3d_core` stays free of a dependency on
/// `inf3d_world` (which owns the `TerrainMaterialId` palette + the `BUILDABLE` list).
/// The picker writes it; the block editor (`inf3d_render`) reads it; save/load
/// persists it. Defaults to [`DEFAULT_BUILD_MATERIAL`].
#[derive(
    Resource, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct SelectedMaterial(pub u8);

impl Default for SelectedMaterial {
    fn default() -> Self {
        Self(DEFAULT_BUILD_MATERIAL)
    }
}

/// Top-level application state. The game boots into [`AppState::MainMenu`] — the
/// world, player, and camera spawn at `Startup` as a live menu backdrop — and
/// enters [`AppState::InGame`] when the player starts or loads a game. The
/// gameplay `GameSet` phases and the fixed-step movement/physics systems run only
/// in `InGame` (and only while not paused); the menu systems run outside that.
#[derive(States, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum AppState {
    #[default]
    MainMenu,
    InGame,
}

/// In-game pause — a sub-state that exists only while [`AppState::InGame`].
/// `Running` is normal play; `Paused` freezes gameplay (the gated `GameSet`
/// phases and the fixed-step systems stop, and avian's `Time<Physics>` is paused)
/// and shows the pause menu. Toggled with Esc. When `AppState` is not `InGame`
/// this sub-state does not exist, and `in_state(Pause::…)` simply reads `false`.
#[derive(SubStates, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[source(AppState = AppState::InGame)]
pub enum Pause {
    #[default]
    Running,
    Paused,
}

/// Relative to `inf3d_core`'s manifest, the live asset tree is one crate over.
/// Baked at compile time (dev/`cargo run` workflow), same hop the foliage loader
/// uses. A missing file is normal (not an error) — the built-in defaults apply.
const QUALITY_CONFIG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../inf3d_app/assets/config/quality.ron"
);

/// Load [`QualitySettings`] from `assets/config/quality.ron`, falling back to the
/// built-in [`Default`] when the file is absent or malformed.
///
/// This is the project's **first data-driven config** — the streaming / foliage /
/// water / post-FX knobs can now be tuned by editing the `.ron` and re-running, no
/// recompile. It also sets the pattern later content (block/material defs, etc.)
/// follows: one typed struct, `#[serde(default)]` for forward-compatible partial
/// files, and a graceful fallback so a bad file degrades to defaults instead of a
/// crash. A parse error is surfaced as a `warn!` so a typo doesn't fail silently.
fn load_quality_settings() -> QualitySettings {
    let Ok(text) = std::fs::read_to_string(QUALITY_CONFIG_PATH) else {
        // No file → defaults. Common and expected; nothing to report.
        return QualitySettings::default();
    };
    match ron::from_str::<QualitySettings>(&text) {
        Ok(settings) => {
            info!("inf3d_core: loaded quality settings from quality.ron");
            settings
        }
        Err(err) => {
            warn!("inf3d_core: quality.ron parse error ({err}); using defaults");
            QualitySettings::default()
        }
    }
}

/// Persist [`QualitySettings`] back to the same `quality.ron` [`load_quality_settings`]
/// reads, so changes made in the in-game settings menu survive a restart.
/// Best-effort: a serialize or write failure is a `warn!`, never a panic — the
/// live settings still apply for the current session regardless.
pub fn save_quality_settings(settings: &QualitySettings) {
    let pretty = ron::ser::PrettyConfig::default();
    match ron::ser::to_string_pretty(settings, pretty) {
        Ok(text) => {
            if let Err(err) = std::fs::write(QUALITY_CONFIG_PATH, text) {
                warn!("inf3d_core: could not write quality.ron ({err}); settings apply this session only");
            } else {
                info!("inf3d_core: saved quality settings to quality.ron");
            }
        }
        Err(err) => warn!("inf3d_core: could not serialize quality settings ({err})"),
    }
}

/// User-facing graphics tiers for the settings menu. Each maps to a bundle of
/// [`QualitySettings`] visual knobs via [`QualityPreset::apply`].
/// `render_distance_chunks` is intentionally NOT touched (it is read once at world
/// build, so changing it needs a restart) — the settings menu surfaces it
/// separately. `water_enabled` is also left on (toggling the water plane fully off
/// needs a restart; presets only vary its amplitude), so a preset never half-
/// disables a build-time system.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum QualityPreset {
    Potato,
    Low,
    Medium,
    High,
}

impl QualityPreset {
    /// The four presets in display order (lowest → highest).
    pub const ALL: [QualityPreset; 4] = [
        QualityPreset::Potato,
        QualityPreset::Low,
        QualityPreset::Medium,
        QualityPreset::High,
    ];

    /// Short label for a settings button.
    pub fn label(self) -> &'static str {
        match self {
            QualityPreset::Potato => "Potato",
            QualityPreset::Low => "Low",
            QualityPreset::Medium => "Medium",
            QualityPreset::High => "High",
        }
    }

    /// Apply this preset's visual knobs onto `q`, leaving `render_distance_chunks`
    /// (restart-only) untouched.
    pub fn apply(self, q: &mut QualitySettings) {
        // Common to all tiers: the water plane stays registered (build-time), only
        // its amplitude varies below.
        q.water_enabled = true;
        match self {
            QualityPreset::Potato => {
                q.foliage_enabled = false;
                q.grass_radius_world = 0.0;
                q.dof_enabled = false;
                q.bloom_enabled = false;
                q.ssao_enabled = false;
                q.motion_blur_enabled = false;
                q.water_amplitude = 0.0;
                q.foliage_ring_max = 4;
                q.terrain_lod_distance = 90.0;
            }
            QualityPreset::Low => {
                q.foliage_enabled = true;
                q.grass_radius_world = 24.0;
                q.dof_enabled = false;
                q.bloom_enabled = true;
                q.ssao_enabled = false;
                q.motion_blur_enabled = false;
                q.water_amplitude = 0.25;
                q.foliage_ring_max = 6;
                q.terrain_lod_distance = 120.0;
            }
            QualityPreset::Medium => {
                q.foliage_enabled = true;
                q.grass_radius_world = 42.0;
                q.dof_enabled = true;
                q.bloom_enabled = true;
                q.ssao_enabled = true;
                q.motion_blur_enabled = false;
                q.water_amplitude = 0.35;
                q.foliage_ring_max = 8;
                q.terrain_lod_distance = 150.0;
            }
            QualityPreset::High => {
                q.foliage_enabled = true;
                q.grass_radius_world = 60.0;
                q.dof_enabled = true;
                q.bloom_enabled = true;
                q.ssao_enabled = true;
                q.motion_blur_enabled = true;
                q.water_amplitude = 0.45;
                q.foliage_ring_max = 9;
                q.terrain_lod_distance = 165.0;
            }
        }
    }
}

/// Registers all engine-wide resources. **Add this plugin first** so other
/// plugins (`WorldPlugin`, `GrassPlugin`, …) see `QualitySettings` at their
/// own `build` time.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AppState>()
            .add_sub_state::<Pause>()
            // Gameplay phases run only during un-paused play. Gating the three
            // gameplay phases here — one lever — stops camera input, pathfinding,
            // streaming, edits, and animation in the menu and when paused.
            //
            // `Fx` is deliberately LEFT UNGATED (only ordered after `Streaming`)
            // so end-of-frame change-detection keeps firing while paused: the
            // settings menu mutates `QualitySettings`, and `apply_quality_to_camera`
            // / `apply_water_quality` / `update_mode_buttons` (gated on
            // `is_changed` / `resource_changed`) live in `Fx`. If they were gated
            // off too, a settings change would only take effect on resume.
            .configure_sets(
                Update,
                (GameSet::Input, GameSet::Logic, GameSet::Streaming)
                    .chain()
                    .run_if(in_state(AppState::InGame).and(in_state(Pause::Running))),
            )
            .configure_sets(Update, GameSet::Fx.after(GameSet::Streaming))
            .insert_resource(load_quality_settings())
            .init_resource::<GrassStats>()
            .init_resource::<FrameStats>()
            .init_resource::<EditMode>()
            .init_resource::<SelectedMaterial>()
            .init_resource::<BlockedCells>()
            .init_resource::<PathTarget>()
            .init_resource::<FpsMoveIntent>();
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
