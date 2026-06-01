//! Custom instanced grass for the voxel world.
//!
//! A grass tuft is built procedurally (a handful of thin, vertex-colored blades
//! with a dark-root → bright-tip gradient — no asset, no texture). It's scattered
//! as lightweight `Mesh3d` instances on land surfaces, streamed as a ring of
//! tiles around the player and distance-culled. Bevy auto-batches the shared
//! mesh+material into instanced draws, so thousands of blades cost a few draws.
//! A few material shades give field color variation.
//!
//! Wind + player-shove animation runs in a custom vertex shader via
//! `ExtendedMaterial<StandardMaterial, GrassWind>` (see `grass.wgsl`).

use std::collections::HashMap;

use bevy::asset::embedded_asset;
use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::Material;
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use inf3d_camera::IsoCamera;
use inf3d_core::FollowTarget;
use inf3d_worldgen::Terrain;

/// Side length (in voxel columns) of one grass streaming tile.
const GRASS_TILE: i32 = 16;
/// Ring radius, in tiles, kept populated around the player.
const GRASS_RADIUS_TILES: i32 = 4;
/// Per-column chance to place a grass tuft (high = thick, spread coverage).
const GRASS_DENSITY: f32 = 0.85;
/// Hard cap on tufts per tile.
const MAX_PER_TILE: usize = 140;
/// Distance from the camera beyond which grass is hidden.
const CULL_DIST: f32 = 110.0;

// Tuft geometry — short, full tufts of tapered-quad blades.
const BLADES_PER_TUFT: usize = 18;
const BLADE_HEIGHT: f32 = 0.42;
const BLADE_WIDTH: f32 = 0.07;
/// Radius the blades fan out within a tuft (wider = fuller tuft).
const TUFT_SPREAD: f32 = 0.22;

/// Wind/shove + color parameters, mirrored 1:1 by `GrassParams` in `grass.wgsl`.
#[derive(Clone, Default, ShaderType)]
struct GrassParams {
    /// Blade base color (linear rgb; a unused).
    base_color: Vec4,
    /// Player world position (xyz); w unused.
    player: Vec4,
    wind_strength: f32,
    bend_radius: f32,
    bend_strength: f32,
    _pad: f32,
}

/// Standalone grass material — deliberately NOT an `ExtendedMaterial<StandardMaterial,_>`
/// (that collides on bind-group slots, leaving binding 100 out of the layout and
/// crashing the vertex pipeline). Here we own group 2: params at binding 0, and
/// grass.wgsl supplies both the wind/shove vertex stage and a simple lit fragment.
#[derive(Asset, AsBindGroup, TypePath, Clone, Default)]
struct GrassMaterial {
    #[uniform(0, visibility(vertex, fragment))]
    params: GrassParams,
}

impl Material for GrassMaterial {
    fn vertex_shader() -> ShaderRef {
        "embedded://inf3d_render/grass.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "embedded://inf3d_render/grass.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Opaque
    }
}

#[derive(Resource)]
struct GrassAssets {
    tuft: Handle<Mesh>,
    materials: Vec<Handle<GrassMaterial>>,
}

/// Tile coord -> parent entity, despawned recursively when out of the ring.
#[derive(Resource, Default)]
struct GrassField {
    tiles: HashMap<IVec2, Entity>,
}

/// Marks an individual grass instance (for distance culling).
#[derive(Component)]
struct GrassInstance;

pub struct GrassPlugin;

impl Plugin for GrassPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "grass.wgsl");
        app.add_plugins(MaterialPlugin::<GrassMaterial>::default())
            .init_resource::<GrassField>()
            .add_systems(Startup, setup_grass)
            .add_systems(Update, (stream_grass, cull_grass, update_grass_wind));
    }
}

fn setup_grass(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GrassMaterial>>,
) {
    let tuft = meshes.add(grass_tuft());

    // A few brighter green shades for natural field variation.
    let shades = [
        Color::srgb(0.30, 0.58, 0.20),
        Color::srgb(0.38, 0.66, 0.26),
        Color::srgb(0.26, 0.52, 0.17),
        Color::srgb(0.44, 0.72, 0.30),
    ];
    let mats: Vec<Handle<GrassMaterial>> = shades
        .iter()
        .map(|c| {
            // Use the sRGB components directly (our fragment is simple/unlit-ish,
            // not full PBR) so the green stays bright instead of darkening to grey.
            let s = c.to_srgba();
            materials.add(GrassMaterial {
                params: GrassParams {
                    base_color: Vec4::new(s.red, s.green, s.blue, 1.0),
                    wind_strength: 0.15,
                    bend_radius: 2.5,
                    bend_strength: 0.8,
                    ..default()
                },
            })
        })
        .collect();

    commands.insert_resource(GrassAssets {
        tuft,
        materials: mats,
    });
}

/// Build a grass tuft: `BLADES_PER_TUFT` short tapered quads fanning outward,
/// with a slightly-dark root → full-color tip gradient. Normals point UP so the
/// blades are lit like the ground (bright), not dark sideways-facing slivers.
fn grass_tuft() -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let root_col = [0.7, 0.7, 0.7, 1.0]; // gently darken the base
    let tip_col = [1.0, 1.0, 1.0, 1.0]; // full material color at the tip
    let up = [0.0, 1.0, 0.0];

    let mut rng = StdRng::seed_from_u64(0xA11CE);
    for i in 0..BLADES_PER_TUFT {
        let angle =
            (i as f32 / BLADES_PER_TUFT as f32) * std::f32::consts::TAU + rng.random_range(-0.5..0.5);
        let dist = rng.random_range(0.0..TUFT_SPREAD);
        let dir = Vec3::new(angle.cos(), 0.0, angle.sin());
        let base = dir * dist;
        // Blade faces perpendicular to its outward direction.
        let face = Vec3::new(angle.sin(), 0.0, -angle.cos());
        let rb = face * (BLADE_WIDTH * 0.5); // base half-width
        let rt = face * (BLADE_WIDTH * 0.18); // tip half-width (taper)
        let height = BLADE_HEIGHT * rng.random_range(0.8..1.15);
        let tip = base + Vec3::Y * height + dir * rng.random_range(0.02..0.1);

        let bl = base - rb;
        let br = base + rb;
        let tl = tip - rt;
        let tr = tip + rt;

        let vi = positions.len() as u32;
        for p in [bl, br, tr, tl] {
            positions.push([p.x, p.y, p.z]);
            normals.push(up);
        }
        colors.extend_from_slice(&[root_col, root_col, tip_col, tip_col]);
        indices.extend_from_slice(&[vi, vi + 1, vi + 2, vi, vi + 2, vi + 3]);
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Stream grass tiles in/out of a ring around the player.
fn stream_grass(
    mut commands: Commands,
    assets: Res<GrassAssets>,
    terrain: Res<Terrain>,
    mut field: ResMut<GrassField>,
    player_q: Query<&Transform, With<FollowTarget>>,
) {
    let Ok(player) = player_q.single() else {
        return;
    };
    let center = IVec2::new(
        (player.translation.x / GRASS_TILE as f32).floor() as i32,
        (player.translation.z / GRASS_TILE as f32).floor() as i32,
    );

    for dx in -GRASS_RADIUS_TILES..=GRASS_RADIUS_TILES {
        for dz in -GRASS_RADIUS_TILES..=GRASS_RADIUS_TILES {
            let tile = center + IVec2::new(dx, dz);
            if field.tiles.contains_key(&tile) {
                continue;
            }
            let entity = spawn_tile(&mut commands, &assets, &terrain, tile);
            field.tiles.insert(tile, entity);
        }
    }

    field.tiles.retain(|tile, entity| {
        let in_ring = (tile.x - center.x).abs() <= GRASS_RADIUS_TILES
            && (tile.y - center.y).abs() <= GRASS_RADIUS_TILES;
        if !in_ring {
            commands.entity(*entity).despawn();
        }
        in_ring
    });
}

/// Build one tile's grass as children of a parent at the origin (so child local
/// transforms equal world positions). Placement is deterministic per tile.
fn spawn_tile(
    commands: &mut Commands,
    assets: &GrassAssets,
    terrain: &Terrain,
    tile: IVec2,
) -> Entity {
    let seed = (tile.x as i64 as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (tile.y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    let mut rng = StdRng::seed_from_u64(seed);

    let base_x = tile.x * GRASS_TILE;
    let base_z = tile.y * GRASS_TILE;

    commands
        .spawn((
            Transform::default(),
            Visibility::default(),
            Name::new(format!("GrassTile {},{}", tile.x, tile.y)),
        ))
        .with_children(|parent| {
            let mut placed = 0usize;
            for lx in 0..GRASS_TILE {
                for lz in 0..GRASS_TILE {
                    if placed >= MAX_PER_TILE {
                        return;
                    }
                    if rng.random::<f32>() > GRASS_DENSITY {
                        continue;
                    }
                    let x = base_x + lx;
                    let z = base_z + lz;
                    if !terrain.is_land(x, z) {
                        continue;
                    }
                    // Jitter within the column so tufts don't grid-align.
                    let pos = terrain.stand_pos(x, z)
                        + Vec3::new(
                            rng.random_range(-0.4..0.4),
                            0.0,
                            rng.random_range(-0.4..0.4),
                        );
                    let yaw = rng.random_range(0.0..std::f32::consts::TAU);
                    let scale = rng.random_range(0.8..1.3);
                    let mat = assets.materials[rng.random_range(0..assets.materials.len())].clone();
                    parent.spawn((
                        Mesh3d(assets.tuft.clone()),
                        MeshMaterial3d(mat),
                        Transform::from_translation(pos)
                            .with_rotation(Quat::from_rotation_y(yaw))
                            .with_scale(Vec3::splat(scale)),
                        NotShadowCaster,
                        GrassInstance,
                    ));
                    placed += 1;
                }
            }
        })
        .id()
}

/// Feed the player's world position into every grass material so the vertex
/// shader can bend blades away from it. Time is read from `globals.time` in the
/// shader, so no time uniform is needed here.
fn update_grass_wind(
    player_q: Query<&Transform, With<FollowTarget>>,
    mut materials: ResMut<Assets<GrassMaterial>>,
) {
    let Ok(player) = player_q.single() else {
        return;
    };
    let p = player.translation.extend(0.0);
    for (_, mat) in materials.iter_mut() {
        mat.params.player = p;
    }
}

/// Hide grass instances beyond [`CULL_DIST`] from the camera.
fn cull_grass(
    camera_q: Query<&GlobalTransform, With<IsoCamera>>,
    mut grass_q: Query<(&GlobalTransform, &mut Visibility), With<GrassInstance>>,
) {
    let Ok(cam) = camera_q.single() else {
        return;
    };
    let cam_pos = cam.translation();
    let cull_sq = CULL_DIST * CULL_DIST;
    for (gt, mut vis) in &mut grass_q {
        *vis = if gt.translation().distance_squared(cam_pos) <= cull_sq {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}
