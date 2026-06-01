use bevy::diagnostic::{
    Diagnostic, DiagnosticPath, Diagnostics, DiagnosticsStore, EntityCountDiagnosticsPlugin,
    FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin, RegisterDiagnostic,
};
use bevy::prelude::*;
use bevy_voxel_world::prelude::Chunk;

use inf3d_world::MainWorld;

/// Live-monitor metrics, logged each second by `LogDiagnosticsPlugin` alongside
/// the built-in FPS / frame-time / entity-count diagnostics.
const DIAG_CHUNKS: DiagnosticPath = DiagnosticPath::const_new("chunks");
const DIAG_MESHES: DiagnosticPath = DiagnosticPath::const_new("meshes");

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            FrameTimeDiagnosticsPlugin::default(),
            EntityCountDiagnosticsPlugin::default(),
            // Streams every registered metric to the console each second, so the
            // whole live game state is visible from logs.
            LogDiagnosticsPlugin::default(),
        ))
        .register_diagnostic(Diagnostic::new(DIAG_CHUNKS))
        .register_diagnostic(Diagnostic::new(DIAG_MESHES))
        .add_systems(Startup, spawn_hud)
        .add_systems(Update, (update_hud, measure_diagnostics));
    }
}

#[derive(Component)]
struct HudText;

fn spawn_hud(mut commands: Commands) {
    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: 14.0,
            ..default()
        },
        TextColor(Color::srgb(0.9, 0.95, 1.0)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(8.0),
            ..default()
        },
        HudText,
    ));
}

fn material_name(m: u8) -> &'static str {
    match m {
        0 => "Ground/Grass",
        3 => "Water",
        _ => "Solid",
    }
}

/// Records live chunk + mesh counts each frame so `LogDiagnosticsPlugin` reports
/// them (the visible-from-logs "monitor").
fn measure_diagnostics(
    mut diagnostics: Diagnostics,
    chunks: Query<(), With<Chunk<MainWorld>>>,
    meshes: Query<(), With<Mesh3d>>,
) {
    diagnostics.add_measurement(&DIAG_CHUNKS, || chunks.iter().count() as f64);
    diagnostics.add_measurement(&DIAG_MESHES, || meshes.iter().count() as f64);
}

fn update_hud(
    diagnostics: Res<DiagnosticsStore>,
    player_q: Query<(&Transform, &inf3d_gameplay::Player)>,
    chunks: Query<(), With<Chunk<MainWorld>>>,
    hover: Res<inf3d_render::Hover>,
    terrain: Res<inf3d_worldgen::Terrain>,
    mut text_q: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = text_q.single_mut() else {
        return;
    };

    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    let frame_ms = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    let entities = diagnostics
        .get(&EntityCountDiagnosticsPlugin::ENTITY_COUNT)
        .and_then(|d| d.value())
        .unwrap_or(0.0);
    let chunk_count = chunks.iter().count();

    let (pos, cell) = match player_q.single() {
        Ok((transform, player)) => (transform.translation, player.cell),
        Err(_) => (Vec3::ZERO, IVec2::ZERO),
    };

    let hover_line = if let Some(v) = hover.voxel {
        let kind = match hover.material {
            Some(m) => material_name(m),
            None => "—",
        };
        let sy = terrain.surface_y(v.x, v.z);
        format!(
            "Tile: ({}, {}, {})  surface_y={}  kind={}",
            v.x, v.y, v.z, sy, kind
        )
    } else {
        "Tile: —".to_string()
    };

    text.0 = format!(
        "FPS: {:.0}  ({:.1} ms)\nEntities: {:.0}   Chunks: {}\nPlayer: ({:.1}, {:.1}, {:.1})  cell=({}, {})\n{}",
        fps, frame_ms, entities, chunk_count, pos.x, pos.y, pos.z, cell.x, cell.y, hover_line
    );
}
