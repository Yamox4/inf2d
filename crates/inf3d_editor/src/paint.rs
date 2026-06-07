//! Click-to-paint interaction: turn a cursor click into a sub-voxel edit.
//!
//! The cursor ray (from [`Camera::viewport_to_world`]) is marched through the
//! sub-voxel grid with a 3D-DDA (Amanatides & Woo) traversal:
//! - **Paint** places a sub-voxel in the empty cell *in front of* the first
//!   solid face the ray hits — or, if the ray hits nothing, on the reference
//!   platform where it crosses `y = 0` (so the first voxel of an empty model
//!   lands ON the platform, never midair). The new cell adopts the active
//!   color + part.
//! - **Erase** removes the first solid cell the ray hits.
//! - **Pick** eyedrops the first solid cell's color + part into the selection.
//!
//! Controls (mouse): **left** drives the active tool; **Shift+left** is a quick
//! erase regardless of tool (the right button is reserved for the orbit camera,
//! see [`crate::camera`]). Clicks are ignored while the pointer is over an egui
//! panel ([`PointerOverUi`]) so UI interaction never paints.
//!
//! ## Scheduling (why this runs in `PostUpdate`)
//!
//! bevy_egui's UI pass — which computes [`PointerOverUi`] — runs in `PostUpdate`
//! (`EguiPrimaryContextPass`, inside `EguiPostUpdateSet::EndPass`). The camera
//! writes its `Transform` in `Update`, and `TransformSystems::Propagate` updates
//! its `GlobalTransform` in `PostUpdate`. So this system runs in `PostUpdate`
//! **after both** of those: that way it reads a *same-frame* pointer-over-UI gate
//! and a *same-frame* camera pose, instead of one-frame-stale values that made
//! the ray miss after orbiting (the placement bug).

use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy::window::PrimaryWindow;
use bevy_egui::EguiPostUpdateSet;

use crate::camera::EditorCamera;
use crate::state::{EditorState, Tool};
use crate::volume::{Cell, VoxelModel};

/// Set each frame by the UI layer: `true` when the egui pointer is over a panel,
/// so the paint system can ignore clicks that belong to the UI.
#[derive(Resource, Default)]
pub struct PointerOverUi(pub bool);

/// Plugin: registers the pointer-gate resource and the click handler.
pub struct PaintPlugin;

impl Plugin for PaintPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PointerOverUi>().add_systems(
            PostUpdate,
            // Run after the egui pass (fresh `PointerOverUi`) and after transform
            // propagation (fresh camera `GlobalTransform`) — see module docs.
            // `EguiPostUpdateSet::EndPass` is the set the `EguiPrimaryContextPass`
            // schedule (where `PointerOverUi` is written) executes inside.
            handle_clicks
                .after(EguiPostUpdateSet::EndPass)
                .after(TransformSystems::Propagate),
        );
    }
}

/// Resolve a left click into a paint / erase / pick edit on [`EditorState`].
fn handle_clicks(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    over_ui: Res<PointerOverUi>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cam: Query<(&Camera, &GlobalTransform), With<EditorCamera>>,
    mut state: ResMut<EditorState>,
) {
    if over_ui.0 {
        return;
    }
    // Only the left button edits; the right/middle buttons drive the camera.
    if !buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, cam_transform)) = cam.single() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_transform, cursor) else {
        return;
    };

    // Shift+left is a quick erase regardless of the active tool (MagicaVoxel
    // convention now that the right button orbits); otherwise apply the tool.
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let tool = if shift { Tool::Erase } else { state.tool };

    let origin = ray.origin;
    let dir = ray.direction.as_vec3();
    let hit = raycast(&state.model, origin, dir);
    match tool {
        Tool::Paint => {
            // Prefer the empty neighbour of the face we hit; if the ray hit no
            // geometry, fall back to the platform cell at `y = 0` so the first
            // layer lands ON the reference platform, not in midair.
            let target = match hit {
                Some(h) => h.place_cell,
                None => floor_cell(&state.model, origin, dir),
            };
            if let Some(cell) = target {
                state.paint(cell);
            }
        }
        Tool::Erase => {
            if let Some(h) = hit {
                state.erase(h.solid_cell);
            }
        }
        Tool::Pick => {
            if let Some(h) = hit {
                state.pick(h.solid_cell);
            }
        }
    }
}

/// Result of a successful grid raycast: the first solid cell hit and the empty
/// cell adjacent to the face that was crossed (where a paint would go).
struct RayHit {
    /// The first solid sub-voxel the ray entered.
    solid_cell: Cell,
    /// The empty neighbor across the entered face — the paint target. `None`
    /// only if that neighbor lies outside the build volume.
    place_cell: Option<Cell>,
}

/// March a ray through the sub-voxel grid and return the first solid cell, using
/// a 3D-DDA. Coordinates are world units; the grid's sub-voxel size scales the
/// step. Bounded by a generous max step count so a ray that never hits anything
/// terminates.
fn raycast(model: &VoxelModel, origin: Vec3, dir: Vec3) -> Option<RayHit> {
    let size = model.sub_voxel_size();
    if dir.length_squared() < 1e-12 || size <= 0.0 {
        return None;
    }
    let dir = dir.normalize();

    // Work in cell units so the DDA is integer-clean: scale world space by
    // 1/size, then each cell is a unit cube. Uniform scaling preserves the ray
    // direction, so the normalized world direction also drives the cell-space
    // traversal (only the `t` magnitudes change, and we only compare them).
    let inv = 1.0 / size;
    let p = origin * inv;
    let extent = model.extent();

    // Current cell (floor of the scaled position) and step direction per axis.
    let mut cell = IVec3::new(p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32);
    let step = IVec3::new(sign(dir.x), sign(dir.y), sign(dir.z));

    // Distance (in cell units) to the next cell boundary on each axis, and the
    // distance between successive boundaries. `INFINITY` for a zero component so
    // that axis never triggers a step.
    let next_boundary = |pos: f32, d: f32, c: i32| -> f32 {
        if d > 0.0 {
            ((c + 1) as f32 - pos) / d
        } else if d < 0.0 {
            (c as f32 - pos) / d
        } else {
            f32::INFINITY
        }
    };
    let mut t_max = Vec3::new(
        next_boundary(p.x, dir.x, cell.x),
        next_boundary(p.y, dir.y, cell.y),
        next_boundary(p.z, dir.z, cell.z),
    );
    let t_delta = Vec3::new(
        if dir.x != 0.0 { (1.0 / dir.x).abs() } else { f32::INFINITY },
        if dir.y != 0.0 { (1.0 / dir.y).abs() } else { f32::INFINITY },
        if dir.z != 0.0 { (1.0 / dir.z).abs() } else { f32::INFINITY },
    );

    // The face we last crossed, as the step that brought us into the current cell
    // — used to compute the paint (empty-neighbor) cell as `cell - last_step`.
    let mut last_step = IVec3::ZERO;

    // Cap the march: at most every cell along the volume's diagonal, plus slack.
    let max_steps = (extent as i64 * 3 + 8) as i32;
    for _ in 0..max_steps {
        // Only test cells inside the build volume; before entering it, keep
        // stepping (the ray may start outside and cross in).
        if in_bounds(cell, extent) {
            let c = Cell::new(cell.x, cell.y, cell.z);
            if model.is_solid(c) {
                // The empty neighbour across the face we entered through. When the
                // ray *starts* inside a solid (last_step == ZERO) there is no
                // entered face, so there is no valid place target.
                let place = if last_step == IVec3::ZERO {
                    None
                } else {
                    let neighbor = cell - last_step;
                    in_bounds(neighbor, extent)
                        .then(|| Cell::new(neighbor.x, neighbor.y, neighbor.z))
                };
                return Some(RayHit {
                    solid_cell: c,
                    place_cell: place,
                });
            }
        } else if past_volume(cell, step, extent) {
            // The ray has gone beyond the volume on a receding axis; no hit.
            return None;
        }

        // Advance to the next cell along the smallest t_max axis.
        if t_max.x < t_max.y && t_max.x < t_max.z {
            cell.x += step.x;
            t_max.x += t_delta.x;
            last_step = IVec3::new(step.x, 0, 0);
        } else if t_max.y < t_max.z {
            cell.y += step.y;
            t_max.y += t_delta.y;
            last_step = IVec3::new(0, step.y, 0);
        } else {
            cell.z += step.z;
            t_max.z += t_delta.z;
            last_step = IVec3::new(0, 0, step.z);
        }
    }
    None
}

/// Where the ray crosses the reference-platform plane (`y = 0`), as the cell just
/// above it (`y = 0`, which rests directly on the platform) — the paint target
/// when the ray hits no geometry. The horizontal cell is clamped into the build
/// footprint so a near-edge click still lands on the platform instead of being
/// dropped. `None` only if the ray is parallel to the platform or points away
/// from it.
fn floor_cell(model: &VoxelModel, origin: Vec3, dir: Vec3) -> Option<Cell> {
    if dir.y.abs() < 1e-6 {
        return None;
    }
    let t = -origin.y / dir.y;
    if t < 0.0 {
        return None; // platform is behind the camera
    }
    let p = origin + dir * t;
    let size = model.sub_voxel_size();
    let max = model.extent() - 1;
    let cx = ((p.x / size).floor() as i32).clamp(0, max);
    let cz = ((p.z / size).floor() as i32).clamp(0, max);
    Some(Cell::new(cx, 0, cz))
}

/// Integer sign of a float as `-1`, `0`, or `1`.
fn sign(v: f32) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// `true` if `cell` is inside `[0, extent)` on every axis.
fn in_bounds(cell: IVec3, extent: i32) -> bool {
    (0..extent).contains(&cell.x)
        && (0..extent).contains(&cell.y)
        && (0..extent).contains(&cell.z)
}

/// `true` once the marching cell has receded past the volume on an axis whose
/// step moves away from `[0, extent)` — i.e. it can never re-enter, so the march
/// can stop early.
fn past_volume(cell: IVec3, step: IVec3, extent: i32) -> bool {
    (step.x > 0 && cell.x >= extent)
        || (step.x < 0 && cell.x < 0)
        || (step.y > 0 && cell.y >= extent)
        || (step.y < 0 && cell.y < 0)
        || (step.z > 0 && cell.z >= extent)
        || (step.z < 0 && cell.z < 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::PartId;
    use crate::volume::Voxel;

    #[test]
    fn floor_cell_lands_inside_footprint() {
        let model = VoxelModel::new(8, 1); // extent 8, sub-voxel 1/8
        // Aim straight down through the middle of the footprint.
        let origin = Vec3::new(0.5, 5.0, 0.5);
        let dir = Vec3::NEG_Y;
        let cell = floor_cell(&model, origin, dir).expect("floor cell");
        assert_eq!(cell.y, 0);
        // x=0.5 world / (1/8) = 4.0 → cell 4
        assert_eq!((cell.x, cell.z), (4, 4));
    }

    #[test]
    fn floor_cell_clamps_to_footprint_edge() {
        let model = VoxelModel::new(8, 1); // extent 8
        // Aim down just outside the +X edge; the cell clamps to the last column.
        let origin = Vec3::new(1.4, 5.0, 0.5);
        let cell = floor_cell(&model, origin, Vec3::NEG_Y).expect("clamped cell");
        assert_eq!((cell.x, cell.z, cell.y), (7, 4, 0));
    }

    #[test]
    fn raycast_hits_solid_and_offers_neighbor() {
        let mut model = VoxelModel::new(8, 1); // sub-voxel 1/8
        // Place a solid voxel at cell (4,4,4): world center (4.5/8 each).
        model.set(
            Cell::new(4, 4, 4),
            Voxel {
                color: 0,
                part: PartId(0),
            },
        );
        // Shoot a ray from -X straight along +X through that row.
        let s = model.sub_voxel_size();
        let origin = Vec3::new(-1.0, 4.5 * s, 4.5 * s);
        let hit = raycast(&model, origin, Vec3::X).expect("hit");
        assert_eq!(hit.solid_cell, Cell::new(4, 4, 4));
        // The paint neighbor is the empty cell just before it on -X.
        assert_eq!(hit.place_cell, Some(Cell::new(3, 4, 4)));
    }

    #[test]
    fn raycast_misses_empty_model() {
        let model = VoxelModel::new(8, 1);
        let origin = Vec3::new(-1.0, 0.5, 0.5);
        assert!(raycast(&model, origin, Vec3::X).is_none());
    }

    #[test]
    fn raycast_inside_solid_has_no_place_cell() {
        let mut model = VoxelModel::new(8, 1);
        model.set(
            Cell::new(4, 4, 4),
            Voxel {
                color: 0,
                part: PartId(0),
            },
        );
        // Origin sits inside the solid cell, aimed +X: the first solid is the
        // cell we start in, so there is no entered face → no place target.
        let s = model.sub_voxel_size();
        let origin = Vec3::new(4.5 * s, 4.5 * s, 4.5 * s);
        let hit = raycast(&model, origin, Vec3::X).expect("hit");
        assert_eq!(hit.solid_cell, Cell::new(4, 4, 4));
        assert_eq!(hit.place_cell, None);
    }
}
