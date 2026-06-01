//! Procedural tile atlas: builds a single RGBA8 image at startup whose horizontal slots
//! line up 1:1 with [`TileKind::atlas_index`], so per-chunk tilemaps can index it directly.
//!
//! Slice 1 ships pixel-art diamonds painted at runtime — no external assets. Each tile
//! gets:
//! - a vertical light→shadow gradient (top brighter, bottom darker), faking a domed
//!   normal so the iso plane reads as 2.5D instead of flat construction-paper diamonds;
//! - a single-pixel darker outline along the diamond perimeter for edge definition;
//! - per-kind detail passes: water gets cross-hatched shimmer highlights, stone gets
//!   a darker bottom band suggesting elevated rock, snow gets sparkle highlights, etc.
//!
//! The atlas can later be swapped for an external sprite sheet; the on-disk asset
//! produced here is a self-contained fallback so the engine boots without art deps.

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureAspect, TextureDimension, TextureFormat, TextureViewDescriptor,
    TextureViewDimension,
};
use bevy_ecs_tilemap::prelude::TilemapTileSize;
use inf2d_world::TileKind;

/// Handle + tile dimensions for the procedurally-built atlas. Inserted by
/// [`setup_tile_atlas`] at `Startup`; consumed by the per-chunk spawner.
///
/// Holds two sibling atlases painted into matching layouts: a diffuse strip
/// ([`handle`](Self::handle)) and a normal map strip ([`normal_handle`](Self::normal_handle))
/// used by the lit tilemap material to shade tiles each frame.
#[derive(Resource, Clone)]
pub struct TileAtlas {
    /// Diffuse (albedo) atlas — the painted look of each tile.
    pub handle: Handle<Image>,
    /// Normal-map atlas; sampled in the lit tilemap material's fragment shader.
    /// Each layer of this 2D-array texture matches the same tile slot in
    /// [`handle`](Self::handle) — the same `tile_id` selects parallel tiles
    /// across both atlases.
    pub normal_handle: Handle<Image>,
    /// Tile dimensions, shared between both atlases.
    pub tile_size: TilemapTileSize,
}

const TILE_W: u32 = 64;
const TILE_H: u32 = 32;

/// Total atlas slot count: 6 land/water TileKind slots + 3 extra animated water-frame
/// slots (6, 7, 8) + 1 stairs slot (9). Slot 2 is water frame 0; slots 6, 7, 8 are
/// water frames 1..4; slot 9 is the stair tile.
const ATLAS_SLOTS: u32 = 10;

/// Atlas slot indices for the 4 animated water frames. Frame 0 lives at slot 2 (the
/// canonical `TileKind::Water` slot); frames 1..4 occupy the three extra slots between
/// the natural `TileKind` range (0..=5) and the stair slot (9).
pub const WATER_FRAMES: [u32; 4] = [2, 6, 7, 8];

/// Per-atlas-slot base mid-tone. Indexed by `TileKind as u8` (which matches the atlas
/// slot for non-`Stairs` kinds) or directly by atlas slot index for the water-animation
/// frames. Public so other renderer subsystems (notably HLOD baking) can recolor without
/// re-running the full per-tile shading pass.
///
/// Layout:
/// * 0..=5: the six land/water `TileKind` base colors (Grass..Snow).
/// * 6..=8: water animation frames — same base color as `Water` at slot 2 so any
///   consumer that picks a slot at random still gets a sensible mid-tone.
/// * 9:    `Stairs` — sandstone neutral.
pub const BASE_COLOR: [[u8; 4]; 10] = [
    [96, 156, 84, 255],   // 0  Grass
    [224, 196, 128, 255], // 1  Sand
    [54, 110, 196, 255],  // 2  Water (frame 0)
    [140, 140, 148, 255], // 3  Stone
    [110, 80, 56, 255],   // 4  Dirt
    [240, 240, 248, 255], // 5  Snow
    [54, 110, 196, 255],  // 6  Water frame 1
    [54, 110, 196, 255],  // 7  Water frame 2
    [54, 110, 196, 255],  // 8  Water frame 3
    [180, 160, 140, 255], // 9  Stairs (sandstone neutral)
];

/// Build a fresh RGBA8 atlas image containing one diamond per `kind` in `kinds`, laid
/// out horizontally, followed by 3 extra phase-shifted water frames at slots 6, 7, 8.
/// Returns an `Image` ready to be added to `Assets<Image>`.
pub fn build_tile_atlas_image(tile_w: u32, tile_h: u32, kinds: &[TileKind]) -> Image {
    let width = tile_w.saturating_mul(ATLAS_SLOTS).max(1);
    let height = tile_h.max(1);
    let stride = width * 4;
    let mut buf = vec![0u8; (stride * height) as usize];

    for (slot, kind) in kinds.iter().enumerate() {
        let x0 = (slot as u32) * tile_w;
        paint_tile(&mut buf, stride, x0, 0, tile_w, tile_h, *kind, 0.0);
    }

    // Extra water animation frames. Frame 0 already sits at slot 2 (painted above as
    // part of the `TileKind` pass); frames 1..4 get their own slots with a quarter-cycle
    // phase shift each so the shimmer sweeps across the tile over time.
    for i in 1..WATER_FRAMES.len() {
        let slot = WATER_FRAMES[i];
        let x0 = slot * tile_w;
        let phase = i as f32 * std::f32::consts::FRAC_PI_2;
        paint_tile(&mut buf, stride, x0, 0, tile_w, tile_h, TileKind::Water, phase);
    }

    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        buf,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// Build a fresh RGBA8 normal-map atlas as a 2D *array* texture, one layer per atlas
/// slot. Each layer is `tile_w × tile_h`; the layer ordering matches
/// [`TileKind::atlas_index`] so the same `tile_id` selects the diffuse and normal
/// slices in lockstep.
///
/// Why a layered texture instead of a wide horizontal strip like the diffuse atlas:
/// the diffuse strip gets sliced by `bevy_ecs_tilemap`'s internal `TextureArrayCache`
/// at GPU upload time, but only because it's plugged in as a `TilemapTexture::Single`.
/// Our normal map binds through a custom `AsBindGroup` so it has to ship as an array
/// texture up-front; the WGSL still indexes it with `(uv, tile_id)`.
///
/// Inside the diamond mask each pixel encodes a fake "dome" surface normal:
///
/// * The center of each diamond points straight up (`n ≈ (0, 0, 1)`, encoded `(128, 128, 255)`).
/// * Edges tilt outward; the left vertex points left, the right vertex right, etc.
/// * Per-kind hash noise perturbs the XY components — finer/higher for water ripples,
///   coarser/higher for stone roughness, subtle elsewhere.
///
/// The output uses linear (non-sRGB) `Rgba8Unorm` so the encoded normals are sampled
/// verbatim; sRGB decode would gamma-curve the XY components and tilt every normal.
pub fn build_tile_normal_atlas_image(tile_w: u32, tile_h: u32, kinds: &[TileKind]) -> Image {
    let layers: u32 = ATLAS_SLOTS;
    let layer_stride = (tile_w * tile_h * 4) as usize;
    let mut buf = vec![0u8; layer_stride * layers as usize];

    for (slot, kind) in kinds.iter().enumerate() {
        let layer = slot as u32;
        let offset = layer as usize * layer_stride;
        paint_tile_normal_layer(
            &mut buf[offset..offset + layer_stride],
            tile_w,
            tile_h,
            *kind,
        );
    }

    // Extra water animation frames mirror the diffuse layout. Reuse the water normal
    // generator so each animation frame lights consistently with its diffuse sibling.
    for i in 1..WATER_FRAMES.len() {
        let layer = WATER_FRAMES[i];
        let offset = layer as usize * layer_stride;
        paint_tile_normal_layer(
            &mut buf[offset..offset + layer_stride],
            tile_w,
            tile_h,
            TileKind::Water,
        );
    }

    let mut image = Image::new(
        Extent3d {
            width: tile_w.max(1),
            height: tile_h.max(1),
            depth_or_array_layers: layers,
        },
        TextureDimension::D2,
        buf,
        // Linear (non-sRGB) so the encoded XY/Z values survive the texture pipeline
        // intact — sRGB would gamma-correct R and G and skew every normal.
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    // Force the GPU view to D2Array so the `texture_2d_array` binding in WGSL
    // matches; without this, wgpu would still infer D2Array for our extents
    // (depth_or_array_layers > 1) but being explicit keeps the contract clear.
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        label: Some("tile_normal_atlas_view"),
        format: Some(TextureFormat::Rgba8Unorm),
        dimension: Some(TextureViewDimension::D2Array),
        aspect: TextureAspect::All,
        base_mip_level: 0,
        mip_level_count: None,
        base_array_layer: 0,
        array_layer_count: Some(layers),
        usage: None,
    });
    image
}

/// Paint one layer (one tile-sized slice) of the normal-map atlas. Outside the diamond
/// mask the pixel is left fully transparent — the fragment shader discards those
/// samples via the diffuse atlas's alpha channel. Inside, encode a domed normal:
///
/// * Local coordinates inside the slot are mapped to `(u, v)` in `[-1, 1]^2`.
/// * The XY part of the normal points outward from the center, weighted by how close
///   the pixel sits to the diamond rim (rim → strongly tilted; center → flat up).
/// * Z is `sqrt(1 − x² − y²)` clamped above `0.15`, keeping the normal biased upward
///   so tile tops always catch some sun even at the rim.
/// * Per-kind hash noise perturbs XY before normalization.
///
/// **Water is special-cased**: water is a flat surface; ripple/shimmer is produced by
/// the [`crate::water::WaterMaterial`] shader on the per-tile quad above. Painting a
/// dome normal here would double-shade water (Lambertian dome here + shimmer above),
/// so every in-mask pixel for water layers is written as the flat-up encoding
/// `(128, 128, 255, 255)`.
fn paint_tile_normal_layer(layer: &mut [u8], w: u32, h: u32, kind: TileKind) {
    if w < 4 || h < 4 {
        return;
    }
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let inset_x = 1.0 / w as f32;
    let inset_y = 1.0 / h as f32;
    let limit = 0.5 - inset_x.max(inset_y);
    let stride = w * 4;

    // Water: paint a flat normal pointing straight up everywhere inside the diamond
    // mask. The water shader quad on top adds shimmer/specular — letting the lit
    // tile shader Lambert-shade a fake dome here would double the lighting and
    // turn each tile into a visible bumpy diamond instead of a flat water surface.
    if matches!(kind, TileKind::Water) {
        for ly in 0..h {
            for lx in 0..w {
                let fx = lx as f32 + 0.5;
                let fy = ly as f32 + 0.5;
                let dx_n = (fx - cx).abs() / w as f32;
                let dy_n = (fy - cy).abs() / h as f32;
                if dx_n + dy_n > limit {
                    continue;
                }
                let off = (ly * stride + lx * 4) as usize;
                if off + 4 <= layer.len() {
                    layer[off] = 128;
                    layer[off + 1] = 128;
                    layer[off + 2] = 255;
                    layer[off + 3] = 255;
                }
            }
        }
        return;
    }

    // Per-kind perturbation strength on the XY components of the encoded normal.
    let perturb = match kind {
        TileKind::Stone => 0.20,
        TileKind::Grass | TileKind::Sand | TileKind::Dirt | TileKind::Snow => 0.05,
        // Stairs: keep the surface read as a clean ramp, no roughness — the
        // step-line detail in the diffuse pass carries the visual signal.
        TileKind::Stairs => 0.02,
        // Water is handled by the early-return above; this arm is unreachable but
        // keeps the match exhaustive without a wildcard that could mask future
        // kinds being added.
        TileKind::Water => 0.0,
    };

    for ly in 0..h {
        for lx in 0..w {
            let fx = lx as f32 + 0.5;
            let fy = ly as f32 + 0.5;
            let dx_n = (fx - cx).abs() / w as f32;
            let dy_n = (fy - cy).abs() / h as f32;
            let dist = dx_n + dy_n;

            if dist > limit {
                continue;
            }

            // Direction from the diamond's center, normalized into [-1, 1].
            let nx_raw = (fx - cx) / (w as f32 * 0.5);
            let ny_raw = (fy - cy) / (h as f32 * 0.5);

            // Falloff: 0 at center → 1 at rim. Drives how much the normal tilts.
            let falloff = (dist / limit).clamp(0.0, 1.0);

            let tilt = falloff * 0.85;
            let mut nx = nx_raw * tilt;
            // Flip ny so the "top of diamond" (low image-y) points away from the
            // viewer; the bottom rim catches more shadow.
            let mut ny = -ny_raw * tilt;

            // Per-kind hash-based perturbation. Deterministic for reproducibility.
            let h32 = hash2(lx, ly);
            let jx = ((h32 & 0xFFFF) as f32 / 65535.0) * 2.0 - 1.0;
            let jy = (((h32 >> 16) & 0xFFFF) as f32 / 65535.0) * 2.0 - 1.0;
            nx += jx * perturb;
            ny += jy * perturb;

            let xy2 = (nx * nx + ny * ny).min(1.0);
            let nz = (1.0 - xy2).max(0.0).sqrt().max(0.15);

            let inv_len = 1.0 / (nx * nx + ny * ny + nz * nz).sqrt();
            let nx = nx * inv_len;
            let ny = ny * inv_len;
            let nz = nz * inv_len;

            let r = ((nx * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
            let g = ((ny * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
            let b = ((nz * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;

            let off = (ly * stride + lx * 4) as usize;
            if off + 4 <= layer.len() {
                layer[off] = r;
                layer[off + 1] = g;
                layer[off + 2] = b;
                layer[off + 3] = 255;
            }
        }
    }
}

/// `Startup` system: builds both the diffuse and normal atlases, stores them in
/// `Assets<Image>`, and exposes them to the rest of the renderer via the [`TileAtlas`]
/// resource. Both atlases share the same horizontal slot layout.
pub fn setup_tile_atlas(mut images: ResMut<Assets<Image>>, mut commands: Commands) {
    let diffuse = build_tile_atlas_image(TILE_W, TILE_H, &TileKind::ALL);
    let normal = build_tile_normal_atlas_image(TILE_W, TILE_H, &TileKind::ALL);
    let handle = images.add(diffuse);
    let normal_handle = images.add(normal);
    commands.insert_resource(TileAtlas {
        handle,
        normal_handle,
        tile_size: TilemapTileSize {
            x: TILE_W as f32,
            y: TILE_H as f32,
        },
    });
    tracing::info!(
        "tile atlas built: {} kinds + {} water frames, {}x{} px (diffuse + normal)",
        TileKind::ALL.len(),
        WATER_FRAMES.len() - 1,
        TILE_W * ATLAS_SLOTS,
        TILE_H
    );
}

/// Paint a single tile slot in the atlas. Computes a per-pixel gradient inside the
/// diamond mask, then runs a per-kind detail pass on top for biome-specific signature.
///
/// `water_phase` is forwarded to the water shimmer pass; it has no effect on other
/// tile kinds and is conventionally `0.0` for non-water slots.
fn paint_tile(
    buf: &mut [u8],
    stride: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    kind: TileKind,
    water_phase: f32,
) {
    if w < 4 || h < 4 {
        return;
    }
    let base = BASE_COLOR[kind as usize];
    let outline = darken(base, 50);
    let highlight = lighten(base, 36);
    let shadow = darken(base, 50);

    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let inset_x = 1.0 / w as f32;
    let inset_y = 1.0 / h as f32;
    let limit = 0.5 - inset_x.max(inset_y);
    let edge_band = (1.0 / w as f32).max(1.0 / h as f32);

    for ly in 0..h {
        for lx in 0..w {
            let fx = lx as f32 + 0.5;
            let fy = ly as f32 + 0.5;
            let dx = (fx - cx).abs() / w as f32;
            let dy = (fy - cy).abs() / h as f32;
            let dist = dx + dy;

            if dist > limit {
                continue;
            }

            // Vertical gradient: top of diamond is bright, bottom is dark.
            // `t` runs 0..1 from top vertex to bottom vertex.
            let t = ly as f32 / (h as f32 - 1.0);
            // Lerp highlight → base → shadow across the y axis.
            let body = if t < 0.5 {
                lerp_rgba(highlight, base, t * 2.0)
            } else {
                lerp_rgba(base, shadow, (t - 0.5) * 2.0)
            };

            // Subtle radial pop on the inside: lighter near center, darker near edge.
            // Smooths the gradient into a domed look.
            let radial = 1.0 - (dist / limit).clamp(0.0, 1.0);
            let body = lerp_rgba(body, lighten(body, 12), radial * 0.35);

            // Bottom-edge band gets an extra darken to suggest the diamond's "underside".
            let rim = if ly as f32 > h as f32 * 0.55 && dist > limit * 0.78 {
                lerp_rgba(body, shadow, 0.55)
            } else {
                body
            };

            // Outline ring sits on the very edge of the diamond mask.
            let pixel = if dist > limit - edge_band {
                outline
            } else {
                rim
            };

            write_pixel(buf, stride, x0 + lx, y0 + ly, pixel);
        }
    }

    // Per-kind detail passes layered on top of the shaded base.
    match kind {
        TileKind::Water => paint_water_shimmer(buf, stride, x0, y0, w, h, base, water_phase),
        TileKind::Stone => paint_stone_cracks(buf, stride, x0, y0, w, h, base),
        TileKind::Snow => paint_snow_sparkle(buf, stride, x0, y0, w, h),
        TileKind::Grass => paint_grass_tufts(buf, stride, x0, y0, w, h, base),
        TileKind::Sand => paint_sand_ripples(buf, stride, x0, y0, w, h, base),
        TileKind::Dirt => paint_dirt_speckle(buf, stride, x0, y0, w, h, base),
        TileKind::Stairs => paint_stairs_steps(buf, stride, x0, y0, w, h, base),
    }
}

/// Faint hash-based mottling on water tiles — just enough variation so the
/// underlying "pond bottom" reads as a water surface rather than a uniform
/// flat-color polygon. The lively shimmer comes from the
/// [`crate::water::WaterMaterial`] shader on the per-tile quad above; baking a
/// strong diagonal cross-hatch here used to fight the shader's animated noise
/// and made the day/night-lit base look busy. The `phase` argument is kept on
/// the signature so the additional animation frames in the atlas still vary
/// slightly slot-to-slot.
fn paint_water_shimmer(
    buf: &mut [u8],
    stride: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    base: [u8; 4],
    phase: f32,
) {
    // ±8 around the base color — much weaker than the previous ±60 cross-hatch,
    // so the shader-quad's shimmer composites cleanly on top.
    let speck_light = lighten(base, 8);
    let speck_dark = darken(base, 8);
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let limit = 0.5 - (1.0 / w as f32).max(1.0 / h as f32);
    // Mix the phase into the hash seed so the 4 animation frames don't all
    // dot identically.
    let phase_seed = (phase * 1024.0).round() as i32 as u32;

    for ly in 0..h {
        for lx in 0..w {
            let dx = ((lx as f32 + 0.5) - cx).abs() / w as f32;
            let dy = ((ly as f32 + 0.5) - cy).abs() / h as f32;
            if dx + dy > limit - 0.04 {
                continue;
            }
            let h32 = hash2(lx ^ phase_seed, ly);
            // Sparse: ~1 in 73 lightens, ~1 in 89 darkens. Below the visual
            // threshold of "pattern" — reads as a slightly noisy flat surface.
            if h32 % 73 == 0 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, speck_light);
            } else if h32 % 89 == 1 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, speck_dark);
            }
        }
    }
}

/// Sparse darker cracks scattered across stone tiles.
fn paint_stone_cracks(
    buf: &mut [u8],
    stride: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    base: [u8; 4],
) {
    let crack = darken(base, 60);
    let chip = lighten(base, 24);
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let limit = 0.5 - (1.0 / w as f32).max(1.0 / h as f32);

    // Deterministic hash-based scatter so the same atlas slot always paints identically.
    for ly in 0..h {
        for lx in 0..w {
            let dx = ((lx as f32 + 0.5) - cx).abs() / w as f32;
            let dy = ((ly as f32 + 0.5) - cy).abs() / h as f32;
            if dx + dy > limit - 0.03 {
                continue;
            }
            let h32 = hash2(lx, ly);
            if h32 % 31 == 0 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, crack);
            } else if h32 % 53 == 1 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, chip);
            }
        }
    }
}

/// Bright single-pixel sparkles on snow tiles.
fn paint_snow_sparkle(buf: &mut [u8], stride: u32, x0: u32, y0: u32, w: u32, h: u32) {
    let sparkle = [255, 255, 255, 255];
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let limit = 0.5 - (1.0 / w as f32).max(1.0 / h as f32);
    for ly in 0..h {
        for lx in 0..w {
            let dx = ((lx as f32 + 0.5) - cx).abs() / w as f32;
            let dy = ((ly as f32 + 0.5) - cy).abs() / h as f32;
            if dx + dy > limit - 0.04 {
                continue;
            }
            if hash2(lx, ly) % 67 == 0 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, sparkle);
            }
        }
    }
}

/// A few short vertical "blade" highlights scattered on grass.
fn paint_grass_tufts(buf: &mut [u8], stride: u32, x0: u32, y0: u32, w: u32, h: u32, base: [u8; 4]) {
    let blade = lighten(base, 28);
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let limit = 0.5 - (1.0 / w as f32).max(1.0 / h as f32);
    for ly in 0..h {
        for lx in 0..w {
            let dx = ((lx as f32 + 0.5) - cx).abs() / w as f32;
            let dy = ((ly as f32 + 0.5) - cy).abs() / h as f32;
            if dx + dy > limit - 0.04 {
                continue;
            }
            if hash2(lx, ly) % 41 == 0 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, blade);
                if ly + 1 < h {
                    write_pixel(buf, stride, x0 + lx, y0 + ly + 1, darken(blade, 30));
                }
            }
        }
    }
}

/// Horizontal ripple lines on sand.
fn paint_sand_ripples(
    buf: &mut [u8],
    stride: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    base: [u8; 4],
) {
    let ripple_l = lighten(base, 18);
    let ripple_d = darken(base, 18);
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let limit = 0.5 - (1.0 / w as f32).max(1.0 / h as f32);
    for ly in 0..h {
        for lx in 0..w {
            let dx = ((lx as f32 + 0.5) - cx).abs() / w as f32;
            let dy = ((ly as f32 + 0.5) - cy).abs() / h as f32;
            if dx + dy > limit - 0.05 {
                continue;
            }
            let wave = ((lx as f32 * 0.4 + ly as f32 * 0.2).sin() * 1.7).round();
            if wave > 0.5 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, ripple_l);
            } else if wave < -0.5 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, ripple_d);
            }
        }
    }
}

/// Random darker dots speckled across dirt.
fn paint_dirt_speckle(
    buf: &mut [u8],
    stride: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    base: [u8; 4],
) {
    let dark = darken(base, 32);
    let light = lighten(base, 14);
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let limit = 0.5 - (1.0 / w as f32).max(1.0 / h as f32);
    for ly in 0..h {
        for lx in 0..w {
            let dx = ((lx as f32 + 0.5) - cx).abs() / w as f32;
            let dy = ((ly as f32 + 0.5) - cy).abs() / h as f32;
            if dx + dy > limit - 0.04 {
                continue;
            }
            let h32 = hash2(lx, ly);
            if h32 % 19 == 0 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, dark);
            } else if h32 % 29 == 1 {
                write_pixel(buf, stride, x0 + lx, y0 + ly, light);
            }
        }
    }
}

/// Horizontal step lines across a stairs tile — three or four parallel slashes
/// inside the diamond mask suggesting "this tile has stairs running across it".
/// The lines are drawn in a darker tint of the base sandstone color so they read
/// as recessed treads rather than highlights, and a one-pixel highlight is
/// painted above each line to fake a step nose catching the sun.
fn paint_stairs_steps(
    buf: &mut [u8],
    stride: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    base: [u8; 4],
) {
    let tread = darken(base, 55);
    let nose = lighten(base, 30);
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let limit = 0.5 - (1.0 / w as f32).max(1.0 / h as f32);

    // Distribute 4 step lines evenly down the diamond, skipping the very top
    // and very bottom rows (which are too narrow inside the mask to read).
    // Indices 1..=4 across `h` produce treads at 1/5, 2/5, 3/5, 4/5 of the
    // tile's height — visible at 32 px tall.
    let line_count: u32 = 4;
    for n in 1..=line_count {
        let ly = ((n as f32 / (line_count as f32 + 1.0)) * h as f32).round() as u32;
        if ly >= h {
            continue;
        }
        for lx in 0..w {
            let dx_n = ((lx as f32 + 0.5) - cx).abs() / w as f32;
            let dy_n = ((ly as f32 + 0.5) - cy).abs() / h as f32;
            if dx_n + dy_n > limit - 0.02 {
                continue;
            }
            // Tread line itself.
            write_pixel(buf, stride, x0 + lx, y0 + ly, tread);
            // Step nose: one pixel above the tread catches the highlight,
            // suggesting the top of the next stair. Guard against underflow
            // and the diamond mask edge at the top of the tile.
            if ly >= 1 {
                let above = ly - 1;
                let dx_a = ((lx as f32 + 0.5) - cx).abs() / w as f32;
                let dy_a = ((above as f32 + 0.5) - cy).abs() / h as f32;
                if dx_a + dy_a <= limit - 0.02 {
                    write_pixel(buf, stride, x0 + lx, y0 + above, nose);
                }
            }
        }
    }
}

/// SplitMix32 — a tiny deterministic 2D-input hash for the detail passes. Same
/// per-pixel value every build so the atlas is reproducible.
#[inline]
fn hash2(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9E37_79B1).wrapping_add(y.wrapping_mul(0x85EB_CA77));
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 16;
    h
}

#[inline]
fn write_pixel(buf: &mut [u8], stride: u32, x: u32, y: u32, rgba: [u8; 4]) {
    let off = (y * stride + x * 4) as usize;
    if off + 4 <= buf.len() {
        buf[off..off + 4].copy_from_slice(&rgba);
    }
}

#[inline]
fn lerp_rgba(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        lerp_u8(a[0], b[0], t),
        lerp_u8(a[1], b[1], t),
        lerp_u8(a[2], b[2], t),
        lerp_u8(a[3], b[3], t),
    ]
}

#[inline]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

#[inline]
fn lighten(rgba: [u8; 4], by: u8) -> [u8; 4] {
    [
        rgba[0].saturating_add(by),
        rgba[1].saturating_add(by),
        rgba[2].saturating_add(by),
        rgba[3],
    ]
}

#[inline]
fn darken(rgba: [u8; 4], by: u8) -> [u8; 4] {
    [
        rgba[0].saturating_sub(by),
        rgba[1].saturating_sub(by),
        rgba[2].saturating_sub(by),
        rgba[3],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_dimensions_match_kinds() {
        let img = build_tile_atlas_image(TILE_W, TILE_H, &TileKind::ALL);
        let ext = img.texture_descriptor.size;
        assert_eq!(ext.width, TILE_W * ATLAS_SLOTS);
        assert_eq!(ext.height, TILE_H);
        assert_eq!(ext.depth_or_array_layers, 1);
        assert_eq!(img.texture_descriptor.format, TextureFormat::Rgba8UnormSrgb);
    }

    #[test]
    fn corners_are_transparent() {
        let img = build_tile_atlas_image(TILE_W, TILE_H, &TileKind::ALL);
        let data = img.data.as_ref().expect("atlas has cpu data");
        let alpha = data[3];
        assert_eq!(alpha, 0, "(0,0) corner of slot 0 lies outside the diamond mask");
    }

    #[test]
    fn normal_atlas_is_2d_array_layout() {
        let img = build_tile_normal_atlas_image(TILE_W, TILE_H, &TileKind::ALL);
        let ext = img.texture_descriptor.size;
        assert_eq!(ext.width, TILE_W);
        assert_eq!(ext.height, TILE_H);
        assert_eq!(ext.depth_or_array_layers, ATLAS_SLOTS);
        assert_eq!(img.texture_descriptor.format, TextureFormat::Rgba8Unorm);
    }

    #[test]
    fn normal_atlas_center_points_up() {
        // The dead-center pixel of layer 0 (grass) should be ≈ (0, 0, 1) which encodes
        // to (128, 128, 255) ± noise.
        let img = build_tile_normal_atlas_image(TILE_W, TILE_H, &TileKind::ALL);
        let data = img.data.as_ref().expect("normal atlas has cpu data");
        let stride = (TILE_W * 4) as usize;
        let cx = (TILE_W / 2) as usize;
        let cy = (TILE_H / 2) as usize;
        let off = cy * stride + cx * 4;
        let r = data[off];
        let g = data[off + 1];
        let b = data[off + 2];
        // Noise is small at center; allow ±20 around (128, 128) and require z near 1.
        assert!((r as i32 - 128).abs() <= 20, "center R ≈ 128, got {r}");
        assert!((g as i32 - 128).abs() <= 20, "center G ≈ 128, got {g}");
        assert!(b > 230, "center Z should encode near 1.0 ⇒ B near 255, got {b}");
    }

    #[test]
    fn water_normal_layer_is_flat_up_everywhere_in_mask() {
        // Water normals must be perfectly flat (encoded `128, 128, 255`) so the
        // lit tile shader doesn't Lambert-shade a fake dome on top of the
        // `WaterMaterial` shader's shimmer. Walk the entire water layer and
        // assert every in-mask pixel encodes (0, 0, 1) exactly.
        let img = build_tile_normal_atlas_image(TILE_W, TILE_H, &TileKind::ALL);
        let data = img.data.as_ref().expect("normal atlas has cpu data");
        let layer_stride = (TILE_W * TILE_H * 4) as usize;
        let water_layer = TileKind::Water as usize;
        let base = water_layer * layer_stride;
        let row_stride = (TILE_W * 4) as usize;
        let mut in_mask_pixels = 0usize;
        for y in 0..TILE_H as usize {
            for x in 0..TILE_W as usize {
                let off = base + y * row_stride + x * 4;
                let a = data[off + 3];
                if a == 0 {
                    continue;
                }
                in_mask_pixels += 1;
                assert_eq!(data[off], 128, "water normal R must be 128 at ({x},{y})");
                assert_eq!(data[off + 1], 128, "water normal G must be 128 at ({x},{y})");
                assert_eq!(data[off + 2], 255, "water normal B must be 255 at ({x},{y})");
            }
        }
        assert!(
            in_mask_pixels > 100,
            "water layer should have a populated diamond mask, got {in_mask_pixels} pixels"
        );
    }
}
