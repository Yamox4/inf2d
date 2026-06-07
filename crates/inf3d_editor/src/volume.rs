//! The editable sub-voxel data model.
//!
//! A [`VoxelModel`] is a sparse grid of colored sub-voxels spread across an
//! `N×N×N` arrangement of reference *blocks*, where each block is one in-game
//! `1×1×1` world voxel subdivided into `resolution³` sub-voxels. The grid is
//! sparse (a [`HashMap`] keyed by integer cell) because models are mostly empty
//! air; only placed sub-voxels cost memory.
//!
//! Each sub-voxel records the [`PaletteColor`](crate::palette::PaletteColor)
//! index that paints it and the [`PartId`](crate::parts::PartId) of the body
//! part it belongs to. Keeping the part tag *on the voxel* is what lets the
//! Phase-2 Animator transform a part's voxels as a rigid group — the part tree
//! ([`crate::parts`]) supplies the pivots, this grid supplies the membership.

use std::collections::HashMap;

use crate::parts::PartId;

/// Lowest sub-voxel resolution offered (sub-voxels per block edge). Coarse, for
/// blocky low-poly models.
pub const RESOLUTION_MIN: u32 = 4;
/// Highest sub-voxel resolution offered. `32³` per block is MagicaVoxel-grade
/// detail; combined with the multi-block extent it bounds the exported `.vox`
/// dimensions (`resolution * blocks` per axis).
pub const RESOLUTION_MAX: u32 = 32;
/// The resolutions the UI lets the user pick (sub-voxels per block edge).
pub const RESOLUTION_CHOICES: [u32; 4] = [8, 16, 24, 32];
/// Default sub-voxel resolution for a fresh model — 16³ per block is a good
/// balance of detail and clarity.
pub const DEFAULT_RESOLUTION: u32 = 16;

/// The smallest multi-block extent (a single reference block, i.e. one in-game
/// voxel of build volume).
pub const BLOCKS_MIN: u32 = 1;
/// The largest multi-block extent offered.
pub const BLOCKS_MAX: u32 = 4;

/// Integer coordinate of one sub-voxel cell within a [`VoxelModel`]'s grid.
///
/// The origin `(0, 0, 0)` is the min corner of the whole build volume. `x`/`z`
/// span the horizontal footprint and `y` is up (Bevy convention). The valid
/// range on each axis is `0..extent()` where `extent = resolution * blocks`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Cell {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Cell {
    /// Construct a cell from raw components.
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

/// One placed sub-voxel: which palette color paints it and which body part owns
/// it. Stored as the value in [`VoxelModel::voxels`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Voxel {
    /// Index into the model's [`Palette`](crate::palette::Palette).
    pub color: u8,
    /// The body part this sub-voxel belongs to (rig membership).
    pub part: PartId,
}

/// The full editable model: a sparse sub-voxel grid plus the extent metadata
/// that defines its coordinate space. The part tree and palette live alongside
/// it in [`EditorState`](crate::state::EditorState); this type owns *only* the
/// geometry so the data model stays single-responsibility.
#[derive(Clone, Debug)]
pub struct VoxelModel {
    /// Sub-voxels per reference-block edge (uniform on all axes).
    resolution: u32,
    /// Reference-block count per edge (the `N` of the `N×N×N` build volume).
    blocks: u32,
    /// Sparse placed sub-voxels, keyed by [`Cell`].
    voxels: HashMap<Cell, Voxel>,
}

impl VoxelModel {
    /// Create an empty model at the given resolution and block extent, clamped
    /// to the supported ranges.
    pub fn new(resolution: u32, blocks: u32) -> Self {
        Self {
            resolution: resolution.clamp(RESOLUTION_MIN, RESOLUTION_MAX),
            blocks: blocks.clamp(BLOCKS_MIN, BLOCKS_MAX),
            voxels: HashMap::new(),
        }
    }

    /// Sub-voxels per reference-block edge.
    pub fn resolution(&self) -> u32 {
        self.resolution
    }

    /// Reference-block count per edge.
    pub fn blocks(&self) -> u32 {
        self.blocks
    }

    /// Total sub-voxel cells per axis (`resolution * blocks`); the exclusive
    /// upper bound for a valid [`Cell`] component.
    pub fn extent(&self) -> i32 {
        (self.resolution * self.blocks) as i32
    }

    /// Edge length of one sub-voxel in world units. A reference block is one
    /// world unit, subdivided into `resolution` parts.
    pub fn sub_voxel_size(&self) -> f32 {
        1.0 / self.resolution as f32
    }

    /// `true` if `cell` lies inside the build volume on every axis.
    pub fn in_bounds(&self, cell: Cell) -> bool {
        let e = self.extent();
        (0..e).contains(&cell.x) && (0..e).contains(&cell.y) && (0..e).contains(&cell.z)
    }

    /// Read the sub-voxel at `cell`, if any.
    pub fn get(&self, cell: Cell) -> Option<Voxel> {
        self.voxels.get(&cell).copied()
    }

    /// `true` if a sub-voxel is present at `cell`.
    pub fn is_solid(&self, cell: Cell) -> bool {
        self.voxels.contains_key(&cell)
    }

    /// Place (or overwrite) a sub-voxel. Out-of-bounds cells are ignored so the
    /// caller never has to pre-validate. Returns `true` if the grid changed.
    pub fn set(&mut self, cell: Cell, voxel: Voxel) -> bool {
        if !self.in_bounds(cell) {
            return false;
        }
        self.voxels.insert(cell, voxel) != Some(voxel)
    }

    /// Erase the sub-voxel at `cell`. Returns `true` if one was removed.
    pub fn clear(&mut self, cell: Cell) -> bool {
        self.voxels.remove(&cell).is_some()
    }

    /// Iterate every placed sub-voxel as `(cell, voxel)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (Cell, Voxel)> + '_ {
        self.voxels.iter().map(|(&c, &v)| (c, v))
    }

    /// Number of placed sub-voxels in the whole model.
    pub fn voxel_count(&self) -> usize {
        self.voxels.len()
    }

    /// Number of placed sub-voxels belonging to one body part.
    pub fn part_voxel_count(&self, part: PartId) -> usize {
        self.voxels.values().filter(|v| v.part == part).count()
    }

    /// Drop every sub-voxel tagged with `part` (used when a part is deleted from
    /// the rig so the geometry can't outlive its owner).
    pub fn remove_part(&mut self, part: PartId) {
        self.voxels.retain(|_, v| v.part != part);
    }

    /// Erase all geometry, keeping the resolution/extent.
    pub fn clear_all(&mut self) {
        self.voxels.clear();
    }

    /// Replace the whole sub-voxel set (used on load). The caller supplies the
    /// already-validated resolution/extent via [`VoxelModel::new`] first.
    pub fn replace_voxels(&mut self, voxels: HashMap<Cell, Voxel>) {
        self.voxels = voxels;
    }

    /// The inclusive min/max occupied cell on each axis, or `None` if empty.
    /// The `.vox` exporter uses this to crop the model to its tight bounds.
    pub fn occupied_bounds(&self) -> Option<(Cell, Cell)> {
        let mut it = self.voxels.keys();
        let first = *it.next()?;
        let mut min = first;
        let mut max = first;
        for &c in it {
            min.x = min.x.min(c.x);
            min.y = min.y.min(c.y);
            min.z = min.z.min(c.z);
            max.x = max.x.max(c.x);
            max.y = max.y.max(c.y);
            max.z = max.z.max(c.z);
        }
        Some((min, max))
    }
}

impl Default for VoxelModel {
    fn default() -> Self {
        Self::new(DEFAULT_RESOLUTION, BLOCKS_MIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::PartId;

    fn vox(color: u8) -> Voxel {
        Voxel {
            color,
            part: PartId(0),
        }
    }

    #[test]
    fn extent_is_resolution_times_blocks() {
        let m = VoxelModel::new(16, 3);
        assert_eq!(m.extent(), 48);
        assert!((m.sub_voxel_size() - 1.0 / 16.0).abs() < 1e-6);
    }

    #[test]
    fn set_respects_bounds() {
        let mut m = VoxelModel::new(8, 1); // extent 8
        assert!(m.set(Cell::new(0, 0, 0), vox(1)));
        assert!(m.set(Cell::new(7, 7, 7), vox(1)));
        assert!(!m.set(Cell::new(8, 0, 0), vox(1))); // out of bounds
        assert!(!m.set(Cell::new(-1, 0, 0), vox(1)));
        assert_eq!(m.voxel_count(), 2);
    }

    #[test]
    fn set_reports_change_only_on_difference() {
        let mut m = VoxelModel::new(8, 1);
        assert!(m.set(Cell::new(1, 1, 1), vox(1)));
        assert!(!m.set(Cell::new(1, 1, 1), vox(1))); // identical → no change
        assert!(m.set(Cell::new(1, 1, 1), vox(2))); // color differs → change
    }

    #[test]
    fn clear_and_remove_part() {
        let mut m = VoxelModel::new(8, 1);
        m.set(
            Cell::new(0, 0, 0),
            Voxel {
                color: 1,
                part: PartId(0),
            },
        );
        m.set(
            Cell::new(1, 0, 0),
            Voxel {
                color: 1,
                part: PartId(1),
            },
        );
        assert_eq!(m.part_voxel_count(PartId(0)), 1);
        m.remove_part(PartId(0));
        assert_eq!(m.voxel_count(), 1);
        assert!(m.clear(Cell::new(1, 0, 0)));
        assert!(!m.clear(Cell::new(1, 0, 0)));
    }

    #[test]
    fn occupied_bounds_tracks_min_max() {
        let mut m = VoxelModel::new(16, 2); // extent 32
        assert!(m.occupied_bounds().is_none());
        m.set(Cell::new(3, 4, 5), vox(1));
        m.set(Cell::new(10, 2, 8), vox(1));
        let (min, max) = m.occupied_bounds().expect("bounds");
        assert_eq!(min, Cell::new(3, 2, 5));
        assert_eq!(max, Cell::new(10, 4, 8));
    }
}
