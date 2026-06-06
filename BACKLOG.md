# inf3d backlog

Tracked work, in execution order. Each item is a **separate commit**. Fix order is:
engine+graphics polish → engine foundation → block-system prerequisites → then the
**block place/break module** (the next feature, built on #3/#4/#5).

Status: `[ ]` todo · `[~]` in progress · `[x]` done

---

## Phase A — Engine & graphics polish (current focus)

- [x] **A1 (#10a) Camera color grading.** Add a `ColorGrading` component to the iso
  camera (subtle contrast + saturation) for a graded "AAA" tone. *File:*
  `inf3d_camera/src/lib.rs`. *Risk:* none (purely tonemapping output).

- [x] **A2 (#8 + #10b) Terrain texture upgrade.** Linear filtering + procedural per-texel
  detail (coarse blotch + fine grain) baked into the layers so flat faces read as textured
  surfaces under SSAO/shadows. *Mipmaps deferred:* Bevy 0.18 has no `Image::generate_mipmaps`;
  detail amplitude kept low so it's acceptable — revisit with manual mip-gen or real textures.
  *File:* `inf3d_world/src/terrain_material.rs`.

- [x] **A3 (#7) Throttle HUD per-frame scans.** `measure_diagnostics`/`update_hud` (which
  scan every Mesh3d/Chunk entity for the readout) now run at ~6.6 Hz via `on_timer`;
  `update_frame_stats` stays per-frame so p95 is accurate. *Monitor kept per-frame* (its
  counts feed per-frame spike deltas; its sort is already `select_nth`, not a full sort).
  *File:* `inf3d_ui/src/lib.rs`.

- [x] **A4 (#9) Harden `footprint_surface` vs water.** Pass an `is_land` check so the
  controller never seats the player on submerged seafloor (latent edge case). *File:*
  `inf3d_physics/src/lib.rs`.

## Phase B — Engine foundation (prereqs for adding block/material content)

- [x] **B1 (#3) Block material PALETTE table + consistency test.** Collapse the 6
  hand-synced material sites (`TerrainMaterialId` enum / `label` / `from_index` /
  texture-array `LAYERS`+palette / `texture_index_mapper` / `get_voxel_fn`) into ONE
  table, generate the rest from it, and add a test asserting table↔enum consistency so a
  missing layer fails loudly at test time instead of silently mis-texturing.
  *Files:* `inf3d_world/src/lib.rs`, `inf3d_world/src/terrain_material.rs`.

- [x] **B2 (#4) Data-driven config: land one RON registry.** `QualitySettings` now loads
  from `assets/config/quality.ron` (serde + `ron`, `#[serde(default)]` for partial/forward-
  compatible files) with a graceful fallback to built-in defaults on missing/bad file. Tune
  render distance / foliage / water / post-FX by editing the `.ron` and re-running — no
  recompile. Establishes the data-driven pattern future block/material defs follow.
  *Files:* `inf3d_core` (`load_quality_settings`), `Cargo.toml` (+`ron`), `quality.ron`.

## Phase C — Block-system prerequisites & deferred content

- [x] **C1 (#6b) ~~Wire~~ Remove the orphaned foliage.** Decided against wiring them in —
  the Bushes/Corn/Flowers/Plants `.vox` (18 models) were referenced by no code (loader only
  reads `Trees`/`Rocks`/`Grass`), so the four unused directories were deleted from
  `crates/inf3d_app/assets/foliage/vox/`. No source change. *Files:* assets only.

- [x] **C2 (#5) `VoxelOverrides` store.** Done. A sparse, `Arc<RwLock>`-shared edit map in
  `inf3d_worldgen` (`VoxelEdit::Placed(u8)`/`Removed`, per-column fast-path index, bounded
  surface scan). ONE instance, cloned into: the mesher (`get_voxel_fn` snapshots it
  lock-free per chunk — empty fast path until first edit), the `Terrain` oracle (so
  `surface_y`/`is_land`/`stand_pos` reflect edits), and a `VoxelOverrides` resource for the
  block module. **Physics ground + pathfinding inherit edits for free** (both read through
  `Terrain`). Re-mesh on edit = mark the chunk `NeedsRemesh` (vendored `remesh_dirty_chunks`
  re-calls the delegate → re-reads the store); that kick is the block module's job. Tests
  in both crates. *Files:* `inf3d_worldgen/src/lib.rs`, `inf3d_world/src/lib.rs`.

- [~] **C3 (#6a) Content systems (large, sequence as needed).** Game-state machine
  (`bevy_state`) **done** — `AppState{MainMenu,InGame}` + `Pause{Running,Paused}` in
  `inf3d_core`, driven by `inf3d_menu`. Save/load **done** (`ron`, `inf3d_menu/save.rs`,
  persists `VoxelOverrides` + player/camera/edit-mode/selected-material). Still TODO:
  inventory + items, harvesting (Tree/Rock/`InteractionTarget` hooks exist, unwired).

## Next feature (after the above)
- [~] **Block placing & breaking module** — built on B1 (material table), C2
  (`VoxelOverrides`), reusing the existing `Hover` raycast for targeting.
  - [x] **Mode selector** — `EditMode` resource (`Walk`/`Build`) in `inf3d_core`;
    two color-coded buttons on the right edge (HUD), active one highlighted.
  - [x] **Edit path** — `inf3d_render::edit` (`EditPlugin`): in Build mode, left-click
    places a block on the hovered face and right-click removes the hovered voxel; writes
    `VoxelOverrides` and marks affected chunk(s) `NeedsRemesh` (vendored prelude now
    re-exports it). In Walk mode left-click pathfinds.
  - [x] **Targeting** — `Hover` gains the face `normal` (for placement). Pathfinder +
    editor gated on `EditMode` and on UI hover so clicks don't double-act.
  - [x] **Placeholder** — a 3-tall stone pillar near spawn (via the same store) to break/test.
  - [x] **Level-aware navigation/standing** — `Terrain::surface_y_near(x,z,ref_y)` resolves
    the standable floor (≥`STAND_HEADROOM` air above) nearest a reference height, not the
    topmost voxel, so you can pathfind/walk INTO a dug tunnel instead of being routed onto
    the ceiling. Pathfinder reference = the **clicked** height (`PathRequest::ref_y` from
    the raycast hit, via a `LeveledTerrain` oracle) so it works even when you dig from
    outside/above; controller reference = the player's feet (stay on your level as you
    move). Unedited terrain unchanged (fast path); building UP still pops you up.
    *Files:* `inf3d_worldgen`, `inf3d_pathfinding`, `inf3d_physics`.
  - [x] **Place/break juice** — a tinted pop-in (place) / crumble (break) cube
    (`BlockFx`) + a dust puff reusing the footstep `DustBurst` (bigger cloud on break;
    place puff emitted under the block + flung outward so it's not hidden inside it).
    Block color exposed via `TerrainMaterialId::color()`. *File:* `inf3d_render/src/edit.rs`.
  - [x] **Grass clears on edit** — editing a cell despawns ONLY that cell's grass blade
    (blades are per-cell entities; no tile flicker, nothing far shifts). Grass scatter is
    now per-cell-seeded and skips edited cells (`Terrain::column_edited`) so reloads don't
    re-add it. *Files:* `foliage/{scatter,spawn,stream,mod}.rs`, `BlockEdited` message.
  - [x] **Nav robustness** — goal snaps to the **clicked level** (`near_target_level` +
    `LEVEL_TOLERANCE`): a too-small/unenterable tunnel snaps to the entrance, not the
    surface above. A* is now **best-effort** — if the goal is unreachable it routes to the
    nearest reachable cell, so a click always walks you as close as possible.
    *File:* `inf3d_pathfinding/src/lib.rs`.
  - [x] **Material picker** — 8 buildable blocks (Stone/Dirt/Grass/Concrete/Glass +
    Neon Cyan/Magenta/Yellow), each a distinct `Built*` material (index ≥
    `BUILT_MATERIAL_BASE`) so player builds stay separable from terrain and pick up
    the see-through cutout once that shader lands. Bottom-center hotbar (click or
    number keys 1–8), gold-ringed selected swatch; gated to Build mode via
    `Display::None` so it never eats Walk-mode clicks. `SelectedMaterial` (`inf3d_core`)
    is the picked block; `BUILDABLE` (`inf3d_world`) is the single source of truth
    for the set. *Files:* `inf3d_world`, `inf3d_core`, `inf3d_ui`, `inf3d_render/edit.rs`.
  - [x] **Edit SFX** — `inf3d_audio` plays a "thunk" on place / "crumble" on break
    (pitch+volume jitter, self-cleaning), driven by the now-public+enriched
    `inf3d_render::BlockEdited` message (`placed`/`material`). Clips are synthesized
    placeholder `.ogg`s under `sfx/world/` (swap in real ones any time).
  - [x] **Save/load persists edits** — already done in `inf3d_menu` (3-slot RON via
    `VoxelOverrides::export()/import()`, + player/camera/edit-mode); now also stores
    the picker's `selected_material` (serde-default for old saves).
  - [x] **See-through cutout (player builds only)** — custom terrain PREPASS
    (`terrain_prepass.wgsl`) discards the same player-built fragments the forward pass
    dithers, so the depth prepass no longer occludes the player behind your walls.
    Only `Built*` materials (index ≥ `BUILT_MATERIAL_BASE`) are affected — terrain /
    city / natural blocks stay solid (cave opacity is a separate future thing). Gated
    in `xray.rs` to presets whose prepass has a fragment (normal/motion), so depth-only
    presets just leave walls opaque instead of punching black holes. Test-map structures
    now stamp `BuiltStone`/`BuiltDirt` so the whole lab reads as player-placed.
    *Files:* `inf3d_world/{terrain_material.rs,terrain_prepass.wgsl,terrain_material.wgsl}`,
    `inf3d_render/xray.rs`, `inf3d_menu/testmap.rs`.
  - [x] **Cutaway is now world-space + block-based** — the screen-space dither circle
    (which caught bystander blocks and looked mushy) is gone. The shared
    `inf3d::terrain_xray::xray_should_discard` removes WHOLE player-built voxels whose
    CENTER sits on the camera→player line (world-space): in front of the player
    (`dot(Δ, view) < 0`) and within `CUT_RADIUS` of the player's vertical segment. Snaps
    to voxel center (stepping inside along the face normal) so blocks cut as a unit, and
    only the blocks actually occluding the character open up — side/back/standing-in-front
    walls stay solid. Forward + prepass call the same fn (deterministic, no dither). Knobs:
    `CUT_RADIUS` (≈0.95 blocks), `PLAYER_HALF_HEIGHT` (1.1) in `xray.rs`.
  - [x] **Cutaway v2 — roof + click-through** — added a **ceiling rule** (cut built
    blocks above the player within `XRAY_CEILING_RADIUS`) so a whole roof opens up, not
    just the camera-line strip; bumped `XRAY_CUT_RADIUS` to 1.4; floor guard
    (`dc.y > -half_h`) so a built floor in front never holes. The cut math now lives in
    `inf3d_world::voxel_cut_by_xray` (CPU mirror of `terrain_xray.wgsl`), and the click
    raycasts in BOTH `inf3d_render::highlight` (Walk mode only) and
    `inf3d_pathfinding::handle_click` skip the cut voxels — so you can click the interior
    floor through a cut roof/wall and walk in. All tuning consts in
    `inf3d_world::terrain_material` (one source for shader + raycasts).
  - [ ] **Next:** perf in dense areas (rd=10 + foliage is the dominant cost; the prepass
    discard also disables terrain early-z) — **deprioritized**, log shows 60fps steady, the
    vertical-clamp structural win already shipped (`MainWorld::chunk_y_bounds`); horizontal
    collision for placed voxels — **already done** via the controller's `is_wall` path
    (2+ tall placed walls block; 1-tall is an intended climbable step); then harvesting
    (Tree/Rock/`InteractionTarget` hooks exist), inventory/items, foliage wind.

## Pathfinding AAA overhaul (current focus)
The click-to-move pathfinding felt broken: jumped off ledges, perma-ran into walls, stuck
on corners, and preferred dropping off a ledge to taking nearby stairs. Root causes were
**planner ↔ locomotion disagreement** + a missing recovery loop. Fixes (all unit-tested;
**user to build + repro**):
- [~] **Smooth descents (the main "jumps off ledges" cause).** `GROUND_SNAP_DISTANCE`
  was `0.5`, but a normal 1-voxel step DOWN drops support `1.0` below the feet → outside
  the snap band → the controller went airborne on *every* downward step (downhill/stairs
  read as constant hopping). Raised to `1.1` to **mirror the climb cap** (`STEP_HEIGHT` /
  `MAX_STEP`): one voxel down now stays grounded + eases; a ≥2-voxel ledge still falls.
  *File:* `inf3d_physics/src/lib.rs`. See [[pathfinding-traversability-invariant]].
- [~] **Stuck-detection → re-path (the "perma-running into walls" cause).** `follow_path`
  steered at the front waypoint forever with no recovery. Added `ActiveGoal` + the
  `repath_when_stuck` system: no XZ progress over a `STUCK_WINDOW` while a route is pending
  → re-path to the same goal; after `MAX_AUTO_REPATHS` give up and stop cleanly (no grind).
  *File:* `inf3d_pathfinding/src/lib.rs`.
- [~] **Height-aware best-effort (the "jumps down instead of stairs" cause).** Unreachable-
  goal partial routes used XZ-only octile → ended at the cliff EDGE over the goal. Now
  scored by `best_effort_score` (XZ + height-to-goal-level penalty) so a partial route ends
  at the foot of the stairs. *File:* `inf3d_pathfinding/src/lib.rs`.
- [~] **Arrival radius (corner orbit).** `follow_path` popped waypoints at `0.1`, below
  per-step travel (`speed*dt ≈ 0.125`) → could orbit a point forever. Raised to
  `ARRIVE_RADIUS = 0.25` + drain-all-reached loop. *File:* `inf3d_gameplay/src/lib.rs`.
- [~] **Height-following search (stairs under an overhang).** A click on a high platform
  resolved EVERY column at the clicked (high) level, so a landing that sits under the
  platform's overhang snapped to the platform floor — the stair→landing step read as a huge
  drop and the goal looked unreachable (character wandered off / stuck on a wall). The search
  is now height-following: `astar_from` seeds the start at the player's REAL stand height
  (`PathRequest::start_y`) and resolves each neighbour relative to the floor it steps FROM
  (`SurfaceOracle::floor_near`), exactly mirroring how the physics controller resolves ground
  from the player's feet. Can't regress tunnels/pits (a ≥2 drop was already Δ>MAX_STEP).
  *Known limit:* single floor per cell on the best path, so a column reachable at two levels
  arrives at whichever wins g-score — upgrade to `(cell, floor)` A* state if floor-precision
  on the goal column ever matters. *File:* `inf3d_pathfinding/src/lib.rs`.
- [ ] **Possible follow-up:** terrain-wall clearance — the planner routes on cell centres
  while the controller uses the 0.45-radius capsule, so it can *rub* a convex wall corner
  (re-path/give-up catches the worst case). A radius-inflated terrain-wall cost would tighten
  it, but risks over-constraining the 1-wide corridors `WALL_SKIN` deliberately enables.

---

## Recently completed (context)
- [x] Footstep audio (`inf3d_audio` sink crate; gap-trimmed `.ogg`; pitch/volume variation).
- [x] Shadow cascade fix (160u / 3 cascades / 4096 map).
- [x] Terrain LOD activated (band sized below the render disc).
- [x] Zoom-scaled chunk render distance (fixes zoomed-out "ocean" edge).
- [x] `LAND_BIAS` worldgen knob (land/water balance).
- [x] Monitor expanded to full pipeline state (CAMERA/GFX/LIGHT/QUALITY) each run.
- [x] Quality locked / F2 preset-cycling removed (was crashing).
