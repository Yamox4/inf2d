Drop your animated grass GLTF here.

Expected file (one of):
  grass.glb     <-- preferred (binary glTF, single file)
  grass.gltf    (+ its .bin and textures alongside)

The grass system loads it from the asset path:
  models/grass/grass.glb#Scene0

If your scene index differs, tell Claude and the loader path will be adjusted.

Notes:
- The asset root for `cargo run -p inf3d_proto` is this crate's `assets/` folder.
- Keep the model low-poly; it will be instanced many times across the terrain.
- Skeletal/animated GLTF is expensive per-instance — the grass system uses a
  capped, distance-LOD'd scatter (top land surfaces only, never on water).
