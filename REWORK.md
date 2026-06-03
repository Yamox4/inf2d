# inf3d Engine Rework — Design & Execution Spec

Single source of truth for the coordinated "AAA / no-bugs" rework. Every rework
agent MUST read this before editing. Goal: remove the invisible runtime coupling
that makes unrelated things break, so features can be added seamlessly.

**Game type:** isometric top-down (Diablo-style 3/4). Orthographic camera,
horizontal-only orbit, zoom = projection scale (camera distance fixed). The
visible area is a wide parallelogram — any radius/culling/LOD must cover the iso
view, never assume a square or a perspective frustum.

**Hard rules for every change:**
- No scaffolding, no stubs, no `unwrap()` outside tests (`let Ok(..) = .. else { return; };`).
- Preserve existing public APIs unless this spec says to change them.
- Match surrounding code style, comment density, and idioms.
- Keep the GNU toolchain pin + `[profile.dev]` PE-size settings.
- The workspace must `cargo check --workspace` and `cargo test --workspace` clean
  at the end.

---

## Target architecture

### 1. Scheduling spine — fixed timestep + interpolation (fixes the zoom jitter)
**Problem:** physics currently runs at a variable timestep in `PostUpdate` (a prior
hack); the kinematic controller mutates `Transform` per render frame. Low FPS
(zoom-out) → coarse uneven sim → player+camera jitter.

**Target:**
- Revert to avian's default fixed schedule (`PhysicsPlugins::default()`,
  `FixedPostUpdate`). Remove `PhysicsPlugins::new(PostUpdate)`.
- `player_controller` runs in the **fixed** schedule (`FixedPostUpdate`, ordered
  `.after(PhysicsSystems::Writeback)`), using the fixed `Time` delta. It still
  reads `DesiredMove` and writes the physics position.
- Add avian **`TransformInterpolation`** to the player so the rendered transform
  is smoothly interpolated between fixed steps → smooth at any FPS.
  - VERIFY the exact API by reading the installed source:
    `~/.cargo/registry/.../avian3d-0.6.1/src/interpolation.rs` (component name,
    whether a plugin must be added, whether it interpolates `Transform` or a
    separate render transform). Implement against what the source actually says.
- `follow_path` (gameplay) stays per-frame in `Update` (sets `DesiredMove` intent,
  pops waypoints by XZ arrival); the fixed-step controller consumes the latest
  intent. Camera `follow_player` stays in `PostUpdate` but must read the
  **interpolated** player transform (order it after avian's interpolation/sync so
  it never reads a mid-step value).

### 2. SystemSet backbone (`inf3d_core::GameSet`)
Define an ordered set enum in core and configure it once in `CorePlugin`:
```
Update order:  Input -> Logic -> Streaming -> Fx
```
- `Input`: raw-input reads (camera_input, cycle_preset_keybinding, handle_click).
- `Logic`: pathfinding dispatch/poll/consume, follow_path, animate_player,
  update_interaction_target.
- `Streaming`: stream_foliage, recenter_voxel_ground, build_prop_colliders.
- `Fx`: dust, highlight/target highlight, apply_quality_to_camera,
  apply_water_quality, measure_diagnostics, update_frame_stats, update_hud.
Every `Update` system gets `.in_set(GameSet::X)`. Fixed-step + PostUpdate systems
keep their avian-relative ordering (spine). Existing `.chain()` within a plugin
stays (it's an intra-set order). This makes order explicit so adding a system
can't perturb unrelated ones.

### 3. Single resource owner
`CorePlugin` is the SOLE `init_resource` for: `QualitySettings`, `BlockedCells`,
`PathTarget`, `GrassStats`, `FrameStats`. Delete every other `init_resource` of
these (camera, water, foliage, gameplay, pathfinding, highlight). Resources that
belong to one crate stay there (Hover→highlight, InteractionTarget→physics,
ActivePathTask/PathTiming→pathfinding, HudStats→ui, FoliageField→foliage,
DustAssets→dust, Terrain→world, WaterSettings→water).

### 4. QualitySettings additions / changes
- Add `grass_radius_world: f32` — dense grass only spawns within this world-radius
  of the player, regardless of zoom (caps the zoom-out cost; sparse trees/rocks
  still fill the iso view to the edges via the existing ring).
- Add `ssao_enabled: bool` and `motion_blur_enabled: bool`; replace the
  `bloom_enabled` proxy gate in camera with these real fields. Keep presets
  monotonic; update the core unit tests accordingly.

### 5. Single sources of truth (kill hand-kept invariants)
- **PlayerDims:** one place (in inf3d_physics) defining radius, half_height, and
  the visual-root offset; gameplay's character root offset derives from it (no
  more coincidental `1.0`).
- **Material indices:** one enum (in inf3d_world) used by `get_voxel_fn`,
  `texture_index_mapper`, the texture palette order, and ui `material_name`.
- **Column kind:** one worldgen helper (e.g. `column(x,z) -> {surface_y, is_water}`)
  used by BOTH `Terrain` and `get_voxel_fn` so land/water can't desync.
  `Terrain`'s public methods (`surface_y/stand_pos/is_land/nearest_land`) must keep
  their signatures (physics + pathfinding depend on them).
- **Dead materials 1/2 (dirt/stone):** either wire them in (e.g. dirt on voxel
  sides / stone with depth) via the material enum, or remove them and the unused
  texture layers. Pick wired-in for the iso look; keep it cheap.

### 6. Delete the redundant voxel-ground collider
Terrain is a pure heightfield, so the controller's downward ground ray can be
answered analytically from the `Terrain` oracle. Remove `VoxelGround`,
`spawn_voxel_ground`, `recenter_voxel_ground`, `build_voxel_ground`. The controller
computes ground height from `Terrain::surface_y` (top face = `surface_y + 1`) for
the column under the player, keeps the same step-up / snap / airborne behavior, and
still uses `move_and_slide` against **Solid props** for horizontal blocking. Props
keep their real colliders; only the terrain voxel patch goes away.

---

## Tasks, file ownership, and the conflict queue

Foundation must finish before anything else (it defines sets, resource ownership,
and the new QualitySettings fields everyone consumes). After it, the spine and the
per-crate leaves edit DISJOINT files and run in parallel.

| ID | Task | Owns (only edits) | Depends on |
|----|------|-------------------|-----------|
| **F** | Foundation: `GameSet` + ordering in CorePlugin; sole resource init; add `grass_radius_world`/`ssao_enabled`/`motion_blur_enabled`; fix core unit tests | `crates/inf3d_core/src/lib.rs` | — |
| **S** | Spine: fixed-timestep + `TransformInterpolation` + delete voxel-ground (§1,§6); camera reads interpolated transform + real ssao/mb flags (§4); adopt sets; remove dup resource inits; PlayerDims source (§5) | `crates/inf3d_app/src/main.rs`, `crates/inf3d_physics/src/lib.rs`, `crates/inf3d_gameplay/src/lib.rs`, `crates/inf3d_camera/src/lib.rs` | F |
| **T** | Terrain consistency: material-index enum, wire/remove dead materials, single column-kind helper (§5); adopt sets; remove dup inits | `crates/inf3d_worldgen/src/lib.rs`, `crates/inf3d_world/src/lib.rs`, `crates/inf3d_world/src/terrain_material.rs`, `.wgsl` | F |
| **G** | Foliage: cap dense grass to `grass_radius_world` (§4) for the iso view; adopt sets; remove dup inits | `crates/inf3d_render/src/foliage/*` | F |
| **P** | Pathfinding: adopt sets; remove dup inits | `crates/inf3d_pathfinding/src/lib.rs` | F |
| **U** | UI/HUD: adopt sets; remove dup inits; surface ssao/mb in HUD if cheap | `crates/inf3d_ui/src/lib.rs` | F |
| **X** | Render FX: adopt sets; remove dup inits | `crates/inf3d_render/src/{dust,fog,highlight,water}.rs` | F |

Invariants the spine (S) must NOT break for parallel safety: keep
`inf3d_physics::SolidPropCollider` public API stable (foliage G depends on it) and
keep `inf3d_worldgen::Terrain`'s public methods stable (T owns its internals).

After all of the above: **Verify** (full `cargo check` + `cargo test`, fix to
green) then **Adversarial review** (check every row of the invariant table below,
confirm no behavior regressions, no new ordering ambiguity).

---

## Invariant landmines (must remain consistent)
- terrain noise change → land/water in both `Terrain` and `get_voxel_fn` (now via
  the shared column-kind helper).
- `WATER_HEIGHT` → meshing material choice, `Terrain::is_land`, water plane height.
- material index meaning → the single enum's consumers.
- player size → the single `PlayerDims`.
- plugin order in `main.rs` → must be neutralized by explicit `GameSet` ordering.

## Acceptance criteria
- `cargo check --workspace` and `cargo test --workspace` clean.
- Walking is smooth at all zooms (no character/camera jitter) — fixed-step + interp.
- F2 cycles presets; SSAO/motion blur/DoF/bloom/water toggle per their real flags.
- No `init_resource` of the shared resources outside CorePlugin.
- No `VoxelGround`/voxel-ground systems remain.
- Materials: no dead/unused texture layers (wired or removed).
