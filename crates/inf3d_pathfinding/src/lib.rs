//! Click-to-move: raycast the cursor into the voxel world, then A* over the
//! terrain surface to fill the player's [`MovePath`].

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_voxel_world::prelude::*;

use inf3d_camera::IsoCamera;
use inf3d_gameplay::{MovePath, Player};
use inf3d_world::MainWorld;
use inf3d_worldgen::Terrain;

/// Max |Δheight| (in voxels) allowed between adjacent cells when walking.
pub const MAX_STEP: i32 = 1;
/// Safety bound on A* expansion so a click into the void can't hang the frame.
pub const MAX_EXPANSIONS: usize = 20_000;

pub struct PathfindPlugin;

impl Plugin for PathfindPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, handle_click);
    }
}

/// Total-orderable wrapper around an A* f-score. `f32` is only `PartialOrd`, so
/// we implement `Ord`/`Eq` via [`f32::total_cmp`] to use it as a [`BinaryHeap`]
/// key (paired with [`Reverse`] for a min-heap).
#[derive(Clone, Copy, PartialEq, PartialOrd)]
struct OrderedF32(f32);

impl Eq for OrderedF32 {}

impl Ord for OrderedF32 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// On left click, raycast the cursor into the voxel world to find a target
/// column, then run A* from the player's current cell and write the resulting
/// route into the player's [`MovePath`].
fn handle_click(
    mouse: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    cam: Query<(&Camera, &GlobalTransform), With<IsoCamera>>,
    voxel_world: VoxelWorld<MainWorld>,
    terrain: Res<Terrain>,
    mut query: Query<(&Player, &mut MovePath)>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }

    let Ok(window) = window.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, cam_gtf)) = cam.single() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_gtf, cursor) else {
        return;
    };

    let Some(hit) = voxel_world.raycast(ray, &|(_p, _v)| true) else {
        return;
    };
    let goal = IVec2::new(hit.position.x.floor() as i32, hit.position.z.floor() as i32);

    let Ok((player, mut move_path)) = query.single_mut() else {
        return;
    };
    let start = player.cell;

    let Some(cells) = astar(&terrain, start, goal) else {
        return;
    };

    move_path.waypoints.clear();
    // Skip the start cell (index 0): the player already stands there.
    for cell in cells.into_iter().skip(1) {
        move_path
            .waypoints
            .push_back(terrain.stand_pos(cell.x, cell.y));
    }
}

/// The 8 neighbor offsets (4 orthogonal + 4 diagonal) on the `(x, z)` grid.
const NEIGHBORS: [IVec2; 8] = [
    IVec2::new(1, 0),
    IVec2::new(-1, 0),
    IVec2::new(0, 1),
    IVec2::new(0, -1),
    IVec2::new(1, 1),
    IVec2::new(1, -1),
    IVec2::new(-1, 1),
    IVec2::new(-1, -1),
];

/// Octile-distance heuristic between two grid cells. Admissible for 8-connected
/// movement with orthogonal cost 1 and diagonal cost `SQRT_2`.
fn octile(a: IVec2, b: IVec2) -> f32 {
    let dx = (a.x - b.x).abs() as f32;
    let dz = (a.y - b.y).abs() as f32;
    let (lo, hi) = if dx < dz { (dx, dz) } else { (dz, dx) };
    (std::f32::consts::SQRT_2 - 1.0) * lo + hi
}

/// 8-connected A* over the terrain surface grid.
///
/// Cells are `(x, z)` columns; heights come from [`Terrain::surface_y`]. An edge
/// `a → b` is walkable iff `|surface_y(a) - surface_y(b)| <= MAX_STEP`. Step cost
/// is `1.0` orthogonal / `SQRT_2` diagonal plus a height penalty of
/// `0.5 * |Δheight|`. Returns the cell path `start..=goal` (inclusive of both),
/// or `None` if `goal == start`, no path exists, or the search exceeds
/// [`MAX_EXPANSIONS`] pops.
fn astar(terrain: &Terrain, start: IVec2, goal: IVec2) -> Option<Vec<IVec2>> {
    if goal == start {
        return None;
    }

    // Heap key keeps the cell as plain `i32` components: glam's `IVec2` isn't
    // `Ord`, so it can't sit inside a `BinaryHeap` ordering tuple.
    let mut open: BinaryHeap<Reverse<(OrderedF32, i32, i32)>> = BinaryHeap::new();
    let mut came_from: HashMap<IVec2, IVec2> = HashMap::new();
    let mut g_score: HashMap<IVec2, f32> = HashMap::new();

    g_score.insert(start, 0.0);
    open.push(Reverse((OrderedF32(octile(start, goal)), start.x, start.y)));

    let mut expansions = 0usize;

    while let Some(Reverse((_, cx, cy))) = open.pop() {
        let current = IVec2::new(cx, cy);
        if current == goal {
            return Some(reconstruct(&came_from, current));
        }

        expansions += 1;
        if expansions >= MAX_EXPANSIONS {
            return None;
        }

        let current_g = *g_score.get(&current).unwrap_or(&f32::INFINITY);
        let h_current = terrain.surface_y(current.x, current.y);

        for offset in NEIGHBORS {
            let next = current + offset;
            // Water (seafloor flats below the water line) is not walkable.
            if !terrain.is_land(next.x, next.y) {
                continue;
            }
            let h_next = terrain.surface_y(next.x, next.y);
            let dh = (h_current - h_next).abs();
            if dh > MAX_STEP {
                continue;
            }

            let diagonal = offset.x != 0 && offset.y != 0;
            let base = if diagonal {
                std::f32::consts::SQRT_2
            } else {
                1.0
            };
            let tentative = current_g + base + 0.5 * dh as f32;

            if tentative < *g_score.get(&next).unwrap_or(&f32::INFINITY) {
                came_from.insert(next, current);
                g_score.insert(next, tentative);
                let f = tentative + octile(next, goal);
                open.push(Reverse((OrderedF32(f), next.x, next.y)));
            }
        }
    }

    None
}

/// Walk `came_from` back from `goal` to the start, returning the cell path in
/// forward order (`start` first, `goal` last).
fn reconstruct(came_from: &HashMap<IVec2, IVec2>, goal: IVec2) -> Vec<IVec2> {
    let mut path = vec![goal];
    let mut current = goal;
    while let Some(&prev) = came_from.get(&current) {
        path.push(prev);
        current = prev;
    }
    path.reverse();
    path
}
