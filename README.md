# inf3d

A 3D voxel open-world game in Rust, built on **Bevy 0.18** with **avian3d** physics
and a vendored **bevy_voxel_world** fork. Procedural infinite voxel terrain with a
custom terrain material (writes the depth/normal/motion prepass), a perspective
third-person **orbit** follow camera with camera-relative WASD movement, a kinematic
character controller (terrain ground derived analytically), block place/break,
biomes with per-biome `.vox` foliage, animated water, audio, a settings/save-load
menu, and a read-only telemetry recorder.

## Build & run

The repo pins the **GNU toolchain** via `rust-toolchain.toml`
(`stable-x86_64-pc-windows-gnu`); MinGW (`gcc`) must be on `PATH` (it is, via
WinLibs). No manual PATH dance — plain cargo works.

```powershell
cargo run -p inf3d_app           # binary is named `inf3d`
cargo run -p inf3d_app --release
cargo check --workspace
cargo test  --workspace
```

`[profile.dev]` keeps `split-debuginfo = "packed"` + `strip = "debuginfo"` so the
debug binary stays under Windows's 2 GB PE limit — don't remove those.

Env toggles: `INF3D_UNCAP_FPS=1` switches the window from `AutoVsync` to `Immediate`
for benchmarking; `INF3D_NO_MONITOR=1` disables the telemetry recorder
(`inf3d-monitor.log`).

## Controls

| Input | Action |
|---|---|
| WASD | Move (camera-relative; the character faces its travel direction) |
| Mouse | Orbit the camera (yaw + pitch); the cursor is captured/hidden in play |
| Scroll | Zoom the boom distance in/out |
| Space | Jump |
| Shift | Sprint |
| F | Toggle free-fly debug camera |
| Hold Left Alt | Free the cursor to click the HUD (Walk/Build buttons, material picker); mouse-look + WASD are suspended while held |
| Build mode | Left-click places / right-click breaks the voxel under the screen-center crosshair (within `BUILD_RANGE` reach) |

Quality presets (Potato/Low/Medium/High) are applied from the in-game **settings
menu**, not a hotkey.

## Workspace (12 crates, acyclic)

```
inf3d_app          binary `inf3d`; plugin composition only
inf3d_core         shared data + the GameSet ordering backbone; sole owner of the
                   shared resources (QualitySettings + presets, BlockedCells,
                   PropSurfaces, MoveIntent, EditMode, SelectedMaterial, GrassStats,
                   FrameStats); AppState/Pause state machine
inf3d_worldgen     terrain noise + Terrain oracle, the single ColumnKind land/water
                   helper, the Biome classifier, and the shared VoxelOverrides edit store
inf3d_world        MainWorld voxel config + LOD, WorldPlugin, lighting, get_voxel_fn,
                   the TerrainMaterialId palette, and the custom TerrainMaterial
inf3d_camera       OrbitCameraPlugin (perspective orbit: mouse yaw+pitch, scroll
                   boom-zoom, boom collision, WASD MoveIntent, free-fly) + post-FX
inf3d_physics      avian3d: kinematic CharacterController (analytic terrain ground),
                   DesiredMove, prop colliders, the screen-center interaction ray
inf3d_render       water, horizon clear color, dust, crosshair hover highlight, the
                   block EditPlugin, the custom cursor, and the foliage module
inf3d_gameplay     PlayerPlugin (spawn, MoveIntent -> DesiredMove, animation)
inf3d_ui           HudPlugin (FPS/frame-ms/entities/chunks/pos/tile) + crosshair +
                   the mode buttons and material picker
inf3d_menu         main menu / pause / settings screens + 3-slot RON save/load
inf3d_audio        downstream SFX sink (footsteps + block-edit sounds)
inf3d_monitor      read-only "flight recorder"; writes inf3d-monitor.log each run
```

A vendored `bevy_voxel_world` fork lives under `vendor/` (don't edit it).
`inf3d_core` depends on nothing else in the workspace; the dependency graph is
one-way and acyclic. See `CLAUDE.md` for the full architecture, the cycle-break
markers, and the Bevy 0.18 integration gotchas.
