# inf3d — Project Guide for the Next Claude Agent

Handover doc. Read it before touching code.

---

## 1. What this is

A **3D voxel open-world game** in Rust + **Bevy 0.18**. Procedural infinite voxel
terrain (via `bevy_voxel_world`), an **orthographic isometric** follow camera
(Diablo-style 3/4 view), click-to-move **A\*** pathfinding over the voxel surface,
a procedural multi-part **player character**, animated **water** (`bevy_water`),
**volumetric fog-of-war**, **dust** particles, instanced **grass**, post-FX
(Bloom + Depth of Field), and a debug **HUD**.

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
cargo check -p inf3d_app
```

`[profile.dev]` keeps `split-debuginfo = "packed"` + `strip = "debuginfo"` so the
debug binary stays under Windows's 2 GB PE limit. **Don't remove those.**

### Controls
| Input | Action |
|---|---|
| Left-click ground | Pathfind + walk there (water is unwalkable) |
| Scroll | Zoom |
| Q / E or middle-drag | Orbit camera (horizontal only — iso preserved) |
| Mouse hover | Highlight the voxel under the cursor |

---

## 3. Crate layout (9 crates, acyclic)

```
inf3d_app          binary `inf3d`; plugin composition only
inf3d_core         shared markers (FollowTarget)
inf3d_worldgen     terrain noise + Terrain oracle (surface_y/stand_pos/is_land/nearest_land), WATER_HEIGHT
inf3d_world        MainWorld voxel config, WorldPlugin, lighting, get_voxel_fn, RENDER_DISTANCE_CHUNKS
inf3d_camera       IsoCameraPlugin (ortho follow, zoom, orbit) + post-FX components
inf3d_render       water, fog, dust, hover-highlight, grass (visual/FX crate)
inf3d_gameplay     PlayerPlugin (spawn, movement, character animation)
inf3d_pathfinding  PathfindPlugin (click → voxel raycast → A* over surface)
inf3d_ui           HudPlugin (FPS/frame-ms/entities/chunks/pos/tile)
```

### Dependency direction (one-way)
- `core` ← everything that needs `FollowTarget`.
- `worldgen` ← world, render, gameplay, pathfinding, ui.
- `world` ← camera, render, pathfinding, ui.
- `camera` ← render, pathfinding.
- `render` ← gameplay, ui.
- `gameplay` ← pathfinding, ui.
- `app` ← all.

### The cycle-break (IMPORTANT)
Camera, fog, and grass all need to follow the player, but `Player` lives in
`inf3d_gameplay`, which depends on `inf3d_render` (for `DustBurst`). If camera/fog
queried `Player`, you'd get `gameplay → render → camera → gameplay`. Instead,
`inf3d_core::FollowTarget` is a marker attached to the player entity, and
camera/fog/grass query `With<FollowTarget>`. **Don't reintroduce a `Player`
dependency in camera/render — use `FollowTarget`.**

---

## 4. Key conventions

- **Voxels are 1×1×1 world units**; chunks are 32³ (`bevy_voxel_world`).
- `inf3d_worldgen::Terrain` is the deterministic height oracle shared by meshing
  (worker threads) and gameplay (pathfinding/standing). It mirrors `get_voxel_fn`.
- `WATER_HEIGHT = 1.6`: seafloor (material 3) stands at y=1, land at y≥2. A column
  is "water" (unwalkable) when its standing height < `WATER_HEIGHT`. Players spawn
  on `nearest_land`.
- `RENDER_DISTANCE_CHUNKS = 40` — the dominant perf cost. Lower it (12–20) if it hitches.
- Grass/camera/fog follow `FollowTarget`, not `Player` (see §3).

---

## 5. Bevy 0.18 gotchas (still relevant)

1. **`Message` vs `Event`.** Buffered events use `#[derive(Message)]` +
   `MessageReader`/`MessageWriter` + `app.add_message`. Built-in input
   (`MouseMotion`, `MouseWheel`, `CursorMoved`) are `Message`s too.
2. **Post-FX live in `bevy::post_process`** — `Bloom` at
   `bevy::post_process::bloom::Bloom`, `DepthOfField` at `…::dof::DepthOfField`.
3. **`Hdr` is a marker component** (`bevy::render::view::Hdr`) — required for Bloom.
4. **`DepthPrepass`** is `bevy::core_pipeline::prepass::DepthPrepass`.
5. **Volumetric fog** types are in `bevy::light` (`FogVolume`, `VolumetricFog`,
   `VolumetricLight`).
6. **The voxel terrain material (`bevy_voxel_world`) has no fog code and opts OUT
   of the depth/normal prepass.** Consequences we hit the hard way:
   - `DistanceFog` does nothing on terrain → we use **volumetric** fog (renders in
     its own pass) for the fog-of-war.
   - SSAO / per-object motion blur / SSR can't shade the terrain (no prepass).
   - `bevy_water`'s **`ssr`** feature and **depth-based** coloring break/blank the
     water for the same reason — so `bevy_water` uses `embed_shaders` +
     `depth_prepass` + `image_utils` only (no `ssr`), and a deep/shallow color blend.
7. **`bevy_water` loads its WGSL from `assets/shaders/`** — they're shipped in
   `crates/inf3d_app/assets/shaders/`. Without them water is invisible.
8. **GNU toolchain pinned** (`rust-toolchain.toml`); **2 GB PE limit** profile
   settings; both must stay.

---

## 6. What's NOT done / next steps

- **Grass** is a static instanced baseline (procedural tufts). The planned next
  step is a **vertex-shader wind** + **player-shove** (bend grass away from the
  player). It lives in `inf3d_render::grass`, scatters on land via a streaming
  tile ring, and culls by distance. It is **not** a model/GLTF anymore.
- **Realistic terrain shading** (SSAO/SSR/real AO on blocks) needs a custom
  terrain material that writes the prepass — `bevy_voxel_world`'s default doesn't.
- **Optimization pass** (one-chunk asset meshing, GPU-instanced grass with vertex
  wind, LOD rings, draw-call batching) is deferred. The HUD shows entity/chunk
  counts + frame-ms to guide it.
- No audio, save/load, combat, mobs, inventory, or items yet.

---

## 7. Don't

- Don't reintroduce a `Player`/gameplay dependency in `inf3d_camera`/`inf3d_render`
  — use `inf3d_core::FollowTarget`.
- Don't re-enable `bevy_water`'s `ssr` or rely on depth-based water coloring (the
  terrain isn't in the prepass).
- Don't remove the GNU toolchain pin or the `[profile.dev]` PE-size settings.
- Don't `unwrap()` outside tests — use `let Ok(..) = .. else { return; };`.
- Don't reverse-project a `Transform` back to a tile for gameplay — `Player`
  stores its logical `cell`.
```
