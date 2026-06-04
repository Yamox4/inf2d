//! Shared visual language for the menus — a clean dark "AAA" theme. Pure value
//! helpers (colors + `Node`/`TextFont` builders) so the screen code stays terse
//! and consistent. No entity spawning here (that lives in `lib.rs`, which owns the
//! button macro), so nothing in this module has to name a child-spawner type.

use bevy::prelude::*;

// --- Palette -------------------------------------------------------------------

/// Full-screen dim over the live world behind the menu (a soft vignette).
pub const OVERLAY: Color = Color::srgba(0.03, 0.04, 0.06, 0.82);
/// The menu card the title + buttons sit on.
pub const PANEL: Color = Color::srgba(0.08, 0.09, 0.12, 0.96);
/// Idle button fill.
pub const BUTTON: Color = Color::srgb(0.16, 0.18, 0.23);
/// Button fill while hovered.
pub const BUTTON_HOVER: Color = Color::srgb(0.24, 0.27, 0.34);
/// Button fill while pressed.
pub const BUTTON_PRESS: Color = Color::srgb(0.30, 0.50, 0.42);
/// Accent fill for an *active*/selected toggle or preset.
pub const ACCENT: Color = Color::srgb(0.30, 0.66, 0.52);
/// Primary text.
pub const TEXT: Color = Color::srgb(0.92, 0.95, 0.98);
/// Secondary / hint text.
pub const TEXT_DIM: Color = Color::srgb(0.60, 0.65, 0.72);
/// Title text.
pub const TITLE: Color = Color::srgb(0.97, 0.99, 1.0);

// --- Layout builders -----------------------------------------------------------

/// Full-screen centered column used as a menu root (carries the dim overlay).
pub fn overlay_node() -> Node {
    Node {
        position_type: PositionType::Absolute,
        top: Val::Px(0.0),
        left: Val::Px(0.0),
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        flex_direction: FlexDirection::Column,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        row_gap: Val::Px(10.0),
        ..default()
    }
}

/// The card that holds a screen's title + controls.
pub fn panel_node() -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        padding: UiRect::axes(Val::Px(34.0), Val::Px(28.0)),
        row_gap: Val::Px(8.0),
        ..default()
    }
}

/// A standard full-width menu button box.
pub fn button_node() -> Node {
    Node {
        width: Val::Px(280.0),
        height: Val::Px(46.0),
        margin: UiRect::vertical(Val::Px(4.0)),
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    }
}

/// A narrower button for inline rows (e.g. the four preset chips).
pub fn chip_node() -> Node {
    Node {
        width: Val::Px(110.0),
        height: Val::Px(38.0),
        margin: UiRect::all(Val::Px(3.0)),
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    }
}

/// A horizontal row container (for chip groups).
pub fn row_node() -> Node {
    Node {
        flex_direction: FlexDirection::Row,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    }
}

pub fn text_font(size: f32) -> TextFont {
    TextFont {
        font_size: size,
        ..default()
    }
}
