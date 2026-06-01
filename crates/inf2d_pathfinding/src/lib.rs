#![deny(unsafe_code)]
//! A* pathfinding over the chunked tile world.
//!
//! Consumers fire a [`PathRequest`] message naming a requester entity, a start tile,
//! and a goal tile. The [`solve_path_requests`] system reads each pending request,
//! runs an 8-connected A* search across the world tile grid using octile distance
//! as the heuristic, and emits a [`PathFound`] message — either with the resolved
//! path or with an empty `path` vector indicating that no route was found (either
//! because the goal is unreachable or because the per-request iteration budget was
//! exhausted).
//!
//! ## Walkability
//!
//! A tile is walkable iff its [`inf2d_world::TileKind`] is non-solid (everything
//! except `Water` and `Stone`). Tiles in chunks that aren't currently loaded
//! optimistically count as walkable — the world streamer pages chunks in around
//! the camera, and we'd rather let the player walk into the fog-of-war than fail
//! the search the moment the destination scrolls off-screen.
//!
//! ## Height-aware edges
//!
//! A tile carries an `i8` elevation step ([`inf2d_world::Tile::height`]). An edge
//! between two adjacent tiles is traversable only if the absolute height
//! difference is at most [`MAX_STEP_HEIGHT`] — actors can step up or down a
//! single terrace but cannot climb cliffs. The check uses the FROM tile's height
//! as the reference point so the same path is rejected in either direction.
//!
//! ## Determinism
//!
//! Tie-breaking is handled via stable ordering on `(f_score, x, y)` instead of
//! Rust's default hasher — repeating the same request on the same map always
//! returns the same path.

use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use inf2d_core::{ChunkPos, SimulationSet, WorldTile};
use inf2d_world::{ChunkData, ChunkManager, TileKind};
use std::collections::BinaryHeap;

/// Maximum height difference (in elevation steps) a unit may traverse between two adjacent
/// tiles. `1` matches Tactics Ogre / FFT: you can step up one terrace, but anything taller
/// reads as a cliff and is impassable until ramps/stairs are added.
pub const MAX_STEP_HEIGHT: i8 = 1;

/// Maximum height difference (in elevation steps) bridged by a stair / ramp tile.
/// When either endpoint of a candidate edge is a [`TileKind::Stairs`] tile this
/// replaces [`MAX_STEP_HEIGHT`], so a stair landing can connect a 2-step drop
/// that would otherwise read as an impassable cliff.
pub const STAIR_STEP_HEIGHT: i8 = 2;

/// Plugin: registers [`PathRequest`] / [`PathFound`] messages and the
/// [`solve_path_requests`] system inside [`SimulationSet`].
pub struct PathfindingPlugin;

impl Plugin for PathfindingPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PathRequest>()
            .add_message::<PathFound>()
            .add_systems(Update, solve_path_requests.in_set(SimulationSet));
    }
}

/// "Please find me a path from `start` to `goal`." `requester` is the entity that
/// will receive the resulting [`PathFound`]; gameplay layers correlate by it. The
/// search bails out after `max_iterations` expanded nodes — pick something in the
/// low thousands (e.g. `5000`) to keep a stray click from hitching the frame.
#[derive(Message, Debug, Clone)]
pub struct PathRequest {
    /// Entity that requested the path. Carried straight through to the response.
    pub requester: Entity,
    /// Start tile (inclusive). Typically the requester's current tile.
    pub start: WorldTile,
    /// Goal tile (inclusive).
    pub goal: WorldTile,
    /// Safety budget on expanded nodes. If exceeded, an empty-path [`PathFound`]
    /// is emitted so the caller knows the search gave up.
    pub max_iterations: usize,
}

/// Resolved (or unresolved) path for a [`PathRequest`]. `path` is empty when no
/// route exists, when the goal is unwalkable, or when `max_iterations` was hit.
/// Otherwise it lists every tile from `start` to `goal` inclusive, in order, with
/// each waypoint annotated with the destination tile's elevation step. Gameplay
/// consumers can read the per-waypoint height directly instead of re-querying the
/// chunk data while walking.
#[derive(Message, Debug, Clone)]
pub struct PathFound {
    /// Same value as the originating [`PathRequest::requester`].
    pub requester: Entity,
    /// Echo of [`PathRequest::start`] so consumers can correlate without storing it.
    pub start: WorldTile,
    /// Echo of [`PathRequest::goal`].
    pub goal: WorldTile,
    /// Empty = no path. Otherwise: every `(tile, height)` from `start` to `goal`
    /// inclusive. `height` is the tile's elevation step (`0` if its chunk hadn't
    /// loaded at the time of pathing — match the optimistic walkability policy).
    pub path: Vec<(WorldTile, i32)>,
}

/// Optional marker on entities that participate in pathfinding queries.
/// Reserved for future use (diagonal preferences, terrain weighting); pathfinding
/// itself doesn't query it today.
#[derive(Component, Debug, Default)]
pub struct Walkable;

// --- A* internals -----------------------------------------------------------

/// 8-connected neighbour deltas plus their movement cost. Cardinals = 1.0,
/// diagonals = sqrt(2). Order is N, NE, E, SE, S, SW, W, NW — kept stable for
/// deterministic tie-breaking when two neighbours share an f-score.
const NEIGHBOURS: [(i32, i32, f32); 8] = [
    (0, 1, 1.0),
    (1, 1, std::f32::consts::SQRT_2),
    (1, 0, 1.0),
    (1, -1, std::f32::consts::SQRT_2),
    (0, -1, 1.0),
    (-1, -1, std::f32::consts::SQRT_2),
    (-1, 0, 1.0),
    (-1, 1, std::f32::consts::SQRT_2),
];

/// One entry in the A* open set. `Ord` is inverted so a `BinaryHeap` becomes a
/// min-heap on `f`; ties break on `(x, y)` for cross-platform-stable ordering.
#[derive(Debug, Clone, Copy)]
struct OpenNode {
    /// f = g + h, packaged as fixed-point for total ordering.
    f_key: u64,
    tile: WorldTile,
}

impl PartialEq for OpenNode {
    fn eq(&self, other: &Self) -> bool {
        self.f_key == other.f_key && self.tile == other.tile
    }
}
impl Eq for OpenNode {}

impl Ord for OpenNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Invert: smaller f_key should pop first.
        other
            .f_key
            .cmp(&self.f_key)
            .then_with(|| other.tile.x.cmp(&self.tile.x))
            .then_with(|| other.tile.y.cmp(&self.tile.y))
    }
}

impl PartialOrd for OpenNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Pack an `f32` cost into a `u64` that preserves the natural ordering of
/// non-negative finite floats. We only ever compare positive scores in A*, so the
/// straight bit-cast on `to_bits()` is monotone.
#[inline]
fn cost_key(cost: f32) -> u64 {
    // For non-negative finite f32, `to_bits()` is monotone in `cost`.
    cost.to_bits() as u64
}

/// Octile distance heuristic — admissible and consistent for 8-connected grids
/// with cardinal cost 1 and diagonal cost sqrt(2).
#[inline]
fn octile_distance(a: WorldTile, b: WorldTile) -> f32 {
    let dx = (a.x - b.x).abs() as f32;
    let dy = (a.y - b.y).abs() as f32;
    let (min, max) = if dx < dy { (dx, dy) } else { (dy, dx) };
    (std::f32::consts::SQRT_2 - 1.0) * min + max
}

/// Look up a tile in the loaded chunk map, returning `None` for any tile whose chunk hasn't
/// streamed in yet (or whose `ChunkData` has gone missing for some reason). Unloaded tiles
/// are handled by the calling oracle, which treats them optimistically.
fn lookup_tile(
    tile: WorldTile,
    manager: &ChunkManager,
    chunk_q: &Query<&ChunkData>,
) -> Option<inf2d_world::Tile> {
    let chunk_pos = ChunkPos::from_tile(tile);
    let entity = manager.get(chunk_pos)?;
    let data = chunk_q.get(entity).ok()?;
    Some(data.get(chunk_pos.local_of(tile)))
}

/// Test the destination tile's standalone walkability — non-solid surface kind. Used as a
/// pre-check on the goal tile before the search; the per-edge oracle layers the height-step
/// gate on top. Tiles in unloaded chunks optimistically count as walkable so the player can
/// path into the streaming halo.
fn is_walkable(
    tile: WorldTile,
    manager: &ChunkManager,
    chunk_q: &Query<&ChunkData>,
) -> bool {
    match lookup_tile(tile, manager, chunk_q) {
        Some(t) => !t.kind.is_solid(),
        None => true,
    }
}

/// Per-edge walkability: can a unit step from `from` to `to`? Solid destination kinds fail;
/// height differences larger than [`MAX_STEP_HEIGHT`] also fail (you can't climb a cliff).
/// Both endpoints in unloaded chunks pass optimistically; a half-loaded pair (one known,
/// one not) also passes — the destination's height becomes knowable on the next chunk
/// stream-in, and refusing now would block exploration into the fog.
fn is_edge_walkable(
    from: WorldTile,
    to: WorldTile,
    manager: &ChunkManager,
    chunk_q: &Query<&ChunkData>,
) -> bool {
    let to_tile = match lookup_tile(to, manager, chunk_q) {
        Some(t) => t,
        None => return true,
    };
    if to_tile.kind.is_solid() {
        return false;
    }
    let Some(from_tile) = lookup_tile(from, manager, chunk_q) else {
        return true;
    };
    let diff = (from_tile.height as i32 - to_tile.height as i32).abs();
    // Stairs bridge a 2-step drop on either side: walking up onto a stair
    // landing from the low side, or stepping off the landing onto the high
    // side. If either endpoint is a stair, allow the larger gap.
    let limit = if to_tile.kind == TileKind::Stairs || from_tile.kind == TileKind::Stairs {
        STAIR_STEP_HEIGHT as i32
    } else {
        MAX_STEP_HEIGHT as i32
    };
    diff <= limit
}

/// Run A* from `start` to `goal`, with the given walkability oracles and an iteration
/// ceiling. Returns `Some(path)` (start..=goal inclusive) on success, `None` if the search
/// exhausts the open set without reaching the goal, and `None` if `max_iterations` is hit.
/// The unit tests below exercise this function directly with closure oracles, keeping the
/// core algorithm `ChunkManager`-free.
///
/// `walkable_tile(tile)` answers "is this tile's surface non-solid?" — used for the goal
/// pre-check and the diagonal-squeeze corner test.
///
/// `walkable_edge(from, to)` answers "can a unit traverse this edge?" — used per neighbour
/// expansion. This is the hook through which height-step impassability flows.
fn astar<FTile, FEdge>(
    start: WorldTile,
    goal: WorldTile,
    max_iterations: usize,
    walkable_tile: FTile,
    walkable_edge: FEdge,
) -> Option<Vec<WorldTile>>
where
    FTile: Fn(WorldTile) -> bool,
    FEdge: Fn(WorldTile, WorldTile) -> bool,
{
    if start == goal {
        return Some(vec![start]);
    }
    if !walkable_tile(goal) {
        return None;
    }

    let mut open: BinaryHeap<OpenNode> = BinaryHeap::new();
    let mut came_from: HashMap<WorldTile, WorldTile> = HashMap::default();
    let mut g_score: HashMap<WorldTile, f32> = HashMap::default();
    let mut closed: HashSet<WorldTile> = HashSet::default();

    g_score.insert(start, 0.0);
    open.push(OpenNode {
        f_key: cost_key(octile_distance(start, goal)),
        tile: start,
    });

    let mut iters = 0usize;
    while let Some(node) = open.pop() {
        iters += 1;
        if iters > max_iterations {
            return None;
        }
        if node.tile == goal {
            return Some(reconstruct(&came_from, goal));
        }
        if !closed.insert(node.tile) {
            // Already expanded with a better f-score earlier.
            continue;
        }

        let current_g = *g_score.get(&node.tile).unwrap_or(&f32::INFINITY);

        for (dx, dy, step_cost) in NEIGHBOURS {
            let next = WorldTile::new(node.tile.x + dx, node.tile.y + dy);
            if closed.contains(&next) {
                continue;
            }
            // Don't allow squeezing diagonally between two solid corners. This
            // keeps the path from clipping into a wall pocket the player can't
            // physically walk through. Use the per-tile oracle here; height-gating
            // a diagonal squeeze through the per-edge oracle would forbid otherwise-
            // legal diagonals around a single corner step.
            if dx != 0 && dy != 0 {
                let side_a = WorldTile::new(node.tile.x + dx, node.tile.y);
                let side_b = WorldTile::new(node.tile.x, node.tile.y + dy);
                if !walkable_tile(side_a) || !walkable_tile(side_b) {
                    continue;
                }
            }
            if !walkable_edge(node.tile, next) {
                continue;
            }

            let tentative_g = current_g + step_cost;
            let prev_g = *g_score.get(&next).unwrap_or(&f32::INFINITY);
            if tentative_g < prev_g {
                came_from.insert(next, node.tile);
                g_score.insert(next, tentative_g);
                let f = tentative_g + octile_distance(next, goal);
                open.push(OpenNode { f_key: cost_key(f), tile: next });
            }
        }
    }
    None
}

/// Walk the `came_from` map backwards from `goal` and reverse to get a forward path.
fn reconstruct(came_from: &HashMap<WorldTile, WorldTile>, goal: WorldTile) -> Vec<WorldTile> {
    let mut path = vec![goal];
    let mut cursor = goal;
    while let Some(&prev) = came_from.get(&cursor) {
        path.push(prev);
        cursor = prev;
    }
    path.reverse();
    path
}

/// Drain the [`PathRequest`] queue, run A* for each one, and emit [`PathFound`].
/// Runs in [`SimulationSet`].
pub fn solve_path_requests(
    mut requests: MessageReader<PathRequest>,
    mut found: MessageWriter<PathFound>,
    manager: Res<ChunkManager>,
    chunk_q: Query<&ChunkData>,
) {
    for req in requests.read() {
        let tiles = astar(
            req.start,
            req.goal,
            req.max_iterations,
            |t| is_walkable(t, &manager, &chunk_q),
            |from, to| is_edge_walkable(from, to, &manager, &chunk_q),
        )
        .unwrap_or_default();

        // Annotate each waypoint with its tile's elevation step. Unloaded chunks
        // default to `0` — mirrors the optimistic walkability policy and gives the
        // walker a sensible vertical position until the chunk streams in and the
        // next path request fires with the real height.
        let path: Vec<(WorldTile, i32)> = tiles
            .into_iter()
            .map(|t| {
                let h = lookup_tile(t, &manager, &chunk_q)
                    .map(|tile| tile.height as i32)
                    .unwrap_or(0);
                (t, h)
            })
            .collect();

        found.write(PathFound {
            requester: req.requester,
            start: req.start,
            goal: req.goal,
            path,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-tile walkability oracle backed by a `HashSet` of blocked tiles. Everything
    /// else counts as open ground. Mirrors the "unloaded = walkable" policy of the live
    /// world but is fully deterministic.
    fn tile_oracle(blocked: &'static [(i32, i32)]) -> impl Fn(WorldTile) -> bool {
        move |t| !blocked.iter().any(|&b| b == (t.x, t.y))
    }

    /// Per-edge oracle that mirrors the per-tile oracle — only the destination is
    /// checked, no height gating. Construct one independently per test (we can't reuse
    /// the per-tile oracle directly because the `astar` signature moves it).
    fn edge_oracle(blocked: &'static [(i32, i32)]) -> impl Fn(WorldTile, WorldTile) -> bool {
        move |_from, to| !blocked.iter().any(|&b| b == (to.x, to.y))
    }

    #[test]
    fn straight_path_3_tiles() {
        let blocked: &'static [(i32, i32)] = &[];
        let path = astar(
            WorldTile::new(0, 0),
            WorldTile::new(2, 0),
            128,
            tile_oracle(blocked),
            edge_oracle(blocked),
        )
        .expect("path");
        assert_eq!(path.first().copied(), Some(WorldTile::new(0, 0)));
        assert_eq!(path.last().copied(), Some(WorldTile::new(2, 0)));
        // 8-connected diagonals collapse straight east into 3 tiles
        // (start, +1, goal). The exact intermediate count depends on
        // tie-breaks but length is bounded by Chebyshev + 1.
        assert!(path.len() <= 3);
        assert!(path.len() >= 2);
    }

    #[test]
    fn around_wall() {
        // Vertical wall blocking x = 1 for y in [-1, 1]. Start left, goal right.
        let blocked: &'static [(i32, i32)] = &[(1, -1), (1, 0), (1, 1)];
        let path = astar(
            WorldTile::new(0, 0),
            WorldTile::new(2, 0),
            1024,
            tile_oracle(blocked),
            edge_oracle(blocked),
        )
        .expect("should detour");
        assert_eq!(path.first().copied(), Some(WorldTile::new(0, 0)));
        assert_eq!(path.last().copied(), Some(WorldTile::new(2, 0)));
        // No tile on the path may be one of the wall tiles.
        for tile in &path {
            assert!(
                !(tile.x == 1 && (tile.y == -1 || tile.y == 0 || tile.y == 1)),
                "path crosses wall at {tile:?}"
            );
        }
    }

    #[test]
    fn no_path_returns_empty() {
        // Enclose the goal entirely in solid tiles.
        let blocked: &'static [(i32, i32)] = &[(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];
        let path = astar(
            WorldTile::new(-3, 0),
            WorldTile::new(0, 0),
            4096,
            tile_oracle(blocked),
            edge_oracle(blocked),
        );
        assert!(path.is_none(), "expected no path, got {path:?}");
    }

    #[test]
    fn max_iter_truncates() {
        // Tiny budget against a long open-grid search → forces an early bail-out.
        let blocked: &'static [(i32, i32)] = &[];
        let path = astar(
            WorldTile::new(0, 0),
            WorldTile::new(50, 50),
            4,
            tile_oracle(blocked),
            edge_oracle(blocked),
        );
        assert!(path.is_none(), "expected budget bail-out, got {path:?}");
    }

    #[test]
    fn cliff_blocks_path() {
        // Heights: all tiles walkable kind, but a "cliff" tile at (1, 0) sits 3
        // steps above the surrounding ground. Adjacent tiles to/from the cliff
        // must be rejected because |dh| > MAX_STEP_HEIGHT.
        let heights: std::collections::HashMap<(i32, i32), i8> = [((1, 0), 3i8)]
            .into_iter()
            .collect();
        let height_of = move |t: WorldTile| heights.get(&(t.x, t.y)).copied().unwrap_or(0);
        let walkable_tile = |_t: WorldTile| true;
        let walkable_edge = move |from: WorldTile, to: WorldTile| {
            (height_of(from) as i32 - height_of(to) as i32).abs() <= MAX_STEP_HEIGHT as i32
        };
        let path = astar(
            WorldTile::new(0, 0),
            WorldTile::new(2, 0),
            1024,
            walkable_tile,
            walkable_edge,
        )
        .expect("should detour around the cliff");
        for tile in &path {
            assert_ne!(
                (tile.x, tile.y),
                (1, 0),
                "path stepped onto cliff tile at {tile:?}"
            );
        }
    }

    #[test]
    fn single_step_up_is_allowed() {
        // A one-step terrace must be traversable.
        let heights: std::collections::HashMap<(i32, i32), i8> =
            [((1, 0), 1i8), ((2, 0), 1i8)].into_iter().collect();
        let height_of = move |t: WorldTile| heights.get(&(t.x, t.y)).copied().unwrap_or(0);
        let walkable_tile = |_t: WorldTile| true;
        let walkable_edge = move |from: WorldTile, to: WorldTile| {
            (height_of(from) as i32 - height_of(to) as i32).abs() <= MAX_STEP_HEIGHT as i32
        };
        let path = astar(
            WorldTile::new(0, 0),
            WorldTile::new(2, 0),
            128,
            walkable_tile,
            walkable_edge,
        )
        .expect("one-step terrace must be walkable");
        assert_eq!(path.first().copied(), Some(WorldTile::new(0, 0)));
        assert_eq!(path.last().copied(), Some(WorldTile::new(2, 0)));
    }

    #[test]
    fn octile_distance_is_zero_at_self() {
        assert_eq!(octile_distance(WorldTile::new(3, 4), WorldTile::new(3, 4)), 0.0);
    }

    #[test]
    fn stair_landing_bridges_two_step_drop() {
        // Layout along the x axis (y = 0):
        //   x=0: grass, h=0
        //   x=1: STAIRS, h=1   (the landing)
        //   x=2: grass, h=2
        //
        // Without the stair-aware rule the 0 ↔ 2 step direct edge would be
        // rejected and any path must detour. With it, the edges 0 ↔ 1 and
        // 1 ↔ 2 each pass because the landing widens the limit to 2.
        use std::collections::HashMap;
        let kinds: HashMap<(i32, i32), TileKind> =
            [((1i32, 0i32), TileKind::Stairs)].into_iter().collect();
        let heights: HashMap<(i32, i32), i8> = [
            ((0i32, 0i32), 0i8),
            ((1, 0), 1),
            ((2, 0), 2),
        ]
        .into_iter()
        .collect();
        let kind_of = move |t: WorldTile| kinds.get(&(t.x, t.y)).copied().unwrap_or(TileKind::Grass);
        let height_of = move |t: WorldTile| heights.get(&(t.x, t.y)).copied().unwrap_or(0);

        let walkable_tile = |_t: WorldTile| true;
        let walkable_edge = move |from: WorldTile, to: WorldTile| {
            let diff = (height_of(from) as i32 - height_of(to) as i32).abs();
            let limit = if kind_of(to) == TileKind::Stairs || kind_of(from) == TileKind::Stairs {
                STAIR_STEP_HEIGHT as i32
            } else {
                MAX_STEP_HEIGHT as i32
            };
            diff <= limit
        };

        let path = astar(
            WorldTile::new(0, 0),
            WorldTile::new(2, 0),
            128,
            walkable_tile,
            walkable_edge,
        )
        .expect("stair landing must let A* cross the 2-step gradient");
        // The path must include the landing at (1, 0) — that's the only
        // tile whose surrounding edges can both be walked.
        assert!(
            path.iter().any(|t| t.x == 1 && t.y == 0),
            "path must traverse the stair landing, got {path:?}"
        );
    }
}
