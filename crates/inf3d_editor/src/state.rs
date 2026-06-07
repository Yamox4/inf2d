//! The central editor state — the single owned resource the whole app reads and
//! writes.
//!
//! [`EditorState`] bundles the active project: its [`VoxelModel`] geometry, the
//! [`PartTree`] rig, the [`Palette`], the [`ObjectType`], and the transient UI
//! selection (current tool, part, color). Systems mutate it directly; the
//! render system rebuilds the visible mesh whenever [`EditorState::dirty`] is
//! set. No global mutable hacks — this is the one resource, `init_resource`'d
//! once in `main`.

use bevy::prelude::*;

use crate::palette::Palette;
use crate::parts::{default_parts, ObjectType, PartId, PartTree};
use crate::volume::{Cell, Voxel, VoxelModel, BLOCKS_MAX, BLOCKS_MIN};

/// The active painting tool. Left-click always applies the active tool; the UI
/// (and right-click) switch between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tool {
    /// Add a sub-voxel of the selected color/part on the face the cursor hits.
    #[default]
    Paint,
    /// Remove the sub-voxel the cursor hits.
    Erase,
    /// Adopt the color of the sub-voxel the cursor hits (eyedropper).
    Pick,
}

impl Tool {
    /// Short label for the toolbar.
    pub const fn label(self) -> &'static str {
        match self {
            Tool::Paint => "Paint",
            Tool::Erase => "Erase",
            Tool::Pick => "Pick",
        }
    }
}

/// The editor's whole working state. Single source of truth for the open model.
#[derive(Resource)]
pub struct EditorState {
    /// Display name of the open project (the file stem on save).
    pub name: String,
    /// What the model represents; drives the rig panel.
    pub object_type: ObjectType,
    /// The editable sub-voxel geometry.
    pub model: VoxelModel,
    /// The body-part hierarchy (rig).
    pub tree: PartTree,
    /// The working color palette.
    pub palette: Palette,

    /// The active tool.
    pub tool: Tool,
    /// The palette index new sub-voxels are painted with.
    pub active_color: u8,
    /// The part new sub-voxels are tagged with (the part being edited).
    pub active_part: PartId,

    /// `true` when the visible mesh is stale and the render system must rebuild.
    pub dirty: bool,
    /// Whether to draw the sub-voxel grid overlay.
    pub show_grid: bool,
    /// Whether to draw the reference-block boundary outline(s).
    pub show_reference: bool,
    /// Whether to draw each part's pivot gizmo (rig preview).
    pub show_pivots: bool,
    /// The last status message (save/load outcome), shown in the UI.
    pub status: String,
}

impl EditorState {
    /// A fresh, empty character project ready to paint.
    pub fn new_default() -> Self {
        let object_type = ObjectType::Character;
        let tree = default_parts(object_type);
        let model = VoxelModel::default();
        Self {
            name: "untitled".to_string(),
            object_type,
            model,
            active_part: tree.root(),
            tree,
            palette: Palette::default_game(),
            tool: Tool::Paint,
            active_color: 0,
            dirty: true,
            show_grid: true,
            show_reference: true,
            show_pivots: false,
            status: "New character".to_string(),
        }
    }

    /// Replace the whole project (used on load / new). Marks the scene dirty and
    /// clamps the selection to valid values for the incoming data.
    pub fn replace(
        &mut self,
        name: String,
        object_type: ObjectType,
        model: VoxelModel,
        tree: PartTree,
        palette: Palette,
    ) {
        self.name = name;
        self.object_type = object_type;
        self.model = model;
        self.active_part = tree.root();
        self.tree = tree;
        self.active_color = self.active_color.min(palette.len().saturating_sub(1) as u8);
        self.palette = palette;
        self.tool = Tool::Paint;
        self.dirty = true;
    }

    /// Start a brand-new project of the given type, discarding the current one.
    pub fn new_project(&mut self, object_type: ObjectType) {
        let tree = default_parts(object_type);
        let model = VoxelModel::new(self.model.resolution(), BLOCKS_MIN);
        self.replace("untitled".to_string(), object_type, model, tree, Palette::default_game());
        self.status = format!("New {}", object_type.label());
    }

    /// Apply a [`Voxel`] at `cell` using the active part/color, marking dirty if
    /// the grid changed.
    pub fn paint(&mut self, cell: Cell) {
        let voxel = Voxel {
            color: self.active_color,
            part: self.active_part,
        };
        if self.model.set(cell, voxel) {
            self.dirty = true;
        }
    }

    /// Erase the sub-voxel at `cell`, marking dirty if one was removed.
    pub fn erase(&mut self, cell: Cell) {
        if self.model.clear(cell) {
            self.dirty = true;
        }
    }

    /// Eyedrop the color (and part) at `cell` into the active selection.
    pub fn pick(&mut self, cell: Cell) {
        if let Some(v) = self.model.get(cell) {
            self.active_color = v.color;
            self.active_part = v.part;
        }
    }

    /// Resize the build volume to `blocks` per edge, clamped to the supported
    /// range. Rebuilds the model at the new extent, keeping every voxel that
    /// still fits. No-op if unchanged.
    pub fn set_blocks(&mut self, blocks: u32) {
        let blocks = blocks.clamp(BLOCKS_MIN, BLOCKS_MAX);
        if blocks == self.model.blocks() {
            return;
        }
        let mut next = VoxelModel::new(self.model.resolution(), blocks);
        for (cell, v) in self.model.iter() {
            // Cells outside the (possibly smaller) new extent are dropped by
            // `set`'s bounds check.
            next.set(cell, v);
        }
        self.model = next;
        self.dirty = true;
    }

    /// Change the sub-voxel resolution. This is destructive (the grid does not
    /// resample), so it clears geometry to avoid leaving voxels at meaningless
    /// coordinates; the caller surfaces the warning. No-op if unchanged.
    pub fn set_resolution(&mut self, resolution: u32) {
        if resolution == self.model.resolution() {
            return;
        }
        self.model = VoxelModel::new(resolution, self.model.blocks());
        self.dirty = true;
        self.status = format!("Resolution set to {resolution} (geometry cleared)");
    }

    /// Switch the active part being edited, if `id` is a live part.
    pub fn select_part(&mut self, id: PartId) {
        if self.tree.get(id).is_some() {
            self.active_part = id;
        }
    }

    /// Clear all geometry (keeps the rig, palette, and extents).
    pub fn clear_geometry(&mut self) {
        self.model.clear_all();
        self.dirty = true;
        self.status = "Cleared geometry".to_string();
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new_default()
    }
}
