#![deny(unsafe_code)]
//! Procedural 3D look-up-table generation.
//!
//! A 3D LUT maps every input RGB color to an output RGB color. Storing the
//! cube directly would need a true 3D texture; the industry-standard trick
//! (Unity, Unreal, Reshade, …) is to unroll the depth axis into a horizontal
//! strip of 2D slices. With `LUT_SIZE = 64`:
//!
//! - Image size: `64 * 64` wide × `64` tall = `4096 × 64` pixels.
//! - Slice index `b ∈ 0..64` maps to the **blue** input.
//! - Within slice `b`, pixel `(x, y)` carries the output of input
//!   `(r, g, b) = (x / 63, y / 63, b / 63)`.
//!
//! Three palettes are generated at startup; they all share the same layout
//! so the shader can sample any two and cross-fade between them.

use bevy::asset::{Handle, RenderAssetUsages};
use bevy::ecs::resource::Resource;
use bevy::image::{Image, ImageSampler};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Maximum opacity of the LUT effect. Kept modest so the world stays
/// recognisable through the grade — the LUT pushes mood, not identity.
pub const MAX_LUT_STRENGTH: f32 = 0.85;

/// Holds the three procedurally-built LUT image handles for the lifetime of
/// the app. The post-process driver picks two of these each frame based on
/// [`crate::daynight::TimeOfDay`] and feeds them to the
/// [`super::lut_post::LutPostProcessNode`].
#[derive(Resource, Clone, Debug)]
pub struct LutPalette {
    /// Identity LUT — output equals input. Used during full daylight.
    pub neutral: Handle<Image>,
    /// Warm dusk grade — boosted reds, soft S-curve, golden-hour feel.
    pub warm_dusk: Handle<Image>,
    /// Cool night grade — hue rotated toward blue, desaturated, value crushed.
    pub cool_night: Handle<Image>,
}

/// Encodes the LUT-pair selection logic:
/// - `5..7`:   neutral → warm dusk
/// - `7..17`:  pure neutral
/// - `17..19`: neutral → warm dusk
/// - else:     warm dusk → cool night
///
/// Returns the two LUT handles and a `t ∈ [0, 1]` cross-fade weight.
pub fn select_lut_pair(
    hours: f32,
    palette: &LutPalette,
) -> (Handle<Image>, Handle<Image>, f32) {
    if (5.0..7.0).contains(&hours) {
        let t = (hours - 5.0) / 2.0;
        (palette.neutral.clone(), palette.warm_dusk.clone(), t)
    } else if (7.0..17.0).contains(&hours) {
        (palette.neutral.clone(), palette.neutral.clone(), 0.0)
    } else if (17.0..19.0).contains(&hours) {
        let t = (hours - 17.0) / 2.0;
        (palette.neutral.clone(), palette.warm_dusk.clone(), t)
    } else {
        // 19..24 and 0..5 — dusk fading to night and back. Map both halves
        // onto a 0..1 ramp where 0 is "fresh after sunset" and 1 is
        // "deepest night just before dawn".
        let t = if hours >= 19.0 {
            ((hours - 19.0) / 10.0).clamp(0.0, 1.0) // 19..24 → 0..0.5
        } else {
            ((hours + 5.0) / 10.0).clamp(0.0, 1.0) // 0..5 → 0.5..1
        };
        (palette.warm_dusk.clone(), palette.cool_night.clone(), t)
    }
}

/// Map the current hour to an overall LUT strength.
///
/// Daylight (7..17) holds at 0 — the neutral LUT is unchanged anyway, but
/// zeroing strength makes the pass a literal bypass and avoids any blend
/// rounding error. Outside daylight we ease up to [`MAX_LUT_STRENGTH`].
pub fn strength_for_hour(hours: f32) -> f32 {
    if (7.0..17.0).contains(&hours) {
        return 0.0;
    }
    // Distance, in hours, from the nearest daylight edge.
    let edge_distance = if hours < 7.0 {
        7.0 - hours
    } else if hours < 19.0 {
        hours - 17.0
    } else {
        // Wrap around midnight: distance to 7.0 next morning.
        (24.0 - hours) + 7.0
    };
    // Fully ramped after 3 hours away from daylight.
    let ramp = (edge_distance / 3.0).clamp(0.0, 1.0);
    ramp * MAX_LUT_STRENGTH
}

/// Per-axis resolution of the 3D LUT cube (64³ entries → industry standard).
pub const LUT_SIZE: u32 = 64;
/// Width of the unrolled 2D strip: one 64-pixel slice per blue level.
pub const LUT_STRIP_WIDTH: u32 = LUT_SIZE * LUT_SIZE;
/// Height of the unrolled 2D strip: a single 64-pixel-tall band.
pub const LUT_STRIP_HEIGHT: u32 = LUT_SIZE;

/// Build a LUT image by evaluating `map_fn` at every cube entry.
///
/// `map_fn` receives the **linear** input color as `[r, g, b]` in `[0, 1]`
/// and must return the desired output color in the same range. The result is
/// uploaded as `Rgba8UnormSrgb` so the GPU automatically handles the sRGB
/// transfer when the shader samples it.
pub fn build_lut_image(map_fn: impl Fn([f32; 3]) -> [f32; 3]) -> Image {
    let w = LUT_STRIP_WIDTH;
    let h = LUT_STRIP_HEIGHT;
    let mut buf = vec![0u8; (w * h * 4) as usize];

    let scale = 1.0 / (LUT_SIZE as f32 - 1.0);

    for b_idx in 0..LUT_SIZE {
        let b = b_idx as f32 * scale;
        for y in 0..LUT_SIZE {
            let g = y as f32 * scale;
            for x in 0..LUT_SIZE {
                let r = x as f32 * scale;
                let out = map_fn([r, g, b]);

                let px_x = b_idx * LUT_SIZE + x;
                let px_y = y;
                let off = ((px_y * w + px_x) * 4) as usize;

                buf[off] = quantize(out[0]);
                buf[off + 1] = quantize(out[1]);
                buf[off + 2] = quantize(out[2]);
                buf[off + 3] = 255;
            }
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        buf,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    // Linear filtering across slice borders is *wrong* for a true 3D LUT (you
    // would bleed neighbouring blue slices together), but at this resolution
    // and given the LUT is sampled via explicit math in the shader, it's
    // harmless and avoids any pixel-aligned shimmering.
    image.sampler = ImageSampler::linear();
    image
}

/// Identity LUT: every input color maps to itself. Used as the "no grading"
/// anchor for the day-time stretch of the cycle.
pub fn generate_neutral_lut() -> Image {
    build_lut_image(|rgb| rgb)
}

/// Warm dusk grade: push reds, slight magenta cast, then a mild contrast
/// S-curve so the result feels golden-hour rather than just pink.
pub fn generate_warm_dusk_lut() -> Image {
    build_lut_image(|[r, g, b]| {
        let r = (r + 0.10).clamp(0.0, 1.0);
        let g = (g + 0.02).clamp(0.0, 1.0);
        let b = (b - 0.05).clamp(0.0, 1.0);
        [smoothstep01(r), smoothstep01(g), smoothstep01(b)]
    })
}

/// Cool night grade: rotate hue 10° toward blue, desaturate 25%, multiply
/// value by 0.7 — moonlit silver-blue without going fully monochrome.
pub fn generate_cool_night_lut() -> Image {
    build_lut_image(|rgb| {
        let mut hsv = rgb_to_hsv(rgb);
        // Hue is in [0, 360). Pushing **toward** blue (240°) by 10° means: if
        // we're below 240, rotate up; if we're above 240, rotate down. The
        // simple sign-aware nudge below is cheap and produces the intended
        // cool cast without flipping warm hues across the wheel.
        let toward_blue = (240.0 - hsv[0]).signum();
        hsv[0] = (hsv[0] + toward_blue * 10.0).rem_euclid(360.0);
        hsv[1] *= 0.75;
        hsv[2] *= 0.7;
        hsv_to_rgb(hsv)
    })
}

#[inline]
fn quantize(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Classic Hermite smoothstep on `[0, 1]`: `3t² - 2t³`. Slight contrast lift
/// — crushes blacks marginally, eases the roll-off into white.
#[inline]
fn smoothstep01(x: f32) -> f32 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Convert linear RGB in `[0, 1]` to HSV with `H ∈ [0, 360)`, `S,V ∈ [0, 1]`.
fn rgb_to_hsv([r, g, b]: [f32; 3]) -> [f32; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let h = if delta <= f32::EPSILON {
        0.0
    } else if (max - r).abs() < f32::EPSILON {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let s = if max <= f32::EPSILON { 0.0 } else { delta / max };
    [h.rem_euclid(360.0), s, max]
}

/// Inverse of [`rgb_to_hsv`].
fn hsv_to_rgb([h, s, v]: [f32; 3]) -> [f32; 3] {
    let c = v * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r1 + m, g1 + m, b1 + m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_lut_has_expected_dimensions() {
        let img = generate_neutral_lut();
        let size = img.texture_descriptor.size;
        assert_eq!(size.width, LUT_STRIP_WIDTH);
        assert_eq!(size.height, LUT_STRIP_HEIGHT);
        assert_eq!(size.depth_or_array_layers, 1);
        assert_eq!(img.texture_descriptor.format, TextureFormat::Rgba8UnormSrgb);
    }

    /// Neutral LUT must round-trip near-identity for a few sample points.
    #[test]
    fn neutral_lut_is_identity() {
        let img = generate_neutral_lut();
        let data = img.data.as_ref().expect("neutral lut has cpu data");

        // Sample (r=0, g=0, b=0) → black.
        let off = 0usize;
        assert_eq!(data[off], 0);
        assert_eq!(data[off + 1], 0);
        assert_eq!(data[off + 2], 0);

        // Sample max (r=63, g=63, b=63) → white.
        let last_slice_x = (LUT_SIZE - 1) * LUT_SIZE + (LUT_SIZE - 1);
        let last_y = LUT_SIZE - 1;
        let off = ((last_y * LUT_STRIP_WIDTH + last_slice_x) * 4) as usize;
        assert_eq!(data[off], 255);
        assert_eq!(data[off + 1], 255);
        assert_eq!(data[off + 2], 255);
    }

    /// The warm grade must brighten mid-gray's red channel relative to neutral.
    #[test]
    fn warm_lut_pushes_red() {
        let neutral = generate_neutral_lut();
        let warm = generate_warm_dusk_lut();
        let n = neutral.data.as_ref().expect("data");
        let w = warm.data.as_ref().expect("data");

        let mid = LUT_SIZE / 2;
        let slice_x = mid * LUT_SIZE + mid; // (r=mid, g=mid)
        let off = ((mid * LUT_STRIP_WIDTH + slice_x) * 4) as usize;

        assert!(w[off] > n[off], "warm r {} should exceed neutral r {}", w[off], n[off]);
    }

    /// The cool grade must darken mid-gray (value × 0.7) relative to neutral.
    #[test]
    fn cool_lut_crushes_value() {
        let neutral = generate_neutral_lut();
        let cool = generate_cool_night_lut();
        let n = neutral.data.as_ref().expect("data");
        let c = cool.data.as_ref().expect("data");

        let mid = LUT_SIZE / 2;
        let slice_x = mid * LUT_SIZE + mid;
        let off = ((mid * LUT_STRIP_WIDTH + slice_x) * 4) as usize;

        // All channels darker (with a small slack for hsv rounding).
        assert!(c[off] < n[off], "cool r should be darker");
        assert!(c[off + 1] < n[off + 1], "cool g should be darker");
        assert!(c[off + 2] <= n[off + 2] + 2, "cool b should be at most slightly brighter");
    }

    #[test]
    fn hsv_roundtrip_is_stable_for_mid_gray() {
        let mid = [0.5, 0.5, 0.5];
        let back = hsv_to_rgb(rgb_to_hsv(mid));
        for i in 0..3 {
            assert!((back[i] - mid[i]).abs() < 1e-4, "channel {i}: {} vs {}", back[i], mid[i]);
        }
    }

    #[test]
    fn daylight_strength_is_zero() {
        assert_eq!(strength_for_hour(12.0), 0.0);
        assert_eq!(strength_for_hour(7.0), 0.0);
        assert_eq!(strength_for_hour(16.999), 0.0);
    }

    #[test]
    fn night_strength_is_capped() {
        let s = strength_for_hour(2.0);
        assert!((s - MAX_LUT_STRENGTH).abs() < 1e-4, "got {s}");
    }

    #[test]
    fn dusk_blends_neutral_to_warm() {
        let palette = LutPalette {
            neutral: Handle::default(),
            warm_dusk: Handle::default(),
            cool_night: Handle::default(),
        };
        let (_, _, t0) = select_lut_pair(5.0, &palette);
        let (_, _, t1) = select_lut_pair(6.0, &palette);
        assert!((t0 - 0.0).abs() < 1e-4);
        assert!((t1 - 0.5).abs() < 1e-4);
    }

    #[test]
    fn midday_pair_is_neutral_neutral() {
        let palette = LutPalette {
            neutral: Handle::default(),
            warm_dusk: Handle::default(),
            cool_night: Handle::default(),
        };
        let (_, _, t) = select_lut_pair(12.0, &palette);
        assert_eq!(t, 0.0);
    }
}
