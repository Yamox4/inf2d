//! Click-to-move: raycast the cursor into the voxel world, then A* over the
//! terrain surface to fill the player's [`MovePath`].
//!
//! A* runs on a [`bevy::tasks::AsyncComputeTaskPool`] worker so a click into the
//! void (which hits [`MAX_EXPANSIONS`]) doesn't stall the frame. Only one search
//! is in flight at a time; a fresh click supersedes any pending task.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::time::Instant;

use bevy::{
    prelude::*,
    tasks::{block_on, poll_once, AsyncComputeTaskPool, Task},
    window::PrimaryWindow,
};
use bevy_voxel_world::prelude::*;

use inf3d_camera::IsoCamera;
use inf3d_gameplay::{MovePath, Player};
use inf3d_world::MainWorld;
use inf3d_worldgen::Terrain;

/// Max |Δheight| (in voxels) allowed between adjacent cells when walking.
pub const MAX_STEP: i32 = 1;
/// Safety bound on A* expansion so a click into the void can't hang the worker.
pub const MAX_EXPANSIONS: usize = 20_000;

pub struct PathfindPlugin;

impl Plugin for PathfindPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PathRequest>()
            .add_message::<PathFound>()
            .init_resource::<ActivePathTask>()
            .init_resource::<PathTiming>()
            .add_systems(
                Update,
                (
                    handle_click,
                    dispatch_path_task,
                    poll_path_task,
                    consume_path_found,
                )
                    .chain(),
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
    query: Query<&Player>,
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

    let Ok(player) = query.single() else {
        return;
    };

    requests.write(PathRequest {
        start: player.cell,
        goal,
    });
}

/// Spawn an A* worker for the most recent [`PathRequest`] this frame, cancelling
/// any previously in-flight search by dropping its handle.
fn dispatch_path_task(
    mut requests: MessageReader<PathRequest>,
    terrain: Res<Terrain>,
    mut active: ResMut<ActivePathTask>,
) {
    // Coalesce: only the newest request in the queue matters — older ones are
    // about to be overwritten anyway. Drains the reader so nothing accumulates.
    let Some(req) = requests.read().last().copied() else {
        return;
    };

    // Cheap snapshot — `Terrain` is just noise parameters; no heap allocations.
    let terrain_snapshot: Terrain = terrain.clone();
    let pool = AsyncComputeTaskPool::get();
    let task = pool.spawn(async move {
        let started = Instant::now();
        let (cells, expansions) = astar(&terrain_snapshot, req.start, req.goal);
        let elapsed_ms = started.elapsed().as_secs_f32() * 1000.0;
        PathSearchResult {
            cells,
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
        // No route or expansion cap hit — nothing to send. Caller already sees
        // the timing update.
        trace!(
            "pathfinding: no path (expansions = {}, {:.2} ms)",
            result.expansions, result.elapsed_ms
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
    move_path.waypoints.clear();
    for wp in &latest.waypoints {
        move_path.waypoints.push_back(*wp);
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

/// 8-connected A* over the terrain surface grid.
///
/// Cells are `(x, z)` columns; heights come from [`SurfaceOracle::surface_y`].
/// An edge `a → b` is walkable iff `|surface_y(a) - surface_y(b)| <= MAX_STEP`.
/// Step cost is `1.0` orthogonal / `SQRT_2` diagonal plus a height penalty of
/// `0.5 * |Δheight|`. Returns `(cells, expansions)` where `cells` is the
/// inclusive `start..=goal` path, or `None` if `goal == start`, no path exists,
/// or the search exceeds [`MAX_EXPANSIONS`] pops. `expansions` is always the
/// number of nodes popped from the open set (useful for diagnostics).
fn astar<O: SurfaceOracle>(
    terrain: &O,
    start: IVec2,
    goal: IVec2,
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

    (None, expansions)
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
        let (path, expansions) = astar(&terrain, IVec2::ZERO, IVec2::ZERO);
        assert!(path.is_none());
        assert_eq!(expansions, 0);
    }

    #[test]
    fn astar_finds_straight_line_on_flat_land() {
        let terrain = FlatLand { height: 5 };
        let start = IVec2::new(0, 0);
        let goal = IVec2::new(4, 0);
        let (path, _expansions) = astar(&terrain, start, goal);
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
        let (path, _expansions) = astar(&terrain, start, goal);
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
        let (path, expansions) = astar(&terrain, IVec2::new(0, 0), IVec2::new(10, 10));
        assert!(path.is_none());
        // Island has no walkable neighbours, so the open set drains immediately
        // after popping `start`. Expansion budget must not be exhausted.
        assert!(expansions < MAX_EXPANSIONS);
    }

    #[test]
    fn astar_expansion_count_is_bounded() {
        // Open flat land + a goal far enough away that A* would expand many
        // cells if it ran to completion. We just need a reachable goal whose
        // search the heuristic guides quickly; bound is sanity, not stress.
        let terrain = FlatLand { height: 5 };
        let (_path, expansions) = astar(&terrain, IVec2::new(0, 0), IVec2::new(20, 20));
        assert!(
            expansions <= MAX_EXPANSIONS,
            "A* exceeded its expansion cap: {expansions}"
        );
    }
}
