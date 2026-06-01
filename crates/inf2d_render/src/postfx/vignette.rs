#![deny(unsafe_code)]
//! Radial screen-edge vignette. Constant base + time-of-day boost (stronger
//! at night).
//!
//! ## Path
//!
//! Same camera-parented `Material2d` quad approach used by
//! [`crate::daynight::DayNightOverlay`] and
//! [`crate::postfx::godrays::GodRaysPlugin`]: one fullscreen quad at a fixed
//! local Z above the camera, with a tiny `#[uniform(0)]` block the shader
//! consumes. We sit at [`RenderLayer::POSTFX`] + 0.2 — above the god-rays
//! streaks so the vignette frames everything below it, but still below
//! world-space UI. (LUT color grading runs as a render-graph post-process
//! instead — see [`crate::postfx::lut_post`].)
//!
//! ## Look
//!
//! Two intensity drivers stacked into a single `strength` uniform:
//!
//! 1. A constant base of `0.10` so the screen edges always have a gentle
//!    frame — kills sterile flat corners during the day.
//! 2. A time-of-day boost shaped as `(distance_from_noon / 12)^1.5 * 0.45`,
//!    peaking at midnight and falling to zero at noon. Range of the sum is
//!    `[0.10, 0.55]`.

use bevy::asset::embedded_asset;
use bevy::math::primitives::Rectangle;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::daynight::{DayNightCameraTarget, TimeOfDay};
use crate::layers::RenderLayer;

/// Z value the vignette overlay quad sits at relative to its camera parent.
/// Sits a small epsilon above the god-rays pass so the vignette frames
/// everything else but stays below world-space UI.
pub const VIGNETTE_Z: f32 = RenderLayer::POSTFX + 0.2;

/// Constant base vignette strength applied at every hour of the cycle. Keeps
/// the screen edges subtly framed even at noon — chosen so it reads as a
/// "cinematic frame" rather than a tunnel.
const BASE_STRENGTH: f32 = 0.10;

/// Peak boost added on top of [`BASE_STRENGTH`] at midnight, ramping down to
/// zero at noon. Picked so total midnight strength (`0.55`) darkens the
/// corners noticeably without crushing them to pure black.
const NIGHT_BOOST_PEAK: f32 = 0.22;

/// Falloff exponent applied to the linear day/night ramp before scaling by
/// [`NIGHT_BOOST_PEAK`]. > 1 keeps daylight nearly base-only and bends the
/// curve toward "ramps up sharply in the last few hours before midnight".
const NIGHT_BOOST_EXPONENT: f32 = 1.5;

/// `Material2d` backing the fullscreen vignette overlay. Single instance,
/// parented to the active camera. The shader reads every field from one
/// `#[uniform(0)]` block packed by `AsBindGroup`.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct VignetteMaterial {
    /// Overall blackness at the corners, in `[0, 1]`. Driven each frame by
    /// [`drive_uniforms`] from [`TimeOfDay`].
    #[uniform(0)]
    pub strength: f32,
    /// Falloff power applied to the radial mask. Larger = tighter / softer
    /// edge; `2.5` keeps the darkening clearly outside the centre 60%.
    #[uniform(0)]
    pub falloff_power: f32,
    /// Padding so the uniform block matches the WGSL `struct` layout
    /// (`f32 + f32 + vec2 + vec4` = 32 bytes, naturally 16-byte aligned).
    #[uniform(0)]
    pub _pad: Vec2,
    /// Colour of the darkening. Near-black with a hint of blue, applied
    /// multiplicatively against the mask — so the vignette tints toward
    /// "cold cinema" rather than pure black.
    #[uniform(0)]
    pub tint: LinearRgba,
}

impl Default for VignetteMaterial {
    fn default() -> Self {
        Self {
            strength: 0.0,
            falloff_power: 2.5,
            _pad: Vec2::ZERO,
            tint: default_tint(),
        }
    }
}

impl Material2d for VignetteMaterial {
    fn fragment_shader() -> ShaderRef {
        // Shader lives in `postfx/`, so the embedded asset path keeps the
        // `postfx/` segment — same convention `lut.wgsl` and `godrays.wgsl`
        // use.
        "embedded://inf2d_render/postfx/vignette.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        // Standard alpha blend: the shader emits `(tint*darken, darken)`, so
        // the corners composite over whatever was already drawn rather than
        // replacing it.
        AlphaMode2d::Blend
    }
}

/// Marker on the spawned fullscreen vignette quad so the driver system can
/// query for it (and so debug tooling can find it by name).
#[derive(Component, Debug)]
pub struct VignetteOverlay;

/// Shared assets for the vignette pass: the oversized fullscreen mesh and
/// the single material instance whose uniforms the driver rewrites each
/// frame.
#[derive(Resource, Clone)]
pub struct VignetteAssets {
    /// Oversized rectangle mesh parented to the camera; sized to always
    /// cover the viewport at any zoom without a window-resize listener.
    pub mesh: Handle<Mesh>,
    /// The single [`VignetteMaterial`] instance used by
    /// [`VignetteOverlay`]. Mutated each frame in [`drive_uniforms`].
    pub material: Handle<VignetteMaterial>,
}

/// Plugin: registers the material, embeds the WGSL, builds the shared
/// assets at `Startup`, spawns the camera-parented overlay, and drives
/// uniforms each frame from [`TimeOfDay`].
pub struct VignettePlugin;

impl Plugin for VignettePlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "vignette.wgsl");

        app.add_plugins(Material2dPlugin::<VignetteMaterial>::default())
            .add_systems(Startup, (build_assets, spawn_overlay).chain())
            .add_systems(Update, drive_uniforms);
    }
}

/// Default tint applied to the vignette: near-black with a hint of blue.
/// Reads as "cold cinema framing" rather than pure black, which keeps the
/// darkened corners feeling like atmosphere instead of a UI mask.
fn default_tint() -> LinearRgba {
    LinearRgba::new(0.04, 0.03, 0.08, 1.0)
}

/// Startup system: create the shared mesh and the single material handle,
/// then insert [`VignetteAssets`] so [`spawn_overlay`] and
/// [`drive_uniforms`] can reach them.
pub fn build_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VignetteMaterial>>,
) {
    // 100k × 100k unit quad — same "cover any viewport at any zoom" trick
    // the day/night, LUT, and god-rays overlays use.
    let mesh = meshes.add(Mesh::from(Rectangle::new(100_000.0, 100_000.0)));
    let material = materials.add(VignetteMaterial::default());
    commands.insert_resource(VignetteAssets { mesh, material });
}

/// Startup system: spawn the fullscreen vignette quad as a child of the
/// active camera. Prefers an entity tagged with [`DayNightCameraTarget`];
/// falls back to the first `Camera2d` it finds.
pub fn spawn_overlay(
    mut commands: Commands,
    assets: Option<Res<VignetteAssets>>,
    cameras_marked: Query<Entity, With<DayNightCameraTarget>>,
    cameras_any: Query<Entity, With<Camera2d>>,
) {
    let Some(assets) = assets else {
        // `build_assets` is chained before this system; if it didn't run
        // (resource missing), bail rather than panicking.
        return;
    };
    let parent = cameras_marked
        .iter()
        .next()
        .or_else(|| cameras_any.iter().next());
    let Some(parent) = parent else {
        // No camera spawned yet during Startup; the overlay simply won't
        // appear. Apps that need it should spawn their camera during the
        // same Startup pass — same constraint as the other postfx
        // overlays.
        return;
    };

    let overlay = commands
        .spawn((
            VignetteOverlay,
            Mesh2d(assets.mesh.clone()),
            MeshMaterial2d(assets.material.clone()),
            Transform::from_xyz(0.0, 0.0, VIGNETTE_Z),
            Visibility::default(),
            Name::new("VignetteOverlay"),
        ))
        .id();
    commands.entity(parent).add_child(overlay);
}

/// Per-frame driver: read [`TimeOfDay`], compute the combined
/// base + night-boost strength, and write it onto the overlay material.
pub fn drive_uniforms(
    tod: Res<TimeOfDay>,
    assets: Option<Res<VignetteAssets>>,
    mut materials: ResMut<Assets<VignetteMaterial>>,
) {
    let Some(assets) = assets else {
        return;
    };
    let Some(material) = materials.get_mut(&assets.material) else {
        return;
    };
    material.strength = vignette_strength_for_hour(tod.hours);
    // `falloff_power`, `_pad`, and `tint` remain at their defaults — only
    // `strength` is time-driven for now.
}

/// Strength curve over the 24h cycle.
///
/// - Base of [`BASE_STRENGTH`] at every hour — gentle constant frame.
/// - On top of that, a night boost shaped as
///   `(|h - 12| / 12)^NIGHT_BOOST_EXPONENT * NIGHT_BOOST_PEAK`. Peaks at
///   midnight (`h = 0` or `h = 24`), zero at noon (`h = 12`).
///
/// Returns a value in `[BASE_STRENGTH, BASE_STRENGTH + NIGHT_BOOST_PEAK]`
/// — i.e. `[0.10, 0.55]` with the defaults.
fn vignette_strength_for_hour(h: f32) -> f32 {
    let dist_from_noon = (h - 12.0).abs();
    let night_factor = (dist_from_noon / 12.0).clamp(0.0, 1.0);
    let night_boost = night_factor.powf(NIGHT_BOOST_EXPONENT) * NIGHT_BOOST_PEAK;
    BASE_STRENGTH + night_boost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noon_is_base_only() {
        // Sun overhead — only the constant base remains.
        let s = vignette_strength_for_hour(12.0);
        assert!((s - BASE_STRENGTH).abs() < 1e-4, "noon strength was {s}");
    }

    #[test]
    fn midnight_is_full_strength() {
        // Both ends of the cycle should map to the same midnight peak.
        let s0 = vignette_strength_for_hour(0.0);
        let s24 = vignette_strength_for_hour(24.0);
        let expected = BASE_STRENGTH + NIGHT_BOOST_PEAK;
        assert!((s0 - expected).abs() < 1e-4, "0h strength was {s0}");
        assert!((s24 - expected).abs() < 1e-4, "24h strength was {s24}");
    }

    #[test]
    fn dawn_is_between_base_and_peak() {
        // Dawn (6h) is 6h from noon → night_factor = 0.5 → boost is
        // `0.5^1.5 * 0.45 ≈ 0.1591`, total ≈ 0.2591.
        let s = vignette_strength_for_hour(6.0);
        let expected = BASE_STRENGTH + 0.5_f32.powf(NIGHT_BOOST_EXPONENT) * NIGHT_BOOST_PEAK;
        assert!((s - expected).abs() < 1e-4, "dawn strength was {s}");
        assert!(s > BASE_STRENGTH);
        assert!(s < BASE_STRENGTH + NIGHT_BOOST_PEAK);
    }

    #[test]
    fn dusk_matches_dawn() {
        // The curve is symmetric around noon, so 6h and 18h should agree.
        let dawn = vignette_strength_for_hour(6.0);
        let dusk = vignette_strength_for_hour(18.0);
        assert!((dawn - dusk).abs() < 1e-4, "dawn={dawn} dusk={dusk}");
    }

    #[test]
    fn strength_stays_in_expected_band() {
        // Sweep the whole cycle; nothing should escape [base, base+peak].
        let lo = BASE_STRENGTH;
        let hi = BASE_STRENGTH + NIGHT_BOOST_PEAK;
        for i in 0..=240 {
            let h = i as f32 * 0.1;
            let s = vignette_strength_for_hour(h);
            assert!(
                (lo - 1e-4..=hi + 1e-4).contains(&s),
                "h={h} produced s={s} (band [{lo}, {hi}])"
            );
        }
    }
}
