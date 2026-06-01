use bevy::diagnostic::{
    DiagnosticsStore, EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin,
};
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use inf2d_camera::{CameraRig, CursorPick};
use inf2d_core::{world_to_tile, ChunkPos, LocalTilePos, CHUNK_SIZE};
use inf2d_gameplay::{Health, Player};
use inf2d_render::{TimeOfDay, BASE_COLOR};
use inf2d_world::{ChunkData, ChunkManager, TileKind};

const HUD_FONT_PX: f32 = 16.0;
const HUD_TITLE_PX: f32 = 18.0;
const HUD_BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(12, 14, 18, 220);
const HUD_BORDER: egui::Color32 = egui::Color32::from_rgba_premultiplied(70, 80, 100, 255);
const HUD_TEXT: egui::Color32 = egui::Color32::from_rgba_premultiplied(232, 236, 244, 255);
const HUD_DIM: egui::Color32 = egui::Color32::from_rgba_premultiplied(160, 168, 182, 255);
const HUD_ACCENT: egui::Color32 = egui::Color32::from_rgba_premultiplied(255, 168, 64, 255);

/// Draws the top-left HUD overlay: performance, camera, world, and cursor sections.
///
/// Scheduled in [`bevy_egui::EguiPrimaryContextPass`]. Safe to run before any chunk
/// or camera entity exists — missing data degrades gracefully.
pub fn hud_panel(
    mut ctx: EguiContexts,
    diagnostics: Res<DiagnosticsStore>,
    manager: Res<ChunkManager>,
    cursor: Res<CursorPick>,
    tod: Res<TimeOfDay>,
    camera_q: Query<&CameraRig>,
    chunk_q: Query<&ChunkData>,
    players: Query<&Player>,
    player_hp: Query<&Health, With<Player>>,
) {
    let Ok(ctx) = ctx.ctx_mut() else {
        return;
    };

    let panel = egui::Frame::default()
        .fill(HUD_BG)
        .stroke(egui::Stroke::new(1.0, HUD_BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .outer_margin(egui::Margin::ZERO);

    egui::Area::new("inf2d_hud".into())
        .anchor(egui::Align2::LEFT_TOP, [14.0, 14.0])
        .movable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            panel.show(ui, |ui| {
                ui.style_mut().override_font_id =
                    Some(egui::FontId::monospace(HUD_FONT_PX));
                ui.style_mut().visuals.override_text_color = Some(HUD_TEXT);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.set_min_width(280.0);

                performance_section(ui, &diagnostics);
                camera_section(ui, &camera_q);
                world_section(ui, &camera_q, &manager, &tod);
                cursor_section(ui, &cursor, &manager, &chunk_q);
                player_hp_bar(ui, &player_hp);
            });
        });

    egui::Area::new("inf2d_minimap".into())
        .anchor(egui::Align2::RIGHT_TOP, [-14.0, 14.0])
        .movable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            panel.show(ui, |ui| {
                ui.style_mut().override_font_id =
                    Some(egui::FontId::monospace(HUD_FONT_PX));
                ui.style_mut().visuals.override_text_color = Some(HUD_TEXT);
                ui.spacing_mut().item_spacing.y = 6.0;

                minimap_panel(ui, &manager, &chunk_q, &cursor, &players, &camera_q);
            });
        });
}

fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let frame = egui::Frame::default()
        .fill(egui::Color32::from_rgba_premultiplied(22, 26, 34, 200))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(45, 52, 66)))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 8));
    frame.show(ui, |ui| {
        ui.label(
            egui::RichText::new(title)
                .font(egui::FontId::proportional(HUD_TITLE_PX))
                .color(HUD_ACCENT)
                .strong(),
        );
        ui.add_space(2.0);
        body(ui)
    })
}

fn kv(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(HUD_DIM));
        ui.label(egui::RichText::new(value).color(HUD_TEXT).strong());
    });
}

fn performance_section(ui: &mut egui::Ui, diagnostics: &DiagnosticsStore) {
    section(ui, "Performance", |ui| {
        let fps = diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FPS)
            .and_then(|d| d.smoothed());
        kv(
            ui,
            "FPS     ",
            &fps.map(|v| format!("{v:>6.1}")).unwrap_or_else(|| "  ---".into()),
        );

        let frame = diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
            .and_then(|d| d.smoothed());
        kv(
            ui,
            "Frame   ",
            &frame
                .map(|ms| format!("{ms:>6.2} ms"))
                .unwrap_or_else(|| "    --- ms".into()),
        );

        let entities = diagnostics
            .get(&EntityCountDiagnosticsPlugin::ENTITY_COUNT)
            .and_then(|d| d.smoothed());
        kv(
            ui,
            "Entities",
            &entities
                .map(|n| format!("{n:>6.0}"))
                .unwrap_or_else(|| "  ---".into()),
        );
    });
}

fn camera_section(ui: &mut egui::Ui, camera_q: &Query<&CameraRig>) {
    section(ui, "Camera", |ui| {
        let Ok(rig) = camera_q.single() else {
            kv(ui, "Pos     ", "( ---, --- )");
            kv(ui, "Zoom    ", "  ---");
            return;
        };
        kv(
            ui,
            "Pos     ",
            &format!("({:>+8.1}, {:>+8.1})", rig.target.x, rig.target.y),
        );
        kv(ui, "Zoom    ", &format!("{:>5.2}x", rig.zoom));
    });
}

fn world_section(
    ui: &mut egui::Ui,
    camera_q: &Query<&CameraRig>,
    manager: &ChunkManager,
    tod: &TimeOfDay,
) {
    section(ui, "World", |ui| {
        kv(ui, "Chunks  ", &format!("{:>6}", manager.loaded_count()));
        let hours = tod.hours.rem_euclid(24.0);
        let h = hours.floor() as u32;
        let m = ((hours - h as f32) * 60.0).floor() as u32;
        let phase = match h {
            5..=6 => "dawn",
            7..=16 => "day",
            17..=18 => "dusk",
            _ => "night",
        };
        kv(ui, "Time    ", &format!("{h:02}:{m:02}  {phase}"));
        let Ok(rig) = camera_q.single() else {
            kv(ui, "Focus   ", "( ---, --- )");
            return;
        };
        let focus = ChunkPos::from_tile(world_to_tile(rig.target));
        kv(ui, "Focus   ", &format!("({:>+4}, {:>+4})", focus.x, focus.y));
    });
}

fn cursor_section(
    ui: &mut egui::Ui,
    cursor: &CursorPick,
    manager: &ChunkManager,
    chunk_q: &Query<&ChunkData>,
) {
    section(ui, "Cursor", |ui| {
        let Some(world) = cursor.world else {
            kv(ui, "Cursor  ", "---");
            return;
        };
        kv(
            ui,
            "World   ",
            &format!("({:>+7.1}, {:>+7.1})", world.x, world.y),
        );

        if let Some(chunk) = cursor.chunk {
            let state = if manager.is_loaded(chunk) {
                "loaded"
            } else {
                "  not "
            };
            kv(
                ui,
                "Chunk   ",
                &format!("({:>+4}, {:>+4})  [{state}]", chunk.x, chunk.y),
            );
        }

        if let Some(tile) = cursor.tile {
            kv(ui, "Tile    ", &format!("({:>+6}, {:>+6})", tile.x, tile.y));
            match cursor_biome(cursor, manager, chunk_q) {
                Some(kind) => kv(ui, "Biome   ", &format!("{kind:?}")),
                None => kv(ui, "Biome   ", "---"),
            }
        }
    });
}

fn cursor_biome(
    cursor: &CursorPick,
    manager: &ChunkManager,
    q: &Query<&ChunkData>,
) -> Option<TileKind> {
    let chunk = cursor.chunk?;
    let tile = cursor.tile?;
    let entity = manager.get(chunk)?;
    let data = q.get(entity).ok()?;
    let local = chunk.local_of(tile);
    Some(data.get(local).kind)
}

/// Width of the player's HP bar in egui pixels.
const HP_BAR_WIDTH: f32 = 200.0;
/// Height of the player's HP bar in egui pixels.
const HP_BAR_HEIGHT: f32 = 12.0;
/// Dark background fill behind the HP bar (empty HP color).
const HP_BAR_BG: egui::Color32 = egui::Color32::from_rgb(40, 24, 24);
/// Bright red fill for the current HP slice.
const HP_BAR_FILL: egui::Color32 = egui::Color32::from_rgb(200, 60, 60);

fn player_hp_bar(ui: &mut egui::Ui, players: &Query<&Health, With<Player>>) {
    section(ui, "Player", |ui| {
        let Ok(hp) = players.single() else {
            ui.label("HP: ---");
            return;
        };
        // Allocate a bar-sized rect from the layout; `Sense::hover` is
        // enough — the bar is purely informational.
        let (rect, _) = ui.allocate_exact_size(
            egui::Vec2::new(HP_BAR_WIDTH, HP_BAR_HEIGHT),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, HP_BAR_BG);
        let fill_rect = egui::Rect::from_min_size(
            rect.min,
            egui::Vec2::new(HP_BAR_WIDTH * hp.fraction(), HP_BAR_HEIGHT),
        );
        painter.rect_filled(fill_rect, 3.0, HP_BAR_FILL);
        ui.label(format!("{:.0} / {:.0}", hp.current, hp.max));
    });
}

/// Side length, in pixels, of the square minimap viewport.
const MINIMAP_SIZE_PX: f32 = 200.0;
/// Pixels per chunk cell drawn on the minimap.
const MINIMAP_CELL_PX: f32 = 8.0;
/// Fallback tint when a chunk's `ChunkData` is unavailable (entity not yet ready).
const MINIMAP_UNKNOWN: egui::Color32 = egui::Color32::from_rgb(60, 60, 60);
/// Player dot color at the minimap's center.
const MINIMAP_PLAYER: egui::Color32 = egui::Color32::from_rgb(245, 90, 60);
/// Background fill behind the chunk grid.
const MINIMAP_BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(8, 10, 14, 220);

fn minimap_panel(
    ui: &mut egui::Ui,
    manager: &ChunkManager,
    chunk_data_q: &Query<&ChunkData>,
    cursor: &CursorPick,
    players: &Query<&Player>,
    camera_q: &Query<&CameraRig>,
) {
    section(ui, "Map", |ui| {
        let size = egui::Vec2::splat(MINIMAP_SIZE_PX);
        let (rect, _response) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 4.0, MINIMAP_BG);

        // Prefer the player's current tile; fall back to the camera target so
        // the minimap stays useful even before the player entity exists.
        let player_chunk = if let Ok(player) = players.single() {
            ChunkPos::from_tile(player.current_tile)
        } else if let Ok(rig) = camera_q.single() {
            ChunkPos::from_tile(world_to_tile(rig.target))
        } else {
            return;
        };

        let center = rect.center();

        for (chunk_pos, entity) in manager.iter() {
            let dx = (chunk_pos.x - player_chunk.x) as f32;
            let dy = (chunk_pos.y - player_chunk.y) as f32;
            // Iso world Y grows northward; egui Y grows downward — invert.
            let on_map = egui::pos2(
                center.x + dx * MINIMAP_CELL_PX,
                center.y - dy * MINIMAP_CELL_PX,
            );
            if !rect.contains(on_map) {
                continue;
            }
            let square =
                egui::Rect::from_center_size(on_map, egui::Vec2::splat(MINIMAP_CELL_PX));
            let tint = chunk_tint(entity, chunk_data_q);
            painter.rect_filled(square, 0.0, tint);
        }

        // Player dot — always at the minimap's geometric center.
        painter.rect_filled(
            egui::Rect::from_center_size(center, egui::Vec2::splat(3.0)),
            0.0,
            MINIMAP_PLAYER,
        );

        // Cursor-focused chunk outline.
        if let Some(chunk) = cursor.chunk {
            let dx = (chunk.x - player_chunk.x) as f32;
            let dy = (chunk.y - player_chunk.y) as f32;
            let on_map = egui::pos2(
                center.x + dx * MINIMAP_CELL_PX,
                center.y - dy * MINIMAP_CELL_PX,
            );
            if rect.contains(on_map) {
                let outline = egui::Rect::from_center_size(
                    on_map,
                    egui::Vec2::splat(MINIMAP_CELL_PX),
                );
                painter.rect_stroke(
                    outline,
                    0.0,
                    egui::Stroke::new(1.0, egui::Color32::WHITE),
                    egui::StrokeKind::Outside,
                );
            }
        }
    });
}

fn chunk_tint(entity: Entity, chunk_data_q: &Query<&ChunkData>) -> egui::Color32 {
    let Ok(data) = chunk_data_q.get(entity) else {
        return MINIMAP_UNKNOWN;
    };
    let center = LocalTilePos::new(CHUNK_SIZE / 2, CHUNK_SIZE / 2);
    let kind = data.get(center).kind;
    let c = BASE_COLOR[kind as usize];
    egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3])
}
