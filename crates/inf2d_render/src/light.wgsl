// 2D additive point-light shader.
//
// Each light is a screen-aligned quad textured with a precomputed RGBA falloff
// texture: white RGB, alpha that's 1.0 at the quad's center and 0.0 at the
// rim (smoothed and squared so the rolloff looks soft, not linear).
//
// The fragment output is `tint.rgb * falloff_alpha * intensity` with
// premultiplied alpha (alpha = falloff_alpha). The Rust pipeline override
// installs an additive BlendState so the contribution is *added* to whatever
// is already in the framebuffer — overlapping torches build up brightness
// instead of replacing it, which is the whole "torch glow" trick.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

// Composite uniform produced by AsBindGroup for the two `#[uniform(0)]` fields
// on the Rust side. Field order MUST match the struct definition there:
// `tint: LinearRgba` then `intensity: f32`.
struct PointLight2DUniform {
    tint: vec4<f32>,
    intensity: f32,
};

@group(2) @binding(0) var<uniform> light: PointLight2DUniform;
@group(2) @binding(1) var falloff_texture: texture_2d<f32>;
@group(2) @binding(2) var falloff_sampler: sampler;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let sample = textureSample(falloff_texture, falloff_sampler, mesh.uv);
    let a = sample.a * light.intensity;
    // Premultiplied output: the blend state expects color contributions
    // already scaled by their effective alpha so SrcAlpha factors cleanly.
    return vec4<f32>(light.tint.rgb * a, a);
}
