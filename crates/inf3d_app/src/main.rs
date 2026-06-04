//! inf3d — 3D voxel game. Plugin composition / binary entry point.

use avian3d::prelude::*;
use bevy::prelude::*;
use bevy::window::{PresentMode, Window, WindowPlugin};

use inf3d_audio::AudioPlugin;
use inf3d_camera::IsoCameraPlugin;
use inf3d_core::CorePlugin;
use inf3d_gameplay::PlayerPlugin;
use inf3d_menu::MenuPlugin;
use inf3d_pathfinding::PathfindPlugin;
use inf3d_physics::PhysicsGamePlugin;
use inf3d_render::{
    CursorPlugin, DustPlugin, EditPlugin, FogPlugin, FoliagePlugin, HighlightPlugin, WaterPlugin,
};
use inf3d_ui::HudPlugin;
use inf3d_world::WorldPlugin;

fn main() {
    let mut app = App::new();
    app
        // AutoVsync for normal play: caps FPS at the monitor refresh so the
        // engine idles between frames instead of pinning CPU+GPU at 100% (which
        // magnifies every other cost and causes thermal throttling). Set
        // `INF3D_UNCAP_FPS=1` to switch to `Immediate` (vsync off, uncapped) for
        // benchmarking the FPS readout.
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                present_mode: if std::env::var("INF3D_UNCAP_FPS").is_ok() {
                    PresentMode::Immediate
                } else {
                    PresentMode::AutoVsync
                },
                ..default()
            }),
            ..default()
        }))
        // CorePlugin must come first: it installs QualitySettings / GrassStats /
        // FrameStats so every downstream plugin can read them at build time.
        .add_plugins(CorePlugin)
        // avian3d ECS physics at its DEFAULT fixed timestep (`FixedPostUpdate`).
        // The kinematic character controller now also runs in `FixedPostUpdate`
        // (after avian's `Writeback`) using the *fixed* delta, and the player
        // carries avian's `TransformInterpolation` so the rendered transform is
        // smoothly eased between fixed ticks (right after `FixedMain`, before
        // `Update`) — that decouples the sim rate from the frame rate, killing
        // the zoom-out jitter the old variable-timestep `PostUpdate` hack tried
        // (and failed) to paper over. `PhysicsInterpolationPlugin` ships inside
        // `PhysicsPlugins` by default, so no extra plugin is needed here. Gravity
        // stays off (the controller applies its own only while airborne).
        .add_plugins(PhysicsPlugins::default())
        .insert_resource(Gravity(Vec3::ZERO))
        .add_plugins(WorldPlugin)
        .add_plugins(PlayerPlugin)
        // Game-specific physics wiring (colliders, character controller,
        // interaction raycast). After PlayerPlugin so the player exists.
        .add_plugins(PhysicsGamePlugin)
        .add_plugins(IsoCameraPlugin)
        .add_plugins(PathfindPlugin)
        .add_plugins(HighlightPlugin)
        .add_plugins(EditPlugin)
        .add_plugins(CursorPlugin)
        .add_plugins(DustPlugin)
        .add_plugins(FogPlugin)
        .add_plugins(HudPlugin)
        // Front-end shell: main menu, pause menu, settings, 3-slot save/load, and
        // the flat test-world stamper. Drives the `AppState`/`Pause` states
        // `CorePlugin` registered; the game boots into the main menu.
        .add_plugins(MenuPlugin)
        .add_plugins(AudioPlugin)
        .add_plugins(WaterPlugin)
        .add_plugins(FoliagePlugin);

    // Read-only telemetry recorder — writes `inf3d-monitor.log` each run. Added
    // last so it observes every other plugin's state. Compiled in only with the
    // `telemetry` feature (on by default for developers; a shipping build uses
    // `--no-default-features` to drop it entirely). When it IS compiled in, the
    // recorder still honors the `INF3D_NO_MONITOR=1` runtime opt-out internally.
    // Inline path so there's no always-compiled top-level `use` to gate.
    #[cfg(feature = "telemetry")]
    app.add_plugins(inf3d_monitor::MonitorPlugin);

    app.run();
}
