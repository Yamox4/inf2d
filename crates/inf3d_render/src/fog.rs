//! Fog-of-war edge haze.
//!
//! A large, camera-following [`FogVolume`] driven by a procedural 3D density
//! texture: CLEAR around the player and THICK/opaque toward the world edge, so
//! it covers the streamed-chunk cutoff and the grey void behind it.
//!
//! The camera is ORTHOGRAPHIC, so per-screen-column fog can't come from camera
//! distance — instead a *radial* density is baked into the 3D texture (clear
//! center → dense edges), constant across the vertical axis so the upward smoke
//! scroll keeps the center clear.
//!
//! Why volumetric and not `DistanceFog`: the voxel terrain uses a custom
//! material shader that contains no fog code, so `DistanceFog` never touches the
//! green terrain. Volumetric fog renders in its own pass and does. It needs
//! `VolumetricFog` on the camera (see camera.rs) and `VolumetricLight` on the
//! sun (see world.rs).

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::FogVolume;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use inf3d_core::FollowTarget;

/// Cool horizon tone; also the clear color so the edge dissolves into it.
const HORIZON: Color = Color::srgb(0.60, 0.67, 0.71);
/// Horizontal coverage (the volume follows the player).
const FOG_EXTENT: f32 = 700.0;
/// Vertical coverage — tall enough to engulf all terrain so there's no visible
/// height gradient (uniform fog of war, not ground fog).
const FOG_TALL: f32 = 240.0;
/// Density at the rim. The rim still needs to be opaque to hide the void, but
/// the previous 2.5 caused the volumetric pass to tint near-camera surfaces
/// (specifically grass, which writes the depth prepass while the voxel terrain
/// does not) a heavy grey-blue. The radial falloff (`smoothstep(0.30, 0.95, r)`)
/// keeps the clear center clear regardless of this max value.
const FOG_DENSITY: f32 = 0.9;
/// Side length of the procedural 3D density texture.
const NOISE_N: usize = 64;

#[derive(Component)]
struct FogOfWar;

pub struct FogPlugin;

impl Plugin for FogPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(HORIZON))
            .add_systems(Startup, spawn_fog)
            .add_systems(Update, follow_fog);
    }
}

/// Hermite smoothstep; 0 below `edge0`, 1 above `edge1`.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Cheap deterministic hash of a 3D integer lattice point → [0,1).
fn hash3(x: i32, y: i32, z: i32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(374761393)
        .wrapping_add((y as u32).wrapping_mul(668265263))
        .wrapping_add((z as u32).wrapping_mul(2147483647));
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^= h >> 16;
    (h as f32) / (u32::MAX as f32)
}

/// Trilinearly-interpolated value noise in [0,1].
fn value_noise_3d(x: f32, y: f32, z: f32) -> f32 {
    let xi = x.floor();
    let yi = y.floor();
    let zi = z.floor();
    let xf = x - xi;
    let yf = y - yi;
    let zf = z - zi;
    // Smooth the cell fractions so octaves don't show lattice creases.
    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = yf * yf * (3.0 - 2.0 * yf);
    let w = zf * zf * (3.0 - 2.0 * zf);
    let (xi, yi, zi) = (xi as i32, yi as i32, zi as i32);

    let c000 = hash3(xi, yi, zi);
    let c100 = hash3(xi + 1, yi, zi);
    let c010 = hash3(xi, yi + 1, zi);
    let c110 = hash3(xi + 1, yi + 1, zi);
    let c001 = hash3(xi, yi, zi + 1);
    let c101 = hash3(xi + 1, yi, zi + 1);
    let c011 = hash3(xi, yi + 1, zi + 1);
    let c111 = hash3(xi + 1, yi + 1, zi + 1);

    let x00 = c000 + (c100 - c000) * u;
    let x10 = c010 + (c110 - c010) * u;
    let x01 = c001 + (c101 - c001) * u;
    let x11 = c011 + (c111 - c011) * u;
    let y0 = x00 + (x10 - x00) * v;
    let y1 = x01 + (x11 - x01) * v;
    y0 + (y1 - y0) * w
}

/// 3-octave fbm of [`value_noise_3d`], normalized to [0,1].
fn fbm(x: f32, y: f32, z: f32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for _ in 0..3 {
        sum += amp * value_noise_3d(x * freq, y * freq, z * freq);
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

/// Bake the procedural radial-falloff smoke into an R8 3D image.
fn build_density_image() -> Image {
    let n = NOISE_N;
    let half = n as f32 * 0.5;
    // A few noise cells across the volume so the smoke has visible structure.
    let scale = 4.0 / n as f32;
    let mut data = vec![0u8; n * n * n];

    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                // Radial falloff in XZ only (constant across Y) — clear center,
                // dense rim — so vertical scroll never moves the clear hole.
                let dx = x as f32 - half;
                let dz = z as f32 - half;
                let r = (dx * dx + dz * dz).sqrt() / half;
                let falloff = smoothstep(0.30, 0.95, r);

                // Heavily reduce noise contribution: previously 65% of the
                // density came from FBM noise, which (with low step_count) shows
                // up as a stippled "dot pattern" projected onto every surface
                // the fog touches (grass, terrain). Keep just a hint of variation.
                let noise = fbm(x as f32 * scale, y as f32 * scale, z as f32 * scale);
                let density = falloff * (0.92 + 0.08 * noise);

                let idx = (z * n * n) + (y * n) + x;
                data[idx] = (density.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: n as u32,
            height: n as u32,
            depth_or_array_layers: n as u32,
        },
        TextureDimension::D3,
        data,
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        ..default()
    });
    image
}

fn spawn_fog(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let density_texture = Some(images.add(build_density_image()));
    commands.spawn((
        FogVolume {
            fog_color: HORIZON,
            density_factor: FOG_DENSITY,
            density_texture,
            scattering: 0.3,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 0.0).with_scale(Vec3::new(FOG_EXTENT, FOG_TALL, FOG_EXTENT)),
        Visibility::default(),
        FogOfWar,
    ));
}

/// Keep the fog volume centered on the player (XZ) so the endless world keeps a
/// consistent fog-of-war edge, and scroll the density texture so the smoke
/// billows upward. Y stays at 0 with a tall extent — uniform, no layer.
fn follow_fog(
    time: Res<Time>,
    player_q: Query<&Transform, (With<FollowTarget>, Without<FogOfWar>)>,
    mut fog_q: Query<(&mut Transform, &mut FogVolume), With<FogOfWar>>,
) {
    let Ok((mut fog, mut volume)) = fog_q.single_mut() else {
        return;
    };
    if let Ok(player) = player_q.single() {
        fog.translation.x = player.translation.x;
        fog.translation.z = player.translation.z;
    }
    // Mostly-vertical scroll keeps the radial clear center fixed while the smoke
    // visibly rises; a tiny X drift breaks up repetition.
    let dt = time.delta_secs();
    volume.density_texture_offset.y += dt * 0.03;
    volume.density_texture_offset.x += dt * 0.005;
}
