// Radial vignette. Darkens screen corners with a configurable falloff curve.
//
// Layout matches `VignetteMaterial`: one `#[uniform(0)]` block packed as
// `f32 + f32 + vec2 + vec4` = 32 bytes.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct VignetteUniforms {
    strength: f32,
    falloff_power: f32,
    _pad: vec2<f32>,
    tint: vec4<f32>,
};

@group(2) @binding(0) var<uniform> v: VignetteUniforms;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    // Distance from screen center in normalized [-1, 1] coords.
    let d = (mesh.uv - vec2<f32>(0.5, 0.5)) * 2.0;
    let r = length(d);

    // Falloff curve: smoothstep gives a soft, rounded edge to the dark band,
    // then a configurable power tunes how aggressively it crushes toward the
    // corners. The smoothstep's outer edge (1.4) sits past the [-1, 1] box's
    // unit circle so the corners reach the full mask while the cardinal
    // edges stay softer — same shape as classic camera vignettes.
    let edge = smoothstep(0.4, 1.4, r);
    let darken = pow(edge, v.falloff_power) * v.strength;

    return vec4<f32>(v.tint.rgb * darken, darken);
}
