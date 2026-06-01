#![deny(unsafe_code)]
//! Volumetric god rays — a fullscreen radial blur originating at a screen-space
//! sun position, modulated by [`TimeOfDay`]. Visible at dawn (~5-8h) and dusk
//! (~16-19h), fades out toward noon and night.
//!
//! ## Path
//!
//! Same approach as [`crate::daynight::DayNightOverlay`]: a single fullscreen
//! `Material2d` quad parented to the camera at a fixed local Z. We sit at
//! [`RenderLayer::POSTFX`] + 0.1 so the warm shafts composite **on top of**
//! world-space UI baseline but stay **below** any HUD layer. (The 3D-LUT
//! grading runs as a true render-graph post-process — see
//! [`crate::postfx::lut_post`] — and applies independently.)
//!
//! ## Look
//!
//! The shader does a classic 16-sample radial-blur "sun shaft" march in UV
//! space from each pixel toward a screen-anchored sun position, modulated by
//! per-sample procedural fbm. The result is a warm streaked overlay that
//! peaks at **dawn (~6h)** and **dusk (~18h)** — when atmospheric god rays
//! actually look dramatic — and drops to zero at noon (overhead light, no
//! streaks) and through the night.

use bevy::asset::embedded_asset;
use bevy::math::primitives::Rectangle;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::daynight::{DayNightCameraTarget, TimeOfDay};
use crate::layers::RenderLayer;

/// Z value the god-rays overlay quad sits at relative to its camera parent.
/// Sits a small epsilon above [`RenderLayer::POSTFX`] so the rays composite on
/// top of the LUT wash but remain below world-space UI.
pub const GODRAYS_Z: f32 = RenderLayer::POSTFX + 0.1;

/// `Material2d` backing the fullscreen god-rays overlay quad. One instance,
/// parented to the active camera. The shader reads every field from a single
/// `#[uniform(0)]` block packed by `AsBindGroup`.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct GodRaysMaterial {
    /// Unit vector pointing from screen center toward the sun in NDC-ish
    /// coords (`+x = right`, `+y = up`). The shader maps this to a UV anchor
    /// on the edge of the screen and marches each pixel toward it.
    #[uniform(0)]
    pub sun_dir_normalized: Vec2,
    /// Strength of the rays in `[0, 1]`. Peaks at dawn/dusk and drops to zero
    /// at noon and at night — see [`god_rays_strength_for_hour`].
    #[uniform(0)]
    pub sun_strength: f32,
    /// Explicit padding so the uniform block matches the WGSL `struct`
    /// (vec2 + f32 + f32 then a vec4 — keeps the natural 16-byte alignment).
    #[uniform(0)]
    pub _pad: f32,
    /// Warm color the streaks pick up. Linear-RGBA; the shader multiplies it
    /// by accumulated occlusion × falloff × strength.
    #[uniform(0)]
    pub tint: LinearRgba,
}

impl Default for GodRaysMaterial {
    fn default() -> Self {
        Self {
            sun_dir_normalized: Vec2::new(0.0, 1.0),
            sun_strength: 0.0,
            _pad: 0.0,
            tint: default_tint(),
        }
    }
}

impl Material2d for GodRaysMaterial {
    fn fragment_shader() -> ShaderRef {
        // Shader lives in `postfx/`, so the embedded asset path keeps the
        // `postfx/` segment — same convention `lut.wgsl` uses.
        "embedded://inf2d_render/postfx/godrays.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        // Standard alpha blend: the fragment shader emits `(tint*intensity,
        // intensity*0.6)` so the streaks composite as a translucent additive
        // wash without overpowering the LUT pass below.
        AlphaMode2d::Blend
    }
}

/// Marker on the spawned fullscreen god-rays quad so the driver system can
/// query for it.
#[derive(Component, Debug)]
pub struct GodRaysOverlay;

/// Shared assets for the god-rays pass: the oversized fullscreen mesh and the
/// single material instance whose uniforms the driver rewrites each frame.
#[derive(Resource, Clone)]
pub struct GodRaysAssets {
    /// Oversized rectangle mesh parented to the camera; sized to always cover
    /// the viewport at any zoom without a window-resize listener.
    pub mesh: Handle<Mesh>,
    /// The single [`GodRaysMaterial`] instance used by [`GodRaysOverlay`].
    /// Cloned out by [`drive_uniforms`] each frame for a mutable lookup in
    /// `Assets<GodRaysMaterial>`.
    pub material: Handle<GodRaysMaterial>,
}

/// Plugin: registers the material, embeds the WGSL, builds the shared assets
/// at `Startup`, spawns the camera-parented overlay, and drives uniforms each
/// frame from [`TimeOfDay`].
pub struct GodRaysPlugin;

impl Plugin for GodRaysPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "godrays.wgsl");

        app.add_plugins(Material2dPlugin::<GodRaysMaterial>::default())
            .add_systems(Startup, (build_assets, spawn_overlay).chain())
            .add_systems(Update, drive_uniforms);
    }
}

/// Default warm-orange tint used for the rays. Roughly golden-hour sunlight —
/// matches the dusk overlay color in [`crate::daynight`].
fn default_tint() -> LinearRgba {
    Color::srgb(1.0, 0.78, 0.45).to_linear()
}

/// Startup system: create the shared mesh and the single material handle, then
/// insert [`GodRaysAssets`] so [`spawn_overlay`] and [`drive_uniforms`] can
/// reach them.
pub fn build_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GodRaysMaterial>>,
) {
    // 100k × 100k unit quad — same "cover any viewport at any zoom" trick the
    // day/night and LUT overlays use.
    let mesh = meshes.add(Mesh::from(Rectangle::new(100_000.0, 100_000.0)));
    let material = materials.add(GodRaysMaterial::default());
    commands.insert_resource(GodRaysAssets { mesh, material });
}

/// Startup system: spawn the fullscreen god-rays quad as a child of the active
/// camera. Prefers an entity tagged with [`DayNightCameraTarget`]; falls back
/// to the first `Camera2d` it finds.
pub fn spawn_overlay(
    mut commands: Commands,
    assets: Option<Res<GodRaysAssets>>,
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
        // Startup pass — same constraint as DayNightOverlay.
        return;
    };

    let overlay = commands
        .spawn((
            GodRaysOverlay,
            Mesh2d(assets.mesh.clone()),
            MeshMaterial2d(assets.material.clone()),
            Transform::from_xyz(0.0, 0.0, GODRAYS_Z),
            Visibility::default(),
            Name::new("GodRaysOverlay"),
        ))
        .id();
    commands.entity(parent).add_child(overlay);
}

/// Per-frame driver: read [`TimeOfDay`], compute sun strength + screen-space
/// direction, and rewrite the overlay material's uniforms.
pub fn drive_uniforms(
    tod: Res<TimeOfDay>,
    assets: Option<Res<GodRaysAssets>>,
    mut materials: ResMut<Assets<GodRaysMaterial>>,
) {
    let Some(assets) = assets else {
        return;
    };
    let Some(material) = materials.get_mut(&assets.material) else {
        return;
    };
    material.sun_strength = god_rays_strength_for_hour(tod.hours);
    material.sun_dir_normalized = unit_vec_for_hour(tod.hours);
    material.tint = default_tint();
    // `_pad` stays zero — only present to align with the WGSL struct layout.
}

/// Strength curve over the 24h cycle.
///
/// Two triangular peaks of width ±2h centered at dawn (6h) and dusk (18h),
/// clamped to `[0, 1]`. At noon (h=12) both peaks evaluate to zero — the sun
/// is overhead and atmospheric shafts would be vertical and invisible. Through
/// the night both peaks are also zero.
fn god_rays_strength_for_hour(h: f32) -> f32 {
    let dawn = (1.0 - ((h - 6.0).abs() / 2.0).min(1.0)).max(0.0);
    let dusk = (1.0 - ((h - 18.0).abs() / 2.0).min(1.0)).max(0.0);
    (dawn + dusk).min(1.0)
}

/// Screen-space direction toward the sun for the given hour.
///
/// The sun arcs from `(+1, 0)` at sunrise (6h) through `(0, +1)` at noon
/// (12h) to `(-1, 0)` at sunset (18h). After sunset it's below the horizon
/// and we don't care — strength is already zero, so the value of the
/// direction vector outside `[6, 18]` is irrelevant.
///
/// `+y` is up in Bevy 2D, so noon-direction `(0, +1)` is correct; if the
/// runtime look ever inverts, flip the sign on the `sin` term.
fn unit_vec_for_hour(h: f32) -> Vec2 {
    let theta = ((h - 6.0).clamp(0.0, 12.0) / 12.0) * std::f32::consts::PI;
    Vec2::new(theta.cos(), theta.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noon_strength_is_zero() {
        // Sun overhead — no streaks.
        assert!(god_rays_strength_for_hour(12.0) < 1e-4);
    }

    #[test]
    fn midnight_strength_is_zero() {
        // Sun below horizon — no streaks.
        assert!(god_rays_strength_for_hour(0.0) < 1e-4);
        assert!(god_rays_strength_for_hour(24.0) < 1e-4);
    }

    #[test]
    fn dawn_and_dusk_peak_at_one() {
        let dawn = god_rays_strength_for_hour(6.0);
        let dusk = god_rays_strength_for_hour(18.0);
        assert!((dawn - 1.0).abs() < 1e-4, "dawn was {dawn}");
        assert!((dusk - 1.0).abs() < 1e-4, "dusk was {dusk}");
    }

    #[test]
    fn strength_is_clamped_to_unit() {
        // Sweep the whole cycle; nothing should ever exceed 1.0.
        for i in 0..240 {
            let h = i as f32 * 0.1;
            let s = god_rays_strength_for_hour(h);
            assert!((0.0..=1.0).contains(&s), "h={h} produced s={s}");
        }
    }

    #[test]
    fn sunrise_direction_points_east() {
        let v = unit_vec_for_hour(6.0);
        assert!((v.x - 1.0).abs() < 1e-4, "sunrise x: {}", v.x);
        assert!(v.y.abs() < 1e-4, "sunrise y: {}", v.y);
    }

    #[test]
    fn noon_direction_points_up() {
        let v = unit_vec_for_hour(12.0);
        assert!(v.x.abs() < 1e-4, "noon x: {}", v.x);
        assert!((v.y - 1.0).abs() < 1e-4, "noon y: {}", v.y);
    }

    #[test]
    fn sunset_direction_points_west() {
        let v = unit_vec_for_hour(18.0);
        assert!((v.x + 1.0).abs() < 1e-4, "sunset x: {}", v.x);
        assert!(v.y.abs() < 1e-4, "sunset y: {}", v.y);
    }

    #[test]
    fn sun_dir_is_always_unit_in_daylight_window() {
        for i in 60..=180 {
            let h = i as f32 * 0.1;
            let v = unit_vec_for_hour(h);
            assert!((v.length() - 1.0).abs() < 1e-4, "h={h} produced len={}", v.length());
        }
    }
}
