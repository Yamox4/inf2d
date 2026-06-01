#![deny(unsafe_code)]
//! Iso-aware entity picking. Wraps Bevy 0.18's upstream `bevy::picking` plus the
//! sprite-bounds backend that ships with `bevy_sprite` so callers can ask
//! "what entity is the cursor over?" and "what entity was just clicked?".
//!
//! ## Architectural commitment: sprites are visual, [`IsoAnchor`] is logical
//!
//! Picking is hit-tested against each entity's **sprite bounds**, but the
//! caller should resolve the picked entity's [`crate::IsoAnchor`] to learn
//! which logical tile is occupied. A tall tree's sprite extends far upward in
//! screen space, yet the tree's `IsoAnchor` stays pinned to the tile under its
//! trunk; clicking on the foliage selects the tree entity, and reading
//! `IsoAnchor.world` then gives you the tile coordinate to (e.g.) chop the
//! tree, route pathfinding around it, or compute attack range.
//!
//! Future sprite-stacks (towers with a base sprite + roof sprite, units stood
//! on a parapet, etc.) work the same way: every visible sprite that belongs to
//! the same logical object propagates its picking event up the [`ChildOf`]
//! hierarchy to the parent entity, and that parent's [`IsoAnchor`] is the
//! single source of truth for the logical tile occupied. Drop shadows render
//! at [`IsoAnchor`], never at sprite center, for the same reason.
//!
//! ## Bevy 0.18 plumbing
//!
//! - [`bevy::picking::PickingPlugin`] + [`bevy::picking::InteractionPlugin`]
//!   provide the core hover map and event dispatch.
//! - `bevy_sprite::SpritePickingPlugin` (auto-added by [`bevy_sprite::SpritePlugin`]
//!   when the `bevy_picking` cargo feature is enabled) supplies the 2D sprite
//!   hit-test backend.
//! - We subscribe to `Pointer<Over>` / `Pointer<Out>` / `Pointer<Click>` via
//!   [`MessageReader`] rather than observers, because we want a single shared
//!   [`EntityPick`] resource that mirrors hover/click state for the whole app
//!   rather than per-entity callbacks.
//! - Gameplay code tags any entity that should be hit-testable with
//!   `bevy::picking::Pickable::default()`. For slice 3 only the player needs it;
//!   slice 4+ adds it to trees, props, doors, etc.

use bevy::picking::events::{Click, Out, Over, Pointer};
use bevy::picking::{InteractionPlugin, PickingPlugin};
use bevy::prelude::*;

/// Per-frame snapshot of which iso entity the cursor interacts with.
///
/// `hovered` is updated continuously from `Pointer<Over>` / `Pointer<Out>` and
/// reflects the current hit-tested entity (or `None` if the cursor is off any
/// pickable sprite). `clicked` is set on every `Pointer<Click>` and **stays
/// set** until cleared by the consumer — gameplay code reads it, acts on it,
/// then writes `pick.clicked = None` (or simply ignores the same `Entity` on
/// subsequent frames).
///
/// This resource sits alongside [`inf2d_camera::CursorPick`]; that one resolves
/// the cursor to a world position / tile / chunk, while this one resolves it
/// to an entity. Callers can mix both — e.g. "clicked entity is `None` so
/// route to `cursor_pick.tile` instead".
#[derive(Resource, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Resource)]
pub struct EntityPick {
    /// The entity the cursor is currently hovering over, if any.
    pub hovered: Option<Entity>,
    /// The entity that was most recently clicked. Stays populated until the
    /// consumer overwrites it; only cleared automatically by the consumer.
    pub clicked: Option<Entity>,
}

/// Plugin that wires Bevy's upstream picking infrastructure (gated by
/// `is_plugin_added` so adding it twice is a no-op) and the inf2d
/// [`EntityPick`] resource + drain system.
pub struct EntityPickingPlugin;

impl Plugin for EntityPickingPlugin {
    fn build(&self, app: &mut App) {
        // Core picking infra. `SpritePickingPlugin` is added automatically by
        // `bevy_sprite::SpritePlugin` when the `bevy_picking` cargo feature is
        // enabled on the bevy dependency, so we deliberately do *not* add it
        // here. Adding just `PickingPlugin` + `InteractionPlugin` (the
        // `PointerInputPlugin` is part of `DefaultPlugins` already) gives us
        // the message types we read below.
        if !app.is_plugin_added::<PickingPlugin>() {
            app.add_plugins(PickingPlugin);
        }
        if !app.is_plugin_added::<InteractionPlugin>() {
            app.add_plugins(InteractionPlugin);
        }

        app.init_resource::<EntityPick>()
            .register_type::<EntityPick>()
            .add_systems(
                Update,
                drain_picking_events.in_set(inf2d_core::RenderPrepSet),
            );
    }
}

/// Drain every `Pointer<Over>` / `Pointer<Out>` / `Pointer<Click>` message
/// emitted this frame and fold them into the shared [`EntityPick`] resource.
///
/// Ordering inside the system matters: we process `Out` before `Over` so that
/// a fast cursor movement that exits one entity and enters another in the same
/// frame ends with `hovered = Some(new_entity)` rather than `None`. `Click`
/// is processed last and always overwrites whatever was previously in
/// `pick.clicked` — consumers are responsible for taking the click via e.g.
/// `pick.clicked.take()` once handled.
fn drain_picking_events(
    mut over: MessageReader<Pointer<Over>>,
    mut out: MessageReader<Pointer<Out>>,
    mut click: MessageReader<Pointer<Click>>,
    mut pick: ResMut<EntityPick>,
) {
    for ev in out.read() {
        if pick.hovered == Some(ev.entity) {
            pick.hovered = None;
        }
    }
    for ev in over.read() {
        pick.hovered = Some(ev.entity);
    }
    for ev in click.read() {
        pick.clicked = Some(ev.entity);
    }
}
