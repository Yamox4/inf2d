# inf3d — Project Guide for the Next Claude Agent

Handover doc. Read it before touching code — it is the high-level architecture map
and the contract: the scheduling spine, single-owner resources, single-source-of-truth
invariants, and the Bevy integration gotchas all live below.

---

## 1. What this is

A **3D voxel open-world game** in Rust + **Bevy 0.18**. Procedural infinite voxel
terrain (via `bevy_voxel_world`) with a **custom terrain material** that writes
the depth/normal/motion prepass, a **perspective third-person orbit** follow
camera (Cube World-style: mouse orbits yaw+pitch, scroll zooms the boom, cursor
captured in play) with **camera-relative WASD** movement,
**avian3d** physics at a **fixed timestep** with a kinematic character controller
(ground derived **analytically** from terrain) + prop colliders, a procedural
multi-part **player character**, animated **water** (`bevy_water`), **`.vox`
foliage** (trees / rocks / grass from MagicaVoxel models, streamed in a tile ring
with a player-centered grass radius cap), **dust** particles, post-FX (Bloom +
Depth of Field + **SSAO** + **motion blur**), a debug **HUD**, an in-game
menu / pause / settings flow (`inf3d_menu`) with **menu-driven quality presets** and
3-slot save/load, block **place/break** editing, **audio** (footsteps + block-edit
SFX), and a read-only **telemetry recorder** (`inf3d_monitor`) that writes
`inf3d-monitor.log` each run.

It grew out of a single-crate prototype (`inf3d_proto`, now removed) and was
migrated into the proper multi-crate workspace below.

---

## 2. Build & run

The repo pins the **GNU toolchain** via `rust-toolchain.toml`
(`stable-x86_64-pc-windows-gnu`). The MSVC default lacks `link.exe` on this
machine; MinGW (`gcc`) must be on PATH (it is, via WinLibs). No manual PATH dance
needed — plain cargo works.

```powershell
cargo run -p inf3d_app          # binary is named `inf3d`
cargo run -p inf3d_app --release
cargo check --workspace
cargo test  --workspace
```

`[profile.dev]` keeps `split-debuginfo = "packed"` + `strip = "debuginfo"` so the
debug binary stays under Windows's 2 GB PE limit. **Don't remove those.**

### Controls
| Input | Action |
|---|---|
| WASD | Move (camera-relative; the character faces its travel direction) |
| Mouse | Orbit the camera (yaw + pitch); the cursor is captured/hidden in play |
| Hold Left Alt | Free the cursor to click the HUD (Walk/Build buttons, material picker); mouse-look + WASD are suspended while held |
| Scroll | Zoom the boom distance in/out |
| Space | Jump |
| Shift | Sprint |
| F | Toggle free-fly debug camera |
| Build mode | Left-click places / right-click breaks the voxel under the **screen-center crosshair** (aimed ahead of the character, within `BUILD_RANGE` reach) |

Quality presets (Potato/Low/Medium/High) are chosen from the in-game **settings
menu** (`inf3d_menu`), which calls `QualityPreset::apply`; there is **no** preset
hotkey.

`INF3D_UNCAP_FPS=1` switches the window from `AutoVsync` to `Immediate` for
benchmarking. `INF3D_NO_MONITOR=1` disables the telemetry recorder
(`inf3d-monitor.log`).

---

## 3. Crate layout (acyclic)

```
inf3d_app          binary `inf3d`; plugin composition only
inf3d_core         shared data + the GameSet ordering backbone; CorePlugin is the
                   SOLE owner/initializer of the shared resources (QualitySettings
                   + presets, BlockedCells, PropSurfaces, MoveIntent, EditMode,
                   SelectedMaterial, GrassStats, FrameStats). Also: AppState/Pause
                   state machine, FollowTarget, Tree/Rock markers, QualityPreset.
inf3d_worldgen     terrain noise + Terrain oracle (surface_y/stand_pos/is_land/
                   nearest_land), build_noise_lod, WATER_HEIGHT, the single
                   ColumnKind / column_kind() land-water helper (shared with world),
                   the Biome classifier, and the shared VoxelOverrides edit store
inf3d_world        MainWorld voxel config + LOD, WorldPlugin, lighting,
                   get_voxel_fn, the single TerrainMaterialId palette enum (+ the
                   BUILDABLE picker set), and a custom TerrainMaterial (writes the prepass)
inf3d_camera       OrbitCameraPlugin (perspective third-person orbit: mouse
                   yaw+pitch, scroll boom-zoom, boom collision, WASD MoveIntent,
                   free-fly) + post-FX (Bloom/DoF/SSAO/motion blur) wiring
inf3d_physics      avian3d: GameLayer, single PlayerDims source of truth, kinematic
                   CharacterController (analytic terrain ground), DesiredMove,
                   SolidPropCollider, InteractionTarget (screen-center crosshair ray)
inf3d_render       water, fog (horizon clear color), dust, crosshair hover highlight,
                   the block EditPlugin (place/break → VoxelOverrides + NeedsRemesh),
                   the custom CursorPlugin (procedural cursor), and the foliage/ module
                   (.vox load + scatter + stream + spawn)
inf3d_gameplay     PlayerPlugin (spawn, MoveIntent → DesiredMove, animation)
inf3d_ui           HudPlugin (FPS/frame-ms/entities/chunks/pos/tile) + crosshair +
                   the Walk/Build mode buttons and the material picker hotbar
inf3d_menu         MenuPlugin — main menu / pause / settings screens (drive AppState/
                   Pause, apply QualityPreset) + 3-slot RON save/load (save.rs) +
                   the flat test-world stamper (testmap.rs)
inf3d_audio        AudioPlugin — downstream SFX sink (footsteps keyed on surface +
                   block-edit place/break sounds); reads game state/events, owns nothing
inf3d_monitor      MonitorPlugin — read-only "flight recorder". Queries ECS state
                   + frame-over-frame count deltas and writes inf3d-monitor.log
                   each run (overwritten per run): periodic SUMMARY lines, a SPIKE
                   line tagged with the likely cause on every frame hitch, and
                   EVENT lines on move start/stop. Pure downstream
                   sink — instruments nothing, adds no coupling (like the HUD).
```

### Dependency direction (one-way; verified acyclic)
- `core` ← everyone.
- `worldgen` ← world, physics, render, gameplay, ui, menu, monitor.
- `world` ← camera, physics, render, ui, menu, monitor.
- `camera` ← physics, render, menu, monitor.
- `physics` ← render, gameplay, menu, monitor.
- `render` ← gameplay, ui, audio.
- `gameplay` ← ui, menu, audio, monitor.
- `app` ← all. `audio`, `menu`, and `monitor` are downstream-only sinks (each
  depends on the crates it listens to, is depended on by none but `app`).

### The cycle-break (IMPORTANT)
Camera, foliage, and physics-ground all need to **follow the player**, but
`Player` lives in `inf3d_gameplay`, which depends on `inf3d_render` (for
`DustBurst`) and `inf3d_physics`. Querying `Player` upstream would cycle. So the
player entity carries marker/data components that live in **upstream** crates and
downstream crates query *those*:
- `inf3d_core::FollowTarget` — camera & foliage follow this marker, never `Player`.
- `inf3d_physics::CharacterController` / `DesiredMove` — the controller drives the
  entity by these, gameplay only *attaches* them + writes `DesiredMove`.
- `inf3d_core::MoveIntent` — the camera (upstream of gameplay) writes the WASD
  intent and gameplay reads it to set `DesiredMove`, without a dependency edge.
- `inf3d_core::BlockedCells` / `PropSurfaces` — foliage (downstream) writes blocked
  prop cells / short-prop steps into a shared resource that the physics controller
  (upstream) reads as walls / climbable steps, without a dependency edge.

**Don't reintroduce a `Player`/gameplay dependency in camera/physics/render —
use the shared markers above.**

### The scheduling backbone (`inf3d_core::GameSet`)
Every **gameplay** `Update` system is tagged `.in_set(GameSet::X)`, and `CorePlugin`
orders the four phases **once** so the order is fixed regardless of plugin
registration order:

```
Input -> Logic -> Streaming -> Fx
```

- `Input`: raw-input reads (camera orbit/WASD, crosshair targeting, block-edit clicks).
- `Logic`: player animation, interaction-target pick.
- `Streaming`: foliage streaming, prop-collider builds.
- `Fx`: dust, highlights, quality application, water quality, diagnostics, HUD.

`Input`/`Logic`/`Streaming` are chained AND gated to `InGame` + `Pause::Running`;
`Fx` is left **ungated** (only ordered after `Streaming`) so end-of-frame change
detection — e.g. `apply_quality_to_camera` reacting to a settings-menu edit — keeps
firing while paused. Fixed-step and `PostUpdate` systems keep their **avian-relative**
ordering (the physics spine, §5.8) instead of a `GameSet`. Intra-plugin `.chain()`
stays — it's an order *within* a set. Adding a system therefore can't perturb
unrelated ones. **Exception:** `inf3d_menu`'s menu/pause/settings systems are
deliberately **untagged** (they must run outside `InGame`, so the gated gameplay sets
don't fit); the "every gameplay system gets a `GameSet`" rule scopes to gameplay,
and the menu is the legitimate exception.

### Single resource owner (IMPORTANT)
`CorePlugin` is the **sole** `init_resource` for `QualitySettings`, `BlockedCells`,
`PropSurfaces`, `MoveIntent`, `EditMode`, `SelectedMaterial`, `GrassStats`, and
`FrameStats`. Never `init_resource` any of these elsewhere. Resources owned by
exactly one crate stay there (e.g. `Hover` in highlight, `InteractionTarget` in
physics, `Terrain`/`VoxelOverrides`/`MainWorld` in world, `WaterSettings` in water).

---

## 4. Key conventions

- **Voxels are 1×1×1 world units**; chunks are 32³ (`bevy_voxel_world`).
- **Single land/water source of truth.** `inf3d_worldgen::column_kind()` (and the
  `ColumnKind { surface_y, is_water }` it returns) is the *one* helper that
  classifies a column. Both `Terrain` (the gameplay oracle) and
  `inf3d_world::get_voxel_fn` (the meshing delegate on worker threads) go through
  it, so the surface a player stands on can never desync from the meshed
  geometry. `Terrain`'s public methods (`surface_y`/`stand_pos`/`is_land`/
  `nearest_land`) keep their signatures — physics depends on them.
- `inf3d_worldgen::Terrain` is the deterministic height oracle (always LOD 0)
  shared by meshing, physics ground, and foliage. Cheap to `clone()`
  — workers snapshot it.
- **Single material palette.** `inf3d_world::TerrainMaterialId` is the one enum used
  by `get_voxel_fn`, `texture_index_mapper`, the procedural texture-array layer order
  (discriminant == layer index), and the HUD's tile label. The terrain materials are
  `Grass=0`, `Dirt=1`, `Stone=2`, `Seafloor=3`, plus the biome surfaces `Sand=4` and
  `Snow=5` (selected by `biome_surface_material`); then the contiguous **player-build**
  range `Built*` from index `6` (`BUILT_MATERIAL_BASE`) up — the `BUILDABLE` picker set
  the material hotbar offers. The whole palette is generated from one `PALETTE` table
  and a `palette_matches_enum` test guards against a missing/desynced layer, so there
  are no dead/unused texture layers.
- `WATER_HEIGHT = 1.6`: seafloor stands at y=1, land at y≥2. A column is "water"
  (unwalkable) when its standing height ≤ `WATER_HEIGHT`. Players spawn on
  `nearest_land`.
- **Render distance is a single global** (`QualitySettings::render_distance_chunks`,
  default **10** from `quality.ron`), read **once** at `WorldPlugin::build` —
  `bevy_voxel_world` can't re-register, so changing it needs a restart. The quality
  presets **do not** touch it (`QualityPreset::apply` leaves it alone, by design); the
  settings menu surfaces it separately as a restart-only knob. It is modest **on
  purpose**: the perspective orbit view is shallow (the boom keeps the camera near
  the player), so a large radius is wasted. Terrain **LOD** (octave reduction +
  coarser meshing past `terrain_lod_distance`, which IS per-preset) keeps the far
  ring cheap. This is still the dominant perf cost; lower the preset if it hitches.
- **Player body dims live in one place:** `inf3d_physics::PLAYER_DIMS`
  (`radius`/`half_height`/`visual_root_offset`). Gameplay derives its character
  visual-root offset from it — no hand-kept `1.0` literal.
- Camera/foliage follow `FollowTarget`, physics follows `CharacterController`
  (see §3).
- `QualitySettings` (in `core`, installed first by `CorePlugin`) is the single
  knob bundle; most fields apply live via `is_changed()`. SSAO and motion blur now
  have **real dedicated fields** (`ssao_enabled` / `motion_blur_enabled`, gated on
  Medium+), and dense grass is bounded to `grass_radius_world` around the player
  (see §6).

---

## 5. Bevy 0.18 + integration gotchas (still relevant)

1. **`Message` vs `Event`.** Buffered events use `#[derive(Message)]` +
   `MessageReader`/`MessageWriter` + `app.add_message`. Built-in input
   (`MouseMotion`, `MouseWheel`, `CursorMoved`) are `Message`s too.
2. **Post-FX live in `bevy::post_process`** — `Bloom` at `…::bloom::Bloom`,
   `DepthOfField` at `…::dof::DepthOfField`, `MotionBlur` at `…::motion_blur::`.
   SSAO is `bevy::pbr::ScreenSpaceAmbientOcclusion`.
3. **`Hdr` is a marker component** (`bevy::render::view::Hdr`) — required for Bloom.
4. **Prepass markers** are `bevy::core_pipeline::prepass::{DepthPrepass,
   NormalPrepass, MotionVectorPrepass}`. SSAO requires Depth+Normal and **`Msaa::Off`**;
   motion blur requires Depth+MotionVector. The camera adds/strips these to match
   the preset (see `apply_quality_to_camera`).
5. **The voxel terrain now writes the prepass.** `bevy_voxel_world`'s default
   material opts OUT of the depth/normal prepass, which historically blanked SSAO /
   motion blur / DoF / water-depth on terrain. `inf3d_world::terrain_material`
   ships a custom `ExtendedMaterial<StandardMaterial, VoxelTerrainExtension>` whose
   forward shader mirrors upstream `voxel_texture.wgsl` but whose **prepass is
   delegated to stock `pbr_prepass.wgsl`** and `enable_prepass() == true`. **This
   is the central graphics enabler — don't revert to the stock voxel material.**
6. **Fog was removed.** `inf3d_render::fog` is now just the horizon `ClearColor`.
   There is no `DistanceFog`/`VolumetricFog` and no `fog_*` field in
   `QualitySettings`. (Volumetric fog lived in `bevy::light` if you re-add it.)
7. **`bevy_water`**: no `ssr` (its SSR uses the deferred path the forward-only
   terrain material doesn't feed); features are `embed_shaders` + `depth_prepass` +
   `image_utils`. It loads WGSL from `crates/inf3d_app/assets/shaders/` — without
   those files water is invisible. The depth-based deep/shallow blend now works
   because the terrain writes depth (gotcha #5).
8. **avian3d 0.6.1 at a FIXED timestep.** `main` adds `PhysicsPlugins::default()`
   (default fixed schedule, `FixedPostUpdate`) — **not** `PhysicsPlugins::new(
   PostUpdate)`. The kinematic `player_controller` runs in `FixedPostUpdate`
   `.after(PhysicsSystems::Writeback)` using the *fixed* delta, and the player
   carries avian's **`TransformInterpolation`** so the rendered transform is eased
   smoothly between fixed ticks (this killed the zoom-out jitter the old
   variable-timestep `PostUpdate` hack fought). `PhysicsInterpolationPlugin` ships
   inside `PhysicsPlugins` — no extra plugin. Global gravity is zero
   (`insert_resource(Gravity(Vec3::ZERO))`); the controller applies its own only
   while airborne. **There is NO terrain collider:** the terrain is a pure
   heightfield, so the controller reads ground **analytically** from
   `Terrain::surface_y` (top face = `surface_y + 1`) for the column under the
   player. The old solid voxel-ground collider (`VoxelGround` + spawn/recenter
   systems) was **deleted** — don't reintroduce it. `move_and_slide` still resolves
   horizontal blocking against **Solid props** only.
9. **Chunk streaming is distance-only (no frustum culling).** `bevy_voxel_world`
   spawns a **disc of chunk entities** within `spawning_distance`
   (`FarAway` despawn + `Close` flood-fill spawn, so the orbit view never reveals an
   unspawned hole at the screen edge). Two tamings: (a) we cap `max_spawn_per_frame`
   (derived from `render_distance_chunks` — roughly one boundary face plus a margin)
   so the initial-fill / fast-travel backlog spreads over frames instead of dumping a
   1000+-chunk spawn burst in one hitch; (b) **the vertical over-spawn is fixed** —
   `MainWorld::chunk_y_bounds` (a vendored-fork hook) clamps streaming to the shallow
   playable chunk band `[-1, 3]`, so the stock 3D *sphere* no longer wastes layers of
   empty-air / fully-solid-invisible chunks above and below. The `inf3d-monitor.log`
   SPIKE lines tag any remaining chunk-burst hitches so you can see them.
10. **GNU toolchain pinned** (`rust-toolchain.toml`); **2 GB PE limit** profile
    settings; both must stay.

---

## 6. What's NOT done / next steps

- **Foliage is `.vox` models** (MagicaVoxel via `dot_vox`), streamed in a tile
  ring with async scatter and per-prop colliders. **Dense grass is capped to
  `QualitySettings::grass_radius_world`** around the player so zooming out doesn't
  blow up the grass carpet — sparse trees/rocks still fill the orbit view to the
  edges via the ring; only the expensive grass is bounded to the player-centered
  circle. It lives in the `inf3d_render::foliage` module (`mod.rs`/`vox_mesh.rs`/
  `scatter.rs`/`stream.rs`/`spawn.rs`). A vertex-shader **wind** + **player-shove**
  is still unbuilt.
- **Optimization pass** (draw-call batching, further LOD tuning) is deferred. The
  HUD shows entity/chunk counts + frame-ms p95; `inf3d-monitor.log` correlates each
  hitch with its cause to guide it. (The vertical chunk over-spawn is already fixed via
  `chunk_y_bounds`, §5.9.)
- **Audio** (footsteps + block-edit SFX), **save/load** (3-slot RON), the
  **AppState/Pause** state machine, the menu/settings UI, and block **place/break**
  are all **done**. Still unbuilt: combat, mobs, inventory/items, foliage wind. The
  `Tree`/`Rock` markers + `InteractionTarget` remain unwired hooks for harvesting.
- **Tier-2 x-ray cleanup is pending** — the terrain-material cutaway code
  (`XrayParams` uniform, `voxel_cut_by_xray`, `terrain_prepass.wgsl`,
  `terrain_xray.wgsl`) still EXISTS but is **inert**: the `XrayPlugin` that drove it
  was deleted, so nothing feeds the uniform and the cut never engages. Excising it is
  deferred until it's GPU-verifiable (see `BACKLOG.md`); keep `enable_prepass()=true`.

---

## 7. Don't

- Don't reintroduce a `Player`/gameplay dependency in `inf3d_camera`/
  `inf3d_physics`/`inf3d_render` — use the shared markers in §3.
- Don't revert the custom `TerrainMaterial` to the stock `bevy_voxel_world`
  material (it would re-blank SSAO / motion blur / DoF / water depth on terrain).
- Don't re-enable `bevy_water`'s `ssr` (forward-only terrain doesn't feed deferred).
- Don't remove the GNU toolchain pin or the `[profile.dev]` PE-size settings.
- Don't `unwrap()` outside tests — use `let Ok(..) = .. else { return; };`.
- Don't reverse-project a `Transform` back to a tile for gameplay — `Player`
  stores its logical `cell`.
- Don't move the player by mutating its `Transform` from gameplay — write
  `DesiredMove` and let `inf3d_physics::player_controller` resolve it.
- Don't reintroduce a terrain collider (`VoxelGround` and friends were deleted) —
  the controller reads ground analytically from `Terrain::surface_y`.
- Don't run physics in `PostUpdate` or at a variable timestep — keep avian's
  default fixed schedule + the player's `TransformInterpolation` (it's what makes
  walking smooth at every zoom).
- Don't `init_resource` any of the shared resources (`QualitySettings`,
  `BlockedCells`, `PropSurfaces`, `MoveIntent`, `EditMode`, `SelectedMaterial`,
  `GrassStats`, `FrameStats`) outside `CorePlugin`, and don't add a **gameplay**
  `Update` system without an `.in_set(GameSet::…)` tag (the `inf3d_menu`
  menu/pause/settings systems are the legit exception — they run outside `InGame`).
- Don't make any crate depend on `inf3d_monitor`, `inf3d_audio`, or `inf3d_menu` —
  they're downstream-only sinks (only `app` depends on them).
