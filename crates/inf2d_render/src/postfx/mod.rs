#![deny(unsafe_code)]
//! Post-process effects.
//!
//! ## LUT color grading
//!
//! The LUT pass is a **real read-from-scene render-graph post-process**: it
//! samples the previous color target via [`ViewTarget::post_process_write`],
//! runs the pixel through two 3D LUTs picked from [`LutPalette`] (cross-faded
//! by [`TimeOfDay`]), and writes back. Implementation lives in
//! [`lut_post::LutPostProcessPlugin`]; the procedural LUT image generators
//! and the day/night selection helpers live in [`lut`].
//!
//! Earlier iterations of this crate used a fullscreen `Material2d` quad
//! parented to the camera that sampled the LUTs at a fixed key tone and laid
//! down a translucent wash. That code is gone — the render-graph node fully
//! replaces it.
//!
//! ## Other overlays
//!
//! [`godrays`], [`heat`], and [`vignette`] remain `Material2d`-on-a-quad
//! overlays. They sit on [`crate::layers::RenderLayer::POSTFX`] (plus small
//! Z biases for ordering). The LUT pass runs in the render graph and does
//! not collide with them.
//!
//! [`ViewTarget::post_process_write`]: bevy::render::view::ViewTarget::post_process_write
//! [`TimeOfDay`]: crate::daynight::TimeOfDay

pub mod godrays;
pub mod heat;
pub mod lut;
pub mod lut_post;
pub mod vignette;

use bevy::prelude::*;

pub use godrays::{GodRaysAssets, GodRaysMaterial, GodRaysOverlay, GodRaysPlugin};
pub use heat::{HeatAssets, HeatMaterial, HeatOverlay, HeatPlugin};
pub use lut::{
    build_lut_image, generate_cool_night_lut, generate_neutral_lut, generate_warm_dusk_lut,
    select_lut_pair, strength_for_hour, LutPalette, LUT_SIZE, LUT_STRIP_HEIGHT, LUT_STRIP_WIDTH,
    MAX_LUT_STRENGTH,
};
pub use lut_post::{LutDriver, LutPostProcessPlugin, LutSettings};
pub use vignette::{VignetteAssets, VignetteMaterial, VignetteOverlay, VignettePlugin};

/// Startup system: synthesize all three LUT images and stash their handles
/// in a [`LutPalette`] resource. Public so the render-graph LUT plugin can
/// schedule its driver `after(build_palette)`.
pub fn build_palette(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let palette = LutPalette {
        neutral: images.add(generate_neutral_lut()),
        warm_dusk: images.add(generate_warm_dusk_lut()),
        cool_night: images.add(generate_cool_night_lut()),
    };
    commands.insert_resource(palette);
}
