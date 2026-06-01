//! Sprite particle system.
//!
//! Each particle is one entity carrying [`Particle`] (lifetime/velocity/fade state)
//! + a `Sprite` (color + size). An [`Emitter`] component spawns particles at a
//! configured rate, with parameter randomization controlled by [`EmitterShape`]
//! and [`EmitterPreset`]. All math runs in `Update` via [`tick_emitters`] and
//! [`age_particles`], in [`inf2d_core::SimulationSet`].
//!
//! ## Wiring
//!
//! Add [`ParticlesPlugin`] (already done by [`crate::RenderPlugin`]). Game code
//! then spawns an entity carrying [`Emitter`] + a `Transform` at the world
//! position where particles should appear. The plugin auto-builds the shared
//! procedural soft-disc texture stored in [`ParticleAssets`] on `Startup`.
//!
//! ## Performance
//!
//! Each particle is its own sprite entity. With Bevy's batched sprite renderer
//! that comfortably handles 1000+ particles at 60 fps on integrated GPUs. The
//! two systems do trivial per-particle math and use a single mutable query each,
//! so they vectorize cleanly under `multi_threaded`.

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rand::Rng;
use std::f32::consts::TAU;

use crate::layers::RenderLayer;

/// Edge length, in pixels, of the procedurally-generated soft-disc texture.
const PARTICLE_TEX_SIZE: u32 = 32;

/// Z offset applied on top of [`RenderLayer::ENTITY`] so particles composite
/// just above ordinary gameplay sprites. Small enough that lights and the
/// day/night overlay still draw on top.
const PARTICLE_Z_OFFSET: f32 = 0.2;

/// Per-particle state carried alongside the `Sprite` component. The two `_`
/// fields on the parent [`Emitter`] are bookkeeping; here every field is
/// gameplay-visible.
///
/// Aging is driven by [`age_particles`]: each tick advances [`Particle::age`],
/// integrates velocity (with gravity), and despawns when `age >= lifetime`.
#[derive(Component, Reflect, Debug, Clone)]
#[reflect(Component)]
pub struct Particle {
    /// Total lifetime in seconds. Set once at spawn and never modified.
    pub lifetime: f32,
    /// Current age in seconds. Starts at 0 and advances each frame.
    pub age: f32,
    /// Linear velocity in world units per second.
    pub velocity: Vec2,
    /// Rotation rate in radians per second. Applied via `Transform::rotate_z`.
    pub angular_velocity: f32,
    /// Initial color (used at `age == 0`). Tints the shared white texture.
    pub start_color: Color,
    /// Final color (used at `age == lifetime`). Alpha is typically 0 to fade out.
    pub end_color: Color,
    /// Sprite size at birth, in world units. Applied as `Vec2::splat`.
    pub start_size: f32,
    /// Sprite size at death; linearly interpolated with `start_size` over life.
    pub end_size: f32,
    /// Gravity acceleration in world units per second^2. `(0, 0)` for top-down
    /// effects that shouldn't fall.
    pub gravity: Vec2,
}

impl Default for Particle {
    fn default() -> Self {
        Self {
            lifetime: 1.0,
            age: 0.0,
            velocity: Vec2::ZERO,
            angular_velocity: 0.0,
            start_color: Color::WHITE,
            end_color: Color::srgba(1.0, 1.0, 1.0, 0.0),
            start_size: 8.0,
            end_size: 4.0,
            gravity: Vec2::ZERO,
        }
    }
}

/// Continuous-spawn particle source. Attach to any entity carrying a
/// `GlobalTransform`; [`tick_emitters`] reads that transform each tick and
/// spawns particles at the emitter's world position with randomized parameters
/// drawn from the configured [`EmitterPreset`] + [`EmitterShape`].
#[derive(Component, Reflect, Debug, Clone)]
#[reflect(Component)]
pub struct Emitter {
    /// Which preset family to spawn (smoke, dust, sparkle, fire). Drives the
    /// per-particle randomization in [`particle_from_preset`].
    pub preset: EmitterPreset,
    /// Geometric spawn region around the emitter's origin.
    pub shape: EmitterShape,
    /// Continuous spawn rate in particles per second. Set to `0.0` for
    /// burst-only emitters.
    pub rate: f32,
    /// Initial burst — this many particles fire on the first tick after spawn.
    pub burst: u32,
    /// Toggle for the emitter. Set to `false` to pause spawning while keeping
    /// already-spawned particles alive.
    pub enabled: bool,
    /// Internal fractional-particle accumulator — driven by [`tick_emitters`],
    /// do not set manually.
    pub _spawn_accum: f32,
    /// Internal — set to `true` after the initial [`Emitter::burst`] has fired.
    pub _burst_done: bool,
}

impl Default for Emitter {
    fn default() -> Self {
        Self {
            preset: EmitterPreset::Smoke,
            shape: EmitterShape::Point,
            rate: 8.0,
            burst: 0,
            enabled: true,
            _spawn_accum: 0.0,
            _burst_done: false,
        }
    }
}

/// Geometric region the emitter samples for each particle's spawn offset.
#[derive(Reflect, Debug, Clone, Copy)]
pub enum EmitterShape {
    /// Spawn at the emitter's origin.
    Point,
    /// Spawn anywhere inside a disc of `radius`.
    Disc {
        /// Disc radius in world units.
        radius: f32,
    },
    /// Spawn along a ring at `radius` (good for splash rings).
    Ring {
        /// Ring radius in world units.
        radius: f32,
    },
}

/// Preset bundles for common VFX. Each preset bakes a [`Particle`] template +
/// color curve so callers don't need to tune every field.
#[derive(Reflect, Debug, Clone, Copy)]
pub enum EmitterPreset {
    /// Soft grey smoke that rises and fades. Use for chimneys, fires.
    Smoke,
    /// Tan dust kicked up by movement. Short-lived, settles fast.
    Dust,
    /// Bright sparkle motes orbiting an entity. Used by magic/charging effects.
    Sparkle,
    /// Bright additive orange/yellow flame motes. Use for torches.
    Fire,
}

/// Shared GPU assets every particle reuses. One soft-disc texture serves every
/// preset because the per-particle `Sprite::color` tints it.
#[derive(Resource, Clone)]
pub struct ParticleAssets {
    /// 32x32 RGBA radial soft-disc, white RGB with smoothstep alpha falloff.
    pub texture: Handle<Image>,
    /// Reserved for callers that want to spawn mesh-based particle variants;
    /// currently unused by the sprite path. Kept on the resource so future
    /// code can attach a unit quad without re-running `Startup`.
    pub mesh: Handle<Mesh>,
}

/// Plugin entry point. Registers reflect types, builds the shared texture on
/// `Startup`, and runs [`tick_emitters`] then [`age_particles`] in
/// [`inf2d_core::SimulationSet`].
pub struct ParticlesPlugin;

impl Plugin for ParticlesPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Particle>()
            .register_type::<Emitter>()
            .register_type::<EmitterShape>()
            .register_type::<EmitterPreset>()
            .add_systems(Startup, build_particle_assets)
            .add_systems(
                Update,
                (tick_emitters, age_particles)
                    .chain()
                    .in_set(inf2d_core::SimulationSet),
            );

        #[cfg(feature = "demo_particles")]
        app.add_systems(Startup, spawn_demo_emitters);
    }
}

/// Build the shared soft-disc texture + unit quad mesh and stash them in
/// [`ParticleAssets`] for [`spawn_particle`] to clone handles from.
fn build_particle_assets(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let texture = images.add(build_soft_disc_image());
    let mesh = meshes.add(Mesh::from(bevy::math::primitives::Rectangle::new(1.0, 1.0)));
    commands.insert_resource(ParticleAssets { texture, mesh });
}

/// Synthesize the 32x32 soft-disc texture in CPU memory. Alpha is
/// `smoothstep(1, 0, r)` squared (same trick the light falloff uses); RGB is
/// pure white so per-particle `Sprite::color` is the only tint knob.
fn build_soft_disc_image() -> Image {
    let size = PARTICLE_TEX_SIZE;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let center = size as f32 * 0.5;
    let max_r = center;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let r = (dx * dx + dy * dy).sqrt() / max_r;
            // smoothstep(1, 0, r) == 1 - smoothstep(0, 1, r)
            let t = (1.0 - r).clamp(0.0, 1.0);
            let smooth = t * t * (3.0 - 2.0 * t);
            // Squared so the bright core is small and the falloff tail long.
            let a = (smooth * smooth * 255.0).round().clamp(0.0, 255.0) as u8;
            let off = ((y * size + x) * 4) as usize;
            buf[off] = 255;
            buf[off + 1] = 255;
            buf[off + 2] = 255;
            buf[off + 3] = a;
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        buf,
        // Linear (non-sRGB) format so the alpha curve we wrote above samples
        // verbatim — sRGB would gamma-apply to RGB and lighten the white core.
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    // Linear filtering keeps the disc smooth as the camera zooms.
    image.sampler = ImageSampler::linear();
    image
}

/// Advance every emitter: fire its one-shot burst on first tick, then spawn
/// continuous particles at `rate * dt` per frame using a fractional accumulator
/// so sub-1-particle rates still emit eventually.
pub fn tick_emitters(
    mut commands: Commands,
    time: Res<Time>,
    assets: Option<Res<ParticleAssets>>,
    mut q: Query<(&mut Emitter, &GlobalTransform)>,
) {
    let Some(assets) = assets else {
        // Texture not built yet (Startup ordering); skip this frame.
        return;
    };
    let dt = time.delta_secs();
    let mut rng = rand::rng();

    for (mut emitter, gt) in &mut q {
        if !emitter.enabled {
            continue;
        }

        let origin = gt.translation().truncate();

        // One-shot burst on first enabled tick after spawn.
        if emitter.burst > 0 && !emitter._burst_done {
            let count = emitter.burst;
            let shape = emitter.shape;
            let preset = emitter.preset;
            for _ in 0..count {
                let pos = sample_spawn_pos(origin, shape, &mut rng);
                let template = particle_from_preset(preset, &mut rng);
                spawn_particle(&mut commands, &assets, pos, template);
            }
            emitter._burst_done = true;
        }

        // Continuous rate, accumulator-driven so fractional rates still emit.
        emitter._spawn_accum += dt * emitter.rate;
        // Cap accumulator catch-up to avoid pathological spawn storms after a
        // long stall (e.g. window minimized, debugger paused). 256 particles
        // per emitter per frame is plenty for any normal effect.
        if emitter._spawn_accum > 256.0 {
            emitter._spawn_accum = 256.0;
        }
        while emitter._spawn_accum >= 1.0 {
            let pos = sample_spawn_pos(origin, emitter.shape, &mut rng);
            let template = particle_from_preset(emitter.preset, &mut rng);
            spawn_particle(&mut commands, &assets, pos, template);
            emitter._spawn_accum -= 1.0;
        }
    }
}

/// Age every live particle by `dt`: integrate velocity (with gravity), lerp
/// color + size between `start_*` and `end_*` endpoints, advance rotation, and
/// despawn once `age >= lifetime`.
pub fn age_particles(
    mut commands: Commands,
    time: Res<Time>,
    mut q: Query<(Entity, &mut Particle, &mut Transform, &mut Sprite)>,
) {
    let dt = time.delta_secs();

    for (entity, mut p, mut tf, mut sprite) in &mut q {
        p.age += dt;
        if p.age >= p.lifetime {
            commands.entity(entity).despawn();
            continue;
        }

        // Velocity integration (semi-implicit Euler — apply gravity to velocity
        // first, then move).
        let gravity = p.gravity;
        p.velocity += gravity * dt;
        let v = p.velocity;
        tf.translation.x += v.x * dt;
        tf.translation.y += v.y * dt;

        // Normalized life parameter in [0, 1].
        let t = (p.age / p.lifetime).clamp(0.0, 1.0);

        sprite.color = lerp_color(p.start_color, p.end_color, t);
        let size = lerp_f32(p.start_size, p.end_size, t);
        sprite.custom_size = Some(Vec2::splat(size));

        let av = p.angular_velocity;
        if av != 0.0 {
            tf.rotate_z(av * dt);
        }
    }
}

/// Spawn a single particle entity at `world_pos` using the given template. The
/// shared soft-disc texture from `assets` is cloned (cheap — handles are
/// reference-counted).
pub fn spawn_particle(
    commands: &mut Commands,
    assets: &ParticleAssets,
    world_pos: Vec2,
    template: Particle,
) {
    let color = template.start_color;
    let size = template.start_size;
    commands.spawn((
        Sprite {
            image: assets.texture.clone(),
            color,
            custom_size: Some(Vec2::splat(size)),
            ..default()
        },
        Transform::from_xyz(
            world_pos.x,
            world_pos.y,
            RenderLayer::ENTITY + PARTICLE_Z_OFFSET,
        ),
        Visibility::default(),
        template,
        Name::new("Particle"),
    ));
}

/// Sample a 2D point within the emitter's [`EmitterShape`] around `origin`.
fn sample_spawn_pos(origin: Vec2, shape: EmitterShape, rng: &mut impl Rng) -> Vec2 {
    match shape {
        EmitterShape::Point => origin,
        EmitterShape::Disc { radius } => {
            // Uniform disc sampling: sqrt(u) for radius keeps density uniform.
            let theta = rng.random_range(0.0..TAU);
            let r = radius * rng.random_range(0.0f32..1.0).sqrt();
            origin + Vec2::new(theta.cos(), theta.sin()) * r
        }
        EmitterShape::Ring { radius } => {
            let theta = rng.random_range(0.0..TAU);
            origin + Vec2::new(theta.cos(), theta.sin()) * radius
        }
    }
}

/// Build a [`Particle`] template from a [`EmitterPreset`], randomizing per-spawn
/// parameters (velocity, lifetime, etc.) for natural-looking variation.
pub fn particle_from_preset(preset: EmitterPreset, rng: &mut impl Rng) -> Particle {
    match preset {
        EmitterPreset::Smoke => Particle {
            lifetime: rng.random_range(1.5..2.5),
            age: 0.0,
            velocity: Vec2::new(rng.random_range(-8.0..8.0), rng.random_range(20.0..40.0)),
            angular_velocity: rng.random_range(-0.4..0.4),
            start_color: Color::srgba(0.7, 0.7, 0.7, 0.6),
            end_color: Color::srgba(0.7, 0.7, 0.7, 0.0),
            start_size: 6.0,
            end_size: 22.0,
            gravity: Vec2::ZERO,
        },
        EmitterPreset::Dust => {
            let theta = rng.random_range(0.0..TAU);
            let speed = rng.random_range(30.0..60.0);
            Particle {
                lifetime: rng.random_range(0.4..0.7),
                age: 0.0,
                velocity: Vec2::new(theta.cos(), theta.sin()) * speed,
                angular_velocity: rng.random_range(-1.0..1.0),
                start_color: Color::srgba(0.78, 0.65, 0.42, 0.8),
                end_color: Color::srgba(0.78, 0.65, 0.42, 0.0),
                start_size: 8.0,
                end_size: 4.0,
                gravity: Vec2::new(0.0, -40.0),
            }
        }
        EmitterPreset::Sparkle => {
            // Random tangential-ish velocity at slow speeds; we just pick a
            // random direction since "tangential" needs an orbit center the
            // particle doesn't know about.
            let theta = rng.random_range(0.0..TAU);
            let speed = rng.random_range(8.0..16.0);
            let sign = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
            Particle {
                lifetime: rng.random_range(0.5..1.0),
                age: 0.0,
                velocity: Vec2::new(theta.cos(), theta.sin()) * speed,
                angular_velocity: sign * 2.0,
                start_color: Color::srgba(1.0, 0.95, 0.6, 1.0),
                end_color: Color::srgba(1.0, 0.95, 0.6, 0.0),
                start_size: 3.0,
                end_size: 1.0,
                gravity: Vec2::ZERO,
            }
        }
        EmitterPreset::Fire => Particle {
            lifetime: rng.random_range(0.6..1.2),
            age: 0.0,
            velocity: Vec2::new(rng.random_range(-4.0..4.0), rng.random_range(40.0..80.0)),
            angular_velocity: rng.random_range(-0.5..0.5),
            start_color: Color::srgba(1.0, 0.55, 0.18, 1.0),
            end_color: Color::srgba(0.4, 0.05, 0.0, 0.0),
            start_size: 10.0,
            end_size: 3.0,
            gravity: Vec2::ZERO,
        },
    }
}

/// Lerp two colors component-wise in linear space. Bevy 0.18's `Color` does not
/// expose a generic mix on the enum, so we route through `LinearRgba`.
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let la = a.to_linear();
    let lb = b.to_linear();
    let lerped = LinearRgba::new(
        lerp_f32(la.red, lb.red, t),
        lerp_f32(la.green, lb.green, t),
        lerp_f32(la.blue, lb.blue, t),
        lerp_f32(la.alpha, lb.alpha, t),
    );
    Color::LinearRgba(lerped)
}

/// Scalar linear interpolation. Inline-friendly; no `clamp` because callers
/// already pre-clamp `t`.
#[inline]
fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Demo: spawn one emitter of each preset at four offset tile positions so the
/// system is immediately visible. Gated behind the `demo_particles` Cargo
/// feature so production builds stay silent.
#[cfg(feature = "demo_particles")]
fn spawn_demo_emitters(mut commands: Commands) {
    use inf2d_core::{tile_to_world, WorldTile};

    let demos: [(WorldTile, EmitterPreset, f32, &str); 4] = [
        (WorldTile::new(0, 0), EmitterPreset::Smoke, 12.0, "DemoEmitter:Smoke"),
        (WorldTile::new(3, 0), EmitterPreset::Dust, 20.0, "DemoEmitter:Dust"),
        (WorldTile::new(0, 3), EmitterPreset::Sparkle, 18.0, "DemoEmitter:Sparkle"),
        (WorldTile::new(3, 3), EmitterPreset::Fire, 30.0, "DemoEmitter:Fire"),
    ];

    for (tile, preset, rate, name) in demos {
        let pos = tile_to_world(tile);
        commands.spawn((
            Emitter {
                preset,
                shape: EmitterShape::Point,
                rate,
                burst: 0,
                enabled: true,
                _spawn_accum: 0.0,
                _burst_done: false,
            },
            Transform::from_xyz(pos.x, pos.y, 0.0),
            Visibility::default(),
            Name::new(name),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_disc_is_correct_size_and_format() {
        let img = build_soft_disc_image();
        let ext = img.texture_descriptor.size;
        assert_eq!(ext.width, PARTICLE_TEX_SIZE);
        assert_eq!(ext.height, PARTICLE_TEX_SIZE);
        assert_eq!(img.texture_descriptor.format, TextureFormat::Rgba8Unorm);
    }

    #[test]
    fn soft_disc_center_bright_edges_transparent() {
        let img = build_soft_disc_image();
        let data = img.data.as_ref().expect("soft disc has cpu data");
        let stride = PARTICLE_TEX_SIZE * 4;
        let cx = PARTICLE_TEX_SIZE / 2;
        let cy = PARTICLE_TEX_SIZE / 2;
        let center_alpha = data[(cy * stride + cx * 4 + 3) as usize];
        let corner_alpha = data[3];
        assert!(center_alpha > 200, "center should be near-opaque, got {center_alpha}");
        assert_eq!(corner_alpha, 0, "corners should be fully transparent");
    }

    #[test]
    fn lerp_color_endpoints_match() {
        let a = Color::srgba(1.0, 0.0, 0.0, 1.0);
        let b = Color::srgba(0.0, 0.0, 1.0, 0.0);
        let at_zero = lerp_color(a, b, 0.0).to_linear();
        let at_one = lerp_color(a, b, 1.0).to_linear();
        let la = a.to_linear();
        let lb = b.to_linear();
        assert!((at_zero.red - la.red).abs() < 1e-5);
        assert!((at_zero.alpha - la.alpha).abs() < 1e-5);
        assert!((at_one.blue - lb.blue).abs() < 1e-5);
        assert!((at_one.alpha - lb.alpha).abs() < 1e-5);
    }

    #[test]
    fn preset_smoke_has_upward_drift() {
        // Hammer the RNG-driven preset to confirm velocity.y stays positive
        // (smoke must rise). Range is 20..40 so this is deterministic per the
        // preset spec, but the test guards against accidental sign flips.
        let mut rng = rand::rng();
        for _ in 0..32 {
            let p = particle_from_preset(EmitterPreset::Smoke, &mut rng);
            assert!(p.velocity.y > 0.0, "smoke must rise, got {}", p.velocity.y);
            assert!(p.gravity == Vec2::ZERO);
        }
    }

    #[test]
    fn preset_dust_settles_with_gravity() {
        let mut rng = rand::rng();
        let p = particle_from_preset(EmitterPreset::Dust, &mut rng);
        assert!(p.gravity.y < 0.0, "dust should be pulled down on screen");
        assert!(p.lifetime <= 0.7 && p.lifetime >= 0.4);
    }

    #[test]
    fn sample_spawn_pos_point_returns_origin() {
        let mut rng = rand::rng();
        let o = Vec2::new(10.0, -3.0);
        assert_eq!(sample_spawn_pos(o, EmitterShape::Point, &mut rng), o);
    }

    #[test]
    fn sample_spawn_pos_ring_lands_on_radius() {
        let mut rng = rand::rng();
        let r = 50.0;
        for _ in 0..16 {
            let p = sample_spawn_pos(Vec2::ZERO, EmitterShape::Ring { radius: r }, &mut rng);
            assert!((p.length() - r).abs() < 1e-3);
        }
    }

    #[test]
    fn sample_spawn_pos_disc_within_radius() {
        let mut rng = rand::rng();
        let r = 25.0;
        for _ in 0..32 {
            let p = sample_spawn_pos(Vec2::ZERO, EmitterShape::Disc { radius: r }, &mut rng);
            assert!(p.length() <= r + 1e-3);
        }
    }
}
