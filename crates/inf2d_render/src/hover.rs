//! Cursor hover-highlight gizmo.
//!
//! Draws a pulsing yellow diamond outline on whichever tile the cursor is over
//! this frame. Uses Bevy's immediate-mode [`Gizmos`] (no shaders, no per-frame
//! entity churn) and runs in [`inf2d_core::RenderPrepSet`] so it reads the
//! already-finalized [`CursorPick`] for the current tick.
//!
//! The hovered tile's height is taken straight from
//! [`CursorPick::height`][inf2d_camera::CursorPick::height], which the camera's
//! picker already resolved against the streamed chunk data. When height is
//! missing (cursor over un-streamed chunks) we fall back to ground-plane `0`
//! rather than disappearing.

use bevy::gizmos::config::{DefaultGizmoConfigGroup, GizmoConfigStore};
use bevy::prelude::*;
use inf2d_camera::CursorPick;
use inf2d_core::{tile_to_world_with_height, TILE_HEIGHT, TILE_WIDTH};

/// Half-width / half-height of the diamond in world units. `tile_to_world_with_height`
/// returns the diamond's *center* for the 2:1 dimetric projection (the doc comment in
/// `inf2d_core::iso` says "left vertex" but the math is centered — verified against
/// the water shader quad which positions a centered rectangle at the same point).
const HALF_W: f32 = TILE_WIDTH * 0.5;
const HALF_H: f32 = TILE_HEIGHT * 0.5;

/// Inset distance for the inner outline — gives the highlight a "double-stroke" feel
/// that reads from farther zoom levels without going opaque.
const INSET: f32 = 4.0;

/// Pulse cycle frequency in radians per second. `4.0` gives a roughly 1.5 Hz
/// breathing rhythm — fast enough to feel "alive", slow enough to not strobe.
const PULSE_RATE: f32 = 4.0;

/// `Startup` system: configure the default 2D gizmo group so the hover diamond
/// is reliably visible above tile geometry.
///
/// In Bevy 0.18 `GizmoConfig::depth_bias` is documented as "effectively always
/// -1" in 2D, which *should* keep gizmos in front of sprites. Setting it
/// explicitly here is belt-and-suspenders: if a future Bevy release relaxes the
/// 2D clamp, we still ride the negative bias and stay drawn IN FRONT of tiles
/// whose Z layers sit at `0.0..=2.0` (`crate::layers::RenderLayer`).
///
/// Line width is pinned at 2.0 (matching the upstream default) so the outline
/// stays at least one pixel wide even when the camera zooms out far enough that
/// the diamond shrinks below ~16 px per side.
pub fn configure_hover_gizmo(mut config_store: ResMut<GizmoConfigStore>) {
    let (config, _) = config_store.config_mut::<DefaultGizmoConfigGroup>();
    config.depth_bias = -1.0;
    config.line.width = 2.0;
}

/// `RenderPrepSet` system: read [`CursorPick`] (which already resolved the
/// elevated tile + height) and draw a pulsing diamond outline at the hovered
/// tile.
///
/// Cheap: 8 gizmo line segments per frame (4 outer + 4 inset), no allocations.
pub fn draw_hover_highlight(
    time: Res<Time>,
    pick: Res<CursorPick>,
    mut gizmos: Gizmos,
    mut frame: Local<u32>,
) {
    // Diagnostic ping every 60 frames so a future "still no highlight" report
    // can be triaged from the log without re-instrumenting: if `pick.tile` is
    // `None` here, the problem is upstream in the camera picker; if it's
    // `Some` but nothing draws, the problem is gizmo configuration or camera
    // render layers.
    *frame = frame.wrapping_add(1);
    if *frame % 60 == 0 {
        tracing::debug!(
            target: "inf2d_render::hover",
            tile = ?pick.tile,
            height = ?pick.height,
            "hover pick state (every 60 frames)",
        );
    }

    let Some(tile) = pick.tile else {
        return;
    };

    // Height comes straight from the picker; treat a missing value as ground so
    // the highlight still draws when only the tile fell back to a ground-plane
    // resolve.
    let height = pick.height.unwrap_or(0);

    // `tile_to_world_with_height` returns the diamond's BOTTOM vertex (not center,
    // as the inf2d_core docstring incorrectly claimed). The other 3 vertices are
    // offset relative to that bottom anchor.
    let anchor = tile_to_world_with_height(tile, height);
    let bottom = anchor;
    let right = anchor + Vec2::new(HALF_W, HALF_H);
    let top = anchor + Vec2::new(0.0, TILE_HEIGHT);
    let left = anchor + Vec2::new(-HALF_W, HALF_H);

    // |sin| -> [0..1], scaled to give a noticeable pulse without ever fully
    // disappearing. Two stacked alpha amplitudes (outer brighter than inset)
    // so depth reads correctly at any zoom.
    let pulse = (time.elapsed_secs() * PULSE_RATE).sin().abs();
    let outer_alpha = 0.45 + 0.45 * pulse;
    let inner_alpha = 0.20 + 0.25 * pulse;

    let outer = Color::srgba(1.0, 0.95, 0.5, outer_alpha);
    let inner = Color::srgba(1.0, 0.95, 0.5, inner_alpha);

    gizmos.line_2d(top, right, outer);
    gizmos.line_2d(right, bottom, outer);
    gizmos.line_2d(bottom, left, outer);
    gizmos.line_2d(left, top, outer);

    // Inset diamond — shrink each vertex toward the diamond's geometric center.
    let geom_center = anchor + Vec2::new(0.0, HALF_H);
    let inset_lerp = |v: Vec2, factor: f32| -> Vec2 { geom_center + (v - geom_center) * factor };
    let shrink = 1.0 - INSET / HALF_H.min(HALF_W);
    let i_top = inset_lerp(top, shrink);
    let i_right = inset_lerp(right, shrink);
    let i_bottom = inset_lerp(bottom, shrink);
    let i_left = inset_lerp(left, shrink);

    gizmos.line_2d(i_top, i_right, inner);
    gizmos.line_2d(i_right, i_bottom, inner);
    gizmos.line_2d(i_bottom, i_left, inner);
    gizmos.line_2d(i_left, i_top, inner);
}
