# inf2d_core

Foundational types shared by every other crate in the workspace:

- `WorldTile`, `ChunkPos`, `LocalTilePos` — coordinate primitives.
- `tile_to_world` / `world_to_tile` — 2:1 dimetric isometric projection.
- `CorePlugin` — registers reflection, states, and global system-set ordering.
- `splitmix64` / `chunk_rng` — deterministic per-chunk RNG seeding.
- `AppState`, `GameState` — top-level state machines.

This crate has zero gameplay logic. It is the dependency arrow's root.
