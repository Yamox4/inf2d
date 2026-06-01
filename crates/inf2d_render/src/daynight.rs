#![deny(unsafe_code)]
//! Day/night cycle + fullscreen color-grading overlay.
//!
//! A `TimeOfDay` resource cycles 0..24 hours over `DayNightConfig::day_length_secs`
//! seconds (default: 240s = 4 real-time minutes per in-game day). A fullscreen
//! quad parented to the camera samples the resource and tints itself, producing
//! a global color cast on top of the world. Cheap, no custom shader required.

use bevy::prelude::*;

use crate::layers::RenderLayer;

/// In-game time, 0..24. Wraps at 24.
#[derive(Resource, Reflect, Debug, Clone, Copy)]
#[reflect(Resource)]
pub struct TimeOfDay {
    pub hours: f32,
}

impl Default for TimeOfDay {
    fn default() -> Self {
        // Start at 10:00 — pleasant morning light, lets you immediately see the
        // cycle moving toward noon → dusk without an opening night phase.
        Self { hours: 10.0 }
    }
}

/// Tunables for the day/night system.
#[derive(Resource, Reflect, Debug, Clone, Copy)]
#[reflect(Resource)]
pub struct DayNightConfig {
    /// Real-time seconds for a full 24h in-game cycle. 240s = 4 minutes/day.
    pub day_length_secs: f32,
    /// Maximum alpha of the night tint (cap at <1.0 so the world stays visible).
    pub night_alpha_max: f32,
    /// Maximum alpha of the dawn/dusk tint.
    pub dusk_alpha_max: f32,
}

impl Default for DayNightConfig {
    fn default() -> Self {
        Self {
            day_length_secs: 240.0,
            night_alpha_max: 0.28,
            dusk_alpha_max: 0.22,
        }
    }
}

/// Marker on the fullscreen overlay sprite entity.
#[derive(Component, Debug)]
pub struct DayNightOverlay;

/// Marker for the camera entity the overlay should parent to. Apps that want a
/// different camera can use this marker on whichever camera they prefer; if the
/// marker isn't present at startup, the overlay parents to the first `Camera2d`
/// it finds.
#[derive(Component, Debug, Default)]
pub struct DayNightCameraTarget;

/// Z value the overlay sits at relative to its camera parent. High enough to be
/// above gameplay sprites, below `UI`.
pub const DAYNIGHT_Z: f32 = RenderLayer::DAYNIGHT;

/// Startup system: spawns the fullscreen overlay sprite, parented to the camera.
/// The sprite is sized to a single huge quad covering the viewport at any zoom;
/// because the camera uses `ScalingMode::WindowSize`, a 100k×100k sprite parented
/// at z=DAYNIGHT_Z will always cover the viewport. Cheaper than tracking window size.
pub fn spawn_overlay(
    mut commands: Commands,
    cameras_marked: Query<Entity, With<DayNightCameraTarget>>,
    cameras_any: Query<Entity, With<Camera2d>>,
) {
    let parent = cameras_marked
        .iter()
        .next()
        .or_else(|| cameras_any.iter().next());
    let Some(parent) = parent else {
        // No camera yet; the system runs once at Startup. If the camera isn't
        // spawned this frame, the overlay simply doesn't appear — apps that need
        // it should ensure the camera is spawned during the same Startup pass.
        return;
    };

    let overlay = commands
        .spawn((
            DayNightOverlay,
            Sprite {
                color: Color::NONE,
                custom_size: Some(Vec2::splat(100_000.0)),
                ..default()
            },
            Transform::from_xyz(0.0, 0.0, DAYNIGHT_Z),
            Name::new("DayNightOverlay"),
        ))
        .id();
    commands.entity(parent).add_child(overlay);
}

/// `Update` system: advance `TimeOfDay` by real-time delta and rewrite the overlay
/// sprite's color from the current hour. Runs in `CoreSet` so downstream systems
/// (lights, post-fx) can read this frame's `TimeOfDay`.
pub fn advance_and_tint(
    time: Res<Time>,
    config: Res<DayNightConfig>,
    mut tod: ResMut<TimeOfDay>,
    mut q: Query<&mut Sprite, With<DayNightOverlay>>,
) {
    let dt = time.delta_secs();
    if config.day_length_secs > 0.0 {
        let hours_per_sec = 24.0 / config.day_length_secs;
        tod.hours = (tod.hours + dt * hours_per_sec).rem_euclid(24.0);
    }

    let color = tint_for_hour(tod.hours, &config);
    for mut sprite in &mut q {
        sprite.color = color;
    }
}

/// Map hour-of-day to an overlay color. Piecewise mix:
/// - 22:00 → 5:00 : deep navy (night)
/// - 5:00  → 7:00 : crossfade to dawn orange
/// - 7:00  → 17:00: transparent (noon)
/// - 17:00 → 19:00: crossfade to dusk orange
/// - 19:00 → 22:00: crossfade to night
fn tint_for_hour(h: f32, cfg: &DayNightConfig) -> Color {
    // Anchor palette.
    let night = Color::srgba(0.05, 0.07, 0.20, cfg.night_alpha_max);
    let dusk = Color::srgba(0.96, 0.50, 0.18, cfg.dusk_alpha_max);
    let day = Color::srgba(1.0, 1.0, 1.0, 0.0);

    if h < 5.0 || h >= 22.0 {
        return night;
    }
    if (5.0..7.0).contains(&h) {
        return lerp_color(night, dusk, (h - 5.0) / 2.0);
    }
    if (7.0..17.0).contains(&h) {
        // Soft midday curve: peak transparency at noon.
        return lerp_color(dusk, day, ((h - 7.0) / 5.0).min(1.0));
    }
    if (17.0..19.0).contains(&h) {
        return lerp_color(day, dusk, (h - 17.0) / 2.0);
    }
    // 19..22
    lerp_color(dusk, night, (h - 19.0) / 3.0)
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let a = a.to_srgba();
    let b = b.to_srgba();
    Color::srgba(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
        a.alpha + (b.alpha - a.alpha) * t,
    )
}

/// Smooth day/night sun-intensity envelope shared by every renderer subsystem
/// that needs to fade an effect with the sun: water specular, future surface
/// god-ray strength, decal sun-bleach, etc. Keeping one curve means the lit
/// tile material's directional sun, the water shimmer spec, and the day/night
/// overlay all transition in lockstep — no harsh seams when crossing dawn/dusk.
///
/// Returns `0.0` outside `[5, 19]` and ramps smoothly to `1.0` between
/// `5..7` and back down `17..19`, plateauing across the working day.
pub fn sun_strength_for_hour(h: f32) -> f32 {
    let h = h.rem_euclid(24.0);
    if !(5.0..19.0).contains(&h) {
        return 0.0;
    }
    let ramp_up = smoothstep_f32(5.0, 7.0, h);
    let ramp_down = 1.0 - smoothstep_f32(17.0, 19.0, h);
    ramp_up.min(ramp_down).clamp(0.0, 1.0)
}

#[inline]
fn smoothstep_f32(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_strength_zero_at_midnight_one_at_noon() {
        assert_eq!(sun_strength_for_hour(0.0), 0.0);
        assert_eq!(sun_strength_for_hour(23.5), 0.0);
        let noon = sun_strength_for_hour(12.0);
        assert!(noon > 0.99, "expected near-1 at noon, got {noon}");
    }

    #[test]
    fn sun_strength_wraps_negative_hours() {
        // Negative hours fold via rem_euclid into the same midnight darkness.
        assert_eq!(sun_strength_for_hour(-1.0), 0.0);
    }

    #[test]
    fn sun_strength_ramps_monotonically_in_dawn_window() {
        let a = sun_strength_for_hour(5.5);
        let b = sun_strength_for_hour(6.0);
        let c = sun_strength_for_hour(6.5);
        assert!(a < b && b < c);
    }
}
