# inf3d audio assets

Where every sound in the game lives. Drop `.ogg` files into the folders below
following the naming convention and the engine auto-loads them — you never edit
Rust to add a *sound*, only to add a new *kind* of sound event.

## Format (important)
- **Ogg Vorbis (`.ogg`) only.** The Bevy build enables the `vorbis` feature; `.wav`
  / `.mp3` / `.flac` are NOT compiled in. If you only have `.wav`, either convert
  to `.ogg` (Audacity → Export as OGG) or ask to enable the `wav` Bevy feature.
- **SFX:** mono, ~44.1 kHz, short (footsteps ≈ 0.2–0.5 s). Mono matters — only mono
  clips can be positioned in 3D space (spatial audio) later.
- **Music / ambience:** stereo is fine.
- Normalize levels so clips sit around the same loudness (roughly −14 LUFS / peak
  ≈ −3 dB). The engine applies per-category volume on top, but consistent source
  levels save pain.

## Naming convention
`<thing>_<variant>_NN.ogg` — lowercase, snake_case, `NN` = a 2-digit variant index
starting at `01`. Multiple numbered variants of the same sound are picked at random
each time it plays, so footsteps don't sound like a metronome. One variant is fine
to start (`_01`), add more whenever.

Example: `footstep_grass_01.ogg`, `footstep_grass_02.ogg`, …

## Folder layout & what goes where

```
assets/audio/
├── sfx/
│   ├── footsteps/   footstep_<surface>_NN.ogg   — per surface under the player
│   ├── player/      player_<action>_NN.ogg      — player actions (land, harvest…)
│   ├── ui/          ui_<action>_NN.ogg           — clicks, destination-set ping
│   └── world/       world_<event>_NN.ogg         — water splash, tree chop, etc.
├── ambient/         ambient_<name>_loop.ogg      — looping beds (wind, ocean)
└── music/           music_<name>.ogg             — background tracks
```

## START HERE — footsteps (the first feature being wired)

Footsteps are chosen by the **surface the player is walking on** (the engine
already classifies terrain: grass / dirt / stone / sand-near-water). Add at least
one variant per surface; add `_02`…`_04` later for variety:

| Drop into `sfx/footsteps/` | When it plays |
|---|---|
| `footstep_grass_01.ogg` (…`_02`, `_03`, `_04`) | walking on grassy land (the common case) |
| `footstep_dirt_01.ogg` …  | walking on bare dirt / paths |
| `footstep_stone_01.ogg` … | walking on high/rocky ground |
| `footstep_sand_01.ogg` …  | walking on beaches / near the waterline |

If a surface has no file yet, the engine just stays silent for it — add files
incrementally, nothing breaks.

## Also wired — block edit SFX (placeholders in place)
Build mode plays a sound on every voxel edit, driven by `inf3d_render::BlockEdited`:

| File in `sfx/world/` | When it plays |
|---|---|
| `world_block_place_01.ogg` | a block is placed (left-click in Build) |
| `world_block_break_01.ogg` | a block is broken (right-click in Build) |

These currently ship as **synthesized placeholder** clips (a short "thunk" / "crumble"
made with ffmpeg). Drop better-sounding `.ogg`s at the same paths to replace them, or
add `_02`…`_NN` variants once the loader is extended to pick variants at random (it
loads the single `_01` today, like footsteps).

### Nice-to-have next (optional, same drill)
- `sfx/ui/ui_click_01.ogg` — a UI/button click ping (menu, mode buttons, picker).
- `sfx/world/world_water_lap_01.ogg` — near-shore water lapping.
- `ambient/ambient_wind_loop.ogg` — a quiet looping wind bed.
- `music/music_explore_01.ogg` — exploration track.

## How this is wired (architecture)
The audio *code* lives in a dedicated crate **`inf3d_audio`** — a downstream
**sink**, exactly like `inf3d_monitor` / `inf3d_ui`: it only *reads* game state and
events (player movement / footsteps, block place/break) and *plays* sounds. It
depends on the gameplay/physics/render crates it listens to, and nothing depends on
it (acyclic graph preserved). Its systems run in `GameSet::Fx` (presentation, end of
frame). At startup it scans these folders and loads every matching `.ogg` into an
`AudioAssets` resource, so adding a sound = dropping a file here.
