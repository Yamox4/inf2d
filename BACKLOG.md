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

- [ ] **C3 (#6a) Content systems (large, sequence as needed).** Game-state machine
  (`bevy_state`), inventory + items, save/load (`serde`/`bincode`). Each is its own
  multi-step effort; pull them in when the block module needs them (e.g. save must
  persist `VoxelOverride`).

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
  - [ ] **Next:** material picker (Build currently hardcodes Stone), edit SFX
    (`inf3d_audio` ready), and save/load persisting `VoxelOverrides`.

---

## Recently completed (context)
- [x] Footstep audio (`inf3d_audio` sink crate; gap-trimmed `.ogg`; pitch/volume variation).
- [x] Shadow cascade fix (160u / 3 cascades / 4096 map).
- [x] Terrain LOD activated (band sized below the render disc).
- [x] Zoom-scaled chunk render distance (fixes zoomed-out "ocean" edge).
- [x] `LAND_BIAS` worldgen knob (land/water balance).
- [x] Monitor expanded to full pipeline state (CAMERA/GFX/LIGHT/QUALITY) each run.
- [x] Quality locked / F2 preset-cycling removed (was crashing).
