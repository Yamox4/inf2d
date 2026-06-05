//! Front-end shell for inf3d: main menu, pause menu, settings, 3-slot save/load,
//! and the flat test-world stamper.
//!
//! ## How it fits the engine
//! `inf3d_core` owns the `AppState { MainMenu, InGame }` + `Pause { Running, Paused }`
//! states and gates the gameplay `GameSet` phases on un-paused play. This crate is
//! a top-level sink (like `inf3d_ui`): it drives those states and renders the
//! menus. Its menu/state systems run as plain `Update` systems **without a
//! `GameSet`** — the sanctioned menu analogue of the physics-spine exception —
//! because they must run while the gameplay phases are gated off (in the menu /
//! when paused).
//!
//! ## World lifecycle (no teardown)
//! The world is procedural terrain + a shared `VoxelOverrides` edit layer, so New
//! Game / Load don't rebuild it: they flip the shared `WorldGen` flat flag, reset
//! or import the edit layer + player position, and re-mesh resident chunks. The
//! game boots into `MainMenu` with the world rendering behind the menu as a live
//! backdrop; Start switches to the flat test world.

mod save;
mod testmap;
mod theme;

use bevy::prelude::*;
use bevy::window::{PresentMode, PrimaryWindow};
use bevy_voxel_world::prelude::{Chunk, NeedsRemesh};

use inf3d_camera::{CameraMode, CameraRig, IsoCamera};
use inf3d_core::{
    save_quality_settings, AppState, EditMode, PathTarget, Pause, QualityPreset, QualitySettings,
    SelectedMaterial,
};
use inf3d_gameplay::{MovePath, Player};
use inf3d_physics::{CharacterController, DesiredMove, PLAYER_DIMS};
use inf3d_world::MainWorld;
use inf3d_worldgen::{Terrain, VoxelOverrides, WorldGen, WorldKind};

use save::{load_from_slot, save_to_slot, slot_summary, SaveGame, SLOT_COUNT};

/// Spawn a styled menu button (a `Node` box with a centered `Text` child) under a
/// child-spawner `$parent`. A macro (not a fn) so it never has to name the Bevy
/// child-spawner type, which the codebase otherwise only uses via inferred closures.
macro_rules! menu_button {
    ($parent:expr, $node:expr, $label:expr, $size:expr, $action:expr) => {
        $parent
            .spawn((
                Button,
                Interaction::default(),
                $node,
                BackgroundColor(theme::BUTTON),
                MenuButton($action),
            ))
            .with_children(|b| {
                b.spawn((
                    Text::new($label),
                    theme::text_font($size),
                    TextColor(theme::TEXT),
                ));
            });
    };
}

/// Spawn a menu screen: a full-screen dim overlay + a centered panel with `$title`,
/// then the caller's `$body` (which fills the panel via the bound `$panel`
/// child-spawner). A macro so it never names the Bevy child-spawner type. The
/// overlay carries [`MenuRoot`] for one-shot teardown.
macro_rules! screen {
    ($commands:expr, $title:expr, $panel:ident => $body:block) => {
        $commands
            .spawn((
                theme::overlay_node(),
                BackgroundColor(theme::OVERLAY),
                MenuRoot,
            ))
            .with_children(|root| {
                root.spawn((theme::panel_node(), BackgroundColor(theme::PANEL)))
                    .with_children(|$panel| {
                        $panel.spawn((
                            Text::new($title),
                            theme::text_font(46.0),
                            TextColor(theme::TITLE),
                        ));
                        $body
                    });
            });
    };
}

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        // States themselves are registered by `CorePlugin`; we only drive them.
        app.init_resource::<MenuScreen>()
            .init_resource::<MenuReturn>()
            .init_resource::<PendingLoad>()
            .init_resource::<PendingSave>()
            // Menu show/hide is keyed off state transitions.
            .add_systems(OnEnter(AppState::MainMenu), (enter_main_menu, spawn_menu_backdrop))
            // The opaque backdrop hides the world in the main menu — no world is
            // shown until you pick one. Removed the moment a game starts.
            .add_systems(OnExit(AppState::MainMenu), despawn_menu_backdrop)
            .add_systems(OnEnter(Pause::Paused), enter_pause_paused)
            .add_systems(OnEnter(Pause::Running), enter_pause_running)
            // Build / load the world the moment we enter play (runs in
            // StateTransition, before the first gameplay frame).
            .add_systems(OnEnter(AppState::InGame), apply_world_load)
            // Menu/state systems run OUTSIDE the gated GameSets (no GameSet tag) so
            // they keep working while gameplay is paused / in the menu.
            .add_systems(
                Update,
                (
                    esc_toggle_pause.run_if(in_state(AppState::InGame)),
                    do_save,
                    button_color_system,
                ),
            )
            // Click handling first, then the router rebuilds on the resulting
            // `MenuScreen` change in the SAME frame.
            .add_systems(
                Update,
                (
                    menu_button_system,
                    menu_router.run_if(resource_changed::<MenuScreen>),
                )
                    .chain(),
            );
    }
}

// ---------------------------------------------------------------------------
// State / navigation resources
// ---------------------------------------------------------------------------

/// Which menu screen is currently shown (or `None` during play). The router
/// rebuilds the UI whenever this changes.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
enum MenuScreen {
    #[default]
    None,
    MainRoot,
    PauseRoot,
    NewWorld,
    Settings,
    LoadSlots,
    SaveSlots,
}

/// Where "Back" returns from Settings / slot screens (Settings is reachable from
/// both the main menu and the pause menu).
#[derive(Resource, Default)]
struct MenuReturn(MenuScreen);

/// A queued world to build/load, consumed once on entering [`AppState::InGame`].
#[derive(Resource, Default)]
struct PendingLoad(Option<LoadKind>);

#[derive(Clone)]
enum LoadKind {
    /// A fresh world backend.
    New { kind: WorldKind },
    /// Restore a saved game.
    Load(SaveGame),
}

/// A queued save (the target slot), consumed by [`do_save`].
#[derive(Resource, Default)]
struct PendingSave(Option<u8>);

/// Marks a menu UI root so the router can despawn the current screen wholesale
/// (despawn is recursive, so the buttons/labels go with it).
#[derive(Component)]
struct MenuRoot;

/// A clickable menu button carrying the action to run when pressed.
#[derive(Component, Clone, Copy)]
struct MenuButton(MenuAction);

#[derive(Clone, Copy)]
enum MenuAction {
    OpenNewWorld,
    StartWorld(WorldKind),
    OpenLoad,
    OpenSettings,
    QuitDesktop,
    Resume,
    OpenSave,
    QuitToMenu,
    Back,
    Slot(SlotOp, u8),
    Preset(QualityPreset),
    Toggle(Setting),
    SaveSettings,
}

#[derive(Clone, Copy)]
enum SlotOp {
    Load,
    Save,
}

/// A live-toggleable graphics setting in the settings menu.
#[derive(Clone, Copy)]
enum Setting {
    Bloom,
    Ssao,
    MotionBlur,
    Dof,
    Foliage,
    Vsync,
}

impl Setting {
    /// Current on/off state (for the green "active" button tint).
    fn is_on(self, q: &QualitySettings, vsync_on: bool) -> bool {
        match self {
            Setting::Bloom => q.bloom_enabled,
            Setting::Ssao => q.ssao_enabled,
            Setting::MotionBlur => q.motion_blur_enabled,
            Setting::Dof => q.dof_enabled,
            Setting::Foliage => q.foliage_enabled,
            Setting::Vsync => vsync_on,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Setting::Bloom => "Bloom",
            Setting::Ssao => "SSAO",
            Setting::MotionBlur => "Motion Blur",
            Setting::Dof => "Depth of Field",
            Setting::Foliage => "Foliage",
            Setting::Vsync => "VSync",
        }
    }
}

// ---------------------------------------------------------------------------
// State transition systems
// ---------------------------------------------------------------------------

fn enter_main_menu(mut screen: ResMut<MenuScreen>) {
    *screen = MenuScreen::MainRoot;
}

/// Full-screen opaque panel shown ONLY in the main menu so no world is visible
/// behind it — the game loads no world until you pick one. (The voxel world still
/// streams, but it's fully hidden; the pause menu deliberately does NOT use this,
/// so a paused game still shows through.)
#[derive(Component)]
struct MenuBackdrop;

fn spawn_menu_backdrop(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::srgb(0.04, 0.05, 0.07)),
        // Behind the menu panels (default z = 0) but in front of the 3D world.
        GlobalZIndex(-10),
        MenuBackdrop,
    ));
}

fn despawn_menu_backdrop(mut commands: Commands, q: Query<Entity, With<MenuBackdrop>>) {
    for entity in &q {
        commands.entity(entity).despawn();
    }
}

fn enter_pause_paused(mut screen: ResMut<MenuScreen>) {
    *screen = MenuScreen::PauseRoot;
}

fn enter_pause_running(mut screen: ResMut<MenuScreen>) {
    *screen = MenuScreen::None;
}

/// Esc toggles pause while in a game. `ButtonInput` is updated in `PreUpdate`
/// (never gated), so this reads it fresh even while the gameplay phases are off.
fn esc_toggle_pause(
    input: Res<ButtonInput<KeyCode>>,
    state: Res<State<Pause>>,
    mut next: ResMut<NextState<Pause>>,
) {
    if input.just_pressed(KeyCode::Escape) {
        next.set(match state.get() {
            Pause::Running => Pause::Paused,
            Pause::Paused => Pause::Running,
        });
    }
}

/// On entering play, build (New) or restore (Load) the world: flip the flat flag,
/// reset/import the edit layer, reposition the player, restore camera/edit-mode,
/// then force every resident chunk to re-mesh so the new surface + edits show.
#[allow(clippy::too_many_arguments)]
fn apply_world_load(
    mut pending: ResMut<PendingLoad>,
    world_gen: Res<WorldGen>,
    overrides: Res<VoxelOverrides>,
    terrain: Res<Terrain>,
    mut edit_mode: ResMut<EditMode>,
    mut selected_mat: ResMut<SelectedMaterial>,
    mut path_target: ResMut<PathTarget>,
    mut player_q: PlayerResetQuery,
    mut rig_q: Query<&mut CameraRig, With<IsoCamera>>,
    mut camera_mode: ResMut<CameraMode>,
    chunks: Query<Entity, With<Chunk<MainWorld>>>,
    mut commands: Commands,
) {
    // Nothing queued (shouldn't happen via the menu) → leave the world as-is.
    let Some(kind) = pending.0.take() else {
        return;
    };

    match kind {
        LoadKind::New { kind } => {
            world_gen.set_kind(kind); // flip BEFORE reading stand_pos below
            overrides.clear_all();
            // The flat lab gets the stamped test map; the procedural world starts
            // clean (no structures). City is generated by its own backend.
            if kind == WorldKind::TestFlat {
                testmap::stamp_test_map(&overrides);
            }
            // Reset the player onto the chosen world's spawn (capsule center =
            // feet + offset, mirroring `spawn_player`).
            let spawn = terrain.nearest_land(IVec2::ZERO);
            let center =
                terrain.stand_pos(spawn.x, spawn.y) + Vec3::Y * PLAYER_DIMS.visual_root_offset;
            reset_player(&mut player_q, center, 0.0, spawn);
            *edit_mode = EditMode::Walk;
            // Fresh world → reset the picker to the default buildable.
            *selected_mat = SelectedMaterial::default();
            camera_mode.set_free_fly(kind == WorldKind::City);
        }
        LoadKind::Load(game) => {
            let kind = if game.flat && game.world_kind == WorldKind::Normal {
                WorldKind::TestFlat
            } else {
                game.world_kind
            };
            world_gen.set_kind(kind);
            let edits: Vec<_> = game
                .edits
                .iter()
                .map(|(p, e)| (IVec3::from_array(*p), *e))
                .collect();
            overrides.import(&edits);
            let center = Vec3::from_array(game.player_pos);
            let cell = IVec2::new(center.x.floor() as i32, center.z.floor() as i32);
            reset_player(&mut player_q, center, game.facing, cell);
            *edit_mode = game.edit_mode;
            *selected_mat = SelectedMaterial(game.selected_material);
            if let Ok(mut rig) = rig_q.single_mut() {
                rig.snap_to(game.camera_yaw, game.camera_zoom);
            }
            camera_mode.set_free_fly(kind == WorldKind::City);
        }
    }

    // Clear any stale click destination; foliage + BlockedCells self-heal as the
    // streamer re-runs around the (possibly teleported) player.
    path_target.0 = None;

    // The world surface / edits changed under already-resident chunks — re-mesh
    // them all so the change is visible (the mesher re-reads the shared store).
    for entity in &chunks {
        commands.entity(entity).try_insert(NeedsRemesh);
    }
    info!("inf3d_menu: world ready ({:?})", world_gen.kind());
}

/// Hard-reposition the single player: snap the transform, clear momentum + path,
/// and resync the logical cell/facing. This is a legitimate menu-driven teleport
/// (New/Load), distinct from the per-frame `DesiredMove` locomotion loop.
type PlayerResetQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Transform,
        &'static mut Player,
        &'static mut MovePath,
        &'static mut CharacterController,
        &'static mut DesiredMove,
    ),
>;

fn reset_player(q: &mut PlayerResetQuery, center: Vec3, facing: f32, cell: IVec2) {
    let Ok((mut transform, mut player, mut path, mut cc, mut desired)) = q.single_mut() else {
        return;
    };
    transform.translation = center;
    player.cell = cell;
    player.facing = facing;
    path.waypoints.clear();
    cc.vertical_velocity = 0.0;
    desired.velocity = Vec3::ZERO;
}

/// Write a queued save: gather the live world/player/camera state and persist it.
#[allow(clippy::too_many_arguments)]
fn do_save(
    mut pending: ResMut<PendingSave>,
    mut screen: ResMut<MenuScreen>,
    world_gen: Res<WorldGen>,
    overrides: Res<VoxelOverrides>,
    edit_mode: Res<EditMode>,
    selected: Res<SelectedMaterial>,
    player_q: Query<(&Transform, &Player)>,
    rig_q: Query<&CameraRig, With<IsoCamera>>,
) {
    let Some(slot) = pending.0.take() else {
        return;
    };
    let Ok((transform, player)) = player_q.single() else {
        return;
    };
    let (yaw, zoom) = rig_q
        .single()
        .map(|r| (r.yaw(), r.zoom()))
        .unwrap_or((0.0, 44.0));
    let game = SaveGame {
        world_kind: world_gen.kind(),
        flat: world_gen.is_flat(),
        edits: overrides
            .export()
            .iter()
            .map(|(p, e)| (p.to_array(), *e))
            .collect(),
        player_pos: transform.translation.to_array(),
        facing: player.facing,
        edit_mode: *edit_mode,
        selected_material: selected.0,
        camera_yaw: yaw,
        camera_zoom: zoom,
        saved_at: save::now_secs(),
    };
    save_to_slot(slot, &game);
    // Back to the pause menu after saving.
    *screen = MenuScreen::PauseRoot;
}

// ---------------------------------------------------------------------------
// Button dispatch + visuals
// ---------------------------------------------------------------------------

/// Handle a pressed menu button: drive states, navigate screens, and apply
/// settings live.
#[allow(clippy::too_many_arguments)]
fn menu_button_system(
    buttons: Query<(&Interaction, &MenuButton), Changed<Interaction>>,
    mut next_app: ResMut<NextState<AppState>>,
    mut next_pause: ResMut<NextState<Pause>>,
    mut screen: ResMut<MenuScreen>,
    mut ret: ResMut<MenuReturn>,
    mut pending_load: ResMut<PendingLoad>,
    mut pending_save: ResMut<PendingSave>,
    mut quality: ResMut<QualitySettings>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut exit: MessageWriter<AppExit>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match button.0 {
            MenuAction::OpenNewWorld => {
                ret.0 = *screen;
                *screen = MenuScreen::NewWorld;
            }
            MenuAction::StartWorld(kind) => {
                pending_load.0 = Some(LoadKind::New { kind });
                next_app.set(AppState::InGame);
            }
            MenuAction::OpenLoad => {
                ret.0 = *screen;
                *screen = MenuScreen::LoadSlots;
            }
            MenuAction::OpenSettings => {
                ret.0 = *screen;
                *screen = MenuScreen::Settings;
            }
            MenuAction::QuitDesktop => {
                exit.write(AppExit::Success);
            }
            MenuAction::Resume => {
                next_pause.set(Pause::Running);
            }
            MenuAction::OpenSave => {
                ret.0 = *screen;
                *screen = MenuScreen::SaveSlots;
            }
            MenuAction::QuitToMenu => {
                next_app.set(AppState::MainMenu);
            }
            MenuAction::Back => {
                *screen = ret.0;
            }
            MenuAction::Slot(SlotOp::Load, n) => {
                if let Some(game) = load_from_slot(n) {
                    pending_load.0 = Some(LoadKind::Load(game));
                    next_app.set(AppState::InGame);
                }
            }
            MenuAction::Slot(SlotOp::Save, n) => {
                pending_save.0 = Some(n);
            }
            MenuAction::Preset(preset) => preset.apply(&mut quality),
            MenuAction::Toggle(setting) => apply_toggle(setting, &mut quality, &mut windows),
            MenuAction::SaveSettings => save_quality_settings(&quality),
        }
    }
}

/// Flip a graphics toggle. Mutating `QualitySettings` is observed by the live
/// appliers (camera post-FX / foliage / water) next frame; VSync is the `Window`.
fn apply_toggle(
    setting: Setting,
    quality: &mut QualitySettings,
    windows: &mut Query<&mut Window, With<PrimaryWindow>>,
) {
    match setting {
        Setting::Bloom => quality.bloom_enabled = !quality.bloom_enabled,
        Setting::Ssao => quality.ssao_enabled = !quality.ssao_enabled,
        Setting::MotionBlur => quality.motion_blur_enabled = !quality.motion_blur_enabled,
        Setting::Dof => quality.dof_enabled = !quality.dof_enabled,
        Setting::Foliage => quality.foliage_enabled = !quality.foliage_enabled,
        Setting::Vsync => {
            if let Ok(mut window) = windows.single_mut() {
                window.present_mode = toggle_present_mode(window.present_mode);
            }
        }
    }
}

fn toggle_present_mode(mode: PresentMode) -> PresentMode {
    match mode {
        PresentMode::AutoVsync | PresentMode::Fifo => PresentMode::AutoNoVsync,
        _ => PresentMode::AutoVsync,
    }
}

fn present_mode_is_vsync(mode: PresentMode) -> bool {
    matches!(mode, PresentMode::AutoVsync | PresentMode::Fifo)
}

/// Tint every menu button by interaction + (for toggles) its on/off state, so a
/// hovered button lifts and an enabled toggle reads green. Runs every frame but is
/// a no-op when no menu is up (no `MenuButton` entities exist).
fn button_color_system(
    quality: Res<QualitySettings>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut q: Query<(&Interaction, &MenuButton, &mut BackgroundColor)>,
) {
    let vsync_on = windows
        .single()
        .map(|w| present_mode_is_vsync(w.present_mode))
        .unwrap_or(true);
    for (interaction, button, mut bg) in &mut q {
        let active = matches!(button.0, MenuAction::Toggle(s) if s.is_on(&quality, vsync_on));
        bg.0 = match interaction {
            Interaction::Pressed => theme::BUTTON_PRESS,
            Interaction::Hovered => {
                if active {
                    theme::ACCENT
                } else {
                    theme::BUTTON_HOVER
                }
            }
            Interaction::None => {
                if active {
                    theme::ACCENT
                } else {
                    theme::BUTTON
                }
            }
        };
    }
}

// ---------------------------------------------------------------------------
// Screen router + builders
// ---------------------------------------------------------------------------

/// Rebuild the visible menu whenever [`MenuScreen`] changes: despawn the current
/// root (recursive) and spawn the new screen.
fn menu_router(
    screen: Res<MenuScreen>,
    quality: Res<QualitySettings>,
    roots: Query<Entity, With<MenuRoot>>,
    mut commands: Commands,
) {
    for entity in &roots {
        commands.entity(entity).despawn();
    }
    match *screen {
        MenuScreen::None => {}
        MenuScreen::MainRoot => spawn_main(&mut commands),
        MenuScreen::PauseRoot => spawn_pause(&mut commands),
        MenuScreen::NewWorld => spawn_new_world(&mut commands),
        MenuScreen::Settings => spawn_settings(&mut commands, quality.render_distance_chunks),
        MenuScreen::LoadSlots => spawn_slots(&mut commands, SlotOp::Load),
        MenuScreen::SaveSlots => spawn_slots(&mut commands, SlotOp::Save),
    }
}

fn spawn_main(commands: &mut Commands) {
    screen!(commands, "inf3d", panel => {
        panel.spawn((
            Text::new("voxel test world"),
            theme::text_font(15.0),
            TextColor(theme::TEXT_DIM),
            Node { margin: UiRect::bottom(Val::Px(12.0)), ..default() },
        ));
        menu_button!(panel, theme::button_node(), "New Game", 22.0, MenuAction::OpenNewWorld);
        menu_button!(panel, theme::button_node(), "Load Game", 22.0, MenuAction::OpenLoad);
        menu_button!(panel, theme::button_node(), "Settings", 22.0, MenuAction::OpenSettings);
        menu_button!(panel, theme::button_node(), "Quit", 22.0, MenuAction::QuitDesktop);
    });
}

fn spawn_pause(commands: &mut Commands) {
    screen!(commands, "Paused", panel => {
        panel.spawn((
            Text::new("Esc to resume"),
            theme::text_font(15.0),
            TextColor(theme::TEXT_DIM),
            Node { margin: UiRect::bottom(Val::Px(12.0)), ..default() },
        ));
        menu_button!(panel, theme::button_node(), "Resume", 22.0, MenuAction::Resume);
        menu_button!(panel, theme::button_node(), "Save Game", 22.0, MenuAction::OpenSave);
        menu_button!(panel, theme::button_node(), "Settings", 22.0, MenuAction::OpenSettings);
        menu_button!(panel, theme::button_node(), "Quit to Menu", 22.0, MenuAction::QuitToMenu);
        menu_button!(panel, theme::button_node(), "Quit to Desktop", 22.0, MenuAction::QuitDesktop);
    });
}

fn spawn_new_world(commands: &mut Commands) {
    screen!(commands, "New World", panel => {
        panel.spawn((
            Text::new("choose a world"),
            theme::text_font(15.0),
            TextColor(theme::TEXT_DIM),
            Node { margin: UiRect::bottom(Val::Px(12.0)), ..default() },
        ));
        menu_button!(panel, theme::button_node(), "Cyberpunk City (free fly)", 20.0, MenuAction::StartWorld(WorldKind::City));
        menu_button!(panel, theme::button_node(), "Test World (flat lab)", 20.0, MenuAction::StartWorld(WorldKind::TestFlat));
        menu_button!(panel, theme::button_node(), "Normal World (procedural)", 20.0, MenuAction::StartWorld(WorldKind::Normal));
        menu_button!(panel, theme::button_node(), "Back", 18.0, MenuAction::Back);
    });
}

fn spawn_settings(commands: &mut Commands, render_distance: u32) {
    screen!(commands, "Settings", panel => {
        // Quality presets (a row of chips).
        panel.spawn((Text::new("Preset"), theme::text_font(15.0), TextColor(theme::TEXT_DIM)));
        panel.spawn(theme::row_node()).with_children(|row| {
            for preset in QualityPreset::ALL {
                menu_button!(row, theme::chip_node(), preset.label(), 16.0, MenuAction::Preset(preset));
            }
        });

        // Individual live toggles (green tint = on).
        panel.spawn((
            Text::new("Toggles"),
            theme::text_font(15.0),
            TextColor(theme::TEXT_DIM),
            Node { margin: UiRect::top(Val::Px(8.0)), ..default() },
        ));
        for setting in [
            Setting::Bloom,
            Setting::Ssao,
            Setting::MotionBlur,
            Setting::Dof,
            Setting::Foliage,
            Setting::Vsync,
        ] {
            menu_button!(panel, theme::button_node(), setting.label(), 18.0, MenuAction::Toggle(setting));
        }

        panel.spawn((
            Text::new(format!("Render distance: {render_distance} chunks (restart to change)")),
            theme::text_font(13.0),
            TextColor(theme::TEXT_DIM),
            Node { margin: UiRect::vertical(Val::Px(8.0)), ..default() },
        ));

        menu_button!(panel, theme::button_node(), "Save Settings", 18.0, MenuAction::SaveSettings);
        menu_button!(panel, theme::button_node(), "Back", 18.0, MenuAction::Back);
    });
}

fn spawn_slots(commands: &mut Commands, op: SlotOp) {
    let title = match op {
        SlotOp::Load => "Load Game",
        SlotOp::Save => "Save Game",
    };
    screen!(commands, title, panel => {
        for slot in 0..SLOT_COUNT {
            let label = match slot_summary(slot) {
                Some((edits, age)) => format!("Slot {} — {} edits, {}", slot + 1, edits, age_label(age)),
                None => format!("Slot {} — empty", slot + 1),
            };
            menu_button!(panel, theme::button_node(), label, 17.0, MenuAction::Slot(op, slot));
        }
        menu_button!(panel, theme::button_node(), "Back", 18.0, MenuAction::Back);
    });
}

/// Coarse "x ago" label for a save's age in seconds (no date library needed).
fn age_label(secs: u64) -> String {
    if secs < 90 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}
