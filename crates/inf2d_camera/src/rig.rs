use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::{Bloom, BloomCompositeMode};
use bevy::prelude::*;
use bevy::render::view::Hdr;
use inf2d_world::ChunkStreamFocus;
use serde::{Deserialize, Serialize};

use crate::shake::ActiveShake;

/// The single camera rig entity. Owns the *logical* camera state (target, zoom,
/// smoothing buffers, optional shake offset) — actual `Transform` and `Projection`
/// are derived from this each frame by the pan/zoom/follow systems.
#[derive(Component, Reflect, Debug, Clone, Copy)]
#[reflect(Component)]
pub struct CameraRig {
    /// Current operating mode (free pan, entity follow, or cinematic).
    pub mode: CameraMode,
    /// World-space point the rig is centered on. Pan/follow systems write here;
    /// `Transform.translation.xy()` is reconciled from this value.
    pub target: Vec2,
    /// Current orthographic scale (`projection.scale`). Lower = more zoomed in.
    pub zoom: f32,
    /// Scale we are smoothing toward. Scroll input mutates this; `zoom` chases it.
    pub zoom_target: f32,
    /// Transient additive offset for screen-shake-style effects.
    pub shake: Vec2,
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            mode: CameraMode::Free,
            target: Vec2::ZERO,
            zoom: 1.0,
            zoom_target: 1.0,
            shake: Vec2::ZERO,
        }
    }
}

/// Camera control mode. Switching modes is just a write to [`CameraRig::mode`];
/// no system needs to be re-registered.
#[derive(Reflect, Debug, Clone, Copy)]
pub enum CameraMode {
    /// User-driven: pan and zoom from input act directly on the rig.
    Free,
    /// Follow an entity's `GlobalTransform`. `lag` is the exponential-smoothing
    /// rate (higher = snappier; same units as [`CameraTuning::zoom_smoothing`]).
    Follow { entity: Entity, lag: f32 },
    /// Reserved for scripted-path playback. Present so downstream consumers can
    /// already `match` exhaustively before the cinematic system lands.
    Cinematic,
}

/// Tunable knobs for the camera rig. Held as a `Resource` so it can be live-edited
/// from an inspector and persisted via `serde`.
#[derive(Resource, Reflect, Debug, Clone, Copy, Serialize, Deserialize)]
#[reflect(Resource)]
pub struct CameraTuning {
    /// World-units-per-screen-pixel multiplier on raw drag deltas. The pan system
    /// already scales by `projection.scale`, so `1.0` keeps things 1:1; raise to
    /// over-pan, lower to feel "heavier".
    pub pan_speed: f32,
    /// Minimum orthographic scale (most zoomed-in).
    pub zoom_min: f32,
    /// Maximum orthographic scale (most zoomed-out).
    pub zoom_max: f32,
    /// Multiplicative factor applied per scroll notch.
    pub zoom_step: f32,
    /// Exponential-smoothing rate for zoom interpolation. Larger = snappier.
    pub zoom_smoothing: f32,
}

impl Default for CameraTuning {
    fn default() -> Self {
        Self {
            pan_speed: 1.0,
            zoom_min: 0.25,
            zoom_max: 4.0,
            zoom_step: 1.15,
            zoom_smoothing: 12.0,
        }
    }
}

/// One-shot bundle that spawns a fully-wired camera rig:
///
/// - `Camera2d` marker, plus an explicit `Camera` with `hdr = true` (this override
///   suppresses the auto-inserted `Camera` from `Camera2d`'s required-components
///   contract so the render graph allocates an HDR target);
/// - an orthographic `Projection` and identity `Transform`;
/// - `Tonemapping::TonyMcMapface` (the HDR-friendly default; pairs well with bloom);
/// - a tuned 2D `Bloom` post-process (low intensity, high prefilter threshold,
///   additive compositing — gentle "glow" rather than the PBR-emissive halo);
/// - `ChunkStreamFocus` so the world streamer follows the camera;
/// - a debug `Name`.
///
/// HDR + bloom are **on by default**. Most sprite/tile shaders write sRGB values
/// clamped to `[0, 1]`, so the bloom prefilter (`threshold = 0.7`) keeps normal
/// tiles untouched; only pixels tinted brighter than ~`0.7` (e.g. emissive light
/// materials, water specular) will bleed into the bloom buffer.
#[derive(Bundle)]
pub struct CameraRigBundle {
    pub rig: CameraRig,
    pub camera_2d: Camera2d,
    pub hdr: Hdr,
    pub projection: Projection,
    pub transform: Transform,
    pub tonemapping: Tonemapping,
    pub bloom: Bloom,
    pub stream_focus: ChunkStreamFocus,
    pub active_shake: ActiveShake,
    pub name: Name,
}

impl Default for CameraRigBundle {
    fn default() -> Self {
        Self {
            rig: CameraRig::default(),
            camera_2d: Camera2d,
            // `Hdr` is a marker component in Bevy 0.18; attaching it to a `Camera2d`
            // tells the render graph to allocate an Rgba16Float color target instead
            // of an Rgba8Unorm one, which is what bloom + tonemapping need.
            hdr: Hdr,
            projection: Projection::from(OrthographicProjection {
                scale: 1.0,
                ..OrthographicProjection::default_2d()
            }),
            transform: Transform::from_xyz(0.0, 0.0, 999.0),
            tonemapping: Tonemapping::TonyMcMapface,
            bloom: default_2d_bloom(),
            stream_focus: ChunkStreamFocus,
            active_shake: ActiveShake::default(),
            name: Name::new("CameraRig"),
        }
    }
}

// Bloom configuration tuned for 2D / pixel-art-ish content.
//
// Defaults inherited from `Bloom::default()` are aggressive (designed for PBR
// emissive materials). For 2D we dial intensity down, bias toward low-frequency
// "glow" rather than wide halos, push the prefilter threshold up so only the
// brightest pixels (torches, water spec, magic VFX) blow out, and switch to
// additive compositing which reads more "neon" on flat colors.
fn default_2d_bloom() -> Bloom {
    let mut bloom = Bloom {
        intensity: 0.15,
        low_frequency_boost: 0.7,
        composite_mode: BloomCompositeMode::Additive,
        ..Bloom::default()
    };
    // Only blow out pixels brighter than ~0.7 in any channel. Sprite shaders
    // clamp to [0, 1] so this effectively gates bloom to emissive overrides
    // (Sprite::color tinted > 0.7) and future >1.0 HDR materials.
    bloom.prefilter.threshold = 0.7;
    bloom
}

/// Startup system: spawns a single [`CameraRigBundle`]. Apps that want a different
/// initial state can skip this and spawn the bundle themselves.
pub fn spawn_camera(mut commands: Commands) {
    commands.spawn(CameraRigBundle::default());
}
