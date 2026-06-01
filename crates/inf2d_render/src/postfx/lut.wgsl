// LUT post-process tint shader.
//
// The plugin picks two LUTs from the palette (the closest pair for the
// current `TimeOfDay::hours`) and a blend weight. This shader samples *both*
// LUTs at a fixed "key tone" — middle gray (0.5, 0.5, 0.5) — using the
// industry-standard horizontal-strip 3D-LUT layout, then cross-fades the two
// outputs and emits the result as a translucent overlay tint.
//
// Why mid-gray and not the underlying pixel: in Bevy 0.18 a `Material2d`
// quad cannot read the previous-pass color target without a custom render-
// graph node. Sampling the LUT at a representative neutral tone gives us the
// *character* of the grade (warm vs cool, lifted vs crushed) and lays it
// down as a translucent wash, which composites on top of the existing flat
// day/night overlay to produce a perceptibly LUT-driven look. The full 3D
// lookup math is wired up — swapping in a true scene-read shader later is a
// matter of replacing `key_tone` with the sampled scene color and dropping
// the per-frame `strength` factor.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct LutUniform {
    // x = blend weight between lut_a (0) and lut_b (1).
    // y = overall tint strength (alpha of the output).
    // z = LUT cube edge size as f32 (e.g. 64.0).
    // w = unused / padding.
    params: vec4<f32>,
};

@group(2) @binding(0) var<uniform> lut_data: LutUniform;
@group(2) @binding(1) var lut_a_texture: texture_2d<f32>;
@group(2) @binding(2) var lut_a_sampler: sampler;
@group(2) @binding(3) var lut_b_texture: texture_2d<f32>;
@group(2) @binding(4) var lut_b_sampler: sampler;

/// Sample a horizontal-strip 3D LUT (`lut_size` × `lut_size * lut_size`,
/// arranged as `lut_size` horizontal slices) at the given linear input color.
///
/// The slice index is derived from blue; within a slice we use red for X and
/// green for Y. Linear filtering across the seam between slices would smear
/// blues together, so we explicitly read the two nearest blue slices and
/// blend their bilinear samples ourselves.
fn sample_lut(tex: texture_2d<f32>, samp: sampler, color: vec3<f32>, lut_size: f32) -> vec3<f32> {
    let strip_w = lut_size * lut_size;
    let strip_h = lut_size;

    // Pixel-space half-texel inset so bilinear filtering inside a slice
    // doesn't leak past slice boundaries.
    let inset = 0.5;

    let b_scaled = clamp(color.b, 0.0, 1.0) * (lut_size - 1.0);
    let b_lo = floor(b_scaled);
    let b_hi = min(b_lo + 1.0, lut_size - 1.0);
    let b_t = b_scaled - b_lo;

    // X within a single slice: inset + r * (lut_size - 1).
    let x_in_slice = inset + clamp(color.r, 0.0, 1.0) * (lut_size - 1.0);
    let y_in_strip = inset + clamp(color.g, 0.0, 1.0) * (lut_size - 1.0);

    let u_lo = (b_lo * lut_size + x_in_slice) / strip_w;
    let u_hi = (b_hi * lut_size + x_in_slice) / strip_w;
    let v = y_in_strip / strip_h;

    let sample_lo = textureSample(tex, samp, vec2<f32>(u_lo, v)).rgb;
    let sample_hi = textureSample(tex, samp, vec2<f32>(u_hi, v)).rgb;
    return mix(sample_lo, sample_hi, b_t);
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let blend = lut_data.params.x;
    let strength = lut_data.params.y;
    let lut_size = lut_data.params.z;

    // The "key tone" we wash the scene with. Mid-gray surfaces the LUT's
    // character without needing the underlying pixel.
    let key_tone = vec3<f32>(0.5, 0.5, 0.5);

    let graded_a = sample_lut(lut_a_texture, lut_a_sampler, key_tone, lut_size);
    let graded_b = sample_lut(lut_b_texture, lut_b_sampler, key_tone, lut_size);
    let graded = mix(graded_a, graded_b, blend);

    // Emit a premultiplied translucent color so standard alpha blending
    // (`AlphaMode2d::Blend`) composites the wash on top of the scene at
    // `strength` opacity.
    return vec4<f32>(graded * strength, strength);
}
