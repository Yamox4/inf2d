use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};
use inf2d_core::AppState;
use inf2d_world::ChunkManager;

const MIN_CHUNKS_FOR_INGAME: usize = 9;

/// Fullscreen egui loading overlay shown while the world streams in.
///
/// Active only during [`AppState::Loading`]. Draws a centered panel with a
/// progress bar driven by [`ChunkManager::loaded_count`], and transitions to
/// [`AppState::InGame`] once `MIN_CHUNKS_FOR_INGAME` chunks are loaded.
pub struct LoadingScreenPlugin;

impl Plugin for LoadingScreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            EguiPrimaryContextPass,
            draw_loading_panel.run_if(in_state(AppState::Loading)),
        );
        app.add_systems(
            Update,
            transition_when_ready.run_if(in_state(AppState::Loading)),
        );
    }
}

fn draw_loading_panel(mut ctx: EguiContexts, manager: Res<ChunkManager>) {
    let Ok(ctx) = ctx.ctx_mut() else {
        return;
    };
    let progress = (manager.loaded_count() as f32 / MIN_CHUNKS_FOR_INGAME as f32)
        .clamp(0.0, 1.0);
    egui::Area::new("loading_screen".into())
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_min_width(420.0);
            egui::Frame::default()
                .fill(egui::Color32::from_rgba_premultiplied(8, 10, 14, 240))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 80, 100)))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(24))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("inf2d")
                                .font(egui::FontId::proportional(36.0))
                                .color(egui::Color32::from_rgb(255, 168, 64)),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Generating world…")
                                .font(egui::FontId::proportional(16.0))
                                .color(egui::Color32::from_rgb(220, 224, 230)),
                        );
                        ui.add_space(20.0);
                        let bar_height = 8.0;
                        let bar_width = 360.0;
                        let (rect, _) = ui.allocate_exact_size(
                            egui::Vec2::new(bar_width, bar_height),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(30, 35, 45));
                        let fill = egui::Rect::from_min_size(
                            rect.min,
                            egui::Vec2::new(bar_width * progress, bar_height),
                        );
                        painter.rect_filled(fill, 4.0, egui::Color32::from_rgb(255, 168, 64));
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "{} / {} chunks",
                                manager.loaded_count(),
                                MIN_CHUNKS_FOR_INGAME
                            ))
                            .font(egui::FontId::monospace(13.0))
                            .color(egui::Color32::from_rgb(160, 168, 182)),
                        );
                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new(
                                "Tip: middle-drag to pan. F to re-engage camera follow.",
                            )
                            .font(egui::FontId::proportional(12.0))
                            .color(egui::Color32::from_rgb(120, 128, 140))
                            .italics(),
                        );
                    });
                });
        });
}

fn transition_when_ready(
    manager: Res<ChunkManager>,
    mut next: ResMut<NextState<AppState>>,
) {
    if manager.loaded_count() >= MIN_CHUNKS_FOR_INGAME {
        next.set(AppState::InGame);
    }
}
