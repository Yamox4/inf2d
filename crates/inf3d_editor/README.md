# inf3d_editor — Voxel Model Editor (Phase 1: Displayer + Creator)

A **standalone** Bevy + egui tool for building voxel models the game (and the
future Animator) can consume. It is **not** wired into the game — it is its own
binary with its own minimal plugin set. Deleting `crates/inf3d_editor/` and the
single `"crates/inf3d_editor"` line in the workspace `members` list removes it
completely.

## Run

```powershell
cargo run -p inf3d_editor          # binary: inf3d-editor
cargo run -p inf3d_editor --release
cargo test  -p inf3d_editor        # data-model, .vox-writer, and rig round-trip tests
```

## What it does (Phase 1)

- A **reference build volume**: an `N×N×N` grid of *reference blocks*, where each
  block is exactly **one in-game `1×1×1` world voxel**. Start at `1` block (a
  size anchor, outlined in orange) and slide up to `4` to see how a model spans
  multiple in-game voxels. Each block is subdivided into `resolution³` editable
  **sub-voxels** (`8/16/24/32` per edge, MagicaVoxel-grade detail).
- A **visible reference platform**: a solid, clearly-lit slab at the base of the
  build volume (`y = 0`) sized to the current block extent, with a bright
  per-block grid, a fine sub-voxel grid, and a cyan footprint border drawn on its
  surface. It updates live with the Blocks/Res sliders, so you always see exactly
  the ground the first layer lands on — placement is never midair.
- **Painting**: **left-click** places a sub-voxel on the face you point at (or on
  the platform for the first voxel); **Shift+left-click** is a quick erase. A
  color palette seeded from the **in-game block colors** plus custom colors. An
  eyedropper (Pick) tool. A MagicaVoxel/Blender-style orbit camera
  (**right/middle-drag orbit, Shift+right/middle-drag pan, scroll zoom**); the
  left button stays free for painting.
- **Object typing + multi-part rigging** (the groundwork the Animator needs):
  choose **Character** or **Item**. **Both** own a **part hierarchy** — each part
  has a **name**, a **pivot**, and a **parent**. A Character seeds the standard
  humanoid rig (`Torso → Head / arms / legs`); an Item seeds a single `Body`
  part. For either type you can **add, rename, re-parent (children re-parent up
  on delete), re-pivot, and delete** parts — so an item can be split into
  independently-animatable pieces (a torch flame, a hinged lid, a flapping
  banner). The part you select is the one new sub-voxels are tagged with, so
  every voxel knows which part owns it. The set is fully data-driven.
- **Project browser**: list / new / save / load / rename / delete models in the
  editor's asset dir, with per-part voxel counts.

## Controls

| Input | Action |
|---|---|
| Left-click | Apply the active tool (Paint / Erase / Pick) |
| Shift + Left-click | Quick erase the sub-voxel under the cursor (any tool) |
| Right-drag / Middle-drag | Orbit the camera about the model |
| Shift + Right-drag / Middle-drag | Pan the focus (slide the view) |
| Scroll | Zoom the boom in/out |
| Left panel | Tool, palette, and the rig (parts) tree (Character **and** Item) |
| Right panel | The project browser |
| Top bar | Name, object type, New/Save, block count, resolution, view toggles |

The left mouse button is reserved for painting; all camera navigation is on the
right/middle buttons (MagicaVoxel/Blender convention), so the two never conflict.

## Save format (the interchange contract)

Each project is a **pair of files** sharing a stem under
`crates/inf3d_editor/assets/models/`:

### `<name>.vox` — geometry
A standard MagicaVoxel `.vox` file written by this crate's hand-rolled writer
(`src/io/vox_writer.rs`). `dot_vox` 5.x is **load-only** (no serializer), so the
writer emits the format's chunk structure directly:

```
"VOX " <version:i32=150>
MAIN
  SIZE  <x> <y> <z>             (3 × i32, little-endian)
  XYZI  <numVoxels> then (x,y,z,colorIndex) bytes per voxel
  RGBA  256 × (r,g,b,a) bytes
```

- **Axis frame**: the game's loader (`inf3d_render::foliage::vox_mesh`) maps
  `.vox (x,y,z) → Bevy (x, z, -y)` (MagicaVoxel Z-up). The exporter applies the
  inverse swap — editor cell `(x, y, z)` (Y-up) → `.vox (x, z, y)` — so a model
  built upright exports upright through **both** MagicaVoxel and the game.
- **Palette indexing**: `dot_vox` normalizes a file color index `c` to in-memory
  `c-1` and reads `palette[c-1]`. The writer therefore stores editor color slot
  `s` at file palette position `s` and writes each voxel's file color index as
  `s+1`, so colors round-trip through both `dot_vox` and MagicaVoxel.

The geometry is cropped to its tight occupied bounds on export.

### `<name>.rig.ron` — the rig sidecar (project source of truth)
Everything the `.vox` can't carry, as RON (`src/io/rig.rs`, schema
`RIG_VERSION = 1`):

```ron
RigDoc(
    version: 1,
    name: "hero",
    object_type: Character,        // or Item
    resolution: 16,                // sub-voxels per reference-block edge
    blocks: 2,                     // N of the NxNxN build volume
    root_part: 0,
    parts: [
        RigPart( id: 0, name: "Torso", parent: None,    pivot: (0.5, 0.9, 0.5) ),
        RigPart( id: 1, name: "Head",  parent: Some(0), pivot: (0.5, 1.5, 0.5) ),
        // ...
    ],
    palette: [ (110, 111, 114), /* ... sRGB triples, slot order ... */ ],
    voxels: [
        RigVoxel( cell: (5, 20, 5), color: 2, part: 1 ),
        // ...
    ],
)
```

Voxels are stored in **editor cell space** (Y-up, the same space the runtime grid
uses), so the Animator can transform a part's cells directly without re-deriving
the `.vox` axis swap. **Loading** reads the `.rig.ron` (the project artifact);
the `.vox` is the export artifact for MagicaVoxel / the game importer.

## How later phases slot in

- **Phase 2 (Animator)** reads `<name>.rig.ron`: walk `parts` as a tree, rotate
  each part about its `pivot` (inheriting its parent), and move the part's
  `voxels` (by `cell`) as a rigid group. The pivot/parent fields exist precisely
  for this.
- **Phase 3 (game importer)** can load the `.vox` straight through the existing
  `dot_vox` loader for a single-part static prop, or read the `.rig.ron` for an
  articulated model (a character **or** a multi-part item). `object_type` tags the
  intent; the generic `parts` list carries the rig for both.

## Code map

```
src/
  main.rs          App + plugin composition (binary `inf3d-editor`)
  state.rs         EditorState — the single owned resource (active project)
  volume.rs        VoxelModel — the sparse sub-voxel grid (resolution + extent)
  parts.rs         ObjectType + the body-part hierarchy (PartTree / Part)
  palette.rs       editor palette (seeded from inf3d_world block colors)
  camera.rs        MagicaVoxel/Blender-style orbit/pan/zoom camera + lights
  paint.rs         click-to-add/erase 3D-DDA raycasting (runs post-UI, fresh pose)
  render.rs        voxel cull-mesher + reference-platform slab + grid/pivot gizmos
  io/
    mod.rs         save/load orchestration + project listing
    vox_writer.rs  hand-written MagicaVoxel .vox writer
    rig.rs         the .ron rig schema (serde)
  ui/
    mod.rs         egui plugin + the pointer-over-UI gate
    panels.rs      the egui panels
assets/models/     saved projects (.vox + .rig.ron pairs)
```

## Dependencies

- `bevy`, `serde`, `ron` from the workspace (`{ workspace = true }`).
- `bevy_egui = "0.39"` (egui 0.33) — **local to this crate**, intentionally not
  added to `[workspace.dependencies]` so the tool stays self-contained/deletable.
- `inf3d_world` — **read-only**, only for the in-game `TerrainMaterialId` palette
  (`color()` / `label()` / `BUILDABLE`) so editor reference colors match the
  game's block hues.
- `dot_vox = "5"` (dev-dependency) — only in tests, to prove the `.vox` writer
  round-trips through the same loader the game uses.
