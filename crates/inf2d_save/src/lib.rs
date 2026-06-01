#![deny(unsafe_code)]
//! Save / load for slice 1+ persistent state.
//!
//! Layout on disk: `%APPDATA%/inf2d/save.ron` (Windows) or
//! `~/.local/share/inf2d/save.ron` (Linux/macOS). A small serde-derived struct
//! holds the world seed, camera pose, and a versioning field so future format
//! changes can migrate cleanly.

use bevy::prelude::*;
use inf2d_camera::CameraRig;
use inf2d_worldgen::BiomeParams;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped whenever the on-disk format changes. Load paths check this and
/// either migrate or refuse to load old data.
pub const SAVE_FORMAT_VERSION: u32 = 1;

/// Top-level on-disk save payload.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SaveFile {
    /// Format version. Bumped whenever the layout changes.
    pub version: u32,
    /// Persistent world parameters (seed today, more later).
    pub world: SaveWorld,
    /// Persistent camera pose.
    pub camera: SaveCamera,
}

/// World-level persistent state.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SaveWorld {
    /// The deterministic world seed used by `inf2d_worldgen::BiomeParams`.
    pub seed: u64,
}

/// Persistent camera pose. Stored as scalars so the file is human-readable.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SaveCamera {
    /// Camera target world-x.
    pub target_x: f32,
    /// Camera target world-y.
    pub target_y: f32,
    /// Orthographic zoom (`projection.scale`).
    pub zoom: f32,
}

/// Errors that can happen while saving or loading.
#[derive(thiserror::Error, Debug)]
pub enum SaveError {
    /// Underlying I/O failure (file not found, permissions, etc).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// RON serialize / deserialize failure.
    #[error("ron parse error: {0}")]
    Ron(String),
    /// The on-disk file declared a version this binary cannot read.
    #[error("unsupported save format version {0}")]
    UnsupportedVersion(u32),
    /// The OS-specific data directory could not be resolved.
    #[error("no save directory could be resolved")]
    NoSaveDir,
}

/// Fire to request a save to disk.
#[derive(Message, Debug, Clone)]
pub struct SaveRequest;

/// Fire to request a load from disk.
#[derive(Message, Debug, Clone)]
pub struct LoadRequest;

/// Emitted after a successful save.
#[derive(Message, Debug, Clone)]
pub struct SaveCompleted;

/// Save plugin: registers the request / completion messages and the
/// `Update`-scheduled handlers that consume them.
pub struct SavePlugin;

impl Plugin for SavePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SaveRequest>()
            .add_message::<LoadRequest>()
            .add_message::<SaveCompleted>()
            .add_systems(Update, (handle_save_requests, handle_load_requests));
    }
}

/// Resolve the canonical save path. Returns [`SaveError::NoSaveDir`] if the
/// OS-specific data directory can't be determined.
pub fn save_path() -> Result<PathBuf, SaveError> {
    directories_helper()
        .map(|base| base.join("save.ron"))
        .ok_or(SaveError::NoSaveDir)
}

/// Internal: best-effort, dependency-free resolution of the per-user data dir.
/// We deliberately avoid pulling in the `directories` crate to keep this slice
/// minimal — APPDATA / HOME cover the two platforms we ship for.
fn directories_helper() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("inf2d"))
    }
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share/inf2d"))
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

/// Serialize `file` to RON and write atomically-ish (create parents first) to
/// `path`.
pub fn save_to_disk(file: &SaveFile, path: &std::path::Path) -> Result<(), SaveError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized = ron::ser::to_string_pretty(file, ron::ser::PrettyConfig::default())
        .map_err(|e| SaveError::Ron(e.to_string()))?;
    std::fs::write(path, serialized)?;
    Ok(())
}

/// Read a [`SaveFile`] from `path`, validating that the format version matches
/// [`SAVE_FORMAT_VERSION`].
pub fn load_from_disk(path: &std::path::Path) -> Result<SaveFile, SaveError> {
    let s = std::fs::read_to_string(path)?;
    let file: SaveFile = ron::de::from_str(&s).map_err(|e| SaveError::Ron(e.to_string()))?;
    if file.version != SAVE_FORMAT_VERSION {
        return Err(SaveError::UnsupportedVersion(file.version));
    }
    Ok(file)
}

fn handle_save_requests(
    mut events: MessageReader<SaveRequest>,
    mut completed: MessageWriter<SaveCompleted>,
    params: Res<BiomeParams>,
    camera_q: Query<&CameraRig>,
) {
    for _ in events.read() {
        let camera_state = camera_q
            .single()
            .ok()
            .map(|rig| SaveCamera {
                target_x: rig.target.x,
                target_y: rig.target.y,
                zoom: rig.zoom,
            })
            .unwrap_or(SaveCamera {
                target_x: 0.0,
                target_y: 0.0,
                zoom: 1.0,
            });

        let file = SaveFile {
            version: SAVE_FORMAT_VERSION,
            world: SaveWorld {
                seed: params.world_seed,
            },
            camera: camera_state,
        };

        match save_path().and_then(|p| save_to_disk(&file, &p).map(|_| p)) {
            Ok(p) => {
                tracing::info!("saved to {}", p.display());
                completed.write(SaveCompleted);
            }
            Err(e) => tracing::error!("save failed: {}", e),
        }
    }
}

fn handle_load_requests(
    mut events: MessageReader<LoadRequest>,
    mut params: ResMut<BiomeParams>,
    mut camera_q: Query<&mut CameraRig>,
) {
    for _ in events.read() {
        let Ok(path) = save_path() else { continue };
        match load_from_disk(&path) {
            Ok(file) => {
                params.world_seed = file.world.seed;
                if let Ok(mut rig) = camera_q.single_mut() {
                    rig.target.x = file.camera.target_x;
                    rig.target.y = file.camera.target_y;
                    rig.zoom = file.camera.zoom;
                    rig.zoom_target = file.camera.zoom;
                }
                tracing::info!("loaded save from {}", path.display());
            }
            Err(e) => tracing::error!("load failed: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_save() -> SaveFile {
        SaveFile {
            version: SAVE_FORMAT_VERSION,
            world: SaveWorld {
                seed: 0xDEAD_BEEF_CAFE_F00D,
            },
            camera: SaveCamera {
                target_x: 12.5,
                target_y: -7.25,
                zoom: 1.5,
            },
        }
    }

    #[test]
    fn roundtrip_save_file() {
        let original = sample_save();
        let s = ron::ser::to_string_pretty(&original, ron::ser::PrettyConfig::default())
            .expect("serialize");
        let parsed: SaveFile = ron::de::from_str(&s).expect("deserialize");
        assert_eq!(parsed.version, original.version);
        assert_eq!(parsed.world.seed, original.world.seed);
        assert_eq!(parsed.camera.target_x, original.camera.target_x);
        assert_eq!(parsed.camera.target_y, original.camera.target_y);
        assert_eq!(parsed.camera.zoom, original.camera.zoom);
    }

    #[test]
    fn version_mismatch_returns_error() {
        let mut bad = sample_save();
        bad.version = SAVE_FORMAT_VERSION + 99;
        let s =
            ron::ser::to_string_pretty(&bad, ron::ser::PrettyConfig::default()).expect("serialize");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("inf2d_save_test_{}.ron", std::process::id()));
        std::fs::write(&path, s).expect("write tmp save");
        let err = load_from_disk(&path).expect_err("must reject mismatched version");
        let _ = std::fs::remove_file(&path);
        match err {
            SaveError::UnsupportedVersion(v) => assert_eq!(v, SAVE_FORMAT_VERSION + 99),
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn save_path_returns_path() {
        // On either windows-with-APPDATA or unix-with-HOME we should resolve a
        // non-empty path ending in `save.ron`. If neither env var is set in the
        // test runner we accept `NoSaveDir` as a valid outcome.
        match save_path() {
            Ok(p) => {
                let s = p.to_string_lossy();
                assert!(!s.is_empty(), "save path should not be empty");
                assert!(
                    s.ends_with("save.ron"),
                    "save path should end with save.ron, got {s}"
                );
            }
            Err(SaveError::NoSaveDir) => {
                // Accept on test runners with no HOME/APPDATA.
            }
            Err(e) => panic!("unexpected error from save_path: {e}"),
        }
    }
}
