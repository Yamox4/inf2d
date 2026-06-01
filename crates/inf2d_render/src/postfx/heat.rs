#![deny(unsafe_code)]
//! Heat distortion shimmer overlay. Scrolling noise in a warm tint, intensity
//! peaks at midday. A cheap stand-in for true refractive heat haze (real
//! refraction needs a custom render-graph node to sample the previous pass).
//!
//! ## Path
//!
//! Same fullscreen-`Material2d`-quad pattern as the LUT and god-rays passes:
//! one oversized rectangle parented to the active camera at a fixed local Z.
//! We sit between god rays ([`RenderLayer::POSTFX`] + 0.1) and the optional
//! vignette pass (+ 0.2) at + 0.15.
//!
//! In Bevy 0.18 a `Material2d` fragment shader cannot read the previous
//! render pass's color attachment — that would need a custom render-graph
//! node sampling the view target. Instead the shader emits a procedural
//! "rising haze" pattern (two scrolling noise octaves in a warm tint) that
//! reads as heat shimmer even though no real refraction is happening.

use bevy::asset::{embedded_asset, Asset};
use bevy::math::primitives::Rectangle;
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::daynight::{DayNightCameraTarget, TimeOfDay};
use crate::layers::RenderLayer;

/// Z value the heat-shimmer overlay sits at relative to its camera parent.
/// Between god rays ([`RenderLayer::POSTFX`] + 0.1) and the optional vignette
/// pass (+ 0.2).
pub const HEAT_Z: f32 = RenderLayer::POSTFX + 0.15;

/// `Material2d` backing the fullscreen heat-shimmer overlay quad. One
/// instance, parented to the active camera. All fields land in a single
/// `#[uniform(0)]` block packed by `AsBindGroup`; the WGSL side mirrors the
/// layout exactly.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct HeatMaterial {
    /// Wall-clock time in seconds, fed to the shader to scroll the noise.
    #[uniform(0)]
    pub time: f32,
    /// Strength of the shimmer in `[0, ~0.18]`. Peaks at noon, zero outside
    /// the 10..16h window — see [`heat_strength_for_hour`].
    #[uniform(0)]
    pub strength: f32,
    /// Explicit padding so the uniform block matches the WGSL `struct`
    /// (two `f32`s + `vec2<f32>` pad lines up the `vec4<f32>` tint on the
    /// natural 16-byte boundary).
    #[uniform(0)]
    pub _pad: Vec2,
    /// Warm tint multiplied through the shimmer output. Linear-RGBA.
    #[uniform(0)]
    pub tint: LinearRgba,
}

impl Default for HeatMaterial {
    fn default() -> Self {
        Self {
            time: 0.0,
            strength: 0.0,
            _pad: Vec2::ZERO,
            tint: LinearRgba::new(1.0, 0.85, 0.6, 1.0),
        }
    }
}

impl Material2d for HeatMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://inf2d_render/postfx/heat.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        // The fragment shader writes premultiplied `(rgb * a, a)`; standard
        // alpha blend composites it on top of whatever post-fx pass below
        // (LUT + god rays) already painted.
        AlphaMode2d::Blend
    }
}

/// Marker on the spawned fullscreen heat-shimmer quad so the driver system
/// can query for it.
#[derive(Component, Debug)]
pub struct HeatOverlay;

/// Shared assets for the heat-shimmer pass: the oversized fullscreen mesh and
/// the single material instance whose uniforms the driver rewrites each frame.
#[derive(Resource, Clone)]
pub struct HeatAssets {
    /// Oversized rectangle mesh parented to the camera; sized to always cover
    /// the viewport at any zoom without a window-resize listener.
    pub mesh: Handle<Mesh>,
    /// The single [`HeatMaterial`] instance used by [`HeatOverlay`]. Cloned out
    /// by [`drive_uniforms`] each frame for a mutable lookup in
    /// `Assets<HeatMaterial>`.
    pub material: Handle<HeatMaterial>,
}

/// Plugin: registers the material, embeds the WGSL, builds the shared assets
/// at `Startup`, spawns the camera-parented overlay, and drives uniforms each
/// frame from [`TimeOfDay`] and the elapsed clock.
pub struct HeatPlugin;

impl Plugin for HeatPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "heat.wgsl");

        app.add_plugins(Material2dPlugin::<HeatMaterial>::default())
            .add_systems(Startup, (build_assets, spawn_overlay).chain())
            .add_systems(Update, drive_uniforms);
    }
}

/// Startup system: create the shared mesh and the single material handle, then
/// insert [`HeatAssets`] so [`spawn_overlay`] and [`drive_uniforms`] can reach
/// them.
pub fn build_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<HeatMaterial>>,
) {
    // 100k × 100k unit quad — same "cover any viewport at any zoom" trick the
    // day/night, LUT, and god-rays overlays use.
    let mesh = meshes.add(Mesh::from(Rectangle::new(100_000.0, 100_000.0)));
    let material = materials.add(HeatMaterial::default());
    commands.insert_resource(HeatAssets { mesh, material });
}

/// Startup system: spawn the fullscreen heat-shimmer quad as a child of the
/// active camera. Prefers an entity tagged with [`DayNightCameraTarget`];
/// falls back to the first `Camera2d` it finds.
pub fn spawn_overlay(
    mut commands: Commands,
    assets: Option<Res<HeatAssets>>,
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
        // appear. Apps that need it should spawn their camera during the same
        // Startup pass — same constraint as the other overlays in this crate.
        return;
    };

    let overlay = commands
        .spawn((
            HeatOverlay,
            Mesh2d(assets.mesh.clone()),
            MeshMaterial2d(assets.material.clone()),
            Transform::from_xyz(0.0, 0.0, HEAT_Z),
            Visibility::default(),
            Name::new("HeatOverlay"),
        ))
        .id();
    commands.entity(parent).add_child(overlay);
}

/// Per-frame driver: write the elapsed clock into `time` and the hour-driven
/// midday curve into `strength`. `tint` and `_pad` stay at their defaults.
pub fn drive_uniforms(
    time: Res<Time>,
    tod: Res<TimeOfDay>,
    assets: Option<Res<HeatAssets>>,
    mut materials: ResMut<Assets<HeatMaterial>>,
) {
    let Some(assets) = assets else {
        return;
    };
    let Some(material) = materials.get_mut(&assets.material) else {
        return;
    };
    material.time = time.elapsed_secs();
    material.strength = heat_strength_for_hour(tod.hours);
}

/// Map the current in-game hour to a heat-shimmer strength.
///
/// Zero outside `10..=16h` — heat haze is only visible around midday.
/// Inside that window, a smoothstep ramps the strength from zero at the edges
/// up to a peak of `0.18` at noon (`h == 12`).
fn heat_strength_for_hour(h: f32) -> f32 {
    let dist = (h - 12.0).abs();
    if dist > 3.0 {
        return 0.0;
    }
    let t = 1.0 - (dist / 3.0);
    let smooth = t * t * (3.0 - 2.0 * t);
    smooth * 0.18
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midnight_strength_is_zero() {
        // Far outside the 10..=16 window — early-out to zero.
        assert_eq!(heat_strength_for_hour(0.0), 0.0);
        assert_eq!(heat_strength_for_hour(24.0), 0.0);
    }

    #[test]
    fn ten_oclock_is_on_the_ramp() {
        // h = 10 sits at the lower edge of the window. dist = 2, t = 1/3,
        // smoothstep(1/3) ≈ 0.2593, * 0.18 ≈ 0.0467 — non-zero but well
        // below the peak.
        let s = heat_strength_for_hour(10.0);
        assert!(s > 0.0, "expected non-zero, got {s}");
        assert!(s < 0.05, "expected well below peak, got {s}");
    }

    #[test]
    fn noon_strength_is_peak() {
        // Smoothstep evaluates to 1 at the centre; output equals the cap.
        let s = heat_strength_for_hour(12.0);
        assert!((s - 0.18).abs() < 1e-4, "got {s}");
    }

    #[test]
    fn afternoon_decays_then_zero_outside_window() {
        // 14h mirrors 10h around noon — the smoothstep is symmetric.
        let at_14 = heat_strength_for_hour(14.0);
        let at_10 = heat_strength_for_hour(10.0);
        assert!((at_14 - at_10).abs() < 1e-6, "10h={at_10} 14h={at_14}");
        assert!(at_14 > 0.0 && at_14 < 0.18);

        // 16h is one hour past the window edge — strictly outside.
        assert_eq!(heat_strength_for_hour(16.0), 0.0);
    }
}
