// Volumetric god rays — radial blur from a screen-space sun position.
//
// Algorithm: for each pixel, walk N samples along the direction TOWARD the sun
// position (in clip-space), accumulating a procedural occlusion mask. Output a
// warm streak whose intensity falls off with distance from the sun.
//
// This is the standard "occlusion ray-march" sun-shafts technique simplified to
// 2D (no scene depth buffer). Procedural occlusion via fbm gives variability
// that reads as "dust/atmosphere catching the light".

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct GodRaysUniforms {
    sun_dir: vec2<f32>,     // unit vector (NDC-ish), points from screen center toward sun
    sun_strength: f32,      // 0..1
    _pad: f32,
    tint: vec4<f32>,
};

@group(2) @binding(0) var<uniform> god: GodRaysUniforms;

fn hash21(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453);
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

fn fbm(p: vec2<f32>) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var freq = 1.0;
    for (var i: i32 = 0; i < 3; i = i + 1) {
        sum = sum + amp * value_noise(p * freq);
        amp = amp * 0.5;
        freq = freq * 2.0;
    }
    return sum;
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    if (god.sun_strength <= 0.001) {
        return vec4<f32>(0.0);
    }

    let uv = mesh.uv;
    // Sun in UV space: map from sun_dir (which is normalized direction "toward sun"
    // from screen center) to a UV anchor on the edge of the screen.
    let sun_uv = vec2<f32>(0.5, 0.5) + god.sun_dir * 0.55;

    // Vector from this pixel TOWARD the sun.
    let to_sun = sun_uv - uv;
    let dist_to_sun = length(to_sun);
    let step = to_sun / 16.0;  // 16 samples along the ray

    // Accumulate procedural occlusion along the ray. Higher sum = more "stuff"
    // catching light = brighter streak.
    var acc = 0.0;
    var pos = uv;
    for (var i: i32 = 0; i < 16; i = i + 1) {
        let n = fbm(pos * 8.0);
        // Bias high values — only bright noise contributes to the streak.
        acc = acc + smoothstep(0.55, 0.8, n);
        pos = pos + step;
    }
    acc = acc / 16.0;

    // Falloff with distance from sun (rays die out far from source).
    let falloff = 1.0 - smoothstep(0.0, 1.2, dist_to_sun);

    let intensity = acc * falloff * god.sun_strength;
    return vec4<f32>(god.tint.rgb * intensity, intensity * 0.6);
}
