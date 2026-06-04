//! A custom, procedurally-drawn mouse cursor: a glowing gold arrow with a dark
//! outline, built in code (no asset file) and installed on the primary window at
//! startup. The gold matches the destination tile-outline theme.
//!
//! The arrow is rasterized 4× supersampled and box-downscaled to 32×32 so its
//! edges are smoothly anti-aliased. Winit accepts the raw RGBA8 bytes directly;
//! the image only needs to live in the main-world `Assets<Image>` (which
//! `RenderAssetUsages::default()` guarantees) for `bevy_winit` to read it.

use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    window::{CursorIcon, CustomCursor, CustomCursorImage, PrimaryWindow},
};

/// Final cursor edge length in pixels.
const SIZE: u32 = 32;
/// Supersample factor for anti-aliasing (rasterize at `SIZE * SS`, then average).
const SS: u32 = 4;

pub struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, install_cursor);
    }
}

/// Build the cursor image once and attach it to the primary window. The hotspot
/// (the pixel that tracks the actual pointer position) sits on the arrow tip.
fn install_cursor(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    window: Query<Entity, With<PrimaryWindow>>,
) {
    let Ok(window) = window.single() else {
        return;
    };
    let handle = images.add(build_cursor_image());
    commands
        .entity(window)
        .insert(CursorIcon::Custom(CustomCursor::Image(CustomCursorImage {
            handle,
            texture_atlas: None,
            flip_x: false,
            flip_y: false,
            rect: None,
            hotspot: (1, 1),
        })));
}

/// Rasterize the gold arrow into an RGBA8 image.
fn build_cursor_image() -> Image {
    // Classic pointer outline (tip at the origin), in a ~12×19 unit grid; scaled
    // up to nearly fill the 32-px canvas, offset 1 px so the tip clears the edge.
    const BASE: [(f32, f32); 7] = [
        (0.0, 0.0),   // tip
        (0.0, 16.0),  // bottom of the left edge (heel)
        (4.0, 12.0),  // concave notch
        (7.0, 19.0),  // tail spike point
        (9.0, 18.0),  // tail spike right
        (6.0, 11.0),  // inner, back up to the body
        (12.0, 11.0), // right wing tip
    ];
    // 0.952 = 1.12 × 0.85 — 15% smaller than the previous arrow (≈40% below the
    // original 1.6× full size). Tip stays ~1 px from the canvas edge so the (1,1)
    // hotspot still lands on the arrow tip.
    let verts: Vec<(f32, f32)> = BASE
        .iter()
        .map(|&(x, y)| (x * 0.952 + 1.0, y * 0.952 + 1.0))
        .collect();

    // Dark outer rim width, in 32-px space.
    let outline_w = 1.3_f32;

    let big = SIZE * SS;
    // Per-output-pixel accumulator of premultiplied color + coverage.
    let mut acc = vec![[0.0_f32; 4]; (SIZE * SIZE) as usize];

    for by in 0..big {
        for bx in 0..big {
            // Sample point mapped back into 32-px space.
            let sx = (bx as f32 + 0.5) / SS as f32;
            let sy = (by as f32 + 0.5) / SS as f32;

            let (r, g, b, a) = if point_in_poly(sx, sy, &verts) {
                // Gold fill with a top-bright → bottom-amber vertical gradient.
                let t = ((sy - 1.0) / 30.0).clamp(0.0, 1.0);
                (1.0, lerp(0.90, 0.60, t), lerp(0.30, 0.04, t), 1.0)
            } else if dist_to_poly(sx, sy, &verts) <= outline_w {
                // Dark outer outline so the cursor reads on any background.
                (0.09, 0.06, 0.02, 1.0)
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };

            let idx = ((by / SS) * SIZE + (bx / SS)) as usize;
            acc[idx][0] += r * a;
            acc[idx][1] += g * a;
            acc[idx][2] += b * a;
            acc[idx][3] += a;
        }
    }

    let inv_samples = 1.0 / (SS * SS) as f32;
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for p in &acc {
        let (sum_r, sum_g, sum_b, sum_a) = (p[0], p[1], p[2], p[3]);
        // Un-premultiply for the color channels; coverage gives the alpha.
        let (r, g, b) = if sum_a > 0.0 {
            (sum_r / sum_a, sum_g / sum_a, sum_b / sum_a)
        } else {
            (0.0, 0.0, 0.0)
        };
        data.push((r * 255.0).round() as u8);
        data.push((g * 255.0).round() as u8);
        data.push((b * 255.0).round() as u8);
        data.push((sum_a * inv_samples * 255.0).round() as u8);
    }

    Image::new(
        Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Even-odd ray-cast point-in-polygon test.
fn point_in_poly(x: f32, y: f32, v: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let n = v.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = v[i];
        let (xj, yj) = v[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Shortest distance from a point to the polygon's edges.
fn dist_to_poly(x: f32, y: f32, v: &[(f32, f32)]) -> f32 {
    let n = v.len();
    let mut best = f32::MAX;
    let mut j = n - 1;
    for i in 0..n {
        best = best.min(dist_to_segment(x, y, v[j], v[i]));
        j = i;
    }
    best
}

fn dist_to_segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (ax, ay) = a;
    let (bx, by) = b;
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    let (ex, ey) = (px - cx, py - cy);
    (ex * ex + ey * ey).sqrt()
}
