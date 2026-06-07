//! 3-slot save/load. A [`SaveGame`] snapshot is serialized to `saves/slotN.ron`.
//!
//! Plain arrays (`[i32;3]`/`[f32;3]`) are used instead of `IVec3`/`Vec3` so the
//! file format never depends on bevy_math's optional `serde` feature. Failures are
//! reported via `warn!` and surfaced as `None`/`false` — saving/loading never
//! panics or takes the game down.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use inf3d_core::{EditMode, DEFAULT_BUILD_MATERIAL};
use inf3d_worldgen::{VoxelEdit, WorldKind};
use serde::{Deserialize, Serialize};

/// Number of save slots the Load/Save menus show.
pub const SLOT_COUNT: u8 = 3;

/// Current on-disk save-format version.
///
/// Version 1 introduced the `Sand`/`Snow` terrain materials at the front of the
/// player-build range, which shifted every `Built*` material index UP BY 2 (old
/// `BuiltStone=4` → `6`, … `BuiltNeonYellow=11` → `13`). Saves written before
/// versioning existed have no `version` field and deserialize as `0` (see
/// [`SaveGame::version`]); [`load_from_slot`] runs [`migrate`] to remap their raw
/// `u8` material indices into the new palette and stamp them as version 1.
///
/// Version 2 reworked the camera from an orthographic-iso rig to a perspective
/// third-person **orbit**: the old `camera_zoom` field (orthographic view height)
/// was replaced by `camera_distance` (the orbit boom distance) and a new
/// `camera_pitch` was added. Both new fields carry `#[serde(default)]`, so a v1
/// save loads gracefully — its now-unknown `camera_zoom` is simply ignored and the
/// camera fields fall back to sensible defaults (no index remap needed, so
/// [`migrate`] only stamps the version).
pub const CURRENT_SAVE_VERSION: u32 = 2;

/// A persisted game: which world kind (flat test world vs procedural), every voxel
/// edit, the player's position/facing, the edit mode, and the camera view.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveGame {
    /// On-disk save-format version. Saves predating versioning lack this field and
    /// deserialize as `0`; [`load_from_slot`] passes every loaded save through
    /// [`migrate`], which upgrades a version-`0` save's raw material indices to the
    /// current palette and stamps it as [`CURRENT_SAVE_VERSION`]. New saves are
    /// always written at [`CURRENT_SAVE_VERSION`] (see `inf3d_menu`'s save path).
    #[serde(default)]
    pub version: u32,
    /// Selected base world backend. Old saves did not have this field; load code
    /// normalizes them from `flat` below.
    #[serde(default)]
    pub world_kind: WorldKind,
    /// `true` = the flat test world; `false` = procedural terrain.
    pub flat: bool,
    /// Every player voxel edit as `([x,y,z], edit)`.
    pub edits: Vec<([i32; 3], VoxelEdit)>,
    /// Player entity (capsule-center) world position.
    pub player_pos: [f32; 3],
    /// Player facing yaw (radians).
    pub facing: f32,
    /// Walk/Build mode at save time.
    pub edit_mode: EditMode,
    /// The picker's selected build material (raw material index). Saves written
    /// before the picker existed lack this field; they fall back to the default
    /// buildable via [`default_selected_material`].
    #[serde(default = "default_selected_material")]
    pub selected_material: u8,
    /// Camera orbit yaw (radians), restored on load.
    pub camera_yaw: f32,
    /// Camera orbit pitch (radians) + boom distance (world units), restored on
    /// load. Both added in save version 2 (the orbit-camera rework); saves predating
    /// it lack these fields and fall back to [`default_camera_pitch`] /
    /// [`default_camera_distance`]. The old `camera_zoom` field is no longer read
    /// (serde simply ignores it as an unknown field on a v1 save).
    #[serde(default = "default_camera_pitch")]
    pub camera_pitch: f32,
    #[serde(default = "default_camera_distance")]
    pub camera_distance: f32,
    /// Seconds-since-epoch the save was written, for the slot list.
    pub saved_at: u64,
}

/// Serde fallback for [`SaveGame::selected_material`] on saves predating the
/// material picker — the default buildable (`BuiltStone`).
fn default_selected_material() -> u8 {
    DEFAULT_BUILD_MATERIAL
}

/// Serde fallback for [`SaveGame::camera_pitch`] on pre-v2 saves — the orbit rig's
/// resting 3/4 down-angle (matches `inf3d_camera::PITCH_DEFAULT`).
fn default_camera_pitch() -> f32 {
    0.5
}

/// Serde fallback for [`SaveGame::camera_distance`] on pre-v2 saves — the orbit
/// rig's default boom distance (matches `inf3d_camera::DISTANCE_DEFAULT`).
fn default_camera_distance() -> f32 {
    12.0
}

/// Upgrade a freshly-parsed [`SaveGame`] toward [`CURRENT_SAVE_VERSION`].
///
/// Version 1 inserted the `Sand`/`Snow` terrain materials ahead of the player-build
/// range, shifting every `Built*` index UP BY 2. A version-`0` save still stores the
/// pre-shift indices, so we remap them: the old player-build range was `4..=11`, and
/// `+2` maps it onto the new `6..=13`. We touch only `Placed(m)` edits with `m >= 4`
/// (the old buildable floor, `BuiltStone`); `Removed` edits carry no material and are
/// left alone, and indices that were already out of the buildable range (`< 4`) stay
/// untouched so they keep falling back to the exact rendering they had before. The
/// picker's `selected_material` is shifted by the same rule.
///
/// Version 2 (the orbit-camera rework) needs **no index remap** — it only swapped
/// `camera_zoom` for `camera_distance` + `camera_pitch`, which serde handles via
/// the field `#[serde(default)]`s when a v1 file is parsed. So a save already at
/// version `>= 1` is returned unchanged (no double-migrate); only the version-`0`
/// material remap runs here.
fn migrate(mut game: SaveGame) -> SaveGame {
    if game.version >= 1 {
        return game;
    }
    for (_, edit) in game.edits.iter_mut() {
        if let VoxelEdit::Placed(m) = edit {
            if *m >= 4 {
                *m += 2;
            }
        }
    }
    if game.selected_material >= 4 {
        game.selected_material += 2;
    }
    game.version = CURRENT_SAVE_VERSION;
    game
}

/// The `saves/` directory (relative to the working dir, like the existing
/// `inf3d-monitor.log` / `save.ron`); git-ignored.
fn saves_dir() -> PathBuf {
    PathBuf::from("saves")
}

fn slot_path(slot: u8) -> PathBuf {
    saves_dir().join(format!("slot{}.ron", slot + 1))
}

/// Current wall-clock seconds since the Unix epoch (0 if the clock is before it).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write `game` to `slot` (0-based). Returns whether it persisted; a failure is a
/// `warn!`, not a panic.
pub fn save_to_slot(slot: u8, game: &SaveGame) -> bool {
    if let Err(err) = std::fs::create_dir_all(saves_dir()) {
        warn!("inf3d_menu: could not create saves/ ({err})");
        return false;
    }
    let pretty = ron::ser::PrettyConfig::default();
    match ron::ser::to_string_pretty(game, pretty) {
        Ok(text) => match std::fs::write(slot_path(slot), text) {
            Ok(()) => {
                info!("inf3d_menu: saved game to slot {}", slot + 1);
                true
            }
            Err(err) => {
                warn!("inf3d_menu: could not write slot {} ({err})", slot + 1);
                false
            }
        },
        Err(err) => {
            warn!("inf3d_menu: could not serialize save ({err})");
            false
        }
    }
}

/// Read `slot` (0-based), or `None` when the slot is empty / unreadable / corrupt.
pub fn load_from_slot(slot: u8) -> Option<SaveGame> {
    let text = std::fs::read_to_string(slot_path(slot)).ok()?;
    match ron::from_str::<SaveGame>(&text) {
        // Normalize every loaded save to the current version so all callers (load
        // into game + slot summaries) see current-palette material indices.
        Ok(game) => Some(migrate(game)),
        Err(err) => {
            warn!("inf3d_menu: slot {} is corrupt ({err})", slot + 1);
            None
        }
    }
}

/// A one-line summary for the slot list: `Some(edit-count, age-seconds)` when the
/// slot holds a save, `None` when empty.
pub fn slot_summary(slot: u8) -> Option<(usize, u64)> {
    let game = load_from_slot(slot)?;
    let age = now_secs().saturating_sub(game.saved_at);
    Some((game.edits.len(), age))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal [`SaveGame`] for migration tests — only the fields the
    /// migration reads (`version`, `edits`, `selected_material`) are meaningful.
    fn sample(version: u32, edits: Vec<([i32; 3], VoxelEdit)>, selected_material: u8) -> SaveGame {
        SaveGame {
            version,
            world_kind: WorldKind::default(),
            flat: false,
            edits,
            player_pos: [0.0, 0.0, 0.0],
            facing: 0.0,
            edit_mode: EditMode::default(),
            selected_material,
            camera_yaw: 0.0,
            camera_pitch: 0.5,
            camera_distance: 12.0,
            saved_at: 0,
        }
    }

    #[test]
    fn migrate_v0_shifts_built_materials_up_by_two() {
        let game = sample(
            0,
            vec![
                ([0, 0, 0], VoxelEdit::Placed(4)),  // old BuiltStone floor → 6
                ([1, 2, 3], VoxelEdit::Placed(11)), // old top of build range → 13
                ([4, 5, 6], VoxelEdit::Removed),    // material-less, untouched
            ],
            4,
        );
        let out = migrate(game);
        assert_eq!(out.version, CURRENT_SAVE_VERSION);
        assert_eq!(
            out.edits,
            vec![
                ([0, 0, 0], VoxelEdit::Placed(6)),
                ([1, 2, 3], VoxelEdit::Placed(13)),
                ([4, 5, 6], VoxelEdit::Removed),
            ]
        );
        assert_eq!(out.selected_material, 6);
    }

    #[test]
    fn migrate_v1_passes_through_unchanged() {
        let game = sample(
            1,
            vec![([0, 0, 0], VoxelEdit::Placed(6)), ([1, 1, 1], VoxelEdit::Removed)],
            6,
        );
        let out = migrate(game.clone());
        assert_eq!(out.version, 1);
        assert_eq!(out.edits, game.edits);
        assert_eq!(out.selected_material, game.selected_material);
    }
}
