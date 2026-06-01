// Heat shimmer overlay. Two scrolling noise layers at different speeds create
// an "ascending haze" pattern. Intensity is gated by the uniform `strength`;
// the shader early-outs to zero when strength is negligible.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct HeatUniforms {
    time: f32,
    strength: f32,
    _pad: vec2<f32>,
    tint: vec4<f32>,
};

@group(2) @binding(0) var<uniform> h: HeatUniforms;

fn hash21(p: vec2<f32>) -> f32 {
    let d = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(d) * 43758.5453);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    if (h.strength <= 0.001) {
        return vec4<f32>(0.0);
    }

    let uv = mesh.uv;

    // Two scrolling noise layers — slow ascend at low scale, faster at higher scale.
    // The y-component of the scroll vector is negative so the haze rises (UV +y is down).
    let n1 = value_noise(uv * 6.0  + vec2<f32>(h.time * 0.05, -h.time * 0.20));
    let n2 = value_noise(uv * 16.0 + vec2<f32>(h.time * -0.03, -h.time * 0.35));

    // Bias toward bright peaks. Squared so quiet regions stay quiet.
    let shimmer = pow(n1 * 0.6 + n2 * 0.4, 2.0);

    // Output a translucent warm wash. Premultiplied for clean AlphaMode2d::Blend.
    let a = shimmer * h.strength;
    return vec4<f32>(h.tint.rgb * a, a);
}
