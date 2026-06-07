# inf3d — More Biomes & More Props: Plan + Seams

Prep doc for the biomes/props task. **No behavior change is described here as
"done"** — this is the design + the exact extension points. Read alongside `CLAUDE.md`
(architecture) and `BACKLOG.md`.

> The biome/prop seams below live in `worldgen`, `world`, and
> `foliage/{mod,scatter,spawn,vox_mesh}.rs`. (The camera is a **perspective orbit**
> rig — the foliage `compute_ring` math in `foliage/stream.rs` already reflects that;
> see [§6](#6-the-perspective-foliage-ring).)

---

## 1. TL;DR — most of the system already exists

This is the single most important thing to internalize before starting: **the biome and
per-biome foliage pipeline is already built, tested, and wired end-to-end.** "More
biomes, more props" is an **extension along existing seams**, not a greenfield build.
Rebuilding any of the below would be a regression.

What already exists today:

- **Biome classification** (`inf3d_worldgen`): a `Biome` enum (`Plains, Forest, Desert,
  Snow, Beach`), a Whittaker temperature×moisture split (`classify_biome`) with a coastal
  `Beach` height override, two dedicated low-frequency Perlin fields
  (`build_temperature_noise` / `build_moisture_noise`), and an LOD-independent oracle
  (`Terrain::biome_at`). **Appearance-only by contract** (never changes `surface_y` /
  `is_water` / geometry), guarded by the `biome_does_not_change_surface_or_water` test.
- **Biome→terrain-material mapping** (`inf3d_world`): `biome_surface_material(Biome) ->
  TerrainMaterialId`, with `Sand` (desert/beach) and `Snow` materials already in the
  `PALETTE` + procedural texture array. The mesher (`get_voxel_fn`) classifies each land
  column's biome (cached per column) and textures it accordingly; the flat test world
  stays uniform grass on purpose.
- **Per-biome foliage policy** (`inf3d_render::foliage`): a `BIOME_POLICIES` table
  (one row per biome) with per-category density multipliers (`tree_mul`/`rock_mul`/
  `grass_mul`), a **tree-variant name-substring filter** (`tree_names`), and a material
  **tint** per biome. Per-biome tinted materials are prebuilt
  (`FoliageAssets::materials[biome as usize]`), and the async scatter worker reads the
  policy to choose what to place.

So the "frozen `Biome` enum" already has 5 members and a lot of lockstep machinery keyed
off `biome as usize`. Extending it is mechanical but touches several files **in lockstep**
(see §3).

---

## 2. Data flow (one classify → three consumers, never desyncing)

```
                         inf3d_worldgen
            temperature/moisture Perlin (LOD-independent)
                              │
                       classify_biome(temp, moist, stand_y, is_water)  ← single source of truth
                              │
        ┌─────────────────────┼──────────────────────────┐
        ▼                     ▼                          ▼
 Terrain::biome_at     get_voxel_fn (mesher,        foliage scatter worker
 (gameplay oracle,     worker threads)              (inf3d_render, off-thread)
  LOD-0 authority)     biome_surface_material →     biome_policy(biome) →
        │              TerrainMaterialId →          density muls + tree_names
        │              PALETTE texture layers       + tint material
        ▼                     ▼                          ▼
 (future gameplay      land voxel top/side          which trees/rocks/grass
  uses: ambience,      texture = grass/sand/snow    spawn on each column, tinted
  spawns, etc.)
```

The invariant that makes this safe: **all three consumers call the *same*
`classify_biome` on the *same* low-frequency, LOD-independent noise**, so a column's
texture, its foliage, and the oracle's biome answer can never disagree — even when a far
chunk meshes at a reduced LOD (the height field drops octaves; the biome fields never do).
Preserve this. Any new biome axis (e.g. elevation) must be fed identically into all three.

---

## 3. Seam A — add a BIOME (the lockstep checklist)

Adding one biome is a coordinated edit across **3 crates**. Append the new variant at the
end of `Biome` (discriminants are load-bearing array indices — never reorder/insert).

1. **`crates/inf3d_worldgen/src/lib.rs`**
   - Add the `Biome::X = N` variant (append; keep `#[repr(u8)]`).
   - Make `classify_biome` return it. Either retune the existing temp/moist thresholds
     (`COLD/HOT/DRY/WET_THRESHOLD`) or add a **third axis** (recommended for richer
     worlds — e.g. an elevation/“weirdness” Perlin) sampled the same LOD-independent way.
     Keep the `Beach` height override first; keep the function **total** (water returns a
     neutral default).
   - If adding an axis: add `build_*_noise` + `sample_*` helpers with a fresh distinct
     seed (current seeds: height `1234`, temp `70_001`, moist `70_002`), and thread it
     through `Terrain` (new field + constructors) AND `get_voxel_fn` (build once per job).
   - Update tests: `classify_biome_rules`, `biome_at_is_deterministic_and_varies`
     (asserts ≥3 biomes appear over a wide area — a new biome should still leave the mix
     healthy).

2. **`crates/inf3d_world/src/lib.rs`** (only if the biome needs a new ground texture)
   - Add a `TerrainMaterialId::X` variant **before** the `Built*` range? **No** — append
     terrain materials *before* `BuiltStone` only if you also shift the `Built*` range
     and bump the save migration (that already happened once: see `save.rs` v0→v1
     `+2` shift). **Cheaper: reuse** `Grass`/`Sand`/`Snow` where possible. If a genuinely
     new surface is needed (e.g. `Mud`, `Mesa`, `Ash`), insert it in the terrain block
     (after `Snow`, before `BuiltStone`), add its `PALETTE` row (in discriminant order),
     add its procedural texture color in `terrain_material::build_terrain_texture`, bump
     `save.rs` `CURRENT_SAVE_VERSION` + `migrate` (shift `Built*` indices by the count you
     inserted), and update the `palette_matches_enum` test's `all` list.
   - Map the biome in `biome_surface_material` (+ the `biome_surface_material_maps_each_biome`
     test). This is `match`-exhaustive over `Biome`, so the compiler *forces* you to handle
     the new biome here — a good guardrail.

3. **`crates/inf3d_render/src/foliage/mod.rs`**
   - Bump `BIOME_COUNT` (length of `BIOME_POLICIES` *and* `FoliageAssets::materials`).
   - Add the `BIOME_POLICIES` row (density muls, `tree_names` substrings, `tint`). The
     `biome_policy_table_is_indexable_for_every_biome` test + the `[_; BIOME_COUNT]`
     arrays make a missing row a compile/early-startup failure.
   - Add the new `Biome::X` to the test match lists (`biome_policy_table_…`, etc.).

4. **`crates/inf3d_render/src/foliage/scatter.rs`** — usually **no change** (it reads the
   policy table generically). Only touch if the biome needs a new *category* (§4) or a
   different base density.

**Biome invariants to uphold** (all currently tested — keep them green):
- Appearance-only: never let a biome perturb `surface_y`/`is_water`/solidity.
- LOD-independence + determinism: low-freq noise, built once per job, seed from `(x,z)` /
  tile coord only.
- `biome as usize` is a valid index into every `[_; BIOME_COUNT]` array.

---

## 4. Seam B — add PROPS

Two granularities:

### 4a. Add a variant to an existing category (cheapest — often zero code)
Drop a MagicaVoxel `.vox` into `assets/foliage/vox/{Trees,Rocks,Grass}/`. It's auto-loaded
at startup (`setup_foliage` → `vox_mesh::load_category`). For **trees**, the file **stem
name** decides which biomes use it via the `tree_names` substring filter in
`BIOME_POLICIES` (e.g. name it `pine_*` → Snow/Forest; `cactus*`/`*stump` → Desert;
`palm*` → Beach; `tree_*` → Plains/Forest). Rocks/grass are biome-gated only by the
density multipliers, not by name. Sizing is automatic (each variant is uniform-scaled so
its tallest axis hits `TREE/ROCK/GRASS_TARGET_HEIGHT`). The **low-prop step gate**
(`is_low_prop`, `LOW_PROP_MAX_HEIGHT = 1.1`) auto-classifies a short prop as a walkable
1-voxel step (no collider, `PropSurfaces` claim) vs. a tall obstacle (`SolidPropCollider`
+ inflated `BlockedCells`) — so a new short rock "just works" for the physics controller
(walkable step vs. wall).

> Base densities live in `scatter.rs`: `TREE_DENSITY = 0.004`, `ROCK_DENSITY = 0.002`,
> `GRASS_DENSITY = 0.018` (per-column probability), scaled by the per-biome multiplier and
> rejected on overlap via `PROP_SPACING = 1.3`. Tune per-biome via `*_mul`, globally here.

### 4b. Add a whole new category (e.g. Bushes, Flowers, Mushrooms, Crystals, Reeds)
This is the larger extension. Touch points, all in `inf3d_render::foliage`:
- `mod.rs`: new dir const (`BUSHES_DIR`), a `FoliageAssets::bushes: Vec<FoliageVariant>`
  field (loaded in `setup_foliage`), a `ScatterCategory::Bush` variant, a
  `*_TARGET_HEIGHT`, and a per-biome density knob in `BiomePolicy` (e.g. `bush_mul`).
- `scatter.rs`: emit `ScatterItem`s for the new category in `scatter_solid` (tall/solid)
  or `scatter_grass` (decorative, no collider) — pick the layer by whether it should
  block/step or be walk-through like grass. Respect the per-tile RNG stream discipline
  (don't perturb existing categories' placement — advance a separate stream or append).
- `spawn.rs`: replay the new `ScatterCategory` into entities (mesh + the biome-tinted
  material; attach `SolidPropCollider` only if tall). Decide its layer membership.
- Consider whether it needs a gameplay marker like `Tree`/`Rock` (in `inf3d_core`) if it's
  harvestable later.

**Prop/foliage invariants to uphold** (tested):
- Determinism: `tile_seed(tile)` is shared by both layers; placement must be bit-identical
  run-to-run (`*_scatter_is_deterministic` tests).
- Layer discipline: solid (trees/rocks, zoom/“view”-driven ring) and grass
  (player-centered, view-independent disc) never touch each other's entities.
- Refcounted cell claims: every `BlockedCells`/`PropSurfaces` claim must be released
  exactly as many times as taken on tile despawn (cross-tile overlap correctness).

---

## 5. Proposed roadmap (suggested scope for the next task)

A concrete, incremental target — adjust to taste:

1. **Richen existing biomes first (no new enum):** add 2–4 `.vox` variants per category
   (more tree silhouettes, boulder shapes, grass tufts) and retune `BIOME_POLICIES`
   densities. Pure asset + table work, highest visual payoff per effort. (Seam 4a.)
2. **New decorative category — `Flowers`/`Bushes`** (Seam 4b), grass-layer (no collider),
   `grass_mul`-gated so dry biomes stay bare. Adds ground-cover variety cheaply.
3. **New biomes** (Seam A), in rough order of asset reuse:
   - `Savanna` (warm, semi-dry: sparse acacia-style `tree_` + tall grass; reuses Sand/Grass).
   - `Swamp` (wet+temperate lowland: dense short trees, reeds, no/!grass; maybe a `Mud`
     material → triggers the §3.2 palette+migration path).
   - `Mesa`/`Badlands` (hot+dry+high: needs an **elevation axis** in `classify_biome` and
     likely a banded `Mesa` material).
   - `Taiga` (cold+wet: dense pines, distinct from `Snow`'s sparse pines).
   Each new biome that needs an elevation/altitude axis should add it once and reuse it for
   all of them (one new Perlin field threaded through the three consumers — §3.1).
4. **Optional polish:** per-biome ambient / horizon clear-color hooks (the render crate
   already centralizes the horizon color — there is no fog; it was removed), and
   biome-aware footstep SFX (the `Footstep` message already carries `pos`).

Sequence the work by **file ownership** if farming to parallel background agents
(per the project's delegation norm): worldgen-only (classification/axis), world-only
(materials/palette/migration), foliage-only (policies/categories/assets). The
`classify_biome` signature is the contract between them — freeze it first if splitting.

---

## 6. The perspective foliage ring

The camera is a **perspective orbit** rig (`OrbitCamera`), so the foliage solid ring is
no longer driven by orthographic zoom — `foliage/stream.rs::compute_ring` returns a fixed,
conservative reach (`RING_PERSPECTIVE`) sized to the roughly-constant perspective horizon
(a harmless `Orthographic` fallback arm is kept only for the zoom-scaling tests). If new
biomes/props change how far foliage should be visible, tune it there. None of the other
biome/prop seams (`worldgen`, `world`, `foliage/{mod,scatter,spawn,vox_mesh}.rs`) depend on
the camera projection, so design/asset work there proceeds independently.

---

## 7. Quick file index (where each seam lives)

| Concern | File | Key symbols |
|---|---|---|
| Biome enum + classification + noise | `crates/inf3d_worldgen/src/lib.rs` | `Biome`, `classify_biome`, `build_temperature_noise`/`build_moisture_noise`, `Terrain::biome_at`, threshold consts |
| Biome → ground material + texture | `crates/inf3d_world/src/lib.rs` | `TerrainMaterialId`, `PALETTE`, `biome_surface_material`, `get_voxel_fn` |
| Material texture colors | `crates/inf3d_world/src/terrain_material.rs` | `build_terrain_texture` (procedural layer colors) |
| Save migration (if palette grows) | `crates/inf3d_menu/src/save.rs` | `CURRENT_SAVE_VERSION`, `migrate` |
| Per-biome foliage policy + assets | `crates/inf3d_render/src/foliage/mod.rs` | `Biome`-indexed `BIOME_POLICIES`, `BIOME_COUNT`, `FoliageAssets`, `setup_foliage`, `is_low_prop` |
| Scatter rules + base densities | `crates/inf3d_render/src/foliage/scatter.rs` | `TREE/ROCK/GRASS_DENSITY`, `PROP_SPACING`, `scatter_solid`/`scatter_grass` |
| Entity spawn + colliders | `crates/inf3d_render/src/foliage/spawn.rs` | `ScatterItem` replay, `SolidPropCollider` |
| `.vox` load + cull mesher | `crates/inf3d_render/src/foliage/vox_mesh.rs` | `load_category` |
| Foliage ring (perspective) | `crates/inf3d_render/src/foliage/stream.rs` | `compute_ring` — fixed perspective reach (§6) |
| Asset files | `crates/inf3d_app/assets/foliage/vox/{Trees,Rocks,Grass}/` | drop new `.vox` here |
