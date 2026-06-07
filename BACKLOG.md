# inf3d backlog

Tracked work, in execution order. Each item is a **separate commit**. Fix order is:
engine+graphics polish → engine foundation → block-system prerequisites → then the
**block place/break module** (the next feature, built on #3/#4/#5).

Status: `[ ]` todo · `[~]` in progress · `[x]` done

---

## Controls rework → third-person orbit + WASD (DONE; supersedes the pathfinding + see-through sections below)

Moved from orthographic-iso **click-to-move** to a **Cube World-style perspective third-person
orbit camera with camera-relative WASD** (mouse orbits yaw+pitch, scroll zooms the boom, cursor
captured in play, the character faces its travel direction, `F` = free-fly). Done by a background
worker + this follow-up; **user to build + repro**.
- [x] **Camera** — `inf3d_camera` rewritten: `OrbitCamera`/`OrbitCameraPlugin`, perspective
  projection, `CameraRig{yaw,pitch,distance}`, **boom collision** (raycasts `VoxelWorld` along the
  boom and clamps the eye short of terrain/builds → caves/houses pull the camera in close, the
  replacement for the see-through). FPS (`G`) mode + view-bob deleted; `F` free-fly kept.
- [x] **Movement** — WASD writes `MoveIntent` (was `FpsMoveIntent`); the kinematic controller
  consumes it via `DesiredMove` UNCHANGED (no physics rework — the architecture made it surgical).
  Hover/interaction raycasts retarget to a **screen-center crosshair** (`inf3d_ui` crosshair dot);
  cursor captured/hidden in play. Save format v2 (camera_zoom→distance + pitch).
- [x] **Pathfinding removed** — the whole `inf3d_pathfinding` crate + `MovePath`/`PathTarget`/
  destination highlight deleted (the old "Pathfinding AAA overhaul" backlog section is gone with it).
- [x] **See-through removed (Tier 1)** — fully third-person; the boom zoom-in replaces the cutaway
  for caves/houses. `XrayPlugin` + `inf3d_render/xray.rs` deleted and unwired and the hover ray's
  xray-skip dropped, so the shader's cut never engages (the inert shader code is the Tier-2 item below).
- [ ] **See-through cleanup (Tier 2).** Excise the now-inert xray code from the terrain material:
  the `XrayParams` uniform (binding 102) + `VoxelTerrainExtension.xray` field, `voxel_cut_by_xray`,
  the `XRAY_*` consts, the custom prepass (`terrain_prepass.wgsl`) + `terrain_xray.wgsl`, and revert
  `prepass_*_shader()`/`specialize()` to the stock prepass — **KEEPING** `enable_prepass()=true`
  (the "central graphics enabler"). **Do this only when buildable + GPU-verifiable** — a blind WGSL
  bind-group/prepass edit can black-screen ALL terrain. *Files:*
  `inf3d_world/{terrain_material.rs,terrain_prepass.wgsl,terrain_xray.wgsl,terrain_material.wgsl}`.
- [x] **Feel overhaul (research-backed AAA pass).** Camera: removed the disorienting forward
  focus offset (pivots on the PLAYER now), RIGID horizontal follow (no position lerp ⇒ no
  floaty lag — only zoom + collision are smoothed), sphere-cast boom collision (a 5-ray bundle,
  not one thin ray that threads voxel edges) with fast-IN / slow-OUT, wider pitch for build
  aiming. Controller: acceleration/deceleration ramp (`move_toward` — snappy ~0.1 s on the
  ground, gentle air momentum), coyote time + jump buffering (armed on the PRESS edge so holding
  Space doesn't bunny-hop), walk anim driven off the ACTUAL ramped speed. Building: crosshair
  aims ahead via the head-height focus, reach capped at `BUILD_RANGE`. Teleports fully reset
  locomotion. Sources: Unreal/Godot spring-arm, the voxel-camera sphere-cast writeup, game-feel
  accel norms (50–200 ms to top speed). *Files:* `inf3d_camera`, `inf3d_physics`, `inf3d_gameplay`,
  `inf3d_render/highlight.rs`, `inf3d_menu`. **User to build + repro; constants are all tunable.**
- [ ] **Biomes/props (next task).** Design + seams in `BIOMES_PLAN.md` — the biome + per-biome
  foliage pipeline is ALREADY built; the task EXTENDS it (new `Biome` variants / materials / props).

## Phase A — Engine & graphics polish (current focus)

- [x] **A1 (#10a) Camera color grading.** Add a `ColorGrading` component to the
  gameplay camera (subtle contrast + saturation) for a graded "AAA" tone. *File:*
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
  block module. **Physics ground inherits edits for free** (the controller reads through
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
    re-exports it). In Walk mode clicks are free (the mouse orbits the camera).
  - [x] **Targeting** — `Hover` gains the face `normal` (for placement). The editor is
    gated on `EditMode::Build` and on UI hover so clicks don't double-act.
  - [x] **Placeholder** — a 3-tall stone pillar near spawn (via the same store) to break/test.
  - [x] **Level-aware standing** — `Terrain::surface_y_near(x,z,ref_y)` resolves
    the standable floor (≥`STAND_HEADROOM` air above) nearest a reference height, not the
    topmost voxel, so you can walk INTO a dug tunnel instead of being seated onto the
    ceiling. Controller reference = the player's feet (stay on your level as you move).
    Unedited terrain unchanged (fast path); building UP still pops you up.
    *Files:* `inf3d_worldgen`, `inf3d_physics`.
  - [x] **Place/break juice** — a tinted pop-in (place) / crumble (break) cube
    (`BlockFx`) + a dust puff reusing the footstep `DustBurst` (bigger cloud on break;
    place puff emitted under the block + flung outward so it's not hidden inside it).
    Block color exposed via `TerrainMaterialId::color()`. *File:* `inf3d_render/src/edit.rs`.
  - [x] **Grass clears on edit** — editing a cell despawns ONLY that cell's grass blade
    (blades are per-cell entities; no tile flicker, nothing far shifts). Grass scatter is
    now per-cell-seeded and skips edited cells (`Terrain::column_edited`) so reloads don't
    re-add it. *Files:* `foliage/{scatter,spawn,stream,mod}.rs`, `BlockEdited` message.
  - [x] **Material picker** — 8 buildable blocks (Stone/Dirt/Grass/Concrete/Glass +
    Neon Cyan/Magenta/Yellow), each a distinct `Built*` material (index ≥
    `BUILT_MATERIAL_BASE`) so player builds stay separable from terrain. Bottom-center
    hotbar (click or number keys 1–8), gold-ringed selected swatch; gated to Build mode
    via `Display::None` so it never eats Walk-mode clicks. `SelectedMaterial`
    (`inf3d_core`) is the picked block; `BUILDABLE` (`inf3d_world`) is the single source
    of truth for the set. *Files:* `inf3d_world`, `inf3d_core`, `inf3d_ui`, `inf3d_render/edit.rs`.
  - [x] **Edit SFX** — `inf3d_audio` plays a "thunk" on place / "crumble" on break
    (pitch+volume jitter, self-cleaning), driven by the now-public+enriched
    `inf3d_render::BlockEdited` message (`placed`/`material`). Clips are synthesized
    placeholder `.ogg`s under `sfx/world/` (swap in real ones any time).
  - [x] **Save/load persists edits** — already done in `inf3d_menu` (3-slot RON via
    `VoxelOverrides::export()/import()`, + player/camera/edit-mode); now also stores
    the picker's `selected_material` (serde-default for old saves).
  - Note: an earlier **see-through cutout** for player builds (a custom prepass + a
    world-space block cutaway driven by an `XrayPlugin`) was built, then **removed in the
    controls rework (Tier 1)** — fully third-person now, the boom zoom-in handles
    caves/houses. The cutaway shader code still ships but is **inert**; excising it is the
    Tier-2 item near the top of this file.
  - [ ] **Next:** perf in dense areas (rd=10 + foliage is the dominant cost) —
    **deprioritized**, log shows 60fps steady, and the vertical-clamp structural win
    already shipped (`MainWorld::chunk_y_bounds`); horizontal collision for placed voxels
    is **already done** via the controller's `is_wall` path (2+ tall placed walls block;
    1-tall is an intended climbable step); then harvesting (Tree/Rock/`InteractionTarget`
    hooks exist), inventory/items, foliage wind.

---

## Recently completed (context)
- [x] Footstep audio (`inf3d_audio` sink crate; gap-trimmed `.ogg`; pitch/volume variation).
- [x] Shadow cascade fix (160u / 3 cascades / 4096 map).
- [x] Terrain LOD activated (band sized below the render disc).
- [x] Vertical chunk clamp (`MainWorld::chunk_y_bounds [-1,3]`) — stops the stock 3D
  sphere streaming empty-air / invisible-underground chunk layers.
- [x] `LAND_BIAS` worldgen knob (land/water balance).
- [x] Monitor expanded to full pipeline state (CAMERA/GFX/LIGHT/QUALITY) each run.
- [x] Quality presets moved to the settings menu (`QualityPreset::apply`); the old
  F2 preset-cycle hotkey is gone (it was crashing).
