//! The `.ron` rig sidecar schema — the editor's interchange format.
//!
//! Geometry exports to `.vox` (so it opens in MagicaVoxel and the game's
//! `dot_vox` loader). Everything the `.vox` can't carry — the object type, the
//! part hierarchy with pivots, which sub-voxels belong to which part, and the
//! resolution / multi-block extent — lives in a sibling `.ron` file. Phase 2
//! (Animator) and a future game importer read THIS to reconstruct the rig.
//!
//! The schema is intentionally explicit and versioned ([`RIG_VERSION`]) so it
//! can evolve without breaking older files. It is the long-lived contract;
//! [`crate::volume`] / [`crate::parts`] are the runtime types it converts to and
//! from. Voxels are stored in **editor cell space** (Y-up, see
//! [`crate::volume::Cell`]), the same space the runtime grid uses, so the
//! Animator can transform a part's cells directly without re-deriving the `.vox`
//! axis swap.

use std::collections::HashMap;

use bevy::prelude::Vec3;
use serde::{Deserialize, Serialize};

use crate::palette::Palette;
use crate::parts::{ObjectType, Part, PartId, PartTree};
use crate::volume::{Cell, Voxel, VoxelModel};

/// Schema version stamped into every sidecar. Bump on a breaking change; the
/// loader rejects versions it doesn't understand.
pub const RIG_VERSION: u32 = 1;

/// File extension for the rig sidecar (paired with the `.vox` of the same stem).
pub const RIG_EXTENSION: &str = "rig.ron";

/// One persisted part: the runtime [`Part`] flattened for serde. Mirrors
/// [`Part`] field-for-field with a serde-friendly pivot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RigPart {
    /// Stable part id (dense index, preserved across save/load).
    pub id: u32,
    /// Display / export name.
    pub name: String,
    /// Parent part id, or `None` for the root.
    pub parent: Option<u32>,
    /// Joint pivot in model-local world units `[x, y, z]`.
    pub pivot: [f32; 3],
}

/// One persisted sub-voxel: its cell coordinate, palette slot, and owning part.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct RigVoxel {
    /// Editor cell coordinate `[x, y, z]` (Y-up).
    pub cell: [i32; 3],
    /// Palette slot (index into [`RigDoc::palette`]).
    pub color: u8,
    /// Owning part id.
    pub part: u32,
}

/// The full sidecar document. One `.rig.ron` per model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RigDoc {
    /// Schema version ([`RIG_VERSION`]).
    pub version: u32,
    /// Human name of the model (matches the file stem).
    pub name: String,
    /// Whether this is a posable `Character` or a single-piece `Item`.
    pub object_type: ObjectType,
    /// Sub-voxels per reference-block edge.
    pub resolution: u32,
    /// Reference-block count per edge (the `N` of the `N×N×N` build volume).
    pub blocks: u32,
    /// The root part id.
    pub root_part: u32,
    /// The part hierarchy.
    pub parts: Vec<RigPart>,
    /// sRGB palette in slot order (`palette[i]` is color index `i`).
    pub palette: Vec<[u8; 3]>,
    /// Every placed sub-voxel.
    pub voxels: Vec<RigVoxel>,
}

impl RigDoc {
    /// Build a sidecar document from the live editor state.
    pub fn from_state(
        name: &str,
        object_type: ObjectType,
        model: &VoxelModel,
        tree: &PartTree,
        palette: &Palette,
    ) -> Self {
        let parts = tree
            .iter()
            .map(|p| RigPart {
                id: p.id.0,
                name: p.name.clone(),
                parent: p.parent.map(|pid| pid.0),
                pivot: [p.pivot.x, p.pivot.y, p.pivot.z],
            })
            .collect();

        let voxels = model
            .iter()
            .map(|(cell, v)| RigVoxel {
                cell: [cell.x, cell.y, cell.z],
                color: v.color,
                part: v.part.0,
            })
            .collect();

        Self {
            version: RIG_VERSION,
            name: name.to_string(),
            object_type,
            resolution: model.resolution(),
            blocks: model.blocks(),
            root_part: tree.root().0,
            parts,
            palette: palette.rgbs(),
            voxels,
        }
    }

    /// Reconstruct the runtime model + part tree + palette from a loaded
    /// document. Returns `None` if the schema version is unsupported or the part
    /// data is internally inconsistent (no root). Out-of-bounds voxels are
    /// dropped rather than rejected so a slightly-corrupt file still loads.
    pub fn into_runtime(self) -> Option<(VoxelModel, PartTree, Palette, ObjectType)> {
        if self.version != RIG_VERSION {
            return None;
        }

        // Rebuild the part tree as a dense, id-indexed arena so PartIds stay the
        // values stored on the voxels. Tombstone any gaps in the id space.
        let max_id = self.parts.iter().map(|p| p.id).max().unwrap_or(0);
        let mut slots: Vec<Option<Part>> = vec![None; (max_id as usize) + 1];
        for rp in &self.parts {
            let part = Part {
                id: PartId(rp.id),
                name: rp.name.clone(),
                parent: rp.parent.map(PartId),
                pivot: Vec3::from_array(rp.pivot),
            };
            if let Some(slot) = slots.get_mut(rp.id as usize) {
                *slot = Some(part);
            }
        }
        // The root must reference a live part.
        if slots
            .get(self.root_part as usize)
            .and_then(|s| s.as_ref())
            .is_none()
        {
            return None;
        }
        let tree = PartTree::from_arena(slots, PartId(self.root_part));

        let mut model = VoxelModel::new(self.resolution, self.blocks);
        let mut voxels: HashMap<Cell, Voxel> = HashMap::with_capacity(self.voxels.len());
        for rv in &self.voxels {
            let cell = Cell::new(rv.cell[0], rv.cell[1], rv.cell[2]);
            if !model.in_bounds(cell) {
                continue; // tolerate a stale/corrupt out-of-range voxel
            }
            voxels.insert(
                cell,
                Voxel {
                    color: rv.color,
                    part: PartId(rv.part),
                },
            );
        }
        model.replace_voxels(voxels);

        let palette = Palette::from_rgbs(&self.palette);
        Some((model, tree, palette, self.object_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::default_character_parts;

    fn build_doc() -> RigDoc {
        let mut model = VoxelModel::new(16, 2);
        let tree = default_character_parts();
        let head = PartId(1);
        model.set(Cell::new(5, 20, 5), Voxel { color: 2, part: head });
        model.set(Cell::new(6, 20, 5), Voxel { color: 3, part: head });
        let palette = Palette::default_game();
        RigDoc::from_state("hero", ObjectType::Character, &model, &tree, &palette)
    }

    #[test]
    fn ron_roundtrip_preserves_model_and_rig() {
        let doc = build_doc();
        let text = ron::ser::to_string_pretty(&doc, ron::ser::PrettyConfig::default())
            .expect("serialize");
        let back: RigDoc = ron::from_str(&text).expect("deserialize");
        let (model, tree, _palette, obj) = back.into_runtime().expect("runtime");
        assert_eq!(obj, ObjectType::Character);
        assert_eq!(model.resolution(), 16);
        assert_eq!(model.blocks(), 2);
        assert_eq!(model.voxel_count(), 2);
        assert_eq!(model.get(Cell::new(5, 20, 5)).map(|v| v.color), Some(2));
        // The whole humanoid rig survived.
        assert_eq!(tree.len(), 6);
        assert_eq!(tree.get(tree.root()).map(|p| p.name.as_str()), Some("Torso"));
    }

    #[test]
    fn version_mismatch_is_rejected() {
        let mut doc = build_doc();
        doc.version = 999;
        assert!(doc.into_runtime().is_none());
    }
}
