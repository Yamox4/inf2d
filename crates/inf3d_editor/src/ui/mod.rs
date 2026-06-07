//! The egui UI layer: panel composition and the pointer-over-UI gate.
//!
//! bevy_egui 0.39 runs UI systems in the [`EguiPrimaryContextPass`] schedule and
//! hands the context out via the [`EguiContexts`] system param, whose
//! [`EguiContexts::ctx_mut`] returns a `Result` (verified against the 0.39
//! `simple` example). This module owns the single UI system; the actual widget
//! code lives in [`panels`]. After drawing, it records whether egui wants the
//! pointer into [`PointerOverUi`] so [`crate::paint`] ignores clicks on panels.

mod panels;

use bevy_egui::{EguiContexts, EguiPrimaryContextPass};
use bevy::prelude::*;

use crate::paint::PointerOverUi;
use crate::state::EditorState;

/// Plugin: registers the per-frame UI system in egui's primary-context pass.
pub struct EditorUiPlugin;

impl Plugin for EditorUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(EguiPrimaryContextPass, ui_system);
    }
}

/// The single UI system. Acquires the egui context (0.39's fallible
/// `ctx_mut()?`), draws every panel, and updates the pointer-over-UI gate.
fn ui_system(
    mut contexts: EguiContexts,
    mut state: ResMut<EditorState>,
    mut over_ui: ResMut<PointerOverUi>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    // `draw` returns whether the pointer is over a panel (computed from the
    // panels' own rects, since egui's `is_pointer_over_area` ignores the
    // Background-order layers panels live on). This gates out clicks on a
    // panel's empty background, not just its interactive widgets.
    over_ui.0 = panels::draw(ctx, &mut state);
    Ok(())
}
