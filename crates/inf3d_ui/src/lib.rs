use std::collections::VecDeque;
use std::time::Duration;

use bevy::diagnostic::{
    Diagnostic, DiagnosticPath, Diagnostics, DiagnosticsStore, EntityCountDiagnosticsPlugin,
    FrameTimeDiagnosticsPlugin, RegisterDiagnostic,
};
// Dev-only console diagnostics spam; gated to debug builds at its use site.
#[cfg(debug_assertions)]
use bevy::diagnostic::LogDiagnosticsPlugin;
use bevy::prelude::*;
use bevy::time::common_conditions::on_timer;
use bevy_voxel_world::prelude::Chunk;

use inf3d_core::{AppState, EditMode, FrameStats, GameSet, QualitySettings, SelectedMaterial};
use inf3d_world::{MainWorld, TerrainMaterialId, BUILDABLE};

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
        // The HUD reads these every frame, so they're always compiled in.
        app.add_plugins((
            FrameTimeDiagnosticsPlugin::default(),
            EntityCountDiagnosticsPlugin::default(),
        ));

        // `LogDiagnosticsPlugin` streams every registered metric to the console
        // each second — useful while developing, but noisy console spam in a
        // shipped build. Dev-only (debug builds): release builds stay quiet.
        #[cfg(debug_assertions)]
        app.add_plugins(LogDiagnosticsPlugin::default());

        app.register_diagnostic(Diagnostic::new(DIAG_CHUNKS))
            .register_diagnostic(Diagnostic::new(DIAG_MESHES))
            .init_resource::<HudStats>()
            .add_systems(
                Startup,
                (
                    spawn_hud,
                    spawn_mode_buttons,
                    spawn_material_picker,
                    spawn_crosshair,
                ),
            )
            // The in-game HUD + mode buttons show only during `AppState::InGame`
            // (they spawn hidden — we boot into the menu). In `Fx`, which stays
            // ungated, so it still runs in the menu / when paused; `state_changed`
            // makes it a no-op except on the actual MainMenu<->InGame transition.
            .add_systems(
                Update,
                sync_hud_visibility
                    .in_set(GameSet::Fx)
                    .run_if(state_changed::<AppState>),
            )
            // Mode buttons: press handling in Input, restyle in Fx.
            .add_systems(Update, mode_button_system.in_set(GameSet::Input))
            .add_systems(
                Update,
                update_mode_buttons
                    .in_set(GameSet::Fx)
                    .run_if(resource_changed::<EditMode>),
            )
            // Material picker: clicks + number keys pick in Input; the selected-
            // swatch highlight restyles in Fx whenever the pick changes; the bar's
            // visibility tracks Build mode + being in-game.
            .add_systems(
                Update,
                (picker_click_system, picker_key_system).in_set(GameSet::Input),
            )
            .add_systems(
                Update,
                update_picker_selection
                    .in_set(GameSet::Fx)
                    .run_if(resource_changed::<SelectedMaterial>),
            )
            // Runs every frame in Fx (cheap — one root entity) but writes
            // Visibility only when it actually flips, so it never thrashes change
            // detection; a plain run avoids needing a combined run-condition.
            .add_systems(Update, sync_picker_visibility.in_set(GameSet::Fx))
            // Graphics settings are currently fixed high; runtime settings UI will
            // be reintroduced later. Everything below is read-only diagnostics /
            // presentation and belongs in `Fx` (end of the frame).
            // `update_frame_stats` samples frame-time EVERY frame so the rolling p95
            // stays accurate. The entity-counting + text rebuild
            // (`measure_diagnostics` → `update_hud`) only need a few Hz for a
            // readout, so throttle them off the per-frame path — they were scanning
            // every `Mesh3d`/`Chunk` entity 60×/s (thousands of entities) just for
            // the HUD numbers.
            .add_systems(Update, update_frame_stats.in_set(GameSet::Fx))
            .add_systems(
                Update,
                (measure_diagnostics, update_hud)
                    .chain()
                    .in_set(GameSet::Fx)
                    .run_if(on_timer(Duration::from_millis(150))),
            );
    }
}

#[derive(Component)]
struct HudText;

/// Marks a UI root that is only visible during [`AppState::InGame`] (the HUD text
/// and the Build/Walk mode buttons). [`sync_hud_visibility`] toggles these on the
/// menu<->game transition; they spawn hidden because the game boots into the menu.
#[derive(Component)]
struct InGameUi;

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
        // Hidden until the player enters a game (we boot into the main menu).
        Visibility::Hidden,
        InGameUi,
        HudText,
    ));
}

/// Diameter (px) of the centered crosshair dot.
const CROSSHAIR_SIZE: f32 = 4.0;

/// Spawn a tiny centered crosshair dot, shown only in-game (the cursor is captured
/// in play, so the player aims with the screen center). Built in the same
/// [`InGameUi`] family as the HUD so [`sync_hud_visibility`] toggles it on the
/// menu<->game transition; it spawns hidden because the game boots into the menu.
/// Positioned by anchoring its top-left at screen center and pulling it back half
/// its size, so its center sits exactly at the viewport center regardless of
/// resolution. A plain (non-`Button`/non-`Interaction`) node captures no clicks.
fn spawn_crosshair(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(CROSSHAIR_SIZE),
            height: Val::Px(CROSSHAIR_SIZE),
            // Pull the dot back by half its size so its CENTER lands on screen center.
            margin: UiRect {
                left: Val::Px(-CROSSHAIR_SIZE / 2.0),
                top: Val::Px(-CROSSHAIR_SIZE / 2.0),
                ..default()
            },
            ..default()
        },
        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.6)),
        // Hidden until the player enters a game (we boot into the main menu); the
        // blanket `InGameUi` show-on-enter reveals it in play.
        Visibility::Hidden,
        InGameUi,
    ));
}

/// Show the in-game HUD + mode buttons only in [`AppState::InGame`]; hide them in
/// the main menu. Runs only on a state change (cheap). Setting `Visibility` on the
/// UI root propagates to its children.
fn sync_hud_visibility(state: Res<State<AppState>>, mut q: Query<&mut Visibility, With<InGameUi>>) {
    let vis = if *state.get() == AppState::InGame {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut v in &mut q {
        *v = vis;
    }
}

/// Human-readable name for a terrain material index. Derived from the single
/// `TerrainMaterialId` enum in `inf3d_world` (the source of truth for both the
/// index meanings and the player-facing labels) so the HUD can't desync if the
/// enum's discriminants or meanings change. `inf3d_ui` already depends on
/// `inf3d_world` and `inf3d_world` does not depend on `inf3d_ui`, so this adds
/// no dependency edge. Indices outside the palette fall back to "Solid".
fn material_name(m: u8) -> &'static str {
    match TerrainMaterialId::from_index(m) {
        Some(id) => id.label(),
        None => "Solid",
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

fn update_hud(
    diagnostics: Res<DiagnosticsStore>,
    player_q: Query<(&Transform, &inf3d_gameplay::Player)>,
    hover: Res<inf3d_render::Hover>,
    terrain: Res<inf3d_worldgen::Terrain>,
    settings: Res<QualitySettings>,
    frame: Res<FrameStats>,
    hud: Res<HudStats>,
    mode: Res<EditMode>,
    selected: Res<SelectedMaterial>,
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

    let on_off = |b: bool| if b { "on" } else { "off" };

    // Mode line: in Build, also show which block the picker has selected (1..8 keys
    // re-bind it). Left-click places / right-click breaks in Build; moves in Walk.
    let mode_line = match *mode {
        EditMode::Build => format!("Mode: Build   Block: {}", material_name(selected.0)),
        EditMode::Walk => "Mode: Walk".to_string(),
    };

    text.0 = format!(
        "FPS: {:.0}  ({:.1} ms, p95 {:.1} ms)\nEntities: {:.0}   Chunks: {}\nPlayer: ({:.1}, {:.1}, {:.1})  cell=({}, {})\nGraphics: fixed high  rd={}  terrain_lod0={:.0}  SSAO {}  MB {}\n{}\n{}",
        fps,
        frame_ms,
        frame.ms_p95,
        entities,
        chunk_count,
        pos.x, pos.y, pos.z,
        cell.x, cell.y,
        settings.render_distance_chunks,
        settings.terrain_lod_distance,
        on_off(settings.ssao_enabled),
        on_off(settings.motion_blur_enabled),
        mode_line,
        hover_line,
    );
}

/// One of the two mode buttons on the right edge of the screen. Carries its
/// [`EditMode`] plus the two background colors used to show whether it is the
/// active mode (vivid) or not (dim).
#[derive(Component, Clone, Copy)]
struct ModeButton {
    mode: EditMode,
    active: Color,
    inactive: Color,
}

/// Spawn the Build (green) and Walk (grey) buttons in a vertical stack on the
/// far-right edge. The active mode is highlighted by [`update_mode_buttons`];
/// clicks are handled by [`mode_button_system`]. Build mode: left-click places a
/// block, right-click breaks one. Walk mode: left-click moves the player.
fn spawn_mode_buttons(mut commands: Commands) {
    // (mode, label, active color, inactive color)
    let modes = [
        (
            EditMode::Build,
            "Build",
            Color::srgb(0.32, 0.66, 0.34),
            Color::srgb(0.19, 0.33, 0.20),
        ),
        (
            EditMode::Walk,
            "Walk",
            Color::srgb(0.47, 0.51, 0.57),
            Color::srgb(0.26, 0.28, 0.32),
        ),
    ];

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(10.0),
                top: Val::Percent(34.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                ..default()
            },
            // Hidden until the player enters a game (we boot into the main menu).
            Visibility::Hidden,
            InGameUi,
        ))
        .with_children(|stack| {
            for (mode, label, active, inactive) in modes {
                stack
                    .spawn((
                        Button,
                        Interaction::default(),
                        Node {
                            width: Val::Px(66.0),
                            height: Val::Px(40.0),
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            ..default()
                        },
                        BackgroundColor(inactive),
                        ModeButton {
                            mode,
                            active,
                            inactive,
                        },
                    ))
                    .with_children(|b| {
                        b.spawn((
                            Text::new(label),
                            TextFont {
                                font_size: 16.0,
                                ..default()
                            },
                            TextColor(Color::srgb(0.96, 0.98, 1.0)),
                        ));
                    });
            }
        });
}

/// Switch [`EditMode`] when a mode button is pressed.
fn mode_button_system(
    buttons: Query<(&Interaction, &ModeButton), Changed<Interaction>>,
    mut mode: ResMut<EditMode>,
) {
    for (interaction, button) in &buttons {
        if *interaction == Interaction::Pressed && *mode != button.mode {
            *mode = button.mode;
        }
    }
}

/// Highlight the active mode button and dim the others. Runs whenever
/// [`EditMode`] changes (including the first frame, so the default `Walk` lights up).
fn update_mode_buttons(
    mode: Res<EditMode>,
    mut buttons: Query<(&ModeButton, &mut BackgroundColor)>,
) {
    for (button, mut bg) in &mut buttons {
        bg.0 = if button.mode == *mode {
            button.active
        } else {
            button.inactive
        };
    }
}

// ---------------------------------------------------------------------------
// Build-mode material picker (hotbar)
// ---------------------------------------------------------------------------

/// Idle frame behind a picker swatch — a thin dark ring (via padding) so adjacent
/// swatches read as separate chips.
const PICKER_FRAME_IDLE: Color = Color::srgba(0.10, 0.11, 0.14, 0.85);
/// Frame behind the SELECTED swatch — a bright gold ring so the current block reads
/// at a glance regardless of the swatch's own color (works over neon or stone alike).
const PICKER_FRAME_SELECTED: Color = Color::srgb(0.97, 0.82, 0.27);

/// Root of the Build-mode material hotbar (bottom-center). Visible only while
/// in-game AND in [`EditMode::Build`] — see [`sync_picker_visibility`]. Spawns
/// hidden because the game boots into the menu.
#[derive(Component)]
struct PickerRoot;

/// One swatch in the material hotbar, carrying the raw material it selects. The
/// material doubles as the selection key (each [`BUILDABLE`] entry is distinct), so
/// [`update_picker_selection`] just compares it to [`SelectedMaterial`].
#[derive(Component, Clone, Copy)]
struct PickerSlot {
    material: u8,
}

/// Spawn the material hotbar: a centered row of color swatches, one per
/// [`BUILDABLE`] entry, each a clickable [`Button`] with its number-key hint. The
/// swatch order / contents come straight from `BUILDABLE` (the single source of
/// truth in `inf3d_world`), so adding a buildable block needs no UI change.
fn spawn_material_picker(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(14.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                // Start collapsed (we boot into the menu). `Display::None` — not
                // `Visibility::Hidden` — so the full-width bar neither renders NOR
                // captures clicks while hidden; otherwise its swatch buttons would
                // swallow Walk-mode world clicks along the bottom of the screen.
                display: Display::None,
                ..default()
            },
            // Visibility is owned solely by `sync_picker_visibility` (which gates on
            // both in-game AND Build mode), so this is deliberately NOT tagged
            // `InGameUi` — the blanket `InGameUi` show-on-enter must not touch it.
            PickerRoot,
        ))
        .with_children(|bar| {
            for (i, material) in BUILDABLE.iter().enumerate() {
                let mat_u8 = *material as u8;
                let [r, g, b] = material.color();
                bar.spawn((
                    Button,
                    Interaction::default(),
                    // The frame: 3px padding draws a ring around the inner swatch;
                    // its color flips to gold when this slot is the selected one.
                    Node {
                        padding: UiRect::all(Val::Px(3.0)),
                        ..default()
                    },
                    BackgroundColor(PICKER_FRAME_IDLE),
                    PickerSlot { material: mat_u8 },
                ))
                .with_children(|frame| {
                    frame
                        .spawn((
                            Node {
                                width: Val::Px(40.0),
                                height: Val::Px(40.0),
                                justify_content: JustifyContent::FlexEnd,
                                align_items: AlignItems::FlexStart,
                                padding: UiRect::all(Val::Px(2.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgb_u8(r, g, b)),
                        ))
                        .with_children(|swatch| {
                            // Number-key hint on a dark chip so it stays legible over
                            // any swatch color (bright neon or near-black glass alike).
                            swatch
                                .spawn((
                                    Node {
                                        padding: UiRect::axes(Val::Px(3.0), Val::Px(0.0)),
                                        ..default()
                                    },
                                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.5)),
                                ))
                                .with_children(|tag| {
                                    tag.spawn((
                                        Text::new((i + 1).to_string()),
                                        TextFont {
                                            font_size: 12.0,
                                            ..default()
                                        },
                                        TextColor(Color::srgb(0.96, 0.98, 1.0)),
                                    ));
                                });
                        });
                });
            }
        });
}

/// Pick a build material when its swatch is clicked. A click on a swatch also
/// suppresses the world edit for that frame (the editor ignores clicks while any
/// `Interaction` is non-`None`), so picking never places a block behind the bar.
fn picker_click_system(
    buttons: Query<(&Interaction, &PickerSlot), Changed<Interaction>>,
    mut selected: ResMut<SelectedMaterial>,
) {
    for (interaction, slot) in &buttons {
        if *interaction == Interaction::Pressed && selected.0 != slot.material {
            selected.0 = slot.material;
        }
    }
}

/// Number keys 1..8 select the corresponding [`BUILDABLE`] block, but only while
/// building (so the keys are free for other actions in Walk mode).
fn picker_key_system(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<EditMode>,
    mut selected: ResMut<SelectedMaterial>,
) {
    if *mode != EditMode::Build {
        return;
    }
    const DIGITS: [KeyCode; 8] = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
    ];
    for (i, key) in DIGITS.iter().enumerate() {
        if keys.just_pressed(*key) {
            if let Some(material) = BUILDABLE.get(i) {
                let m = *material as u8;
                if selected.0 != m {
                    selected.0 = m;
                }
            }
        }
    }
}

/// Re-tint every swatch frame so the selected one is gold and the rest dim. Runs
/// whenever [`SelectedMaterial`] changes (including frame one, so the default block
/// is highlighted immediately).
fn update_picker_selection(
    selected: Res<SelectedMaterial>,
    mut slots: Query<(&PickerSlot, &mut BackgroundColor)>,
) {
    for (slot, mut bg) in &mut slots {
        bg.0 = if slot.material == selected.0 {
            PICKER_FRAME_SELECTED
        } else {
            PICKER_FRAME_IDLE
        };
    }
}

/// Show the material hotbar only while in-game AND in Build mode; collapse it in
/// Walk mode and in the menu. Toggles `Display` (not `Visibility`) so a hidden bar
/// captures no clicks — see the note in [`spawn_material_picker`].
fn sync_picker_visibility(
    state: Res<State<AppState>>,
    mode: Res<EditMode>,
    mut roots: Query<&mut Node, With<PickerRoot>>,
) {
    let show = *state.get() == AppState::InGame && *mode == EditMode::Build;
    let target = if show { Display::Flex } else { Display::None };
    for mut node in &mut roots {
        // Only write on an actual change so we don't force a layout recompute every
        // frame (mutating `Node` marks it dirty for the UI layout pass).
        if node.display != target {
            node.display = target;
        }
    }
}
