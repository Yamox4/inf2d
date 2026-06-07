//! Save / load orchestration: the on-disk project format and its directory.
//!
//! A *project* is a pair of files sharing a stem under [`models_dir`]:
//! - `<name>.vox` — the geometry, written by the hand-rolled
//!   [`vox_writer`] (opens in MagicaVoxel and the game's `dot_vox` loader).
//! - `<name>.rig.ron` — the rig sidecar ([`rig::RigDoc`]): object type, part
//!   tree + pivots, per-voxel part membership, resolution, multi-block extent.
//!
//! Saving writes both atomically-enough for an editor (write `.rig.ron`, then
//! `.vox`); loading reads the sidecar (the source of truth for the rig) and
//! rebuilds the runtime model from it. The `.vox` is the *export* artifact; the
//! `.rig.ron` is the *project* artifact, so load goes through the sidecar.

pub mod rig;
pub mod vox_writer;

use std::fs;
use std::path::{Path, PathBuf};

use crate::palette::Palette;
use crate::parts::{ObjectType, PartTree};
use crate::volume::VoxelModel;
use rig::{RigDoc, RIG_EXTENSION};
use vox_writer::{write_vox, VoxScene, VoxVoxel};

/// Directory the editor reads/writes projects from. Resolved at compile time to
/// `<crate>/assets/models/` so a checkout has a stable, self-contained location
/// and deleting the crate folder removes every editor model with it.
pub fn models_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/models"))
}

/// A project discovered on disk: its stem (used as the display name and the file
/// base) and whether the rig sidecar exists (a bare `.vox` with no sidecar is
/// importable but has no rig yet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    /// File stem (the name shown in the browser; no extension).
    pub name: String,
    /// `true` if a `<name>.rig.ron` sidecar exists next to the `.vox`.
    pub has_rig: bool,
}

/// Outcome of an IO operation, surfaced to the UI status line.
#[derive(Clone, Debug)]
pub enum IoError {
    /// The model has no placed voxels, so there is nothing to export.
    Empty,
    /// The model is too large to fit the `.vox` 256/axis voxel limit.
    TooLarge,
    /// A filesystem error (with the offending path and message).
    Fs(String),
    /// The sidecar could not be parsed / is an unsupported version.
    BadSidecar(String),
}

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IoError::Empty => write!(f, "model is empty — nothing to save"),
            IoError::TooLarge => write!(f, "model exceeds the .vox 256-per-axis limit"),
            IoError::Fs(m) => write!(f, "filesystem error: {m}"),
            IoError::BadSidecar(m) => write!(f, "could not read rig sidecar: {m}"),
        }
    }
}

/// List the projects in [`models_dir`], sorted by name. A directory that does
/// not exist yet yields an empty list (it is created lazily on first save).
pub fn list_projects() -> Vec<ProjectEntry> {
    let dir = models_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out: Vec<ProjectEntry> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("vox") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let has_rig = rig_path(&dir, stem).exists();
        out.push(ProjectEntry {
            name: stem.to_string(),
            has_rig,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Path of the rig sidecar for `stem` inside `dir`.
fn rig_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}.{RIG_EXTENSION}"))
}

/// Path of the `.vox` for `stem` inside `dir`.
fn vox_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}.vox"))
}

/// Save a project: write the `.rig.ron` sidecar then the `.vox` geometry. The
/// directory is created if missing. Returns the cropped `.vox` dimensions on
/// success for the UI to report.
pub fn save_project(
    name: &str,
    object_type: ObjectType,
    model: &VoxelModel,
    tree: &PartTree,
    palette: &Palette,
) -> Result<[u32; 3], IoError> {
    let scene = build_scene(model, palette).ok_or(IoError::Empty)?;
    let vox_bytes = write_vox(&scene).ok_or(IoError::TooLarge)?;

    let dir = models_dir();
    fs::create_dir_all(&dir).map_err(|e| IoError::Fs(e.to_string()))?;

    let doc = RigDoc::from_state(name, object_type, model, tree, palette);
    let ron_text = ron::ser::to_string_pretty(&doc, ron::ser::PrettyConfig::default())
        .map_err(|e| IoError::Fs(e.to_string()))?;
    fs::write(rig_path(&dir, name), ron_text).map_err(|e| IoError::Fs(e.to_string()))?;
    fs::write(vox_path(&dir, name), vox_bytes).map_err(|e| IoError::Fs(e.to_string()))?;

    Ok(scene.size)
}

/// Load a project by stem from its rig sidecar, reconstructing the runtime
/// model, part tree, palette, and object type. The `.vox` is the export
/// artifact; the rig sidecar is the project source of truth, so load goes
/// through it.
pub fn load_project(
    name: &str,
) -> Result<(VoxelModel, PartTree, Palette, ObjectType), IoError> {
    let dir = models_dir();
    let text = fs::read_to_string(rig_path(&dir, name))
        .map_err(|e| IoError::Fs(e.to_string()))?;
    let doc: RigDoc = ron::from_str(&text).map_err(|e| IoError::BadSidecar(e.to_string()))?;
    doc.into_runtime()
        .ok_or_else(|| IoError::BadSidecar("unsupported version or no root part".into()))
}

/// Rename a project (both files). A no-op if the target name is unchanged.
pub fn rename_project(old: &str, new: &str) -> Result<(), IoError> {
    if old == new {
        return Ok(());
    }
    let dir = models_dir();
    // Move the `.vox` if present, then the sidecar if present. A project may
    // exist as a bare `.vox` (imported) with no sidecar, so a missing sidecar is
    // not an error here.
    let (ov, nv) = (vox_path(&dir, old), vox_path(&dir, new));
    if ov.exists() {
        fs::rename(&ov, &nv).map_err(|e| IoError::Fs(e.to_string()))?;
    }
    let (orr, nrr) = (rig_path(&dir, old), rig_path(&dir, new));
    if orr.exists() {
        fs::rename(&orr, &nrr).map_err(|e| IoError::Fs(e.to_string()))?;
    }
    Ok(())
}

/// Delete a project (both files). Missing files are ignored.
pub fn delete_project(name: &str) -> Result<(), IoError> {
    let dir = models_dir();
    for p in [vox_path(&dir, name), rig_path(&dir, name)] {
        if p.exists() {
            fs::remove_file(&p).map_err(|e| IoError::Fs(e.to_string()))?;
        }
    }
    Ok(())
}

/// Build a [`VoxScene`] from the editor model: crop to the occupied bounds and
/// convert each editor cell `(ex, ey, ez)` (Y-up) into MagicaVoxel voxel space
/// `(ex, ez, ey)` (Z-up). Returns `None` if the model is empty.
///
/// The axis swap is the exact inverse of the game loader's `(x, y, z) → (x, z,
/// -y)` map (the loader re-applies the Y flip on read), so a model built upright
/// in the editor exports upright through both MagicaVoxel and the game.
fn build_scene(model: &VoxelModel, palette: &Palette) -> Option<VoxScene> {
    let (min, max) = model.occupied_bounds()?;
    // Cropped editor-space extents (Y-up).
    let ex = (max.x - min.x + 1) as u32;
    let ey = (max.y - min.y + 1) as u32;
    let ez = (max.z - min.z + 1) as u32;
    // MagicaVoxel size: x stays x, y ← editor z, z ← editor y.
    let size = [ex, ez, ey];

    let voxels = model
        .iter()
        .map(|(cell, v)| VoxVoxel {
            x: (cell.x - min.x) as u8,
            y: (cell.z - min.z) as u8,
            z: (cell.y - min.y) as u8,
            palette_slot: v.color,
        })
        .collect();

    Some(VoxScene {
        size,
        voxels,
        palette: palette.rgbs(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::PartId;
    use crate::volume::{Cell, Voxel};

    #[test]
    fn scene_swaps_y_and_z_and_crops() {
        let mut model = VoxelModel::new(16, 2); // extent 32
        // A voxel high up (y) and shallow (z) — after the swap, .vox z should be
        // the tall axis and .vox y the shallow one.
        model.set(
            Cell::new(4, 10, 2),
            Voxel {
                color: 0,
                part: PartId(0),
            },
        );
        let palette = Palette::default_game();
        let scene = build_scene(&model, &palette).expect("scene");
        // Single voxel → 1×1×1 cropped.
        assert_eq!(scene.size, [1, 1, 1]);
        let v = scene.voxels[0];
        assert_eq!((v.x, v.y, v.z), (0, 0, 0));
    }

    #[test]
    fn empty_model_has_no_scene() {
        let model = VoxelModel::new(8, 1);
        assert!(build_scene(&model, &Palette::default_game()).is_none());
    }
}
