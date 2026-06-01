// Read-from-scene 3D-LUT color grading post-process.
//
// This shader runs inside a render-graph node placed between the 2D core
// pipeline's tonemapping pass and `EndMainPassPostProcessing`. It samples the
// *previous* color target (`scene_tex`) for every pixel, runs the linear RGB
// triplet through TWO 3D LUTs (the active pair selected per-frame from the
// `LutPalette`), cross-fades between them by `settings.blend`, and finally
// blends back toward the original scene color by `settings.strength`. That
// last blend is what gives the day/night dial a soft ramp instead of a hard
// pop, and keeps the world clearly visible at every hour.
//
// 3D-LUT layout: horizontal strip, `LUT_SIZE * LUT_SIZE` pixels wide and
// `LUT_SIZE` pixels tall. The blue channel selects which 64-pixel-wide slice
// to read; within a slice red is X and green is Y. To avoid linear filtering
// smearing neighbouring blue slices together we explicitly sample the two
// nearest slices and do the blue interpolation in shader code.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct LutSettings {
    // Cross-fade between LUT A (0) and LUT B (1). Driven from
    // `select_lut_pair(TimeOfDay::hours)` on the CPU.
    blend: f32,
    // Overall pass strength. 0 = bypass the LUT, 1 = fully graded. The
    // CPU-side driver eases this from 0 around midday to a capped value
    // around dusk / night.
    strength: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var lut_a_tex: texture_2d<f32>;
@group(0) @binding(3) var lut_a_sampler: sampler;
@group(0) @binding(4) var lut_b_tex: texture_2d<f32>;
@group(0) @binding(5) var lut_b_sampler: sampler;
@group(0) @binding(6) var<uniform> settings: LutSettings;

const LUT_SIZE: f32 = 64.0;

// Sample one horizontal-strip 3D LUT at the given linear-RGB input color.
// Returns the graded color in `[0, 1]^3`.
fn sample_lut(tex: texture_2d<f32>, samp: sampler, color: vec3<f32>) -> vec3<f32> {
    let strip_w = LUT_SIZE * LUT_SIZE;
    let strip_h = LUT_SIZE;

    // Half-texel inset within each slice so the GPU's bilinear filter never
    // pulls in pixels from a neighbouring blue slice.
    let inset = 0.5;

    let b_scaled = clamp(color.b, 0.0, 1.0) * (LUT_SIZE - 1.0);
    let b_lo = floor(b_scaled);
    let b_hi = min(b_lo + 1.0, LUT_SIZE - 1.0);
    let b_t = b_scaled - b_lo;

    let x_in_slice = inset + clamp(color.r, 0.0, 1.0) * (LUT_SIZE - 1.0);
    let y_in_strip = inset + clamp(color.g, 0.0, 1.0) * (LUT_SIZE - 1.0);

    let u_lo = (b_lo * LUT_SIZE + x_in_slice) / strip_w;
    let u_hi = (b_hi * LUT_SIZE + x_in_slice) / strip_w;
    let v = y_in_strip / strip_h;

    let sample_lo = textureSampleLevel(tex, samp, vec2<f32>(u_lo, v), 0.0).rgb;
    let sample_hi = textureSampleLevel(tex, samp, vec2<f32>(u_hi, v), 0.0).rgb;
    return mix(sample_lo, sample_hi, b_t);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let scene = textureSampleLevel(scene_tex, scene_sampler, in.uv, 0.0);

    let graded_a = sample_lut(lut_a_tex, lut_a_sampler, scene.rgb);
    let graded_b = sample_lut(lut_b_tex, lut_b_sampler, scene.rgb);
    let graded = mix(graded_a, graded_b, settings.blend);

    let out_rgb = mix(scene.rgb, graded, settings.strength);
    return vec4<f32>(out_rgb, scene.a);
}
