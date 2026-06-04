//! 3-slot save/load. A [`SaveGame`] snapshot is serialized to `saves/slotN.ron`.
//!
//! Plain arrays (`[i32;3]`/`[f32;3]`) are used instead of `IVec3`/`Vec3` so the
//! file format never depends on bevy_math's optional `serde` feature. Failures are
//! reported via `warn!` and surfaced as `None`/`false` — saving/loading never
//! panics or takes the game down.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use inf3d_core::EditMode;
use inf3d_worldgen::VoxelEdit;
use serde::{Deserialize, Serialize};

/// Number of save slots the Load/Save menus show.
pub const SLOT_COUNT: u8 = 3;

/// A persisted game: which world kind (flat test world vs procedural), every voxel
/// edit, the player's position/facing, the edit mode, and the camera view.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveGame {
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
    /// Camera orbit yaw + orthographic zoom, restored on load.
    pub camera_yaw: f32,
    pub camera_zoom: f32,
    /// Seconds-since-epoch the save was written, for the slot list.
    pub saved_at: u64,
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
        Ok(game) => Some(game),
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
