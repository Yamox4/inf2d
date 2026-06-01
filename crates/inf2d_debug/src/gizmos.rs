use bevy::prelude::*;
use inf2d_core::{chunk_origin_world, tile_to_world, ChunkPos, WorldTile, CHUNK_SIZE, TILE_WIDTH};
use inf2d_input::InputState;
use inf2d_world::ChunkManager;

/// Whether chunk-border gizmos are currently drawn. Toggled by `F5`.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct ChunkGizmosEnabled(pub bool);

const GIZMO_COLOR: Color = Color::srgb(1.0, 0.4, 0.2);
const ORIGIN_CROSS_HALF: f32 = TILE_WIDTH * 0.25;

/// Flips [`ChunkGizmosEnabled`] on F5 (`InputState::toggle_chunk_gizmos`).
pub fn toggle_chunk_gizmos(mut state: ResMut<ChunkGizmosEnabled>, input: Res<InputState>) {
    if input.toggle_chunk_gizmos {
        state.0 = !state.0;
    }
}

/// Draws each loaded chunk's screen-space parallelogram outline and a small `+`
/// at its world-space origin while [`ChunkGizmosEnabled`] is on.
pub fn draw_chunk_gizmos(
    mut gizmos: Gizmos,
    state: Res<ChunkGizmosEnabled>,
    manager: Res<ChunkManager>,
) {
    if !state.0 {
        return;
    }

    let last = (CHUNK_SIZE as i32) - 1;
    for (pos, _entity) in manager.iter() {
        let ChunkPos { x: cx, y: cy } = pos;
        let base_x = cx * CHUNK_SIZE as i32;
        let base_y = cy * CHUNK_SIZE as i32;

        let top = tile_to_world(WorldTile::new(base_x, base_y));
        let right = tile_to_world(WorldTile::new(base_x + last, base_y));
        let left = tile_to_world(WorldTile::new(base_x, base_y + last));
        let bottom = tile_to_world(WorldTile::new(base_x + last, base_y + last));

        gizmos.line_2d(top, right, GIZMO_COLOR);
        gizmos.line_2d(right, bottom, GIZMO_COLOR);
        gizmos.line_2d(bottom, left, GIZMO_COLOR);
        gizmos.line_2d(left, top, GIZMO_COLOR);

        let origin = chunk_origin_world(pos);
        gizmos.line_2d(
            origin + Vec2::new(-ORIGIN_CROSS_HALF, 0.0),
            origin + Vec2::new(ORIGIN_CROSS_HALF, 0.0),
            GIZMO_COLOR,
        );
        gizmos.line_2d(
            origin + Vec2::new(0.0, -ORIGIN_CROSS_HALF),
            origin + Vec2::new(0.0, ORIGIN_CROSS_HALF),
            GIZMO_COLOR,
        );
    }
}
