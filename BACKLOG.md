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

- [ ] **B2 (#4) Data-driven config: land one RON registry.** Use the already-declared
  `serde` to load one config from a `.ron` (start with graphics settings and/or quality
  presets) so values can be tuned without a recompile — establishes the pattern + speeds
  up visual iteration. *Files:* `inf3d_core`, an `assets/config/*.ron`.

## Phase C — Block-system prerequisites & deferred content

- [ ] **C1 (#6b) Wire the orphaned foliage.** Bushes/Corn/Flowers/Plants `.vox` (18
  models) currently sit only in the dead root `assets/` and load nowhere. Move into the
  live asset dir and add foliage categories so they appear in-world.
  *Files:* `inf3d_render/src/foliage/*`, assets.

- [ ] **C2 (#5) `VoxelOverride` store.** The foundation for block place/break: a sparse
  map of player-edited voxels that the mesher (`get_voxel_fn`), the `Terrain` oracle,
  the controller's analytic ground, and the pathfinder ALL consult, so an edited block is
  solid/visible/walkable/route-blocking consistently. *Files:* `inf3d_worldgen`,
  `inf3d_world`, (read by physics/pathfinding). **This is the load-bearing prereq for the
  block module.**

- [ ] **C3 (#6a) Content systems (large, sequence as needed).** Game-state machine
  (`bevy_state`), inventory + items, save/load (`serde`/`bincode`). Each is its own
  multi-step effort; pull them in when the block module needs them (e.g. save must
  persist `VoxelOverride`).

## Next feature (after the above)
- [ ] **Block placing & breaking module** — built on B1 (material table), C2
  (`VoxelOverride`), reusing the existing `Hover` raycast for targeting.

---

## Recently completed (context)
- [x] Footstep audio (`inf3d_audio` sink crate; gap-trimmed `.ogg`; pitch/volume variation).
- [x] Shadow cascade fix (160u / 3 cascades / 4096 map).
- [x] Terrain LOD activated (band sized below the render disc).
- [x] Zoom-scaled chunk render distance (fixes zoomed-out "ocean" edge).
- [x] `LAND_BIAS` worldgen knob (land/water balance).
- [x] Monitor expanded to full pipeline state (CAMERA/GFX/LIGHT/QUALITY) each run.
- [x] Quality locked / F2 preset-cycling removed (was crashing).
