//! Streaming instanced grass.
//!
//! Loads the grass GLTF's **mesh + material as shared handles** (via Bevy's gltf
//! asset labels) and renders many lightweight `Mesh3d` instances — rather than
//! spawning the full GLTF *scene* per blade (which is both a perf killer and, for
//! this model, crashes the scene spawner on an unregistered reflected type).
//!
//! Placement streams as a ring of tiles around the player: each tile
//! deterministically scatters a capped number of instances on land surfaces
//! (never water), parented to a tile entity so it despawns in one call when the
//! player walks away. A distance cull hides far instances.
//!
//! Wind animation (the model's morph-target clip) is intentionally not played —
//! per-instance morph animation doesn't scale. The optimized path is a vertex
//! shader wobble, which belongs in the rendering pass.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::gltf::GltfAssetLabel;
use bevy::light::NotShadowCaster;
use bevy::mesh::VertexAttributeValues;
use bevy::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::camera::IsoCamera;
use crate::player::Player;
use crate::world::Terrain;

/// Source GLTF; we pull its first mesh primitive + first material.
const GRASS_GLTF: &str = "textures/grass_model/scene.gltf";

/// Side length (in voxel columns) of one grass streaming tile.
const GRASS_TILE: i32 = 16;
/// Ring radius, in tiles, kept populated around the player.
const GRASS_RADIUS_TILES: i32 = 3;
/// Per-column chance to place a grass instance.
const GRASS_DENSITY: f32 = 0.05;
/// Hard cap on instances per tile.
const MAX_PER_TILE: usize = 12;
/// Target footprint of one grass instance, in world units (1 = one voxel tile).
/// The model's auto-measured bounding box is scaled to this, so it fits one tile
/// regardless of the model's native units.
const TILE_FIT: f32 = 1.0;
/// Distance from the camera beyond which grass is hidden.
const CULL_DIST: f32 = 120.0;

/// Shared mesh + material for all grass instances.
#[derive(Resource)]
struct GrassAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    /// Scale that makes the model span exactly [`TILE_FIT`]; computed from the
    /// mesh AABB once it loads.
    fit_scale: f32,
}

/// Tile coord -> parent entity, despawned recursively when out of the ring.
#[derive(Resource, Default)]
struct GrassField {
    tiles: HashMap<IVec2, Entity>,
}

/// True once the grass mesh has loaded and had its morph-target data stripped.
/// Streaming waits for this so instances never use the morphed pipeline (which
/// would crash without per-instance morph weights).
#[derive(Resource, Default)]
struct GrassReady(bool);

/// Marks an individual grass instance (for distance culling).
#[derive(Component)]
struct GrassInstance;

pub struct GrassPlugin;

impl Plugin for GrassPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GrassField>()
            .init_resource::<GrassReady>()
            .add_systems(Startup, load_grass)
            .add_systems(Update, (prepare_grass, stream_grass, cull_grass).chain());
    }
}

fn load_grass(mut commands: Commands, asset_server: Res<AssetServer>) {
    let mesh = asset_server.load(
        GltfAssetLabel::Primitive {
            mesh: 0,
            primitive: 0,
        }
        .from_asset(GRASS_GLTF),
    );
    let material = asset_server.load(
        GltfAssetLabel::Material {
            index: 0,
            is_scale_inverted: false,
        }
        .from_asset(GRASS_GLTF),
    );
    commands.insert_resource(GrassAssets {
        mesh,
        material,
        fit_scale: 1.0,
    });
}

/// Once the grass mesh loads, strip its morph-target data so the static
/// instances render via the model-only pipeline (avoids the morphed-pipeline
/// bind-group mismatch crash). Gates streaming via [`GrassReady`].
fn prepare_grass(
    asset_server: Res<AssetServer>,
    mut assets: ResMut<GrassAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut ready: ResMut<GrassReady>,
) {
    if ready.0 {
        return;
    }
    if !matches!(
        asset_server.get_load_state(assets.mesh.id()),
        Some(bevy::asset::LoadState::Loaded)
    ) {
        return;
    }
    let Some(src) = meshes.get(&assets.mesh) else {
        return;
    };

    // Auto-fit: measure the model's bounds from its vertex positions and scale
    // so its largest XZ footprint spans one tile (model native units vary wildly).
    if let Some(VertexAttributeValues::Float32x3(positions)) =
        src.attribute(Mesh::ATTRIBUTE_POSITION)
    {
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for p in positions {
            let v = Vec3::from_array(*p);
            min = min.min(v);
            max = max.max(v);
        }
        let size = max - min;
        let footprint = size.x.max(size.z).max(1e-3);
        assets.fit_scale = TILE_FIT / footprint;
    }

    // Strip morph-target data so instances use the model-only pipeline.
    if src.has_morph_targets() {
        let stripped = strip_morph(src);
        if let Some(dst) = meshes.get_mut(&assets.mesh) {
            *dst = stripped;
        }
    }
    ready.0 = true;
}

/// Rebuild a mesh copying only its standard vertex attributes + indices, leaving
/// out morph-target data (a fresh mesh has none).
fn strip_morph(src: &Mesh) -> Mesh {
    let mut mesh = Mesh::new(src.primitive_topology(), RenderAssetUsages::default());
    for attr in [
        Mesh::ATTRIBUTE_POSITION,
        Mesh::ATTRIBUTE_NORMAL,
        Mesh::ATTRIBUTE_UV_0,
        Mesh::ATTRIBUTE_TANGENT,
        Mesh::ATTRIBUTE_COLOR,
    ] {
        if let Some(values) = src.attribute(attr) {
            mesh.insert_attribute(attr, values.clone());
        }
    }
    if let Some(indices) = src.indices() {
        mesh.insert_indices(indices.clone());
    }
    mesh
}

/// Stream grass tiles in/out of a ring around the player.
fn stream_grass(
    mut commands: Commands,
    assets: Res<GrassAssets>,
    terrain: Res<Terrain>,
    ready: Res<GrassReady>,
    mut field: ResMut<GrassField>,
    player_q: Query<&Transform, With<Player>>,
) {
    if !ready.0 {
        return;
    }
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
                    let pos = terrain.stand_pos(x, z);
                    let yaw = rng.random_range(0.0..std::f32::consts::TAU);
                    let scale = assets.fit_scale * rng.random_range(0.8..1.2);
                    parent.spawn((
                        Mesh3d(assets.mesh.clone()),
                        MeshMaterial3d(assets.material.clone()),
                        Transform::from_translation(pos)
                            .with_rotation(Quat::from_rotation_y(yaw))
                            .with_scale(Vec3::splat(scale)),
                        // Grass shadow-casting is the dominant cost (a second
                        // high-poly pass). Skip it — huge perf win, barely visible.
                        NotShadowCaster,
                        GrassInstance,
                    ));
                    placed += 1;
                }
            }
        })
        .id()
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
