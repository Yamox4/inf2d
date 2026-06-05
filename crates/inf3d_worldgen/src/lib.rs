//! Procedural terrain generation and the shared height oracle used by both
//! meshing (on worker threads) and gameplay (pathfinding/standing).

use bevy::prelude::*;
use inf3d_city::CITY_SURFACE_HEIGHT;
use noise::{HybridMulti, NoiseFn, Perlin};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

/// World-space height of the water surface (the `bevy_water` plane). The lowest
/// terrain tier (the seafloor, material 3) stands at `y = 1`; land tiers stand
/// at `y >= 2`. Water at 1.6 therefore submerges only the seafloor flats and
/// leaves land dry. A column is "water" (unwalkable) when its standing height
/// is below this. Single source of truth shared with `water.rs` + pathfinding.
pub const WATER_HEIGHT: f32 = 1.6;

/// Canonical octave count at full (LOD 0) detail.
pub const TERRAIN_OCTAVES: usize = 5;

/// Topmost-solid Y of every column in the **flat test world** (the lab level used
/// by the test map). High enough to stand well above [`WATER_HEIGHT`] and leave
/// room to dig a water basin below the feet, low enough to keep stacked test
/// structures on-screen.
pub const FLAT_SURFACE_Y: i32 = 8;

/// Continuous surface height the flat world feeds to the SAME `surface < y` /
/// [`ColumnKind::from_height`] logic the procedural path uses, so the oracle and
/// the mesher agree. The `+ 0.5` keeps it off the integer boundary so
/// `floor()` lands cleanly on [`FLAT_SURFACE_Y`] (a solid at exactly `y` would be
/// ambiguous under `y < surface`).
pub const FLAT_SURFACE_HEIGHT: f64 = FLAT_SURFACE_Y as f64 + 0.5;

/// Base world backend selected for procedural voxel lookups.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum WorldKind {
    /// Normal noise terrain.
    #[default]
    Normal = 0,
    /// Flat test lab, optionally stamped with test structures by the menu.
    TestFlat = 1,
    /// Deterministic infinite cyberpunk city backend (`inf3d_city`).
    City = 2,
}

impl WorldKind {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::TestFlat,
            2 => Self::City,
            _ => Self::Normal,
        }
    }
}

/// Shared, cheap-to-clone selector for the base world backend. Mirrors the
/// [`VoxelOverrides`] sharing pattern: ONE selector is cloned into the [`Terrain`]
/// oracle, the meshing config, and exposed as a resource, so the surface the oracle
/// reports and the surface the mesher builds can never disagree. `Relaxed` ordering
/// is fine — a one-frame stale read across a switch/remesh boundary is harmless.
#[derive(Resource, Clone, Default)]
pub struct WorldGen(Arc<AtomicU8>);

impl WorldGen {
    pub fn new() -> Self {
        Self::default()
    }

    /// Selected base world backend.
    pub fn kind(&self) -> WorldKind {
        WorldKind::from_u8(self.0.load(Ordering::Relaxed))
    }

    /// Select the base world backend. The caller must re-mesh resident chunks
    /// afterwards for the new surface to show.
    pub fn set_kind(&self, kind: WorldKind) {
        self.0.store(kind as u8, Ordering::Relaxed);
    }

    /// Whether the world is the flat test world (`true`) rather than procedural.
    pub fn is_flat(&self) -> bool {
        self.kind() == WorldKind::TestFlat
    }

    /// Select the flat test world (`true`) or procedural terrain (`false`). The
    /// caller must re-mesh resident chunks afterwards for the new surface to show.
    pub fn set_flat(&self, flat: bool) {
        self.set_kind(if flat {
            WorldKind::TestFlat
        } else {
            WorldKind::Normal
        });
    }
}

/// Build the terrain noise with the canonical parameters. Used in two places:
/// the meshing delegate (per worker thread) and the [`Terrain`] gameplay oracle.
pub fn build_noise() -> HybridMulti<Perlin> {
    build_noise_lod(0)
}

/// Build the terrain noise with an octave count reduced for the given voxel
/// LOD level. Coarser LODs (larger `lod`) sample fewer octaves, which both
/// cheapens generation on the worker threads and avoids encoding
/// high-frequency surface detail that a downsampled (coarse) chunk mesh can't
/// represent anyway. The gameplay oracle ([`Terrain`]) always uses LOD 0 so
/// pathfinding/standing stay consistent with the finest visible geometry.
///
/// At least two octaves are always kept so the broad landmass shape survives.
pub fn build_noise_lod(lod: u8) -> HybridMulti<Perlin> {
    let mut noise = HybridMulti::<Perlin>::new(1234);
    noise.octaves = TERRAIN_OCTAVES.saturating_sub(lod as usize).max(2);
    noise.frequency = 1.1;
    noise.lacunarity = 2.8;
    noise.persistence = 0.4;
    noise
}

/// Vertical bias (in voxels) added to EVERY terrain height sample — THE knob for
/// the land/water balance. The terrain noise (`HybridMulti<Perlin>`) sits near 0,
/// and a column is WATER whenever its height is below 1.0 (see
/// [`ColumnKind::from_height`] + [`WATER_HEIGHT`]), so with no bias roughly half
/// the world falls below the water line and reads as ocean. Lifting every column
/// shifts the coastline out to lower-noise columns → more dry land. Coasts stay
/// gentle (a column right at the waterline still has height ~1, because the noise
/// is smooth) and deep basins stay ocean. Each +1 lowers the water cutoff by
/// ~0.02 in noise units. Raise for more land, lower (toward 0) for more water.
pub const LAND_BIAS: f64 = 6.0;

/// Raw terrain height sample (in voxel units) at a world column. Solid voxels
/// fill `y < sample` (plus a sea floor below `y = 1`). Includes [`LAND_BIAS`], so
/// meshing and the [`Terrain`] oracle stay consistent (both go through here).
pub fn sample_height(noise: &HybridMulti<Perlin>, x: i32, z: i32) -> f64 {
    noise.get([x as f64 / 1000.0, z as f64 / 1000.0]) * 50.0 + LAND_BIAS
}

/// Resolved classification of a single terrain column. The single source of
/// truth for the land/water/seafloor *classification*: both the [`Terrain`]
/// gameplay oracle and `inf3d_world::get_voxel_fn` (the meshing delegate on
/// worker threads) derive their land/water answer from [`column_kind`], so the
/// surface a player stands/pathfinds on agrees with the material picked for a
/// voxel — given the same sampled height. The height itself is not identical
/// everywhere: meshing may sample LOD-reduced noise for far chunks (shifting
/// their visual coastline), whereas the oracle always samples LOD-0; see
/// `inf3d_world::get_voxel_fn` for the full caveats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnKind {
    /// Y index of the topmost solid voxel in the column (>= 0).
    pub surface_y: i32,
    /// Whether the column's standing height sits below [`WATER_HEIGHT`], i.e.
    /// it is submerged seafloor (unwalkable) rather than dry land.
    pub is_water: bool,
}

impl ColumnKind {
    /// World Y an entity standing on this column rests its feet at: the top
    /// face of the topmost solid voxel.
    pub fn stand_y(self) -> i32 {
        self.surface_y + 1
    }

    /// Classify a column from an already-sampled raw height (voxel units). The
    /// pure core of the land/water split, factored out so the meshing closure
    /// — which memoizes the raw `sample_height` per worker — can reuse its
    /// cached value instead of re-sampling the noise, while still going through
    /// the *exact* same logic as [`column_kind`].
    pub fn from_height(height: f64) -> Self {
        let surface_y = (height.floor() as i32).max(0);
        // Standing height = top face of the topmost solid voxel. A column is
        // water when an entity standing there would be at or below the water
        // line. LOD-independent (it only depends on the standing height vs
        // `WATER_HEIGHT`) so coastlines stay put across LODs.
        let is_water = (surface_y + 1) as f32 <= WATER_HEIGHT;
        Self {
            surface_y,
            is_water,
        }
    }
}

/// Classify a single column. The `noise` is the only per-call input so callers
/// can pass an LOD-reduced noise (meshing worker) or the canonical LOD-0 noise
/// (gameplay oracle); both run the *same* classification, but on whatever height
/// their noise samples — so an LOD-reduced caller can land on a slightly
/// different coastline than the LOD-0 oracle (the oracle is the navigation
/// authority). Thin wrapper over [`ColumnKind::from_height`] for callers that
/// haven't already sampled the height.
pub fn column_kind(noise: &HybridMulti<Perlin>, x: i32, z: i32) -> ColumnKind {
    ColumnKind::from_height(sample_height(noise, x, z))
}

// ---------------------------------------------------------------------------
// Player voxel edits (the foundation for block place/break)
// ---------------------------------------------------------------------------

/// How far below a column's base surface the override scan looks before giving
/// up and falling back to the base height. Player digs are shallow in practice;
/// this just bounds the scan for a column that was hollowed out unrealistically
/// deep. Comfortably below any standing surface (water columns stand at y≈1).
const OVERRIDE_SCAN_FLOOR: i32 = -16;

/// Air voxels that must sit above a solid voxel for it to count as a *standable*
/// floor (room for the player to stand). A tunnel/overhang must be at least this
/// tall to be walkable; a 1-high gap is not a floor.
const STAND_HEADROOM: i32 = 2;

/// A single player edit to one voxel — the delta layered over the procedural
/// terrain. The material is stored as the raw `TerrainMaterialId` discriminant
/// (a `u8`) so this upstream crate stays free of `inf3d_world`'s palette enum;
/// the mesher in `inf3d_world` interprets it (and its texture mapper already
/// falls back gracefully for an unknown index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VoxelEdit {
    /// The player placed a solid block of this material here (overrides whatever
    /// the procedural terrain had — air or a different material).
    Placed(u8),
    /// The player removed the voxel here; it now reads as air regardless of the
    /// procedural terrain underneath.
    Removed,
}

/// Per-column bookkeeping so unedited columns cost nothing and edited ones know
/// how high to start scanning.
#[derive(Clone, Copy, Debug)]
struct ColumnSummary {
    /// Number of live edits in the column. The column drops out of the index
    /// (back to the zero-cost fast path) when this hits 0.
    count: u32,
    /// Highest edited Y in the column — the scan ceiling. Only ever raised (a
    /// `clear` leaves it), so it's a safe over-estimate: the scan may start a few
    /// voxels high but always lands on the correct topmost solid.
    max_y: i32,
}

#[derive(Default)]
struct VoxelOverrideData {
    /// The sparse edit map: world voxel coord → edit. Absent = use procedural.
    edits: HashMap<IVec3, VoxelEdit>,
    /// Columns that contain at least one edit, for the surface fast path.
    columns: HashMap<IVec2, ColumnSummary>,
    /// Monotonic counter bumped on every mutation, so cheap consumers (the
    /// built-block renderer) can detect "edits changed" without diffing the map
    /// every frame. Wraps harmlessly — only equality vs the last seen value matters.
    version: u64,
}

/// Sparse, shared store of player voxel edits — **the single place an edited
/// block lives**, so it stays consistent everywhere it matters:
///
/// * the **mesher** ([`crate`] consumer `inf3d_world::get_voxel_fn`) snapshots it
///   so an edited block is meshed (visible) exactly as placed/removed;
/// * the [`Terrain`] oracle consults it so `surface_y` / `is_land` / `stand_pos`
///   reflect edits — and because the **physics controller's analytic ground** and
///   the **pathfinder** both read through `Terrain`, they inherit edits for free
///   (walkable + route-blocking) with no extra wiring.
///
/// Cloning is cheap (an `Arc` bump) and every clone shares the one store, so the
/// resource handed to the block module, the copy inside `Terrain`, and the copy
/// inside the voxel-world config all read/write the same edits across threads.
/// Reads take a shared lock; writes (rare — a player click) take an exclusive
/// one. A poisoned lock degrades to "no edits" rather than panicking.
#[derive(Resource, Clone, Default)]
pub struct VoxelOverrides(Arc<RwLock<VoxelOverrideData>>);

/// A point-in-time, lock-free copy of the edits, taken once per chunk-meshing
/// job so the worker reads edits without touching the lock per voxel. Empty when
/// there are no edits (the common case), so meshing keeps its original cost.
#[derive(Clone, Default)]
pub struct VoxelOverrideSnapshot {
    edits: HashMap<IVec3, VoxelEdit>,
}

impl VoxelOverrideSnapshot {
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// The edit at `pos`, if the player has touched that voxel.
    pub fn get(&self, pos: IVec3) -> Option<VoxelEdit> {
        self.edits.get(&pos).copied()
    }
}

impl VoxelOverrides {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an edit at `pos` (place or remove). Last write wins.
    pub fn set(&self, pos: IVec3, edit: VoxelEdit) {
        let Ok(mut data) = self.0.write() else {
            return;
        };
        let col = IVec2::new(pos.x, pos.z);
        let is_new = data.edits.insert(pos, edit).is_none();
        let summary = data.columns.entry(col).or_insert(ColumnSummary {
            count: 0,
            max_y: i32::MIN,
        });
        if is_new {
            summary.count += 1;
        }
        summary.max_y = summary.max_y.max(pos.y);
        data.version = data.version.wrapping_add(1);
    }

    /// Place a solid block of `material` (the [`VoxelEdit::Placed`] discriminant).
    pub fn place(&self, pos: IVec3, material: u8) {
        self.set(pos, VoxelEdit::Placed(material));
    }

    /// Mark the voxel at `pos` removed ([`VoxelEdit::Removed`]) — now air.
    pub fn remove(&self, pos: IVec3) {
        self.set(pos, VoxelEdit::Removed);
    }

    /// Drop the edit at `pos`, reverting that voxel to procedural terrain.
    pub fn clear(&self, pos: IVec3) {
        let Ok(mut data) = self.0.write() else {
            return;
        };
        if data.edits.remove(&pos).is_none() {
            return;
        }
        data.version = data.version.wrapping_add(1);
        let col = IVec2::new(pos.x, pos.z);
        if let Some(s) = data.columns.get_mut(&col) {
            s.count = s.count.saturating_sub(1);
        }
        if data.columns.get(&col).is_some_and(|s| s.count == 0) {
            // `max_y` is intentionally not recomputed; the column simply leaves
            // the index, restoring the zero-cost fast path.
            data.columns.remove(&col);
        }
    }

    /// The edit at `pos`, if any.
    pub fn get(&self, pos: IVec3) -> Option<VoxelEdit> {
        self.0.read().ok()?.edits.get(&pos).copied()
    }

    /// Whether column `(x, z)` contains any player edit. O(1) via the column index;
    /// used by foliage to drop grass off edited cells.
    pub fn column_has_edits(&self, x: i32, z: i32) -> bool {
        self.0
            .read()
            .map(|d| d.columns.contains_key(&IVec2::new(x, z)))
            .unwrap_or(false)
    }

    pub fn is_empty(&self) -> bool {
        self.0.read().map(|d| d.edits.is_empty()).unwrap_or(true)
    }

    pub fn len(&self) -> usize {
        self.0.read().map(|d| d.edits.len()).unwrap_or(0)
    }

    /// Lock-free copy of all edits for the meshing workers (see
    /// [`VoxelOverrideSnapshot`]).
    pub fn snapshot(&self) -> VoxelOverrideSnapshot {
        let edits = self.0.read().map(|d| d.edits.clone()).unwrap_or_default();
        VoxelOverrideSnapshot { edits }
    }

    /// Every live edit as a flat `(pos, edit)` list — the form save/load
    /// serializes. Order is unspecified (HashMap iteration). Empty when there are
    /// no edits.
    pub fn export(&self) -> Vec<(IVec3, VoxelEdit)> {
        self.0
            .read()
            .map(|d| d.edits.iter().map(|(&p, &e)| (p, e)).collect())
            .unwrap_or_default()
    }

    /// Drop ALL edits, reverting the whole world to its procedural/flat base.
    /// Used by New Game (before stamping the test map) and as the first step of
    /// Load. Every previously-edited chunk must be re-meshed afterwards.
    pub fn clear_all(&self) {
        if let Ok(mut data) = self.0.write() {
            data.edits.clear();
            data.columns.clear();
            data.version = data.version.wrapping_add(1);
        }
    }

    /// Monotonic edit version — changes on every mutation (place/remove/clear/
    /// import). Consumers compare it against the last value they saw to skip work
    /// when nothing changed. See [`VoxelOverrideData::version`].
    pub fn version(&self) -> u64 {
        self.0.read().map(|d| d.version).unwrap_or(0)
    }

    /// Replace all edits with `edits` (clears first, then re-applies through
    /// [`set`](Self::set) so the per-column index rebuilds correctly). Used by
    /// Load. Every affected chunk must be re-meshed afterwards.
    pub fn import(&self, edits: &[(IVec3, VoxelEdit)]) {
        self.clear_all();
        for &(pos, edit) in edits {
            self.set(pos, edit);
        }
    }

    /// Topmost solid voxel in column `(x, z)` **given the edits**, or `None` when
    /// the column has no edits (the caller then keeps the base surface — the
    /// zero-cost fast path). Done under a single read lock.
    ///
    /// A base voxel (no override) is solid iff `y <= base_surface_y`, so with no
    /// removals the scan returns the base immediately; placed blocks raise the
    /// surface, removed top voxels lower it.
    pub fn resolved_surface_y(&self, x: i32, z: i32, base_surface_y: i32) -> Option<i32> {
        let data = self.0.read().ok()?;
        let summary = data.columns.get(&IVec2::new(x, z))?;
        let scan_start = base_surface_y.max(summary.max_y);
        let mut y = scan_start;
        while y >= OVERRIDE_SCAN_FLOOR {
            let solid = match data.edits.get(&IVec3::new(x, y, z)) {
                Some(VoxelEdit::Placed(_)) => true,
                Some(VoxelEdit::Removed) => false,
                None => y <= base_surface_y,
            };
            if solid {
                return Some(y);
            }
            y -= 1;
        }
        // Hollowed out below the scan floor — fall back to the base surface.
        Some(base_surface_y)
    }

    /// The standable floor in column `(x, z)` whose top is **nearest `ref_y`**,
    /// given the edits — `None` when the column has no edits (caller keeps the
    /// single base surface: the fast path, so normal terrain is untouched). A
    /// "floor" is a solid voxel with at least [`STAND_HEADROOM`] air above it.
    /// This is what lets navigation and standing follow the player's *level* into
    /// a tunnel / under a built overhang instead of always snapping to the very
    /// top voxel of the column. Done under a single read lock.
    pub fn standing_floor_near(
        &self,
        x: i32,
        z: i32,
        base_surface_y: i32,
        ref_y: i32,
    ) -> Option<i32> {
        let data = self.0.read().ok()?;
        let summary = data.columns.get(&IVec2::new(x, z))?;
        let solid = |y: i32| match data.edits.get(&IVec3::new(x, y, z)) {
            Some(VoxelEdit::Placed(_)) => true,
            Some(VoxelEdit::Removed) => false,
            None => y <= base_surface_y,
        };
        // Scan ceiling: above every solid voxel in the column (so the topmost
        // open-sky floor is always seen). Pick the floor whose stand height
        // (`y + 1`) is closest to `ref_y`.
        let scan_hi = base_surface_y.max(summary.max_y) + 1;
        let mut best = None;
        let mut best_dist = i32::MAX;
        let mut y = scan_hi;
        while y >= OVERRIDE_SCAN_FLOOR {
            if solid(y) && (1..=STAND_HEADROOM).all(|h| !solid(y + h)) {
                let dist = ((y + 1) - ref_y).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best = Some(y);
                }
            }
            y -= 1;
        }
        best.or(Some(base_surface_y))
    }
}

/// Gameplay-side terrain oracle: deterministic surface heights that match the
/// meshed geometry, available regardless of which chunks are currently loaded.
///
/// `Clone` is cheap (just a copy of the noise parameters) so worker threads can
/// snapshot the oracle and run searches off the main thread.
#[derive(Resource, Clone)]
pub struct Terrain {
    noise: HybridMulti<Perlin>,
    /// Player edits layered over the procedural surface (see [`VoxelOverrides`]).
    /// Shared (an `Arc`) with the mesher and the block module, so the surface the
    /// oracle reports — and therefore physics ground + pathfinding, which read
    /// through this oracle — always matches the edited, meshed geometry.
    overrides: VoxelOverrides,
    /// Shared flat-vs-procedural selector (see [`WorldGen`]). Shared with the
    /// mesher so the oracle's surface and the meshed surface agree in both worlds.
    world_gen: WorldGen,
}

impl Terrain {
    /// Construct a terrain oracle with its **own empty** edit store. Convenient
    /// for tests / standalone use; the live game uses [`Terrain::with_overrides`]
    /// so the oracle shares the one store with the mesher and the block module.
    pub fn new() -> Self {
        Self {
            noise: build_noise(),
            overrides: VoxelOverrides::default(),
            world_gen: WorldGen::default(),
        }
    }

    /// Construct the oracle sharing an existing [`VoxelOverrides`] store, with its
    /// own (default, procedural) [`WorldGen`] flag. Convenient for tests; the live
    /// game uses [`Terrain::with_shared`] so the oracle, the mesher, and the block
    /// module all read/write the *same* edits AND the *same* flat flag.
    pub fn with_overrides(overrides: VoxelOverrides) -> Self {
        Self {
            noise: build_noise(),
            overrides,
            world_gen: WorldGen::default(),
        }
    }

    /// Construct the live oracle sharing BOTH the edit store and the flat-world
    /// flag with the mesher (and the resources exposed for save/load), so the
    /// surface the oracle reports can never disagree with the meshed geometry in
    /// either the procedural or the flat test world.
    pub fn with_shared(overrides: VoxelOverrides, world_gen: WorldGen) -> Self {
        Self {
            noise: build_noise(),
            overrides,
            world_gen,
        }
    }

    /// Classify the column at `(x, z)`: the procedural classification from the
    /// oracle's (LOD-0) noise, then **player edits applied on top**. The one
    /// helper that all the public accessors below delegate to, so the oracle
    /// applies the *same* land/water classification as the meshing closure
    /// (which calls [`column_kind`] directly) AND the same edits the mesher sees.
    /// The oracle always samples LOD-0, so it is the authority navigation trusts
    /// where an LOD-reduced far chunk's visual coastline would differ.
    ///
    /// Unedited columns (the overwhelming majority) hit the [`VoxelOverrides`]
    /// fast path and cost exactly what they did before edits existed.
    /// The flat-or-procedural classification of a column BEFORE player edits — the
    /// SINGLE flat-aware base that BOTH [`Terrain::column`] and
    /// [`Terrain::surface_y_near`] build on, so the oracle's reported surface (and
    /// therefore physics ground + pathfinding) can never disagree with the meshed
    /// world in either the flat test world or procedural terrain.
    ///
    /// Flat test world: every column is the same constant surface (no noise),
    /// classified through the exact same helper as procedural so edits, water, and
    /// standing heights behave identically. (Regression guard: `surface_y_near`
    /// once sampled the procedural noise directly here, so under a flat-meshed
    /// world the player walked/pathfound on invisible procedural hills.)
    fn base_kind(&self, x: i32, z: i32) -> ColumnKind {
        match self.world_gen.kind() {
            WorldKind::Normal => column_kind(&self.noise, x, z),
            WorldKind::TestFlat => ColumnKind::from_height(FLAT_SURFACE_HEIGHT),
            // The city backend is primarily a fly-through visual/debug world. Keep
            // the gameplay oracle on the street plane so free-fly/iso starts on the
            // ground instead of snapping to skyscraper roofs.
            WorldKind::City => ColumnKind::from_height(CITY_SURFACE_HEIGHT),
        }
    }

    fn column(&self, x: i32, z: i32) -> ColumnKind {
        let base = self.base_kind(x, z);
        let Some(surface_y) = self.overrides.resolved_surface_y(x, z, base.surface_y) else {
            return base;
        };
        ColumnKind {
            surface_y,
            // Re-derive walkability from the EDITED surface: stacking blocks above
            // the water line turns a water column into land; digging land down to
            // the water line turns it back into water.
            is_water: (surface_y + 1) as f32 <= WATER_HEIGHT,
        }
    }

    /// Y index of the topmost solid voxel in column `(x, z)`.
    pub fn surface_y(&self, x: i32, z: i32) -> i32 {
        self.column(x, z).surface_y
    }

    /// World-space point at the center-top of column `(x, z)` — where an entity
    /// standing on the surface should rest its feet.
    pub fn stand_pos(&self, x: i32, z: i32) -> Vec3 {
        let kind = self.column(x, z);
        Vec3::new(x as f32 + 0.5, kind.stand_y() as f32, z as f32 + 0.5)
    }

    /// Topmost solid voxel in column `(x, z)` whose top is the standable surface
    /// **nearest `ref_y`** (the floor an entity at height `ref_y` belongs to).
    /// For unedited / single-layer columns this is exactly [`Terrain::surface_y`]
    /// (the fast path ignores `ref_y`); where the player has dug a tunnel or built
    /// a structure with a gap, it returns the floor closest to `ref_y` so the
    /// player navigates / stands at their *current* level instead of being snapped
    /// to the very top of the column.
    pub fn surface_y_near(&self, x: i32, z: i32, ref_y: i32) -> i32 {
        // Flat-aware base (NOT the raw procedural noise) so a flat world's walking
        // surface matches its flat mesh — see [`Terrain::base_kind`].
        let base = self.base_kind(x, z).surface_y;
        self.overrides
            .standing_floor_near(x, z, base, ref_y)
            .unwrap_or(base)
    }

    /// Level-aware companion to [`Terrain::stand_pos`]: the standing point on the
    /// floor of column `(x, z)` nearest `ref_y`.
    pub fn stand_pos_near(&self, x: i32, z: i32, ref_y: i32) -> Vec3 {
        let surface_y = self.surface_y_near(x, z, ref_y);
        Vec3::new(x as f32 + 0.5, (surface_y + 1) as f32, z as f32 + 0.5)
    }

    /// Whether the player has edited any voxel in column `(x, z)`. Foliage uses
    /// this to keep grass off edited cells (a placed/broken block clears the grass).
    pub fn column_edited(&self, x: i32, z: i32) -> bool {
        self.overrides.column_has_edits(x, z)
    }

    /// Whether a column is walkable land (its surface stands above the water
    /// line). Seafloor flats sit under the water and are not walkable.
    pub fn is_land(&self, x: i32, z: i32) -> bool {
        !self.column(x, z).is_water
    }

    /// Nearest land column to `start` (spiral ring search), so entities never
    /// spawn in the water. Falls back to `start` if nothing is found nearby.
    pub fn nearest_land(&self, start: IVec2) -> IVec2 {
        if self.is_land(start.x, start.y) {
            return start;
        }
        for r in 1..256i32 {
            // Walk only the perimeter of the radius-`r` square (O(perimeter),
            // not O((2r+1)^2)). We preserve the original visit order — outer
            // loop over `dx` ascending, inner over `dz` ascending — so ties
            // (equidistant land cells) resolve to the same cell as before:
            //   * on the left/right edge columns (|dx| == r) every dz is on the
            //     ring, so we scan the full -r..=r column;
            //   * on interior columns (|dx| < r) only dz == -r and dz == r lie
            //     on the ring.
            for dx in -r..=r {
                if dx.abs() == r {
                    for dz in -r..=r {
                        let c = IVec2::new(start.x + dx, start.y + dz);
                        if self.is_land(c.x, c.y) {
                            return c;
                        }
                    }
                } else {
                    for dz in [-r, r] {
                        let c = IVec2::new(start.x + dx, start.y + dz);
                        if self.is_land(c.x, c.y) {
                            return c;
                        }
                    }
                }
            }
        }
        start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Find a walkable land column near the origin (LAND_BIAS makes most of the
    /// near world land, so this resolves immediately).
    fn land_column(t: &Terrain) -> IVec2 {
        for r in 0..256 {
            for c in [
                IVec2::new(r, 0),
                IVec2::new(0, r),
                IVec2::new(r, r),
                IVec2::new(-r, -r),
            ] {
                if t.is_land(c.x, c.y) {
                    return c;
                }
            }
        }
        panic!("no land column found near origin");
    }

    #[test]
    fn unedited_columns_match_base() {
        // With an empty store every column must equal the raw procedural answer,
        // and must take the zero-cost fast path (resolved_surface_y == None).
        let t = Terrain::new();
        let noise = build_noise();
        for (x, z) in [(0, 0), (13, -7), (-40, 100), (200, 250)] {
            assert_eq!(t.surface_y(x, z), column_kind(&noise, x, z).surface_y);
            assert_eq!(t.overrides.resolved_surface_y(x, z, 0), None);
        }
    }

    #[test]
    fn place_raises_surface_and_stand_pos() {
        let store = VoxelOverrides::default();
        let t = Terrain::with_overrides(store.clone());
        let c = land_column(&t);
        let base = t.surface_y(c.x, c.y);

        store.place(IVec3::new(c.x, base + 1, c.y), 0);
        assert_eq!(
            t.surface_y(c.x, c.y),
            base + 1,
            "placed block raises surface"
        );
        // Feet rest on the top face of the new topmost voxel.
        assert_eq!(t.stand_pos(c.x, c.y).y, (base + 2) as f32);
    }

    #[test]
    fn remove_lowers_surface() {
        let store = VoxelOverrides::default();
        let t = Terrain::with_overrides(store.clone());
        let c = land_column(&t);
        let base = t.surface_y(c.x, c.y);

        store.remove(IVec3::new(c.x, base, c.y));
        assert_eq!(
            t.surface_y(c.x, c.y),
            base - 1,
            "removing the top drops surface"
        );
    }

    #[test]
    fn stack_then_partial_remove() {
        let store = VoxelOverrides::default();
        let t = Terrain::with_overrides(store.clone());
        let c = land_column(&t);
        let base = t.surface_y(c.x, c.y);

        store.place(IVec3::new(c.x, base + 1, c.y), 0);
        store.place(IVec3::new(c.x, base + 2, c.y), 0);
        assert_eq!(t.surface_y(c.x, c.y), base + 2);

        store.clear(IVec3::new(c.x, base + 2, c.y));
        assert_eq!(
            t.surface_y(c.x, c.y),
            base + 1,
            "clearing the top voxel exposes the one below"
        );
    }

    #[test]
    fn digging_below_water_flips_land_then_refilling_restores_it() {
        let store = VoxelOverrides::default();
        let t = Terrain::with_overrides(store.clone());
        let c = land_column(&t);
        let base = t.surface_y(c.x, c.y); // land => base >= 1
        assert!(t.is_land(c.x, c.y));

        // Dig the whole column down to y=0 (standing at y=1 <= WATER_HEIGHT).
        for y in 1..=base {
            store.remove(IVec3::new(c.x, y, c.y));
        }
        assert!(
            !t.is_land(c.x, c.y),
            "column dug below the water line reads as water"
        );

        // Build back up above the water line.
        store.place(IVec3::new(c.x, 1, c.y), 0);
        store.place(IVec3::new(c.x, 2, c.y), 0);
        assert!(
            t.is_land(c.x, c.y),
            "refilled above the water line reads as land again"
        );
        assert_eq!(t.surface_y(c.x, c.y), 2);
    }

    #[test]
    fn clear_reverts_to_base_and_frees_the_column() {
        let store = VoxelOverrides::default();
        let t = Terrain::with_overrides(store.clone());
        let c = land_column(&t);
        let base = t.surface_y(c.x, c.y);

        store.place(IVec3::new(c.x, base + 1, c.y), 0);
        assert_eq!(store.len(), 1);

        store.clear(IVec3::new(c.x, base + 1, c.y));
        assert_eq!(
            t.surface_y(c.x, c.y),
            base,
            "cleared column reverts to procedural surface"
        );
        assert!(store.is_empty());
        // Back on the fast path: column no longer in the edit index.
        assert_eq!(store.resolved_surface_y(c.x, c.y, base), None);
    }

    // REGRESSION (the flat-world "invisible hills"): in flat mode EVERY oracle
    // accessor — including `surface_y_near`, which the physics controller and
    // pathfinder use for the WALKING surface — must report the constant flat
    // surface, NOT the procedural noise. The bug was `surface_y_near` sampling the
    // noise directly while `surface_y`/`column` honored the flat flag, so the
    // player walked on invisible procedural hills under a flat-meshed world.
    #[test]
    fn flat_world_surface_is_constant_for_all_accessors() {
        let store = VoxelOverrides::default();
        let world_gen = WorldGen::new();
        let t = Terrain::with_shared(store, world_gen.clone());
        world_gen.set_flat(true);

        for (x, z) in [(0, 0), (37, -12), (-100, 250), (5, 5)] {
            assert_eq!(
                t.surface_y(x, z),
                FLAT_SURFACE_Y,
                "flat surface_y at ({x},{z})"
            );
            assert_eq!(
                t.surface_y_near(x, z, FLAT_SURFACE_Y + 1),
                FLAT_SURFACE_Y,
                "flat surface_y_near must equal surface_y at ({x},{z}) — the invisible-hills bug"
            );
            assert!(
                t.is_land(x, z),
                "flat world stands above the water line at ({x},{z})"
            );
        }
    }
}
