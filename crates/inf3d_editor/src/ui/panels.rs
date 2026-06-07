//! The egui panel widgets — the editor's whole UI surface.
//!
//! Layout (MagicaVoxel-inspired):
//! - **Top bar**: project name, object type, New/Save, build-volume controls
//!   (block count + resolution), and view toggles.
//! - **Left panel**: the tool selector, the color palette grid + custom-color
//!   picker, and (for a Character) the body-part hierarchy with per-part voxel
//!   counts and pivot editing.
//! - **Right panel**: the project browser — list of saved models with
//!   Load/Rename/Delete.
//!
//! Immediate-mode widgets can't borrow [`EditorState`] mutably *and* run IO in
//! the same closure cleanly, so panels collect a [`PendingAction`] and the
//! caller applies it after the frame. This keeps the IO (file reads/writes) out
//! of the draw closures and the borrow checker happy.

use bevy_egui::egui;

use crate::io;
use crate::palette::MAX_COLORS;
use crate::parts::{ObjectType, PartId};
use crate::state::{EditorState, Tool};

use crate::volume::{RESOLUTION_CHOICES, BLOCKS_MAX, BLOCKS_MIN};

/// A deferred action raised by a panel and applied after the draw pass, so file
/// IO and structural edits don't fight egui's borrow of the closures.
enum PendingAction {
    /// Save the current project under its current name.
    Save,
    /// Start a new project of the given type.
    New(ObjectType),
    /// Load the named project.
    Load(String),
    /// Rename `from` → `to` (both on disk and in the editor if it's the open one).
    Rename { from: String, to: String },
    /// Delete the named project from disk.
    Delete(String),
    /// Add a child part under `parent` named `name`.
    AddPart { parent: PartId, name: String },
    /// Rename the given part to `name`.
    RenamePart { id: PartId, name: String },
    /// Re-parent `child` under `new_parent` (rejected on cycle / root).
    Reparent { child: PartId, new_parent: PartId },
    /// Delete the given part (and its geometry).
    DeletePart(PartId),
    /// Clear all geometry.
    ClearGeometry,
}

/// UI-local scratch that must persist between frames (text edit buffers, the
/// project list). Stored in egui's per-id memory so the panels stay stateless
/// from Bevy's side.
#[derive(Clone, Default)]
struct UiScratch {
    /// Cached project list (refreshed on demand, not every frame).
    projects: Vec<io::ProjectEntry>,
    /// Rename target buffer, keyed to the project being renamed.
    rename_buf: String,
    /// Which project (if any) is mid-rename.
    renaming: Option<String>,
    /// New-part name buffer.
    new_part_buf: String,
    /// Rename buffer for the active part, and the part id it is bound to (so the
    /// buffer resets when the selection changes).
    part_rename_buf: String,
    /// Which part the rename buffer currently holds text for.
    part_rename_for: Option<u32>,
    /// Custom color being mixed in the palette picker (sRGB bytes).
    custom_rgb: [u8; 3],
}

/// Draw the whole UI for one frame, apply any deferred action, and return
/// `true` if the pointer is over any panel.
///
/// egui's `is_pointer_over_area()` ignores `Order::Background` layers — which is
/// what panels use — so it can't gate clicks on a panel's *empty* background.
/// We instead union the panels' own rects (each `show` returns an
/// `InnerResponse` carrying its rect) and test the pointer against that, OR-ed
/// with `wants_pointer_input()` for active widget interaction. That reliably
/// stops viewport painting from firing through a panel.
pub fn draw(ctx: &egui::Context, state: &mut EditorState) -> bool {
    // Pull the persistent scratch out of egui memory (clone), draw with it, then
    // store it back. egui memory is the idiomatic home for cross-frame UI buffers.
    let id = egui::Id::new("inf3d_editor_scratch");
    let mut scratch: UiScratch = ctx
        .memory_mut(|m| m.data.get_temp::<UiScratch>(id))
        .unwrap_or_else(|| {
            UiScratch {
                custom_rgb: [200, 200, 200],
                projects: io::list_projects(),
                ..Default::default()
            }
        });

    let mut action: Option<PendingAction> = None;

    let panel_rects = [
        top_bar(ctx, state, &mut action),
        left_panel(ctx, state, &mut scratch, &mut action),
        right_panel(ctx, &mut scratch, &mut action),
        bottom_bar(ctx, state),
    ];

    if let Some(act) = action {
        apply_action(act, state, &mut scratch);
    }

    ctx.memory_mut(|m| m.data.insert_temp(id, scratch));

    // Pointer over any panel rect, or egui actively wants the pointer.
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    let over_panel = pointer
        .map(|p| panel_rects.iter().any(|r| r.contains(p)))
        .unwrap_or(false);
    over_panel || ctx.wants_pointer_input()
}

/// Top bar: identity, new/save, build-volume controls, view toggles. Returns its
/// screen rect (for the pointer-over-UI gate).
fn top_bar(
    ctx: &egui::Context,
    state: &mut EditorState,
    action: &mut Option<PendingAction>,
) -> egui::Rect {
    egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.heading("inf3d editor");
            ui.separator();

            ui.label("Name:");
            ui.add(egui::TextEdit::singleline(&mut state.name).desired_width(120.0));

            ui.separator();
            // Object type selector — drives whether the rig panel shows a part tree.
            egui::ComboBox::from_id_salt("object_type")
                .selected_text(state.object_type.label())
                .show_ui(ui, |ui| {
                    for obj in [ObjectType::Character, ObjectType::Item] {
                        if ui
                            .selectable_label(state.object_type == obj, obj.label())
                            .clicked()
                            && state.object_type != obj
                        {
                            *action = Some(PendingAction::New(obj));
                        }
                    }
                });

            ui.separator();
            if ui.button("New").clicked() {
                *action = Some(PendingAction::New(state.object_type));
            }
            if ui.button("Save").clicked() {
                *action = Some(PendingAction::Save);
            }

            ui.separator();
            // Build volume: N blocks per edge (the multi-block reference toggle).
            ui.label("Blocks:");
            let mut blocks = state.model.blocks();
            if ui
                .add(egui::Slider::new(&mut blocks, BLOCKS_MIN..=BLOCKS_MAX).integer())
                .changed()
            {
                state.set_blocks(blocks);
            }

            ui.separator();
            // Sub-voxel resolution (destructive; clears geometry).
            ui.label("Res:");
            egui::ComboBox::from_id_salt("resolution")
                .selected_text(format!("{}³", state.model.resolution()))
                .show_ui(ui, |ui| {
                    for &r in &RESOLUTION_CHOICES {
                        if ui
                            .selectable_label(state.model.resolution() == r, format!("{r}³"))
                            .clicked()
                        {
                            state.set_resolution(r);
                        }
                    }
                });

            ui.separator();
            ui.checkbox(&mut state.show_grid, "Grid");
            ui.checkbox(&mut state.show_reference, "Reference");
            ui.checkbox(&mut state.show_pivots, "Pivots");
        });
    })
    .response
    .rect
}

/// Left panel: tools, palette, and the rig (parts) tree. Returns its screen rect.
fn left_panel(
    ctx: &egui::Context,
    state: &mut EditorState,
    scratch: &mut UiScratch,
    action: &mut Option<PendingAction>,
) -> egui::Rect {
    egui::SidePanel::left("tools_panel")
        .resizable(true)
        .default_width(240.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                tool_section(ui, state, action);
                ui.separator();
                palette_section(ui, state, scratch);
                ui.separator();
                rig_section(ui, state, scratch, action);
            });
        })
        .response
        .rect
}

/// The tool selector + geometry stats.
fn tool_section(ui: &mut egui::Ui, state: &mut EditorState, action: &mut Option<PendingAction>) {
    ui.heading("Tools");
    ui.horizontal(|ui| {
        for tool in [Tool::Paint, Tool::Erase, Tool::Pick] {
            if ui
                .selectable_label(state.tool == tool, tool.label())
                .clicked()
            {
                state.tool = tool;
            }
        }
    });
    ui.label(format!("Voxels: {}", state.model.voxel_count()));
    if ui.button("Clear geometry").clicked() {
        *action = Some(PendingAction::ClearGeometry);
    }
}

/// The color palette grid + custom color picker.
fn palette_section(ui: &mut egui::Ui, state: &mut EditorState, scratch: &mut UiScratch) {
    ui.heading("Palette");
    let swatch = egui::vec2(22.0, 22.0);
    let cols = 8;
    egui::Grid::new("palette_grid").spacing([3.0, 3.0]).show(ui, |ui| {
        let mut col = 0;
        // Snapshot the (index, rgb, selected) tuples first so we don't borrow the
        // palette while also mutably selecting into `state`.
        let entries: Vec<(u8, [u8; 3])> =
            state.palette.iter().map(|(i, c)| (i, c.rgb)).collect();
        for (idx, rgb) in entries {
            let selected = idx == state.active_color;
            let fill = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            let (rect, resp) = ui.allocate_exact_size(swatch, egui::Sense::click());
            ui.painter().rect_filled(rect, 3.0, fill);
            if selected {
                ui.painter().rect_stroke(
                    rect,
                    3.0,
                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                    egui::StrokeKind::Outside,
                );
            }
            if resp.clicked() {
                state.active_color = idx;
            }
            col += 1;
            if col % cols == 0 {
                ui.end_row();
            }
        }
    });

    // Show + edit the active color in place.
    let active = state.active_color;
    let mut rgb = state.palette.rgb(active);
    ui.horizontal(|ui| {
        ui.label("Active:");
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            state.palette.set_rgb(active, rgb);
            state.dirty = true; // re-tint the mesh
        }
        if let Some(c) = state.palette.get(active) {
            ui.label(c.name);
        }
    });

    // Mix and add a brand-new custom color.
    ui.horizontal(|ui| {
        ui.label("New:");
        ui.color_edit_button_srgb(&mut scratch.custom_rgb);
        let full = state.palette.len() >= MAX_COLORS;
        if ui
            .add_enabled(!full, egui::Button::new("Add"))
            .on_hover_text("Append this color to the palette")
            .clicked()
        {
            if let Some(idx) = state.palette.push_custom(scratch.custom_rgb) {
                state.active_color = idx;
            }
        }
    });
}

/// The rig section: the part hierarchy editor. Works identically for a
/// `Character` and an `Item` — both own a [`PartTree`] and support multiple
/// named, hierarchical, animatable parts (an item just starts with a single
/// `Body` part). The active part is the one new sub-voxels are tagged with.
fn rig_section(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    scratch: &mut UiScratch,
    action: &mut Option<PendingAction>,
) {
    ui.heading("Rig");
    let noun = match state.object_type {
        ObjectType::Character => "body part",
        ObjectType::Item => "part",
    };
    ui.label(format!(
        "Select the {noun} you are editing ({} parts):",
        state.tree.len()
    ));

    // Walk the tree from the root, rendering an indented, selectable row per part
    // with its live voxel count. Snapshot the rows first (id, name, depth, count)
    // so the recursion doesn't borrow `state` while we also mutate it.
    let rows = flatten_tree(state);
    for row in &rows {
        ui.horizontal(|ui| {
            ui.add_space(row.depth as f32 * 14.0);
            let selected = row.id == state.active_part;
            let label = format!("{} ({})", row.name, row.count);
            if ui.selectable_label(selected, label).clicked() {
                state.select_part(row.id);
            }
        });
    }

    let active = state.active_part;
    let is_root = active == state.tree.root();

    // Rename the active part. Keep the buffer synced to the current selection so
    // switching parts loads that part's name into the edit field.
    if let Some(name) = state.tree.get(active).map(|p| p.name.clone()) {
        if scratch.part_rename_for != Some(active.0) {
            scratch.part_rename_buf = name;
            scratch.part_rename_for = Some(active.0);
        }
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.add(
                egui::TextEdit::singleline(&mut scratch.part_rename_buf)
                    .desired_width(110.0),
            );
            let trimmed = scratch.part_rename_buf.trim().to_string();
            let changed = !trimmed.is_empty()
                && state.tree.get(active).map(|p| p.name.as_str()) != Some(trimmed.as_str());
            if ui.add_enabled(changed, egui::Button::new("Rename")).clicked() {
                *action = Some(PendingAction::RenamePart { id: active, name: trimmed });
            }
        });
    }

    // Pivot editor for the active part (the rig groundwork the Animator needs for
    // both characters and items). Snapshot the pivot + name first so the immutable
    // borrow of `state.tree` is released before the mutable write-back below.
    let part_info = state.tree.get(active).map(|p| (p.pivot, p.name.clone()));
    if let Some((mut pivot, name)) = part_info {
        ui.label(format!("Pivot of {name}:"));
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= ui.add(egui::DragValue::new(&mut pivot.x).speed(0.02).prefix("x ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut pivot.y).speed(0.02).prefix("y ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut pivot.z).speed(0.02).prefix("z ")).changed();
        });
        if changed {
            if let Some(p) = state.tree.get_mut(active) {
                p.pivot = pivot;
            }
            state.dirty = true; // pivot gizmo preview moves
        }
    }

    // Add a child part under the active part.
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut scratch.new_part_buf)
                .hint_text("new part name")
                .desired_width(110.0),
        );
        let can_add = !scratch.new_part_buf.trim().is_empty();
        if ui.add_enabled(can_add, egui::Button::new("Add child")).clicked() {
            *action = Some(PendingAction::AddPart {
                parent: active,
                name: scratch.new_part_buf.trim().to_string(),
            });
            scratch.new_part_buf.clear();
        }
    });

    // Re-parent the active part under another (rig editing). Offer every part
    // except the active one as a target; cycle/root moves are rejected by
    // `PartTree::reparent` when applied. The root has no parent to change.
    if !is_root {
        let current_parent = state.tree.get(active).and_then(|p| p.parent);
        let candidates: Vec<(PartId, String)> = state
            .tree
            .iter()
            .filter(|p| p.id != active)
            .map(|p| (p.id, p.name.clone()))
            .collect();
        let current_name = current_parent
            .and_then(|pid| state.tree.get(pid))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "—".to_string());
        ui.horizontal(|ui| {
            ui.label("Parent:");
            egui::ComboBox::from_id_salt("reparent")
                .selected_text(current_name)
                .show_ui(ui, |ui| {
                    for (pid, name) in candidates {
                        if ui
                            .selectable_label(current_parent == Some(pid), name)
                            .clicked()
                            && current_parent != Some(pid)
                        {
                            *action = Some(PendingAction::Reparent {
                                child: active,
                                new_parent: pid,
                            });
                        }
                    }
                });
        });
    }

    if ui
        .add_enabled(!is_root, egui::Button::new("Delete part"))
        .on_hover_text("Removes the part and its voxels; children re-parent up")
        .clicked()
    {
        *action = Some(PendingAction::DeletePart(active));
    }
}

/// Right panel: the project browser (load/rename/delete saved models). Returns
/// its screen rect.
fn right_panel(
    ctx: &egui::Context,
    scratch: &mut UiScratch,
    action: &mut Option<PendingAction>,
) -> egui::Rect {
    egui::SidePanel::right("project_panel")
        .resizable(true)
        .default_width(220.0)
        .show(ctx, |ui| {
            ui.heading("Projects");
            if ui.button("Refresh").clicked() {
                scratch.projects = io::list_projects();
            }
            ui.separator();

            if scratch.projects.is_empty() {
                ui.label("No saved models yet. Paint something and press Save.");
            }

            // Snapshot the list so the row closures don't borrow `scratch.projects`
            // while we also mutate `scratch.renaming`/`rename_buf`.
            let projects = scratch.projects.clone();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for entry in &projects {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.strong(&entry.name);
                            if !entry.has_rig {
                                ui.label(egui::RichText::new("(.vox only)").weak());
                            }
                        });
                        // Inline rename row when this entry is being renamed.
                        if scratch.renaming.as_deref() == Some(entry.name.as_str()) {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut scratch.rename_buf)
                                        .desired_width(120.0),
                                );
                                if ui.button("OK").clicked() {
                                    let to = scratch.rename_buf.trim().to_string();
                                    if !to.is_empty() && to != entry.name {
                                        *action = Some(PendingAction::Rename {
                                            from: entry.name.clone(),
                                            to,
                                        });
                                    }
                                    scratch.renaming = None;
                                }
                                if ui.button("Cancel").clicked() {
                                    scratch.renaming = None;
                                }
                            });
                        } else {
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(entry.has_rig, egui::Button::new("Load"))
                                    .clicked()
                                {
                                    *action = Some(PendingAction::Load(entry.name.clone()));
                                }
                                if ui.button("Rename").clicked() {
                                    scratch.renaming = Some(entry.name.clone());
                                    scratch.rename_buf = entry.name.clone();
                                }
                                if ui.button("Delete").clicked() {
                                    *action = Some(PendingAction::Delete(entry.name.clone()));
                                }
                            });
                        }
                    });
                }
            });
        })
        .response
        .rect
}

/// Bottom status bar: the last save/load message + a quick controls reminder.
/// Returns its screen rect.
fn bottom_bar(ctx: &egui::Context, state: &EditorState) -> egui::Rect {
    egui::TopBottomPanel::bottom("status_bar")
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&state.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        "LMB: tool  •  Shift+LMB: erase  •  RMB/MMB-drag: orbit  •  \
                         Shift+RMB/MMB-drag: pan  •  Scroll: zoom",
                    );
                });
            });
        })
        .response
        .rect
}

/// One flattened rig row for the parts list.
struct PartRow {
    id: PartId,
    name: String,
    depth: usize,
    count: usize,
}

/// Flatten the part tree depth-first into rows with indent depth + voxel counts.
fn flatten_tree(state: &EditorState) -> Vec<PartRow> {
    let mut rows = Vec::new();
    fn recurse(state: &EditorState, id: PartId, depth: usize, rows: &mut Vec<PartRow>) {
        if let Some(part) = state.tree.get(id) {
            rows.push(PartRow {
                id,
                name: part.name.clone(),
                depth,
                count: state.model.part_voxel_count(id),
            });
            // Collect child ids first (children() borrows the tree).
            let children: Vec<PartId> = state.tree.children(id).map(|c| c.id).collect();
            for child in children {
                recurse(state, child, depth + 1, rows);
            }
        }
    }
    recurse(state, state.tree.root(), 0, &mut rows);
    rows
}

/// Apply a deferred [`PendingAction`] after the draw pass.
fn apply_action(action: PendingAction, state: &mut EditorState, scratch: &mut UiScratch) {
    match action {
        PendingAction::Save => match io::save_project(
            &state.name,
            state.object_type,
            &state.model,
            &state.tree,
            &state.palette,
        ) {
            Ok(size) => {
                state.status = format!(
                    "Saved '{}'  ({}×{}×{} vox)",
                    state.name, size[0], size[1], size[2]
                );
                scratch.projects = io::list_projects();
            }
            Err(e) => state.status = format!("Save failed: {e}"),
        },
        PendingAction::New(obj) => {
            state.new_project(obj);
        }
        PendingAction::Load(name) => match io::load_project(&name) {
            Ok((model, tree, palette, obj)) => {
                let count = model.voxel_count();
                state.replace(name.clone(), obj, model, tree, palette);
                state.status = format!("Loaded '{name}'  ({count} vox)");
            }
            Err(e) => state.status = format!("Load failed: {e}"),
        },
        PendingAction::Rename { from, to } => match io::rename_project(&from, &to) {
            Ok(()) => {
                if state.name == from {
                    state.name = to.clone();
                }
                state.status = format!("Renamed '{from}' → '{to}'");
                scratch.projects = io::list_projects();
            }
            Err(e) => state.status = format!("Rename failed: {e}"),
        },
        PendingAction::Delete(name) => match io::delete_project(&name) {
            Ok(()) => {
                state.status = format!("Deleted '{name}'");
                scratch.projects = io::list_projects();
            }
            Err(e) => state.status = format!("Delete failed: {e}"),
        },
        PendingAction::AddPart { parent, name } => {
            let id = state.tree.add(name, parent);
            state.select_part(id);
            // Force the rename buffer to reload for the newly-selected part.
            scratch.part_rename_for = None;
            state.status = "Added part".to_string();
        }
        PendingAction::RenamePart { id, name } => {
            let renamed = if let Some(part) = state.tree.get_mut(id) {
                part.name = name;
                true
            } else {
                false
            };
            if renamed {
                // Reload the buffer so it reflects the new name next frame.
                scratch.part_rename_for = None;
                state.status = "Renamed part".to_string();
            }
        }
        PendingAction::Reparent { child, new_parent } => {
            if state.tree.reparent(child, new_parent) {
                state.dirty = true; // bone preview re-links
                state.status = "Re-parented part".to_string();
            } else {
                state.status = "Re-parent rejected (cycle or root)".to_string();
            }
        }
        PendingAction::DeletePart(id) => {
            for removed in state.tree.remove(id) {
                state.model.remove_part(removed);
            }
            state.active_part = state.tree.root();
            // The selection changed; reload the rename buffer for it next frame.
            scratch.part_rename_for = None;
            state.dirty = true;
            state.status = "Deleted part".to_string();
        }
        PendingAction::ClearGeometry => state.clear_geometry(),
    }
}
