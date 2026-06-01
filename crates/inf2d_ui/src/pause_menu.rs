#![deny(unsafe_code)]
//! Esc pause menu + settings panel.
//!
//! When `Esc` is just-pressed and `GameState::Playing`, transitions to `Paused`
//! and shows a modal panel with Resume / Settings / Quit. Inside Settings, a
//! master-volume slider mutates `inf2d_audio::MasterVolumes`. Esc-while-paused
//! resumes.

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};
use inf2d_audio::MasterVolumes;
use inf2d_core::{AppState, GameState};
use inf2d_input::InputState;

/// Registers the pause-menu UI state resource, the Esc-toggle system, and the
/// egui draw system. The draw system is gated to
/// `AppState::InGame` + `GameState::Paused`; the toggle system is gated to
/// `AppState::InGame` so Esc has no effect during loading.
pub struct PauseMenuPlugin;

/// Transient UI state for the pause menu. `show_settings` flips the panel
/// between the root menu (Resume / Settings / Quit) and the settings sub-panel
/// (volume sliders + Back).
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct PauseUiState {
    /// `true` when the Settings sub-panel is visible, `false` for the root menu.
    pub show_settings: bool,
}

impl Plugin for PauseMenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PauseUiState>()
            .add_systems(
                Update,
                toggle_pause_on_esc.run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                EguiPrimaryContextPass,
                draw_pause_menu
                    .run_if(in_state(AppState::InGame))
                    .run_if(in_state(GameState::Paused)),
            );
    }
}

fn toggle_pause_on_esc(
    input: Res<InputState>,
    current: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut ui_state: ResMut<PauseUiState>,
) {
    if !input.toggle_pause {
        return;
    }
    match current.get() {
        GameState::Playing => next.set(GameState::Paused),
        GameState::Paused => {
            next.set(GameState::Playing);
            ui_state.show_settings = false;
        }
    }
}

fn draw_pause_menu(
    mut ctx: EguiContexts,
    mut ui_state: ResMut<PauseUiState>,
    mut volumes: ResMut<MasterVolumes>,
    mut next_state: ResMut<NextState<GameState>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(ctx) = ctx.ctx_mut() else {
        return;
    };

    // Dim background.
    egui::Area::new("pause_dim".into())
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Background)
        .show(ctx, |ui| {
            let screen = ui.ctx().screen_rect();
            ui.painter().rect_filled(
                screen,
                0.0,
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 140),
            );
        });

    egui::Area::new("pause_menu".into())
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::default()
                .fill(egui::Color32::from_rgba_premultiplied(14, 18, 26, 245))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 80, 100)))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(28))
                .show(ui, |ui| {
                    ui.set_min_width(320.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new(if ui_state.show_settings {
                                "Settings"
                            } else {
                                "Paused"
                            })
                            .font(egui::FontId::proportional(28.0))
                            .color(egui::Color32::from_rgb(255, 168, 64)),
                        );
                        ui.add_space(16.0);

                        if ui_state.show_settings {
                            ui.label("Master volume");
                            ui.add(egui::Slider::new(&mut volumes.master, 0.0..=1.0));
                            ui.label("SFX volume");
                            ui.add(egui::Slider::new(&mut volumes.sfx, 0.0..=1.0));
                            ui.label("Music volume");
                            ui.add(egui::Slider::new(&mut volumes.music, 0.0..=1.0));
                            ui.add_space(20.0);
                            if menu_button(ui, "Back") {
                                ui_state.show_settings = false;
                            }
                        } else {
                            if menu_button(ui, "Resume") {
                                next_state.set(GameState::Playing);
                            }
                            ui.add_space(8.0);
                            if menu_button(ui, "Settings") {
                                ui_state.show_settings = true;
                            }
                            ui.add_space(8.0);
                            if menu_button(ui, "Quit") {
                                exit.write(AppExit::Success);
                            }
                        }
                    });
                });
        });
}

fn menu_button(ui: &mut egui::Ui, label: &str) -> bool {
    let btn = egui::Button::new(
        egui::RichText::new(label)
            .font(egui::FontId::proportional(18.0))
            .color(egui::Color32::from_rgb(232, 236, 244)),
    )
    .min_size(egui::Vec2::new(220.0, 40.0))
    .fill(egui::Color32::from_rgb(35, 42, 56))
    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 80, 100)));
    ui.add(btn).clicked()
}
