# inf3d — Project Guide for the Next Claude Agent

Handover doc. Read it before touching code.

---

## 1. What this is

A **3D voxel open-world game** in Rust + **Bevy 0.18**. Procedural infinite voxel
terrain (via `bevy_voxel_world`) with a **custom terrain material** that writes
the depth/normal/motion prepass, an **orthographic isometric** follow camera
(Diablo-style 3/4 view), click-to-move **A\*** pathfinding over the voxel surface,
**avian3d** physics (kinematic character controller + prop colliders), a
procedural multi-part **player character**, animated **water** (`bevy_water`),
**`.vox` foliage** (trees / rocks / grass from MagicaVoxel models, streamed in a
tile ring), **dust** particles, post-FX (Bloom + Depth of Field + **SSAO** +
**motion blur**), and a debug **HUD** with runtime quality presets (F2).

It grew out of a single-crate prototype (`inf3d_proto`, now removed) and was
migrated into the proper multi-crate workspace below. The old 2.5D `inf2d_*`
engine has been deleted.

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
| Left-click ground | Pathfind + walk there (water is unwalkable, props are detoured) |
| Scroll | Zoom |
| Q / E or middle-drag | Orbit camera (horizontal only — iso preserved) |
| Mouse hover | Highlight the voxel under the cursor |
| F2 | Cycle quality preset (Potato → Low → Medium → High) |

`INF3D_UNCAP_FPS=1` switches the window from `AutoVsync` to `Immediate` for
benchmarking.

---

## 3. Crate layout (10 crates, acyclic)

```
inf3d_app          binary `inf3d`; plugin composition only
inf3d_core         shared data: FollowTarget, BlockedCells, PathTarget, Tree/Rock
                   markers, QualitySettings + presets, GrassStats/FrameStats
inf3d_worldgen     terrain noise + Terrain oracle (surface_y/stand_pos/is_land/
                   nearest_land), build_noise_lod, WATER_HEIGHT
inf3d_world        MainWorld voxel config + LOD, WorldPlugin, lighting,
                   get_voxel_fn, custom TerrainMaterial (writes the prepass)
inf3d_camera       IsoCameraPlugin (ortho follow, zoom, orbit) + post-FX
                   (Bloom/DoF/SSAO/motion blur) wiring
inf3d_physics      avian3d: GameLayer, solid voxel-ground collider, kinematic
                   CharacterController, SolidPropCollider, InteractionTarget
inf3d_render       water, fog (clear color), dust, hover/destination highlight,
                   foliage/ module (.vox load + scatter + stream + spawn)
inf3d_gameplay     PlayerPlugin (spawn, path-follow → DesiredMove, animation)
inf3d_pathfinding  PathfindPlugin (click → voxel raycast → async A* over surface)
inf3d_ui           HudPlugin (FPS/frame-ms/entities/chunks/pos/tile/quality)
```

### Dependency direction (one-way; verified acyclic)
- `core` ← everyone.
- `worldgen` ← world, physics, render, gameplay, pathfinding, ui.
- `world` ← camera, physics, render, pathfinding, ui.
- `camera` ← physics, render, pathfinding.
- `physics` ← render, gameplay.
- `render` ← gameplay, ui.
- `gameplay` ← pathfinding, ui.
- `app` ← all.

### The cycle-break (IMPORTANT)
Camera, foliage, and physics-ground all need to **follow the player**, but
`Player` lives in `inf3d_gameplay`, which depends on `inf3d_render` (for
`DustBurst`) and `inf3d_physics`. Querying `Player` upstream would cycle. So the
player entity carries marker/data components that live in **upstream** crates and
downstream crates query *those*:
- `inf3d_core::FollowTarget` — camera & foliage follow this marker, never `Player`.
- `inf3d_physics::CharacterController` / `DesiredMove` — the controller drives the
  entity by these, gameplay only *attaches* them + writes `DesiredMove`.
- `inf3d_core::BlockedCells` / `PathTarget` — foliage (downstream) writes blocked
  prop cells into a shared resource that the pathfinder (upstream) reads, without
  a dependency edge.

**Don't reintroduce a `Player`/gameplay dependency in camera/physics/render —
use the shared markers above.**

---

## 4. Key conventions

- **Voxels are 1×1×1 world units**; chunks are 32³ (`bevy_voxel_world`).
- `inf3d_worldgen::Terrain` is the deterministic height oracle (always LOD 0)
  shared by meshing (worker threads), pathfinding, physics ground, and foliage.
  It mirrors `get_voxel_fn`. Cheap to `clone()` — workers snapshot it.
- `WATER_HEIGHT = 1.6`: seafloor (material 3) stands at y=1, land at y≥2. A column
  is "water" (unwalkable) when its standing height < `WATER_HEIGHT`. Players spawn
  on `nearest_land`.
- **Render distance is per-preset** (`QualitySettings::render_distance_chunks`,
  5/8/12/16 for Potato/Low/Medium/High) and read **once** at `WorldPlugin::build`
  — `bevy_voxel_world` can't re-register, so changing it needs a restart. Terrain
  **LOD** (octave reduction + coarser meshing past `terrain_lod_distance`) keeps
  far chunks cheap, so the distance can be modest. This is still the dominant perf
  cost; lower the preset if it hitches.
- Camera/foliage follow `FollowTarget`, physics follows `CharacterController`
  (see §3).
- `QualitySettings` (in `core`, installed first by `CorePlugin`) is the single
  knob bundle; most fields apply live via `is_changed()`. SSAO + motion blur are
  currently gated on `bloom_enabled` (no dedicated field — see §6).

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
8. **avian3d 0.6.1** targets bevy ^0.18. Global gravity is set to zero in `main`;
   the kinematic controller applies its own. Features pinned in the workspace dep
   (`3d`, `f32`, `parry-f32`, `collider-from-mesh`, `parallel`, `bevy_scene`).
9. **GNU toolchain pinned** (`rust-toolchain.toml`); **2 GB PE limit** profile
   settings; both must stay.

---

## 6. What's NOT done / next steps

- **Foliage is `.vox` models** (MagicaVoxel via `dot_vox`), streamed in a tile
  ring with async scatter, per-prop colliders, and a far-distance LOD that drops
  grass. It lives in the `inf3d_render::foliage` module (`mod.rs`/`vox_mesh.rs`/
  `scatter.rs`/`stream.rs`/`spawn.rs`). A vertex-shader **wind** + **player-shove**
  is still unbuilt.
- **Redundant voxel-ground collider.** `inf3d_physics` builds a ~49×49×3 solid
  `Collider::voxels` patch (rebuilt on recenter) whose *only* consumer is the
  controller's downward ground ray. The terrain is a pure heightfield, so that ray
  could be answered analytically from the `Terrain` oracle — letting you delete
  `VoxelGround` + `spawn_voxel_ground` + `recenter_voxel_ground` entirely.
- **SSAO + motion blur share `bloom_enabled` as their quality gate** (no dedicated
  `QualitySettings` fields yet). `MotionBlur.samples == 1` is nearly invisible;
  bump it if you want a real smear.
- **Optimization pass** (draw-call batching, further LOD tuning) is deferred. The
  HUD shows entity/chunk counts + frame-ms p95 to guide it.
- No audio, save/load, combat, mobs, inventory, or items yet. `Tree`/`Rock`
  markers + `InteractionTarget` exist as hooks for harvesting.

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
