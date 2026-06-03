use std::collections::VecDeque;

use bevy::diagnostic::{
    Diagnostic, DiagnosticPath, Diagnostics, DiagnosticsStore, EntityCountDiagnosticsPlugin,
    FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin, RegisterDiagnostic,
};
use bevy::prelude::*;
use bevy_voxel_world::prelude::Chunk;

use inf3d_core::{FrameStats, QualitySettings};
use inf3d_world::MainWorld;

/// Live-monitor metrics, logged each second by `LogDiagnosticsPlugin` alongside
/// the built-in FPS / frame-time / entity-count diagnostics.
const DIAG_CHUNKS: DiagnosticPath = DiagnosticPath::const_new("chunks");
const DIAG_MESHES: DiagnosticPath = DiagnosticPath::const_new("meshes");

/// Width of the rolling frame-time window used to compute the p95 metric.
/// 120 frames ≈ 2 s at 60 fps — long enough to see hitches, short enough to
/// react when the engine recovers.
const FRAME_WINDOW: usize = 120;
/// Index of the p95 sample inside a sorted FRAME_WINDOW. With 120 samples
/// sorted ascending, the 95th percentile sits at index 114 (the 6th worst).
const FRAME_P95_INDEX: usize = 114;

pub struct HudPlugin;

/// Per-frame HUD bookkeeping that we'd otherwise recompute redundantly:
/// the live chunk/mesh entity counts (measured once in `measure_diagnostics`
/// and read by `update_hud` instead of re-scanning every entity), plus a
/// reused scratch buffer for the p95 percentile selection so we don't
/// allocate a fresh `Vec` every frame.
#[derive(Resource, Default)]
struct HudStats {
    chunk_count: usize,
    mesh_count: usize,
    /// Reused scratch for the p95 partial-selection; never reallocated once
    /// it has grown to `FRAME_WINDOW`.
    p95_scratch: Vec<f32>,
}

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
        .init_resource::<HudStats>()
        .add_systems(Startup, spawn_hud)
        .add_systems(
            Update,
            (
                measure_diagnostics,
                update_frame_stats,
                cycle_preset_keybinding,
                update_hud,
            ),
        );
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
    mut stats: ResMut<HudStats>,
) {
    // Scan each entity set exactly once per frame and cache the counts so
    // `update_hud` can read them instead of re-iterating every `Mesh3d`.
    let chunk_count = chunks.iter().count();
    let mesh_count = meshes.iter().count();
    stats.chunk_count = chunk_count;
    stats.mesh_count = mesh_count;
    diagnostics.add_measurement(&DIAG_CHUNKS, || chunk_count as f64);
    diagnostics.add_measurement(&DIAG_MESHES, || mesh_count as f64);
}

/// Pulls per-frame frame-time samples from Bevy's diagnostics, maintains a
/// rolling window of the last `FRAME_WINDOW` values, and writes p95 to
/// `FrameStats`. While the window is still filling we report the worst
/// observed sample, so the HUD has something meaningful from frame one.
fn update_frame_stats(
    diagnostics: Res<DiagnosticsStore>,
    mut window: Local<VecDeque<f32>>,
    mut stats: ResMut<FrameStats>,
    mut hud: ResMut<HudStats>,
) {
    let Some(diag) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME) else {
        return;
    };
    let Some(value) = diag.value() else {
        return;
    };
    let sample = value as f32;

    if window.len() == FRAME_WINDOW {
        window.pop_front();
    }
    window.push_back(sample);

    let p95 = if window.len() == FRAME_WINDOW {
        // Reuse a scratch buffer and do a partial selection (no full sort): we
        // only need the element that would land at FRAME_P95_INDEX once sorted.
        // `select_nth_unstable_by` is O(n) and places that element correctly.
        let scratch = &mut hud.p95_scratch;
        scratch.clear();
        scratch.extend(window.iter().copied());
        let (_, nth, _) = scratch.select_nth_unstable_by(FRAME_P95_INDEX, |a, b| {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        });
        *nth
    } else {
        window
            .iter()
            .copied()
            .fold(0.0_f32, |acc, v| if v > acc { v } else { acc })
    };

    stats.ms_p95 = p95;
}

/// F2 cycles the active quality preset (Potato → Low → Medium → High → Ultra →
/// Potato). `QualitySettings` is mutated by-value so other systems can react
/// via `is_changed()`; render distance specifically is read once at startup
/// and won't move until the binary restarts.
fn cycle_preset_keybinding(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<QualitySettings>,
) {
    if keys.just_pressed(KeyCode::F2) {
        let next = settings.preset.cycle();
        *settings = QualitySettings::from_preset(next);
        info!("Quality preset → {}", settings.preset.name());
    }
}

fn update_hud(
    diagnostics: Res<DiagnosticsStore>,
    player_q: Query<(&Transform, &inf3d_gameplay::Player)>,
    hover: Res<inf3d_render::Hover>,
    terrain: Res<inf3d_worldgen::Terrain>,
    settings: Res<QualitySettings>,
    frame: Res<FrameStats>,
    hud: Res<HudStats>,
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
    // Read the count measured this frame in `measure_diagnostics` instead of
    // re-scanning every chunk entity here.
    let chunk_count = hud.chunk_count;

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
        "FPS: {:.0}  ({:.1} ms, p95 {:.1} ms)\nEntities: {:.0}   Chunks: {}\nPlayer: ({:.1}, {:.1}, {:.1})  cell=({}, {})\nQuality: {}  rd={}  [F2]\n{}",
        fps,
        frame_ms,
        frame.ms_p95,
        entities,
        chunk_count,
        pos.x, pos.y, pos.z,
        cell.x, cell.y,
        settings.preset.name(),
        settings.render_distance_chunks,
        hover_line,
    );
}
