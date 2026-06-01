# inf2d — Project Guide for the Next Claude Agent

This file is the handover document. If you're a new Claude session opening this project, read it end-to-end before touching code. Everything that surprised earlier agents is captured here.

---

## 1. What this project is

A 2.5D isometric infinite-world engine built in Rust + Bevy 0.18, no premade game engine. Visual target: Diablo / Tactics Ogre / RimWorld style — click-to-move, terraced terrain, day/night cycle, post-FX stack, sprite-stacked entities.

**Not** a game yet — it's an engine with gameplay scaffolding. The remaining work is content (more mobs, items, dialog, dungeons, UI screens, audio assets, real tile art) rather than engine plumbing.

### Visual stack (top of frame)

```
UI (egui — HUD, minimap, loading, pause)              z=10
Post-FX (vignette, heat, god rays)                    z=7.1..7.2
LUT color grading (real render-graph node)            (post-process, no layer)
Day/night overlay (legacy color cast)                 z=5
Entities (player, mobs, trees — sprite-stacked)       z=2
Shadows (soft ellipse, drop-shadow per entity)        z=1.5
Decals                                                z=1
Water shader quad (per chunk, shimmer + spec)         z=tile.height*0.01 + 0.005
Tile diamonds (bevy_ecs_tilemap, lit material)        z=0..0.07 by height
HLOD imposters (far chunks)                           z=0
```

### Engine stack

- **Bevy 0.18.1** with custom subset of features (no PBR; we run pure 2D + sprite_render).
- **bevy_ecs_tilemap 0.18.1** with custom `MaterialTilemap` (`LitTilemapMaterial`) for normal-mapped tile lighting.
- **avian2d 0.6** for physics. Player is `RigidBody::Kinematic`; terrain colliders are greedy-meshed parallelograms.
- **bevy_egui 0.39** for HUD/menu/minimap.
- **bevy_picking** for entity-level picking; `bevy::picking::events::Pointer<E>` consumed via `MessageReader`.
- **bevy_tweening 0.15** for camera/UI tweens.
- **bevy_post_process** (separate Bevy crate) for `Bloom` and the LUT pass.
- **bevy_inspector_egui 0.36** behind F3 for live editing.
- **noise 0.9** for procedural worldgen.
- **avian2d** physics with custom `GameLayer` collision layers (renamed from `PhysicsLayer` to dodge avian trait name collision — see §7).

---

## 2. How to build and run

### Toolchain prerequisites (Windows-specific)

The author's machine has Rust stable installed as `stable-x86_64-pc-windows-gnu` and a MinGW64 install at `C:\Users\yaman.salman.MHPSALDEV19\mingw64\bin`. The Rust GNU toolchain bundles its own `dlltool.exe` *inside* the toolchain folder but doesn't put it on PATH — so we prepend MinGW manually in every build command. The MSVC toolchain is also installed but lacks `link.exe` (no Visual Studio Build Tools).

If on a different machine: install **one** of
- Visual Studio Build Tools 2022 (provides `link.exe`) → switch to `stable-x86_64-pc-windows-msvc`, **or**
- MSYS2 / MinGW-w64 → keep the GNU toolchain.

Linux/macOS users: standard `rustup default stable`; no extra dance.

### Building

```powershell
# From the project root C:\Users\yaman.salman\Desktop\inf2d
$env:Path = 'C:\Users\yaman.salman.MHPSALDEV19\mingw64\bin;' + $env:USERPROFILE + '\.cargo\bin;' + $env:Path
cargo build -p inf2d_app                  # debug, ~184 MB exe
cargo build -p inf2d_app --release        # release, much smaller
```

**Important:** `[profile.dev]` in `Cargo.toml` sets `split-debuginfo = "packed"` + `strip = "debuginfo"`. Without this, the debug binary grows past Windows's 2 GB PE limit and refuses to launch with "not a valid application for this OS platform." Don't remove these settings.

### Running

```powershell
.\target\debug\inf2d.exe
```

Window opens at 1280×720 centered on monitor index 1 (falls back to primary if no second monitor).

### Controls

| Input | Action |
|---|---|
| Middle / right mouse drag | Pan camera (kicks out of Follow into Free mode) |
| Scroll wheel | Smooth zoom |
| Left click on walkable tile | Pathfind + walk |
| Left click on non-walkable tile | Red rejection puff |
| Left click on slime | Deal damage |
| F | Toggle Follow / Free camera mode |
| F3 | bevy_inspector_egui world inspector |
| F4 | Avian physics debug renderer |
| F5 | Chunk-border gizmos |
| Esc | Pause menu |

---

## 3. Workspace structure

13 crates in `crates/`. Dependency graph is acyclic; arrows point downward.

```
inf2d_app                 binary, plugin composition, main()
├── inf2d_core            coords, iso math, RNG (SplitMix64), states, system sets
├── inf2d_input           native Bevy input → InputState resource
├── inf2d_world           chunks, ChunkManager, Generator trait, async streaming, props (Tree)
├── inf2d_worldgen        Perlin fBm biome generator → ChunkData
├── inf2d_render          tile atlas, lit material, water shader, day/night, post-FX, lights, particles, shadows, hover, cliffs, HLOD, sprite stacks
├── inf2d_physics         Avian2D plugin, per-chunk greedy-meshed colliders, GameLayer collision categories
├── inf2d_camera          rig (Free/Follow/Cinematic), pan/zoom/picking, shake
├── inf2d_pathfinding     A* with height-aware walkability, replan on chunk events
├── inf2d_gameplay        player, click-to-move, follow camera, walking, footsteps, mobs, stats, tree visual bridge
├── inf2d_audio           bevy_audio mixer + PlaySfx/PlayMusic/StopMusic messages
├── inf2d_save            serde+ron save/load (camera, world seed)
├── inf2d_ui              HUD, loading screen, minimap, pause menu
└── inf2d_debug           inspector toggle, chunk gizmos
```

Dependency direction summary:
- `inf2d_render` depends on `inf2d_core`, `inf2d_world`, `inf2d_camera` (one-way; render bridges to camera for cursor data via `CursorPick`).
- `inf2d_gameplay` is the **integration layer** — depends on render, world, camera, input, pathfinding, physics. New cross-cutting features (like the tree visual bridge) often live here because it's the only place that can see all the pieces without cycling.
- `inf2d_worldgen` writes a `WorldSeed` resource owned by `inf2d_world` so `inf2d_world::props` can use the same seed for deterministic scatter.

---

## 4. Coordinate conventions (READ THIS)

These are the most common landmines. Every agent so far has hit at least one.

### Iso projection

```rust
inf2d_core::tile_to_world(tile)             // returns the diamond's BOTTOM vertex
inf2d_core::tile_to_world_with_height(tile, h)  // bottom vertex + (0, h * HEIGHT_STEP_PX)
```

The doc comment in `inf2d_core::iso` *historically* claimed "left vertex." The math says bottom vertex. **Always treat as bottom vertex.** Diamond corners relative to anchor B:
- Bottom: B
- Left:   B + (-W/2, +H/2)
- Top:    B + (0, +H)
- Right:  B + (+W/2, +H/2)

With `W = TILE_WIDTH = 64`, `H = TILE_HEIGHT = 32`, `HEIGHT_STEP_PX = 16`.

### `world_to_tile` is height-blind

It picks the **ground-plane** tile under a world point. Use `inf2d_camera::CursorPick::tile` instead — that's height-aware (iterates layers top-down).

### Chunk layout

- `CHUNK_SIZE = 32` tiles per side; `CHUNK_TILES = 1024` per chunk.
- Chunks indexed by signed `ChunkPos { x: i32, y: i32 }`.
- `ChunkPos::from_tile(world_tile)` and `chunk_pos.local_of(world_tile)` are your friends.
- Chunk entity's `Transform` sits at `chunk_origin_world(pos)` = `tile_to_world(chunk.origin_tile())`. Tile entities are children with local positions relative to that anchor.

### Tile height

- `Tile { kind: TileKind, height: i8 }`. `height` is in **steps**, not pixels. One step = `HEIGHT_STEP_PX` (16 px) of upward screen-Y shift.
- Water sits at height -1 (recessed). Grass 0–2. Stone 4–5. Snow 6–7. Max is `BiomeParams::max_height_steps` (currently 14).
- Pathfinding allows `|Δh| ≤ 1` between adjacent tiles, OR any value across a `TileKind::Stairs` tile.

### Render z-order

`inf2d_render::layers::RenderLayer` consts. Per-chunk tilemaps shift z by `height * 0.01` so taller tiles win Z-sort against shorter neighbors. Water shader quads sit at `tile.height * 0.01 + 0.005` (between water tile and any non-water at height ≥ 0). The legacy flat `RenderLayer::WATER = 0.5` exists for backward compat but **isn't used** for water rendering anymore.

---

## 5. System set conventions

Top-level scheduling (Bevy `Update`):

```
CoreSet           input sampling, camera pan/zoom, shake, day/night advance
  ↓
SimulationSet     gameplay logic — runs only when GameState::Playing
  ↓
RenderPrepSet     cursor pick, hover gizmo, tilemap spawn/despawn on ChunkLoaded
```

Plus state gating: the whole chain runs only `in_state(AppState::InGame)`. Pause menu freezes `SimulationSet` only — camera/UI keep ticking.

### Pause is centralized

If you're adding a new gameplay system that should freeze on pause, put it `.in_set(SimulationSet)`. Don't roll your own `run_if(in_state(GameState::Playing))`. The gating happens once in `inf2d_core::CorePlugin::configure_sets`.

---

## 6. State machines

```rust
inf2d_core::AppState::{Loading, InGame}        // top-level
inf2d_core::GameState::{Playing, Paused}       // sub-state of AppState::InGame
```

- `AppState::Loading` is entered at startup. The loading screen renders until `ChunkManager::loaded_count() >= MIN_CHUNKS_FOR_INGAME (9)` then auto-transitions to `InGame`.
- `GameState::Paused` is toggled by Esc once in `InGame`.

---

## 7. Bevy 0.18 quirks the agents kept tripping over

1. **`Event` vs `Message`.** Bevy 0.18 split observable events (`Event`) from buffered events (`Message`). All our buffered events use `#[derive(Message)]`, `MessageReader<T>`, `MessageWriter<T>`, `app.add_message::<T>()`. Don't use `EventReader`/`EventWriter` — they're for observers.

2. **`bevy_post_process` is a separate crate.** Bloom/LUT/etc. live in `bevy::post_process::*`, not `bevy::core_pipeline::*`. The workspace bevy features list includes `"bevy_post_process"`.

3. **`bevy::camera::*`.** `Camera2d`, `Projection`, `OrthographicProjection`, `Hdr` all live in `bevy_camera` (workspace feature: `"bevy_camera"`). Old paths like `bevy::render::camera::Projection` are now private.

4. **`Hdr` is a marker component.** Attach to camera entity to opt into Rgba16Float color targets. `Camera::hdr` field no longer exists.

5. **`MessageReader<Pointer<Click>>` for picking events.** Picking observers also work, but the codebase uses MessageReader to consolidate into the `EntityPick` resource.

6. **`avian2d::PhysicsLayer` is both a trait AND a derive macro.** Naming your local layer enum `PhysicsLayer` collides with the trait re-exported from the prelude. The workspace uses `GameLayer` for that reason.

7. **`Material2d` lives in `bevy::sprite_render`,** not `bevy::sprite`. `Mesh2d` and `MeshMaterial2d` too.

8. **`ShaderRef` is in `bevy::shader`,** not `bevy::render::render_resource`.

9. **`embedded_asset!(app, "foo.wgsl")` registers at `embedded://<crate>/foo.wgsl`.** For files in subdirectories like `postfx/lut_post.wgsl`, the path is `embedded://<crate>/postfx/lut_post.wgsl`.

10. **WGSL partial derivatives are `dpdx`/`dpdy`,** not `dFdx`/`dFdy` (which are HLSL/GLSL names).

11. **`bevy_ecs_tilemap` ships a `MaterialTilemap` trait + `MaterialTilemapBundle<M>`.** Custom tile materials work; we use it for `LitTilemapMaterial`. The default vertex shader doesn't pass `world_position` — we have a `lit_tile_vertex.wgsl` that adds it.

12. **`bevy_egui` v0.39 needs `EguiPrimaryContextPass` schedule** for HUD systems, not `Update`. The multi-pass egui model changed.

13. **`bevy_tweening 0.15` renamed `Animator` to `TweenAnim`,** changed `Lens::lerp` signature to take `Mut<T>` instead of `&mut dyn Targetable<T>`. `EaseFunction` is at `bevy::math::curve::EaseFunction`.

14. **Windows PE 2 GB limit.** Without `split-debuginfo = "packed"` and `strip = "debuginfo"` in `[profile.dev]`, the binary balloons past 2 GB and Windows refuses to launch it.

---

## 8. Where things live

### Iso math and constants
`crates/inf2d_core/src/iso.rs` — `TILE_WIDTH`, `TILE_HEIGHT`, `HEIGHT_STEP_PX`, projection functions.

### Worldgen tuning
`crates/inf2d_worldgen/src/params.rs` — `BiomeParams` has every knob: noise frequencies/octaves, biome thresholds, height range, seed. Live-editable via F3 inspector.

### Chunk streaming
`crates/inf2d_world/src/streaming.rs` — async generation, load/unload rings. Tunables in `StreamingConfig` (`load_radius: 2, hlod_radius: 5, unload_radius: 7` currently).

### Tile rendering
`crates/inf2d_render/src/tilemap.rs` — spawns one `MaterialTilemapBundle<LitTilemapMaterial>` per chunk per height level (so a hill chunk spawns several stacked tilemaps).

### Tile shading
`crates/inf2d_render/src/lit_tile.wgsl` — diffuse + normal sample, Lambertian sun + falloff point lights. `crates/inf2d_render/src/lit_tile_material.rs` drives the uniforms from `TimeOfDay` + `PointLight2D` queries.

### Cliffs
`crates/inf2d_render/src/cliffs.rs` — **one merged mesh per chunk**, four iso-aligned parallelograms per tile-drop, skipped against water/stair neighbors. Custom `ChunkCliffMaterial` with per-vertex color attribute.

### Water shader
`crates/inf2d_render/src/water.rs` + `water.wgsl` — **one quad per chunk** with an R8 mask texture. Two-frequency fbm shimmer, sun spec + moon spec, foam ring removed (was a chunk-seam artifact).

### Post-FX
- `crates/inf2d_render/src/postfx/lut.rs` + `lut_post.rs` + `lut_post.wgsl` — real read-from-scene 3D LUT via render-graph node.
- `crates/inf2d_render/src/postfx/godrays.rs` / `heat.rs` / `vignette.rs` — overlay Material2d quads.

### Lights
`crates/inf2d_render/src/lights.rs` — `PointLight2D` component, additive radial sprite with custom Material2d.

### Player
`crates/inf2d_gameplay/src/lib.rs::spawn_player` — synchronously generates chunk (0,0), picks closest non-solid tile to center, spawns player with `SpriteStack` + `IsoAnchor` + `DropShadow` + kinematic Avian body.

### Path solver
`crates/inf2d_pathfinding/src/lib.rs` — 8-connected A* with octile heuristic, height-aware edge cost, deterministic tie-breaking. `PathRequest` → `PathFound` message round-trip.

### Mobs
`crates/inf2d_gameplay/src/mobs.rs` — Slime archetype, Poisson-disk per-chunk spawn, wander AI, click-to-damage.

### Stats
`crates/inf2d_gameplay/src/stats.rs` — `Health` component, `DamageEvent` + `DeathEvent` messages, `apply_damage` + `despawn_dead` systems.

### UI
- `crates/inf2d_ui/src/hud.rs` — top-left HUD + top-right minimap.
- `crates/inf2d_ui/src/loading.rs` — loading screen between AppState transitions.
- `crates/inf2d_ui/src/pause_menu.rs` — Esc modal + settings sliders.

---

## 9. How to add common things

### A new tile kind

1. Add variant to `inf2d_world::TileKind` (auto-derives `atlas_index()`).
2. Update `TileKind::ALL` + `is_solid()` if applicable.
3. Append a color to `inf2d_render::atlas::BASE_COLOR`.
4. Increment `ATLAS_SLOTS` if needed; teach `paint_tile` how to paint the new biome (variation pass).
5. In `inf2d_worldgen::biome::classify_biome`, decide where in the Whittaker grid the new kind belongs.
6. Optional: update cliff-skip rules in `inf2d_render::cliffs.rs` and pathfinding walkability if it's a special-case tile (like stairs).

### A new entity archetype (mob, prop, item)

1. Define a marker component + any stats in a new module under `crates/inf2d_gameplay/src/`.
2. Spawn via `MessageReader<ChunkLoaded>` (deterministic per-chunk RNG) or one-shot via a system.
3. Attach `SpriteStack` (from `inf2d_render::SpriteStack`) + `IsoAnchor::default()` + `DropShadow` for the iso voxel look.
4. If logical-vs-visual position matters (likely yes for anything that walks), store the logical `WorldTile` separately and write `Transform` derived from `tile_to_world_with_height`.

### A new key bind

1. Add a field to `inf2d_input::InputState` (e.g. `pub jump: bool`).
2. In `inf2d_input::lib.rs::read_inputs`, populate it from `Res<ButtonInput<KeyCode>>` etc.
3. Consume via `Res<InputState>` in whatever system needs it. Never reach into `ButtonInput` directly outside `inf2d_input`.

### A new shader / Material2d

1. Add the WGSL file under the consumer crate's `src/`. Use `embedded_asset!` in the plugin's `build`.
2. Define a struct with `#[derive(AsBindGroup, Asset, TypePath, Clone)]` + `#[uniform(0)]` for the UBO + `#[texture(N)] #[sampler(N+1)]` for textures.
3. Impl `Material2d` (path: `bevy::sprite_render::Material2d`). `fragment_shader()` returns `"embedded://<crate>/your.wgsl".into()`.
4. `app.add_plugins(Material2dPlugin::<YourMaterial>::default())`.
5. Spawn `(Mesh2d(handle), MeshMaterial2d(material_handle), Transform, Visibility)`.

For real post-process (sampling the rendered scene), don't use Material2d — use a render-graph `ViewNode`. See `crates/inf2d_render/src/postfx/lut_post.rs` as the template.

### A new chunk-level system

Listen for `MessageReader<ChunkLoaded>` (and optionally `ChunkUnloaded`). Look up `ChunkData` via the chunk's entity. Spawn whatever as a CHILD of the chunk entity so it auto-despawns when the chunk unloads (Bevy's `ChildOf` cascade).

---

## 10. Wave history (what landed when)

| Wave | Headline |
|---|---|
| Slice 1 | Workspace + iso math + chunked streaming + bevy_ecs_tilemap + camera pan/zoom + HUD + procedural tile atlas |
| Slice 2 | Water shader (frame-cycling), day/night overlay, additive point lights, post-FX stack (god rays, heat, vignette, LUT wash), HDR + bloom, Gerstner WGSL water |
| Slice 3 | Player, A* pathfinding, click-to-move, follow camera, drop shadows, IsoAnchor, bevy_picking, audio crate, save crate |
| Slice 4 | Tile elevation (height field, terraced rendering, height-aware A*), async chunk generation, HLOD imposters, greedy collider meshing |
| Wave 7 | Hover highlight, smoother walking, footstep dust, click ripple, cliff parallelograms |
| Wave 8 | Height-aware iso raycast, cliffs on all 4 iso sides, walking arrival bug fix |
| Wave 9 | Real read-from-scene LUT post-process, pathfinding replan on chunk events, player physics collider, cross-chunk cliff continuity, generic `SpriteStack` component |
| Wave 10 | Cliffs as mesh parallelograms (proper block silhouette), hover animation visibility fix, red rejection puff for unreachable clicks |
| Wave 11 | Tree props with Poisson scatter, moonlight water shimmer, **chunk render consolidation** (cliffs/water merged to one entity per chunk), bevy_tweening, **load radius tightened** to 2/5/7 |
| Wave 12 | Minimap widget, camera shake driver, loading screen |
| Wave 13 | Stairs/ramps tile kind, Slime mob + Health/Damage stats, pause menu + settings |

---

## 11. What's still NOT done

### Engine-side
- **Asset pipeline.** There are zero binary art assets. Everything is procedural diamonds + sprite stacks. Replacing with authored sprite art is straightforward — drop PNGs in `assets/`, swap the atlas builder.
- **Asset hot reload** — dev-only nicety.
- **Determinism in `FixedUpdate`** — physics + AI could run on fixed timestep for replay/netcode safety. Not done.
- **Real audio assets** — `inf2d_audio` is plumbed end-to-end but no `.ogg`/`.wav` files ship. Add files in `assets/audio/` and send `PlaySfx` messages.
- **Save UI** — `inf2d_save` works; press-button-to-save UI not built. `SaveRequest` / `LoadRequest` messages exist.

### Gameplay
- More mob archetypes (only Slime).
- Combat depth — currently click-to-deal-fixed-damage. No attack ranges, cooldowns, abilities.
- Inventory + items (component stub doesn't exist).
- Quest/dialog systems.
- Multiple zones / dungeons.
- Win condition / progression loop.

### Polish
- Real tile sprite art (procedural diamonds throughout).
- Trees/mobs use `SpriteStack` placeholder palettes — would benefit from per-archetype tuning.
- No music tracks shipped.
- No tutorial.

---

## 12. Known gotchas / open bugs

- **Cliff sprites at chunk seams** can flicker briefly during async chunk generation because cliffs are rebuilt when a new neighbor loads. Visible only at fast pan speeds.
- **`Couldn't get monitor selected with: Index(1)` warning** on startup if you don't have a second monitor — winit silently falls back to primary. Harmless.
- **9 inspector-egui warnings on startup** for 3D-pipeline types (`StandardMaterial`, `PointLight`, `DirectionalLight`, etc.) we don't use. Cosmetic.
- **Player collider doesn't push other physics bodies** because we use `Kinematic` with Transform-driven motion. Future work: dynamic mobs that respect the player's collider.
- **Replan-on-chunk-load** doesn't dedupe within a frame — if 5 chunks load simultaneously and the path crosses all 5, we issue 5 PathRequests. Wasteful but correct.

---

## 13. Useful one-liners

```powershell
# Build + run, dev mode
cargo run -p inf2d_app

# Release build (slow compile, smooth runtime)
cargo run -p inf2d_app --release

# Lint
cargo clippy --workspace

# Tests
cargo test --workspace

# Full check (faster than build)
cargo check --workspace

# Watch a build
cargo build -p inf2d_app --message-format=short 2>&1 | Select-String -Pattern 'error|warning:|Finished'

# Inspect crate tree
cargo tree -p inf2d_app
```

---

## 14. Top-level `Cargo.toml` features list (workspace dependency)

```toml
bevy = { version = "0.18", default-features = false, features = [
    "bevy_asset", "bevy_audio", "bevy_camera", "bevy_color",
    "bevy_core_pipeline", "bevy_gizmos", "bevy_post_process",
    "bevy_image", "bevy_input_focus", "bevy_log", "bevy_mesh",
    "bevy_picking", "bevy_render", "bevy_shader", "bevy_sprite",
    "bevy_sprite_render", "bevy_state", "bevy_text", "bevy_ui",
    "bevy_ui_render", "bevy_window", "bevy_winit", "default_font",
    "multi_threaded", "png", "tonemapping_luts", "vorbis", "x11",
] }
```

Don't drop features unless you know what depends on them. Adding `bevy_pbr` would balloon compile time for zero benefit (we're 2D).

---

## 15. The deferred non-goals

These were discussed and explicitly **deferred** or **rejected**:

- **`bevy_seedling` audio backend.** `bevy_audio` works fine until we have assets.
- **`big_brain` / `bevy_behave` AI frameworks.** Premature with 1 mob archetype. Add when 5+ mobs need decision trees.
- **`bevy_water` 3D ocean plane.** Doesn't fit 2D iso — it's a Gerstner-wave 3D mesh. Our 2D shader is the right answer.
- **bevy_kira_audio.** Same reasoning as bevy_seedling — switching backends without assets is rearranging.
- **Per-chunk lit material handles for HLOD alpha cross-fade.** Option C in the consolidation agent's brief — fade ONLY the imposter, leave the tilemap fully visible. Real cross-fade needs per-chunk materials which is a lot of plumbing for marginal visual gain.

---

## 16. Bevy version churn warning

Bevy ships breaking changes every release. Pinning `bevy = "=0.18"` exactly is intentional. Upgrading to 0.19+ will require:
- Update every `Message` reference (Bevy keeps renaming the event/message split).
- Verify `bevy_ecs_tilemap`, `avian2d`, `bevy_egui`, `bevy-inspector-egui`, `bevy_tweening` all have 0.19-compatible releases.
- Re-read `bevy_post_process` API — it's young and the API hasn't stabilized.

Don't upgrade Bevy speculatively. Wait for ecosystem to catch up. Last verified: Bevy 0.18.1 + bevy_ecs_tilemap 0.18.1 + avian2d 0.6.1 + bevy_egui 0.39.1 + bevy_tweening 0.15.0 all work together as of June 2026.

---

## 17. For the next agent: don't do these things

1. **Don't bypass `inf2d_input::InputState`.** Every key/mouse read should go through it.
2. **Don't reverse-project Transform → WorldTile** for gameplay logic. Store the logical tile separately (the `Player` component does this with `current_tile` / `current_height`).
3. **Don't spawn per-tile entities** if you can avoid it. We just consolidated cliffs/water from per-tile to per-chunk; don't undo that.
4. **Don't add features to the engine without using them.** Empty `Inventory` modules with no items are the same kind of speculative scaffolding the chunk consolidation refactor was undoing.
5. **Don't write `unwrap()` outside `#[cfg(test)]`.** Use `let Ok(...) = ... else { return; };` or `let-else`.
6. **Don't widen `inf2d_render`'s dependency surface.** It already depends on world + camera + core. Adding gameplay would cycle.
7. **Don't add docs/comments that don't carry information** — Rust convention here is `///` only on `pub` items, `//` for the "why" of internal blocks.

---

End of guide. The codebase is roughly 18k LOC across 13 crates. Welcome aboard.
