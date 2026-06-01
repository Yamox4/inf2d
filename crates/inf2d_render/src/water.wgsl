// Real-time water surface for a whole chunk in a single shader quad.
//
// The quad covers the chunk's tile-grid bounding box (an axis-aligned
// rectangle around the chunk's diamond footprint). For each fragment we:
//   1. Convert the rectangle UV to chunk-local pixel coords.
//   2. Inverse-iso into tile coords `(lx, ly)` ∈ approximately `[0, CHUNK_SIZE)`.
//   3. Discard if `(lx, ly)` falls outside the chunk's tile grid (the rect
//      corners — outside the diamond — land here).
//   4. Sample the per-chunk mask at `((lx+0.5)/CHUNK_SIZE, (ly+0.5)/CHUNK_SIZE)`
//      with nearest filtering. The mask is 32×32 R8: byte `(ly * 32 + lx)` is
//      255 over water tiles and 0 elsewhere.
//   5. Discard if mask < 0.5.
//   6. Remaining fragments are water; run the existing shimmer/spec pipeline.
//
// Inputs (one uniform block at @group(2) @binding(0)):
//   - time          : seconds, drives two scrolling fbm noise inputs
//   - sun_angle     : radians, sun azimuth in screen-space xy
//   - sun_strength  : 0..1, day=1 / night=0 — gates the warm sun specular lobe
//   - moon_strength : 0..1, day=0 / night=1 — gates the cool moon specular lobe
//   - base_color    : deep water RGB (alpha = surface opacity)
//   - shallow_color : sunlit / shallow water RGB
//
// Texture bindings:
//   - @binding(1)/(2) : water mask (R8). Sampled nearest-neighbor at tile
//                       centers so the in/out-of-water test is sharp at tile
//                       boundaries.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

const CHUNK_SIZE_F: f32 = 32.0;
const TILE_WIDTH: f32 = 64.0;
const TILE_HEIGHT: f32 = 32.0;

struct WaterUniforms {
    time: f32,
    sun_angle: f32,
    sun_strength: f32,
    moon_strength: f32,
    base_color: vec4<f32>,
    shallow_color: vec4<f32>,
};

@group(2) @binding(0) var<uniform> water: WaterUniforms;
@group(2) @binding(1) var water_mask: texture_2d<f32>;
@group(2) @binding(2) var water_mask_sampler: sampler;

// --- procedural noise ----------------------------------------------------

fn hash21(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);

    let a = hash21(i + vec2<f32>(0.0, 0.0));
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));

    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var amp = 0.5;
    var freq = 1.0;
    var sum = 0.0;
    for (var i: i32 = 0; i < 4; i = i + 1) {
        sum = sum + amp * value_noise(p * freq);
        freq = freq * 2.0;
        amp = amp * 0.5;
    }
    return sum;
}

// --- fragment shader ----------------------------------------------------

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    // Inverse-iso to recover the tile (lx, ly) for this fragment.
    //
    // The quad's local rectangle spans `[-w/2, +w/2] x [-h/2, +h/2]` where
    // `w = CHUNK_SIZE * TILE_WIDTH` and `h = CHUNK_SIZE * TILE_HEIGHT`. The
    // entity is placed at `(0, h/2, z)` in chunk-local space, so the quad
    // origin's chunk-local Y is `h/2`. Rectangle UV: `uv = (0,0)` at the
    // top-left vertex `(-w/2, +h/2)`, `uv = (1,1)` at the bottom-right
    // vertex `(+w/2, -h/2)`. Chunk-local pixel coords are therefore:
    //   cx = (uv.x - 0.5) * w
    //   cy = (1.0 - uv.y) * h
    // The iso projection is:
    //   cx = (lx - ly) * TILE_WIDTH / 2
    //   cy = (lx + ly) * TILE_HEIGHT / 2
    // Solving for (lx, ly):
    //   lx = cx / TILE_WIDTH + cy / TILE_HEIGHT
    //   ly = cy / TILE_HEIGHT - cx / TILE_WIDTH
    let uv = mesh.uv;
    let cx = (uv.x - 0.5) * CHUNK_SIZE_F * TILE_WIDTH;
    let cy = (1.0 - uv.y) * CHUNK_SIZE_F * TILE_HEIGHT;
    let lx = cx / TILE_WIDTH + cy / TILE_HEIGHT;
    let ly = cy / TILE_HEIGHT - cx / TILE_WIDTH;

    // Outside the chunk's diamond → no tile here. The axis-aligned bbox
    // corners fall outside the chunk's tile grid; discard them so the
    // merged quad's visible footprint matches the chunk's diamond.
    if (lx < 0.0 || lx >= CHUNK_SIZE_F || ly < 0.0 || ly >= CHUNK_SIZE_F) {
        discard;
    }

    // Sample the mask at the tile center. Nearest-neighbor (set on the CPU
    // side by `ImageSampler::linear`, but at this center alignment the
    // filter is functionally a point sample) keeps the coastline sharp at
    // tile edges. The +0.5 offset hits the center of the texel for tile
    // (floor(lx), floor(ly)).
    let mask_uv = vec2<f32>(
        (floor(lx) + 0.5) / CHUNK_SIZE_F,
        (floor(ly) + 0.5) / CHUNK_SIZE_F,
    );
    let mask_value = textureSample(water_mask, water_mask_sampler, mask_uv).r;
    if (mask_value < 0.5) {
        discard;
    }

    let t = water.time;

    // Sample noise in WORLD space for cross-chunk continuity. Dividing by
    // 64 (TILE_WIDTH) keeps the visual ripple frequency identical to the
    // previous per-tile shader.
    let wp = mesh.world_position.xy / 64.0;

    let n1 = fbm(wp * 4.0 + vec2<f32>(t * 0.05, t * 0.03));
    let n2 = fbm(wp * 12.0 + vec2<f32>(t * -0.13, t * 0.07));

    let combined = n1 * 0.5 + n2 * 0.5;
    let shimmer = pow(combined, 1.8);

    let depth_color = mix(
        water.shallow_color.rgb,
        water.base_color.rgb,
        smoothstep(0.0, 1.0, shimmer),
    );

    let grad = vec2<f32>(dpdx(n1 + n2), dpdy(n1 + n2));
    let grad_len = length(grad);
    var grad_n = vec2<f32>(0.0, 0.0);
    if (grad_len > 1e-5) {
        grad_n = grad / grad_len;
    }
    let sun_dir = vec2<f32>(cos(water.sun_angle), sin(water.sun_angle));
    let spec_dot = max(0.0, dot(grad_n, sun_dir));
    let spec = pow(spec_dot, 16.0) * water.sun_strength;
    let spec_color = vec3<f32>(1.0, 0.95, 0.85) * spec;

    let moon_dir = vec2<f32>(0.0, 1.0);
    let moon_spec_dot = max(0.0, dot(grad_n, moon_dir));
    let moon_spec = pow(moon_spec_dot, 32.0) * water.moon_strength;
    let moon_color = vec3<f32>(0.55, 0.65, 0.95) * moon_spec;

    let day_factor = water.sun_strength;
    let cool_mix = 1.0 - day_factor;
    let night_tint = vec3<f32>(0.55, 0.70, 1.00);
    let temp_factor = mix(vec3<f32>(1.0, 1.0, 1.0), night_tint, cool_mix);

    var final_rgb = depth_color * temp_factor + spec_color + moon_color;
    final_rgb = clamp(final_rgb, vec3<f32>(0.0), vec3<f32>(1.2));

    let a = water.base_color.a;
    return vec4<f32>(final_rgb * a, a);
}
