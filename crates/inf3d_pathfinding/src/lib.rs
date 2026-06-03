//! Click-to-move: raycast the cursor into the voxel world, then A* over the
//! terrain surface to fill the player's [`MovePath`].
//!
//! A* runs on a [`bevy::tasks::AsyncComputeTaskPool`] worker so a click into the
//! void (which hits [`MAX_EXPANSIONS`]) doesn't stall the frame. Only one search
//! is in flight at a time; a fresh click supersedes any pending task.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::Instant;

use bevy::{
    prelude::*,
    tasks::{block_on, poll_once, AsyncComputeTaskPool, Task},
    window::PrimaryWindow,
};
use bevy_voxel_world::prelude::*;

use inf3d_camera::IsoCamera;
use inf3d_core::{BlockedCells, GameSet, PathTarget};
use inf3d_gameplay::{MovePath, Player};
use inf3d_world::MainWorld;
use inf3d_worldgen::Terrain;

/// Max |Δheight| (in voxels) allowed between adjacent cells when walking.
pub const MAX_STEP: i32 = 1;
/// Safety bound on A* expansion so a click into the void can't hang the worker.
/// The search runs off-thread on an [`AsyncComputeTaskPool`] worker, so this can
/// be generous: a far/blocked click that would otherwise fail silently at a low
/// cap gets enough budget to actually find (or rule out) a route.
pub const MAX_EXPANSIONS: usize = 60_000;
/// Max ring radius (in cells) the goal-snap spiral searches before giving up.
/// Bounds the cost of snapping a click on water/props to a reachable cell.
pub const MAX_GOAL_SNAP: i32 = 32;

pub struct PathfindPlugin;

impl Plugin for PathfindPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PathRequest>()
            .add_message::<PathFound>()
            .init_resource::<ActivePathTask>()
            .init_resource::<PathTiming>()
            // Click is raw input; the dispatch/poll/consume trio is logic. The
            // set tags place input ahead of all logic globally (Input runs before
            // Logic), and `.chain()` preserves the exact per-frame data flow:
            // read click → dispatch → poll → consume.
            .add_systems(Update, handle_click.in_set(GameSet::Input))
            .add_systems(
                Update,
                (dispatch_path_task, poll_path_task, consume_path_found)
                    .chain()
                    .in_set(GameSet::Logic),
            );
    }
}

/// Request an A* search from `start` to `goal` (both in voxel-column `(x, z)`
/// coordinates). Supersedes any in-flight search.
#[derive(Message, Clone, Copy, Debug)]
pub struct PathRequest {
    pub start: IVec2,
    pub goal: IVec2,
}

/// A path the worker found, as world-space standing waypoints (no start cell).
#[derive(Message, Clone, Debug)]
pub struct PathFound {
    pub waypoints: Vec<Vec3>,
}

/// Holds the currently-running A* worker task, if any. Dropping the handle
/// abandons the previous search's result (it never reaches `consume_path_found`).
#[derive(Resource, Default)]
pub struct ActivePathTask(Option<Task<PathSearchResult>>);

/// Last A* timing for diagnostics (read by the HUD or inspector). Written each
/// time a worker task finishes (whether it produced a path or hit the cap).
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct PathTiming {
    pub last_ms: f32,
    pub last_expansions: usize,
}

/// What a worker returns: the cell path (if any) and how many expansions it
/// burned. `cells` is the inclusive `start..=goal` route, or `None` when no
/// route exists or the expansion cap was hit.
#[derive(Clone)]
struct PathSearchResult {
    cells: Option<Vec<IVec2>>,
    /// The goal cell this search targeted; published to [`PathTarget`] on the
    /// main thread when the search yields a real route so the destination stays
    /// highlighted until the player arrives.
    goal: IVec2,
    expansions: usize,
    elapsed_ms: f32,
    /// Snapshot of the terrain the worker used; reused on the main thread to
    /// derive standing positions for the waypoints so we don't pay a second
    /// clone (and so positions stay deterministic with the search).
    terrain: Terrain,
}

/// On left click, raycast the cursor into the voxel world and queue an A*
/// search. Pathfinding itself runs on a worker (see [`dispatch_path_task`]).
fn handle_click(
    mouse: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    cam: Query<(&Camera, &GlobalTransform), With<IsoCamera>>,
    voxel_world: VoxelWorld<MainWorld>,
    query: Query<&Transform, With<Player>>,
    mut requests: MessageWriter<PathRequest>,
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

    let Ok(t) = query.single() else {
        return;
    };
    let start = IVec2::new(t.translation.x.floor() as i32, t.translation.z.floor() as i32);

    requests.write(PathRequest { start, goal });
}

/// Spawn an A* worker for the most recent [`PathRequest`] this frame, cancelling
/// any previously in-flight search by dropping its handle.
fn dispatch_path_task(
    mut requests: MessageReader<PathRequest>,
    terrain: Res<Terrain>,
    blocked: Res<BlockedCells>,
    mut active: ResMut<ActivePathTask>,
) {
    // Coalesce: only the newest request in the queue matters — older ones are
    // about to be overwritten anyway. Drains the reader so nothing accumulates.
    let Some(req) = requests.read().last().copied() else {
        return;
    };

    // Cheap snapshot — `Terrain` is just noise parameters; no heap allocations.
    let terrain_snapshot: Terrain = terrain.clone();
    // Snapshot the prop-blocked cells so the worker (which touches no ECS) can
    // treat them as impassable. The resident set is bounded by the foliage ring,
    // so this clone is small.
    let blocked_snapshot: HashSet<IVec2> = blocked.iter().collect();
    let pool = AsyncComputeTaskPool::get();
    let task = pool.spawn(async move {
        let started = Instant::now();
        // Snap the goal to the nearest walkable, unblocked cell so clicking a
        // tree/rock walks to a reachable spot beside it and clicking water walks
        // to the shore. If nothing suitable is in range, fall back to the raw
        // goal — A* will then report no path (existing behavior).
        let goal = snap_goal(&terrain_snapshot, req.goal, &blocked_snapshot).unwrap_or(req.goal);
        let (cells, expansions) = astar(&terrain_snapshot, req.start, goal, &blocked_snapshot);
        // String-pull the blocky 8-connected route into long straight diagonals
        // (Diablo feel) while keeping every dropped corner's clearance. No-op for
        // `None`/empty routes.
        let cells =
            cells.map(|route| smooth_path(&terrain_snapshot, &route, &blocked_snapshot));
        let elapsed_ms = started.elapsed().as_secs_f32() * 1000.0;
        PathSearchResult {
            cells,
            goal,
            expansions,
            elapsed_ms,
            terrain: terrain_snapshot,
        }
    });

    // Drops the prior `Task` (if any). The pool detaches the future; we just
    // stop caring about its result, which is exactly the "cancel" we want.
    active.0 = Some(task);
}

/// Poll the worker once per frame. When it completes, emit the path (if any)
/// as a [`PathFound`] message and refresh [`PathTiming`].
fn poll_path_task(
    mut active: ResMut<ActivePathTask>,
    mut path_found: MessageWriter<PathFound>,
    mut timing: ResMut<PathTiming>,
    mut target: ResMut<PathTarget>,
) {
    let Some(task) = active.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(poll_once(task)) else {
        return;
    };
    // Task finished — clear the slot before doing anything else.
    active.0 = None;

    *timing = PathTiming {
        last_ms: result.elapsed_ms,
        last_expansions: result.expansions,
    };

    let Some(cells) = result.cells else {
        // No route — truly unreachable or the expansion cap was hit. Leave
        // `PathTarget` unset (untouched) so no false destination marker appears,
        // and surface the failure explicitly: a click that goes nowhere should be
        // visible, not silent. `info!` (not `trace!`) so it shows by default; the
        // expansion count distinguishes "cap hit" (== MAX_EXPANSIONS) from "no
        // route" (< cap, open set drained). The timing update already landed.
        info!(
            "pathfinding: no path to goal {:?} (expansions = {}/{}, {:.2} ms)",
            result.goal, result.expansions, MAX_EXPANSIONS, result.elapsed_ms
        );
        return;
    };

    // Skip the start cell (index 0): the player already stands there.
    let waypoints: Vec<Vec3> = cells
        .into_iter()
        .skip(1)
        .map(|c| result.terrain.stand_pos(c.x, c.y))
        .collect();

    if waypoints.is_empty() {
        return;
    }

    // A real route exists: highlight the destination until the player arrives
    // (gameplay clears `PathTarget` once `MovePath` empties).
    target.0 = Some(result.goal);

    path_found.write(PathFound { waypoints });
}

/// Apply a finished search to the player's [`MovePath`]. Multiple paths in one
/// frame is unreachable (one task at a time), but handled defensively: only the
/// newest is taken.
fn consume_path_found(
    mut found: MessageReader<PathFound>,
    mut query: Query<&mut MovePath, With<Player>>,
) {
    let Some(latest) = found.read().last() else {
        return;
    };
    let Ok(mut move_path) = query.single_mut() else {
        return;
    };
    move_path.waypoints = latest.waypoints.iter().copied().collect();
}

/// Total-orderable wrapper around an A* f-score. `f32` is only `PartialOrd`, so
/// we implement `Ord`/`Eq` via [`f32::total_cmp`] to use it as a [`BinaryHeap`]
/// key (paired with [`Reverse`] for a min-heap).
#[derive(Clone, Copy, PartialEq)]
struct OrderedF32(f32);

impl Eq for OrderedF32 {}

impl Ord for OrderedF32 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl PartialOrd for OrderedF32 {
    // Defer to `Ord` (total_cmp) so `PartialOrd` and `Ord` never disagree —
    // the derived `f32` partial order is NaN-unsafe and would violate the
    // Ord/PartialOrd contract.
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
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

/// Abstraction the A* solver uses to query the world. Implemented for the
/// production [`Terrain`] oracle; tests can implement it on tiny in-memory
/// fixtures without needing the full noise generator.
trait SurfaceOracle {
    fn surface_y(&self, x: i32, z: i32) -> i32;
    fn is_land(&self, x: i32, z: i32) -> bool;
}

impl SurfaceOracle for Terrain {
    fn surface_y(&self, x: i32, z: i32) -> i32 {
        Terrain::surface_y(self, x, z)
    }
    fn is_land(&self, x: i32, z: i32) -> bool {
        Terrain::is_land(self, x, z)
    }
}

/// Snap a requested goal to the nearest cell that is BOTH walkable
/// (`is_land`) AND not in `blocked` (a cell the player capsule can actually
/// occupy). Returns `goal` unchanged when it is already suitable, otherwise the
/// nearest suitable cell found by a bounded outward ring search, or `None` if
/// none lies within [`MAX_GOAL_SNAP`] cells.
///
/// Mirrors [`Terrain::nearest_land`]: each radius `r` scans only the perimeter
/// of the radius-`r` square (O(perimeter), not O((2r+1)^2)) with `dx` ascending
/// then `dz` ascending, so equidistant ties resolve deterministically.
fn snap_goal<O: SurfaceOracle>(
    terrain: &O,
    goal: IVec2,
    blocked: &HashSet<IVec2>,
) -> Option<IVec2> {
    let suitable = |c: IVec2| terrain.is_land(c.x, c.y) && !blocked.contains(&c);
    if suitable(goal) {
        return Some(goal);
    }
    for r in 1..=MAX_GOAL_SNAP {
        for dx in -r..=r {
            if dx.abs() == r {
                // Left/right edge columns: every dz lies on the ring.
                for dz in -r..=r {
                    let c = IVec2::new(goal.x + dx, goal.y + dz);
                    if suitable(c) {
                        return Some(c);
                    }
                }
            } else {
                // Interior columns: only the top/bottom rows lie on the ring.
                for dz in [-r, r] {
                    let c = IVec2::new(goal.x + dx, goal.y + dz);
                    if suitable(c) {
                        return Some(c);
                    }
                }
            }
        }
    }
    None
}

/// 8-connected A* over the terrain surface grid.
///
/// Cells are `(x, z)` columns; heights come from [`SurfaceOracle::surface_y`].
/// An edge `a → b` is walkable iff `|surface_y(a) - surface_y(b)| <= MAX_STEP`
/// and `b` is not in `blocked` — where `blocked` is the set of cells the PLAYER
/// CAPSULE cannot occupy (prop footprints already inflated by the player
/// radius). The only escape hatch is `start`: a player standing on a prop cell
/// can always step off it. The `goal` gets no escape hatch — callers pass a goal
/// already guaranteed walkable+unblocked by [`snap_goal`].
///
/// DIAGONAL CORNER-CUT PREVENTION: a diagonal move is allowed only when BOTH
/// orthogonally-adjacent cells (`current + (dx,0)` and `current + (0,dz)`) are
/// themselves walkable, unblocked, and within [`MAX_STEP`] of `current` — so the
/// capsule never slips through a blocked/cliff corner between two props.
///
/// Step cost is `1.0` orthogonal / `SQRT_2` diagonal plus a height penalty of
/// `0.5 * |Δheight|`. Returns `(cells, expansions)` where `cells` is the
/// inclusive `start..=goal` path, or `None` if `goal == start`, no path exists,
/// or the search exceeds [`MAX_EXPANSIONS`] pops. `expansions` is always the
/// number of nodes popped from the open set (useful for diagnostics).
///
/// [`MAX_STEP`] is kept consistent with the physics `STEP_HEIGHT` so a route the
/// solver accepts is one the player capsule can actually climb.
fn astar<O: SurfaceOracle>(
    terrain: &O,
    start: IVec2,
    goal: IVec2,
    blocked: &HashSet<IVec2>,
) -> (Option<Vec<IVec2>>, usize) {
    if goal == start {
        return (None, 0);
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
            return (Some(reconstruct(&came_from, current)), expansions);
        }

        expansions += 1;
        if expansions >= MAX_EXPANSIONS {
            return (None, expansions);
        }

        let current_g = *g_score.get(&current).unwrap_or(&f32::INFINITY);
        let h_current = terrain.surface_y(current.x, current.y);

        // Whether `cell` is a surface the capsule may stand on, reachable from
        // `current` in a single step: walkable land, not capsule-blocked, and
        // within MAX_STEP of `current`'s height. The lone exception is `start`,
        // which is always passable so a player atop a prop can step off.
        let passable = |cell: IVec2| {
            if !terrain.is_land(cell.x, cell.y) {
                return false;
            }
            if blocked.contains(&cell) && cell != start {
                return false;
            }
            (h_current - terrain.surface_y(cell.x, cell.y)).abs() <= MAX_STEP
        };

        for offset in NEIGHBORS {
            let next = current + offset;
            // Water (seafloor flats below the water line) is not walkable.
            if !terrain.is_land(next.x, next.y) {
                continue;
            }
            // Cells the player capsule cannot occupy (inflated prop footprints)
            // are impassable, forcing the route to detour around props. Escape
            // hatch: `start` is always allowed so a player standing on a prop
            // cell is never trapped. The goal carries no escape hatch — it was
            // snapped to a walkable, unblocked cell before the search began.
            if blocked.contains(&next) && next != start {
                continue;
            }
            let h_next = terrain.surface_y(next.x, next.y);
            let dh = (h_current - h_next).abs();
            if dh > MAX_STEP {
                continue;
            }

            let diagonal = offset.x != 0 && offset.y != 0;
            // Corner-cut prevention: a diagonal step is only safe when both
            // orthogonally-adjacent cells are themselves passable, so the
            // capsule never clips a blocked/cliff corner between two props.
            if diagonal
                && (!passable(current + IVec2::new(offset.x, 0))
                    || !passable(current + IVec2::new(0, offset.y)))
            {
                continue;
            }
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

    (None, expansions)
}

/// String-pull a blocky 8-connected cell route into one with long straight
/// diagonals. Greedily drop intermediate cells: from an anchor, advance the
/// frontier as far as there is [`cell_line_of_sight`] from the anchor, then
/// commit the last visible cell as the next anchor. Start and goal are always
/// kept.
///
/// CLEARANCE: line-of-sight reuses the exact A* edge rules (`is_land`,
/// not-`blocked`, and within [`MAX_STEP`] height across each grid step), so a
/// smoothed shortcut never cuts a corner the capsule couldn't have walked. The
/// `start` escape hatch is honored so a player on a blocked prop cell isn't
/// pinned — only the anchor end may be the (possibly blocked) `start`.
///
/// Routes of `< 3` cells are already minimal and returned as-is.
fn smooth_path<O: SurfaceOracle>(
    terrain: &O,
    route: &[IVec2],
    blocked: &HashSet<IVec2>,
) -> Vec<IVec2> {
    if route.len() < 3 {
        return route.to_vec();
    }
    let start = route[0];

    let mut out = vec![start];
    let mut anchor = 0usize;
    while anchor < route.len() - 1 {
        // Furthest cell still visible in a straight line from the anchor. Seed
        // with `anchor + 1` so we always make progress even if the immediate
        // next step somehow fails the check (it can't, but stay defensive).
        let mut furthest = anchor + 1;
        for j in (anchor + 2)..route.len() {
            if cell_line_of_sight(terrain, route[anchor], route[j], start, blocked) {
                furthest = j;
            } else {
                // Grid LOS is not strictly monotone for arbitrary obstacles, but
                // stopping at the first occlusion keeps the route conservative
                // (never optimistically skips past a wall it can't see through).
                break;
            }
        }
        out.push(route[furthest]);
        anchor = furthest;
    }
    out
}

/// Whether the capsule can walk in a straight line from cell `a` to cell `b`,
/// using the same clearance rules A* applies to a single step. Walks the
/// supercover set of cells the segment `a → b` touches (a Bresenham variant that
/// includes BOTH cells when the line crosses a grid corner, so no blocked corner
/// is skipped) and checks each consecutive pair: every cell must be walkable
/// land and not `blocked`, and each transition must stay within [`MAX_STEP`]
/// height. `start` is exempt from the `blocked` test (the escape hatch).
fn cell_line_of_sight<O: SurfaceOracle>(
    terrain: &O,
    a: IVec2,
    b: IVec2,
    start: IVec2,
    blocked: &HashSet<IVec2>,
) -> bool {
    // A cell the capsule may stand on: walkable land and not blocked (except the
    // `start` escape hatch). Height continuity is checked between consecutive
    // cells below, mirroring A*'s per-edge MAX_STEP rule.
    let standable = |c: IVec2| {
        if !terrain.is_land(c.x, c.y) {
            return false;
        }
        if blocked.contains(&c) && c != start {
            return false;
        }
        true
    };

    let cells = supercover(a, b);
    // Endpoints must themselves be standable; A* already guarantees this for the
    // real route cells, but the supercover walk relies on it for the interior.
    let mut prev: Option<IVec2> = None;
    for c in cells {
        if !standable(c) {
            return false;
        }
        if let Some(p) = prev {
            let dh = (terrain.surface_y(p.x, p.y) - terrain.surface_y(c.x, c.y)).abs();
            if dh > MAX_STEP {
                return false;
            }
        }
        prev = Some(c);
    }
    true
}

/// Supercover line rasterization between two grid cells, inclusive of both
/// endpoints. Unlike plain Bresenham (which may "jump" a diagonal and skip the
/// two cells flanking a grid corner), this yields EVERY cell the continuous
/// segment passes through — so a wall corner can never hide between two emitted
/// cells. Steps one unit in x or z at a time (or both on an exact diagonal),
/// driven by the running error term.
fn supercover(a: IVec2, b: IVec2) -> Vec<IVec2> {
    let mut x = a.x;
    let mut z = a.y;
    let dx = (b.x - a.x).abs();
    let dz = (b.y - a.y).abs();
    let sx = if b.x > a.x { 1 } else { -1 };
    let sz = if b.y > a.y { 1 } else { -1 };

    let mut cells = vec![IVec2::new(x, z)];
    // `err` tracks dx - dz scaled by 2 so we never need fractions. When the line
    // passes exactly through a lattice corner (err == 0) we advance both axes,
    // emitting only the diagonal cell — there is no corner to slip through there.
    let mut err = dx - dz;
    while x != b.x || z != b.y {
        let e2 = 2 * err;
        if e2 > -dz && e2 < dz {
            // Straddling a corner exactly: single diagonal step.
            x += sx;
            z += sz;
            err += dx - dz;
        } else if e2 > -dz {
            err -= dz;
            x += sx;
        } else {
            // e2 < dz
            err += dx;
            z += sz;
        }
        cells.push(IVec2::new(x, z));
    }
    cells
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty blocked-cell set for tests that don't exercise prop avoidance.
    fn no_blocked() -> HashSet<IVec2> {
        HashSet::new()
    }

    /// Flat synthetic terrain: every column has the same surface height and is
    /// land. Useful for verifying A* on an open grid without the noise crate.
    struct FlatLand {
        height: i32,
    }

    impl SurfaceOracle for FlatLand {
        fn surface_y(&self, _x: i32, _z: i32) -> i32 {
            self.height
        }
        fn is_land(&self, _x: i32, _z: i32) -> bool {
            true
        }
    }

    /// Terrain with a finite vertical wall: column `x = wall_x` is unwalkable
    /// for `wall_z_min..=wall_z_max`, everything else is flat land. The wall
    /// has both ends open so A* can route around either side.
    struct WalledLand {
        wall_x: i32,
        wall_z_min: i32,
        wall_z_max: i32,
    }

    impl SurfaceOracle for WalledLand {
        fn surface_y(&self, _x: i32, _z: i32) -> i32 {
            5
        }
        fn is_land(&self, x: i32, z: i32) -> bool {
            !(x == self.wall_x && z >= self.wall_z_min && z <= self.wall_z_max)
        }
    }

    /// Flat low land with a one-column-thick raised RIDGE: every column is land,
    /// but the single column `x == ridge_x` (over `ridge_z_min..=ridge_z_max`)
    /// stands `MAX_STEP + 1` voxels higher than the surrounding low tier. Both
    /// seams into that column (from `ridge_x-1` and from `ridge_x+1`) exceed
    /// [`MAX_STEP`], so the ridge is an uncrossable barrier the route must skirt
    /// — exercising the `dh > MAX_STEP` clearance branch that the constant-height
    /// fixtures never touch, while keeping start/goal on the reachable low tier.
    /// Both z-ends of the ridge are open so A* can detour around either end.
    struct StepLand {
        ridge_x: i32,
        ridge_z_min: i32,
        ridge_z_max: i32,
        low: i32,
        high: i32,
    }

    impl SurfaceOracle for StepLand {
        fn surface_y(&self, x: i32, z: i32) -> i32 {
            if x == self.ridge_x && z >= self.ridge_z_min && z <= self.ridge_z_max {
                self.high
            } else {
                self.low
            }
        }
        fn is_land(&self, _x: i32, _z: i32) -> bool {
            true
        }
    }

    /// Tiny enclosed pocket: only `(0,0)` is land. Any goal is unreachable.
    struct Island;

    impl SurfaceOracle for Island {
        fn surface_y(&self, _x: i32, _z: i32) -> i32 {
            5
        }
        fn is_land(&self, x: i32, z: i32) -> bool {
            x == 0 && z == 0
        }
    }

    #[test]
    fn octile_is_zero_for_equal_cells() {
        let p = IVec2::new(3, -7);
        assert_eq!(octile(p, p), 0.0);
    }

    #[test]
    fn octile_orthogonal_matches_manhattan() {
        let h = octile(IVec2::new(0, 0), IVec2::new(4, 0));
        assert!((h - 4.0).abs() < 1e-6);
    }

    #[test]
    fn octile_diagonal_uses_sqrt2() {
        let h = octile(IVec2::new(0, 0), IVec2::new(3, 3));
        // Pure diagonal of length 3: cost = 3 * sqrt(2).
        let expected = 3.0 * std::f32::consts::SQRT_2;
        assert!((h - expected).abs() < 1e-6, "got {h}, want {expected}");
    }

    #[test]
    fn reconstruct_walks_chain_forward() {
        let mut came_from = HashMap::new();
        came_from.insert(IVec2::new(1, 0), IVec2::new(0, 0));
        came_from.insert(IVec2::new(2, 1), IVec2::new(1, 0));
        came_from.insert(IVec2::new(3, 2), IVec2::new(2, 1));
        let path = reconstruct(&came_from, IVec2::new(3, 2));
        assert_eq!(
            path,
            vec![
                IVec2::new(0, 0),
                IVec2::new(1, 0),
                IVec2::new(2, 1),
                IVec2::new(3, 2),
            ]
        );
    }

    #[test]
    fn astar_returns_none_when_start_equals_goal() {
        let terrain = FlatLand { height: 5 };
        let (path, expansions) = astar(&terrain, IVec2::ZERO, IVec2::ZERO, &no_blocked());
        assert!(path.is_none());
        assert_eq!(expansions, 0);
    }

    #[test]
    fn astar_finds_straight_line_on_flat_land() {
        let terrain = FlatLand { height: 5 };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(4, 0);
        let (path, _expansions) = astar(&terrain, start, goal, &no_blocked());
        let path = path.expect("flat land must always be reachable");
        // Inclusive of start and goal.
        assert_eq!(path.first().copied(), Some(start));
        assert_eq!(path.last().copied(), Some(goal));
        // Monotone in x (every step moves the player by exactly one in one of
        // the 8 directions; on a straight line the shortest is 4 steps + start).
        assert_eq!(path.len(), 5);
        for window in path.windows(2) {
            let d = window[1] - window[0];
            assert!(
                d.x.abs() <= 1 && d.y.abs() <= 1 && d != IVec2::ZERO,
                "non-adjacent step: {:?} -> {:?}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn astar_routes_around_a_wall() {
        // Wall at x=2 spans z ∈ [-2, 2]; both ends are open.
        let terrain = WalledLand {
            wall_x: 2,
            wall_z_min: -2,
            wall_z_max: 2,
        };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(4, 0);
        let (path, _expansions) = astar(&terrain, start, goal, &no_blocked());
        let path = path.expect("there is a route around the wall");
        // The path must never set foot on the walled cells.
        for cell in &path {
            assert!(
                terrain.is_land(cell.x, cell.y),
                "path stepped onto a walled cell: {:?}",
                path
            );
        }
        assert_eq!(path.first().copied(), Some(start));
        assert_eq!(path.last().copied(), Some(goal));
        // The path must detour: a straight 4-step run would clip the wall.
        // The shortest route around (over z=3 or z=-3) requires extra steps.
        assert!(path.len() > 5, "expected detour, got direct path: {:?}", path);
    }

    #[test]
    fn astar_gives_up_when_goal_is_unreachable() {
        let terrain = Island;
        let (path, expansions) =
            astar(&terrain, IVec2::new(0, 0), IVec2::new(10, 10), &no_blocked());
        assert!(path.is_none());
        // Island has no walkable neighbours, so the open set drains immediately
        // after popping `start`. Expansion budget must not be exhausted.
        assert!(expansions < MAX_EXPANSIONS);
    }

    #[test]
    fn astar_routes_around_blocked_prop_cells() {
        // Open flat land, but a vertical line of "prop" cells at x=2 for
        // z ∈ [-2, 2] is blocked — A* must detour around it just like a wall.
        let terrain = FlatLand { height: 5 };
        let mut blocked = HashSet::new();
        for z in -2..=2 {
            blocked.insert(IVec2::new(2, z));
        }
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(4, 0);
        let (path, _expansions) = astar(&terrain, start, goal, &blocked);
        let path = path.expect("there is a route around the blocked cells");
        for cell in &path {
            assert!(
                !blocked.contains(cell),
                "path stepped onto a blocked prop cell: {:?}",
                path
            );
        }
        assert_eq!(path.first().copied(), Some(start));
        assert_eq!(path.last().copied(), Some(goal));
        assert!(path.len() > 5, "expected detour, got direct path: {:?}", path);
    }

    #[test]
    fn astar_escape_hatch_allows_blocked_start() {
        // The player stands on a blocked prop cell. The start escape hatch must
        // still let them step off and reach an unblocked goal. (The goal carries
        // no escape hatch — callers snap it to an unblocked cell first.)
        let terrain = FlatLand { height: 5 };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(3, 0);
        let mut blocked = HashSet::new();
        blocked.insert(start);
        let (path, _expansions) = astar(&terrain, start, goal, &blocked);
        let path = path.expect("start escape hatch keeps the route valid");
        assert_eq!(path.first().copied(), Some(start));
        assert_eq!(path.last().copied(), Some(goal));
    }

    #[test]
    fn snap_goal_returns_goal_when_already_walkable() {
        // A goal on open, unblocked land needs no snapping.
        let terrain = FlatLand { height: 5 };
        let goal = IVec2::new(4, 2);
        assert_eq!(snap_goal(&terrain, goal, &no_blocked()), Some(goal));
    }

    #[test]
    fn snap_goal_snaps_blocked_goal_to_adjacent_reachable_cell() {
        // Clicking a single blocked prop cell on open land snaps to one of its
        // walkable, unblocked neighbours, and A* then reaches that snapped cell.
        let terrain = FlatLand { height: 5 };
        let goal = IVec2::new(4, 0);
        let mut blocked = HashSet::new();
        blocked.insert(goal);

        let snapped = snap_goal(&terrain, goal, &blocked).expect("a free neighbour exists");
        assert_ne!(snapped, goal, "goal was blocked, must snap elsewhere");
        assert!(!blocked.contains(&snapped) && terrain.is_land(snapped.x, snapped.y));
        // The snapped cell is immediately adjacent (first ring) to the click.
        let d = snapped - goal;
        assert!(d.x.abs() <= 1 && d.y.abs() <= 1 && d != IVec2::ZERO);

        let start = IVec2::new(0, 0);
        let (path, _expansions) = astar(&terrain, start, snapped, &blocked);
        let path = path.expect("route to the snapped goal exists");
        assert_eq!(path.first().copied(), Some(start));
        assert_eq!(path.last().copied(), Some(snapped));
        // The route must never set foot on the blocked prop cell.
        for cell in &path {
            assert!(!blocked.contains(cell), "path stepped onto a blocked cell: {path:?}");
        }
    }

    #[test]
    fn snap_goal_gives_up_outside_radius() {
        // Only the click cell itself is land, and it is blocked: no suitable
        // cell exists anywhere in range, so the snap fails.
        let terrain = Island; // only (0,0) is land
        let mut blocked = HashSet::new();
        blocked.insert(IVec2::ZERO);
        assert_eq!(snap_goal(&terrain, IVec2::ZERO, &blocked), None);
    }

    #[test]
    fn astar_no_diagonal_when_both_cardinals_blocked() {
        // Two props sit at the cardinal cells (1,0) and (0,1) flanking the
        // diagonal step start -> (1,1). Corner-cut prevention must forbid that
        // diagonal, forcing any route to (1,1) to go the long way around — which
        // here is impossible, so no path exists.
        let terrain = FlatLand { height: 5 };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(1, 1);
        let mut blocked = HashSet::new();
        blocked.insert(IVec2::new(1, 0));
        blocked.insert(IVec2::new(0, 1));
        // Wall off the only alternate approaches so the diagonal is the sole
        // candidate edge into the goal; with corner-cut prevention it is denied.
        blocked.insert(IVec2::new(2, 1));
        blocked.insert(IVec2::new(1, 2));
        let (path, _expansions) = astar(&terrain, start, goal, &blocked);
        assert!(
            path.is_none(),
            "diagonal corner-cut between two blocked cardinals must be denied: {path:?}"
        );
    }

    #[test]
    fn astar_allows_diagonal_when_cardinals_clear() {
        // With no blocked cells, the diagonal step start -> (1,1) is a legal
        // shortcut: A* takes the single diagonal rather than two orthogonals.
        let terrain = FlatLand { height: 5 };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(1, 1);
        let (path, _expansions) = astar(&terrain, start, goal, &no_blocked());
        let path = path.expect("open diagonal is reachable");
        assert_eq!(path, vec![start, goal]);
    }

    #[test]
    fn astar_expansion_count_is_bounded() {
        // Open flat land + a goal far enough away that A* would expand many
        // cells if it ran to completion. We just need a reachable goal whose
        // search the heuristic guides quickly; bound is sanity, not stress.
        let terrain = FlatLand { height: 5 };
        let (_path, expansions) =
            astar(&terrain, IVec2::new(0, 0), IVec2::new(20, 20), &no_blocked());
        assert!(
            expansions <= MAX_EXPANSIONS,
            "A* exceeded its expansion cap: {expansions}"
        );
    }

    #[test]
    fn max_expansions_is_raised_for_async_worker() {
        // M3: the cap lives off-thread, so it was raised from 20k to a safer
        // value. Pin the new floor so a regression that lowers it back into the
        // "fails far clicks silently" range is caught.
        assert!(
            MAX_EXPANSIONS >= 60_000,
            "expansion cap regressed below the async-worker floor: {MAX_EXPANSIONS}"
        );
    }

    #[test]
    fn supercover_straight_line_is_dense() {
        // A horizontal line touches every cell between the endpoints, inclusive.
        let cells = supercover(IVec2::new(0, 0), IVec2::new(3, 0));
        assert_eq!(
            cells,
            vec![
                IVec2::new(0, 0),
                IVec2::new(1, 0),
                IVec2::new(2, 0),
                IVec2::new(3, 0),
            ]
        );
    }

    #[test]
    fn supercover_diagonal_walks_corners() {
        // A pure diagonal collapses to the diagonal cells (the segment passes
        // exactly through each lattice corner — no orthogonal cell to slip past).
        let cells = supercover(IVec2::new(0, 0), IVec2::new(2, 2));
        assert_eq!(
            cells,
            vec![IVec2::new(0, 0), IVec2::new(1, 1), IVec2::new(2, 2)]
        );
        // Consecutive cells are always grid-adjacent (the LOS check relies on it).
        for w in cells.windows(2) {
            let d = w[1] - w[0];
            assert!(d.x.abs() <= 1 && d.y.abs() <= 1 && d != IVec2::ZERO);
        }
    }

    #[test]
    fn line_of_sight_is_clear_on_open_land() {
        let terrain = FlatLand { height: 5 };
        assert!(cell_line_of_sight(
            &terrain,
            IVec2::new(0, 0),
            IVec2::new(5, 3),
            IVec2::new(0, 0),
            &no_blocked(),
        ));
    }

    #[test]
    fn line_of_sight_is_blocked_through_a_wall() {
        // A wall column at x=2 sits squarely between the endpoints, so the
        // straight line crosses an unwalkable cell — LOS must fail.
        let terrain = WalledLand {
            wall_x: 2,
            wall_z_min: -2,
            wall_z_max: 2,
        };
        assert!(!cell_line_of_sight(
            &terrain,
            IVec2::new(0, 0),
            IVec2::new(4, 0),
            IVec2::new(0, 0),
            &no_blocked(),
        ));
    }

    #[test]
    fn line_of_sight_is_blocked_through_a_height_cliff() {
        // A height cliff at x=2 (the high tier is MAX_STEP+1 above the low tier)
        // sits between the endpoints. Both cells are land, so this exercises the
        // `dh > MAX_STEP` clearance branch — not the is_land wall path — and LOS
        // must fail because the straight line crosses an unclimbable seam.
        let terrain = StepLand {
            ridge_x: 2,
            ridge_z_min: -2,
            ridge_z_max: 2,
            low: 5,
            high: 5 + MAX_STEP + 1,
        };
        // Sanity: the seam really does exceed MAX_STEP.
        let dh = (terrain.surface_y(1, 0) - terrain.surface_y(2, 0)).abs();
        assert!(dh > MAX_STEP, "fixture seam must exceed MAX_STEP");
        assert!(!cell_line_of_sight(
            &terrain,
            IVec2::new(0, 0),
            IVec2::new(4, 0),
            IVec2::new(0, 0),
            &no_blocked(),
        ));
    }

    #[test]
    fn smooth_path_keeps_bend_around_a_height_cliff() {
        // Mirrors `smooth_path_keeps_bend_around_a_wall`, but the obstacle is a
        // HEIGHT cliff (all land) rather than a water/unwalkable wall. The detour
        // A* finds around the cliff's z-extent must survive smoothing: the result
        // keeps a genuine bend and no smoothed hop's straight line crosses the
        // unclimbable seam. This pins the cliff-clearance path in cell_line_of_sight
        // that the constant-height fixtures leave untested.
        let terrain = StepLand {
            ridge_x: 2,
            ridge_z_min: -2,
            ridge_z_max: 2,
            low: 5,
            high: 5 + MAX_STEP + 1,
        };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(4, 0);
        let (route, _) = astar(&terrain, start, goal, &no_blocked());
        let route = route.expect("a route around the cliff exists");

        let smoothed = smooth_path(&terrain, &route, &no_blocked());

        // Endpoints preserved.
        assert_eq!(smoothed.first().copied(), Some(start));
        assert_eq!(smoothed.last().copied(), Some(goal));
        // A real bend remains — smoothing did NOT string a line through the cliff.
        assert!(
            smoothed.len() >= 3,
            "smoothing flattened the necessary cliff detour: {smoothed:?}"
        );
        // Every kept hop must itself be a clear straight line: no segment may
        // cross the unclimbable seam.
        for hop in smoothed.windows(2) {
            assert!(
                cell_line_of_sight(&terrain, hop[0], hop[1], start, &no_blocked()),
                "smoothed hop {:?} -> {:?} crosses the height cliff",
                hop[0],
                hop[1]
            );
        }
    }

    #[test]
    fn smooth_path_collapses_straight_open_field() {
        // A straight A* run across open flat land must string-pull down to just
        // its endpoints (~2 points): every interior cell has LOS to the goal.
        let terrain = FlatLand { height: 5 };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(6, 0);
        let (route, _) = astar(&terrain, start, goal, &no_blocked());
        let route = route.expect("flat land is reachable");
        let smoothed = smooth_path(&terrain, &route, &no_blocked());
        assert_eq!(
            smoothed,
            vec![start, goal],
            "open-field route should collapse to start + goal, got {smoothed:?}"
        );
    }

    #[test]
    fn smooth_path_collapses_straight_open_diagonal() {
        // A pure diagonal run likewise collapses to its endpoints.
        let terrain = FlatLand { height: 5 };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(5, 5);
        let (route, _) = astar(&terrain, start, goal, &no_blocked());
        let route = route.expect("flat land is reachable");
        let smoothed = smooth_path(&terrain, &route, &no_blocked());
        assert_eq!(smoothed, vec![start, goal]);
    }

    #[test]
    fn smooth_path_keeps_bend_around_a_wall() {
        // The detour around a wall must survive smoothing: the result keeps a
        // genuine bend (more than 2 points) and never has a segment whose
        // straight line crosses the wall.
        let terrain = WalledLand {
            wall_x: 2,
            wall_z_min: -2,
            wall_z_max: 2,
        };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(4, 0);
        let (route, _) = astar(&terrain, start, goal, &no_blocked());
        let route = route.expect("a route around the wall exists");
        let smoothed = smooth_path(&terrain, &route, &no_blocked());

        // Endpoints preserved.
        assert_eq!(smoothed.first().copied(), Some(start));
        assert_eq!(smoothed.last().copied(), Some(goal));
        // A real bend remains — it did NOT smooth straight through the wall.
        assert!(
            smoothed.len() >= 3,
            "smoothing flattened the necessary detour: {smoothed:?}"
        );
        // No smoothed segment may have line-of-sight crossing the wall: every
        // kept hop must itself be a clear straight line over walkable cells.
        for hop in smoothed.windows(2) {
            assert!(
                cell_line_of_sight(&terrain, hop[0], hop[1], start, &no_blocked()),
                "smoothed hop {:?} -> {:?} crosses the wall",
                hop[0],
                hop[1]
            );
        }
    }

    #[test]
    fn smooth_path_preserves_short_routes() {
        // Routes shorter than 3 cells are already minimal and returned verbatim.
        let terrain = FlatLand { height: 5 };
        let two = vec![IVec2::new(0, 0), IVec2::new(1, 0)];
        assert_eq!(smooth_path(&terrain, &two, &no_blocked()), two);
        let one = vec![IVec2::new(0, 0)];
        assert_eq!(smooth_path(&terrain, &one, &no_blocked()), one);
    }
}
