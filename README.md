# inf2d

Custom-built isometric (2.5D) infinite-world engine in Rust + Bevy.

## Prerequisites (one-time, Windows)

Bevy needs a C++ linker. Rust on this machine is installed without one — install **Visual Studio Build Tools** (free, ~3 GB):

```powershell
# Requires admin
winget install --id Microsoft.VisualStudio.2022.BuildTools -e `
  --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621"
```

After install, open a fresh PowerShell so `link.exe` is on `PATH`, then:

```powershell
rustup default stable-x86_64-pc-windows-msvc
```

(Alternative: MinGW-w64 via MSYS2 — works with the GNU toolchain, smaller, no admin. Tell me if you prefer this path and I'll wire it.)

## Run

```powershell
# Debug build (faster compile, slower runtime, dev tools enabled):
cargo run -p inf2d_app

# Release build (slow compile, fast runtime, dev tools still on):
cargo run -p inf2d_app --release

# Release without dev tools (no inspector, no physics debug plugin):
cargo run -p inf2d_app --release --no-default-features --features release
```

## Controls

| Input | Action |
|---|---|
| Middle / Right mouse drag | Pan camera |
| Scroll wheel | Zoom in/out |
| Left click | Select (gameplay hook — emits in slice 2) |
| **F3** | Toggle world inspector |
| **F4** | Toggle physics debug renderer (when `--features dev`) |
| **F5** | Toggle chunk-border gizmos |
| Esc | Pause / resume |

## Workspace

```
inf2d_app          binary, plugin composition
inf2d_core         coords (TilePos/ChunkPos), iso math, RNG, states, system sets
inf2d_input        leafwing actions + default bindings
inf2d_world        chunks, chunk manager, Generator trait, streaming
inf2d_worldgen     deterministic biome generator (Perlin fBm, Whittaker classifier)
inf2d_render       procedural tile atlas + bevy_ecs_tilemap per chunk
inf2d_physics      Avian2D, per-chunk compound colliders from solid tiles
inf2d_camera       camera rig (Free/Follow/Cinematic), pan/zoom/picking
inf2d_ui           egui HUD overlay
inf2d_debug        inspector + chunk gizmos + iyes_perf_ui overlay
```

Dependency arrows are acyclic. `inf2d_core` depends on nothing in the workspace.
