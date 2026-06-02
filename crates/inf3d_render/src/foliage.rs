//! Voxel foliage scatter: trees, grass clumps, and rocks loaded from the
//! MagicaVoxel `.vox` files under `assets/foliage/vox/{Trees,Rocks,Grass}/`.
//!
//! We parse each `.vox` file with [`dot_vox`] directly and build a Bevy
//! [`Mesh`] via a small cull-face mesher: every solid voxel emits face quads
//! only where its neighbor is air. Per-face vertex colors come from the
//! MagicaVoxel palette, so a single shared white [`StandardMaterial`] +
//! vertex colors gives the full per-voxel look (and Bevy auto-batches every
//! instance of a variant into one draw call).
//!
//! This route replaced an earlier `bevy_vox_scene` attempt: that crate
//! panics on `.vox` files without `MATL` material chunks (our files don't
//! have them — they're palette-only exports), so we cut out the middleman
//! and use `dot_vox` ourselves.
//!
//! Each variant is loaded once at startup. Spawned props are scattered around
//! the player on a tile-ring (see [`stream_foliage`]) and parented to a
//! per-tile entity so leaving the ring cascades the despawn. The ring scales
//! with the camera's orthographic viewport so that zooming out shows props
//! all the way to the screen edges (clamped per quality preset).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::camera::{Projection, ScalingMode};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use inf3d_camera::IsoCamera;
use inf3d_core::{FollowTarget, QualitySettings, Rock, Tree};
use inf3d_worldgen::Terrain;

/// Side length of one streaming tile, in voxel columns.
const TILE: i32 = 16;
/// Minimum ring radius the streamer ever uses, regardless of zoom level.
const RING_MIN: i32 = 2;
/// Fallback ring radius used when the camera entity hasn't spawned yet (or
/// isn't orthographic).
const RING_FALLBACK: i32 = 3;
/// Multiplier from the camera's orthographic `viewport_height` to the
/// world-XZ radius the foliage ring needs to cover.
const RING_ZOOM_COVERAGE: f32 = 0.9;

// Per-column probability of spawning each foliage category.
const TREE_DENSITY: f32 = 0.004;
const GRASS_DENSITY: f32 = 0.018;
const ROCK_DENSITY: f32 = 0.002;

// World-space target heights per category. Each variant is uniform-scaled so
// its tallest dimension hits this value, keeping props reasonably sized
// regardless of the source `.vox` model's voxel count.
const TREE_TARGET_HEIGHT: f32 = 5.0;
const GRASS_TARGET_HEIGHT: f32 = 0.8;
const ROCK_TARGET_HEIGHT: f32 = 1.8;

// Asset directories (paths relative to the app crate's `assets/`).
const TREES_DIR: &str = "foliage/vox/Trees";
const ROCKS_DIR: &str = "foliage/vox/Rocks";
const GRASS_DIR: &str = "foliage/vox/Grass";

/// Filesystem root the dev-mode `AssetPlugin` reads from. Bevy resolves
/// `<CARGO_MANIFEST_DIR>/assets/foo` in dev. The render crate's manifest dir
/// is `crates/inf3d_render/`, so we hop up one level and back into `inf3d_app`.
const APP_ASSETS_ROOT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../inf3d_app/assets");

/// Loaded foliage meshes, grouped by category. The vector index is the
/// "variant" the per-tile RNG picks.
#[derive(Resource)]
struct FoliageAssets {
    trees: Vec<Handle<Mesh>>,
    rocks: Vec<Handle<Mesh>>,
    grass: Vec<Handle<Mesh>>,
    /// One shared material — vertex colors carry the per-voxel palette, so a
    /// white base lets every instance share the same draw call.
    material: Handle<StandardMaterial>,
}

#[derive(Component)]
struct FoliageTile;

#[derive(Resource, Default)]
struct FoliageField {
    tiles: HashMap<IVec2, Entity>,
}

pub struct FoliagePlugin;

impl Plugin for FoliagePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<QualitySettings>()
            .init_resource::<FoliageField>()
            .add_systems(Startup, setup_foliage)
            .add_systems(Update, stream_foliage);
    }
}

fn setup_foliage(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let trees = load_category(TREES_DIR, TREE_TARGET_HEIGHT, &mut meshes);
    let rocks = load_category(ROCKS_DIR, ROCK_TARGET_HEIGHT, &mut meshes);
    let grass = load_category(GRASS_DIR, GRASS_TARGET_HEIGHT, &mut meshes);

    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.95,
        metallic: 0.0,
        reflectance: 0.0,
        ..default()
    });

    info!(
        "foliage: loaded {} trees, {} rocks, {} grass",
        trees.len(),
        rocks.len(),
        grass.len()
    );

    commands.insert_resource(FoliageAssets {
        trees,
        rocks,
        grass,
        material,
    });
}

/// Enumerate `.vox` files under `<APP_ASSETS_ROOT>/<rel_dir>`, parse each
/// with `dot_vox`, build a cull-face mesh per file, and return mesh handles.
fn load_category(
    rel_dir: &str,
    target_height: f32,
    meshes: &mut Assets<Mesh>,
) -> Vec<Handle<Mesh>> {
    let abs_dir = format!("{}/{}", APP_ASSETS_ROOT, rel_dir);
    let entries = match fs::read_dir(Path::new(&abs_dir)) {
        Ok(e) => e,
        Err(err) => {
            warn!("foliage: could not read {}: {}", abs_dir, err);
            return Vec::new();
        }
    };

    let mut handles = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("vox") {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) => {
                warn!("foliage: failed to read {}: {}", path.display(), err);
                continue;
            }
        };
        let data = match dot_vox::load_bytes(&bytes) {
            Ok(d) => d,
            Err(err) => {
                warn!("foliage: failed to parse {}: {}", path.display(), err);
                continue;
            }
        };
        let Some(mesh) = build_voxel_mesh(&data, target_height) else {
            warn!(
                "foliage: empty/unsupported model in {}, skipping",
                path.display()
            );
            continue;
        };
        handles.push(meshes.add(mesh));
    }
    handles
}

/// Build a Bevy [`Mesh`] from the first non-empty model in a [`DotVoxData`].
///
/// Algorithm: scan every voxel, emit face quads for each of the 6 faces whose
/// neighbor in that direction is empty. Per-vertex color is the voxel's
/// palette color converted from sRGB→linear (Bevy's pipeline does the
/// reverse on output, so this round-trips correctly).
///
/// MagicaVoxel uses **Z-up, right-handed**. Bevy uses **Y-up, right-handed**.
/// We apply `(x, y, z) -> (x, z, -y)` per vertex so the model's natural up
/// axis lines up with the world's. The mesh is then translated so the
/// bottom-center sits at `(0, 0, 0)` and uniform-scaled so the **tallest
/// dimension** equals `target_height`.
fn build_voxel_mesh(data: &dot_vox::DotVoxData, target_height: f32) -> Option<Mesh> {
    let model = data.models.iter().find(|m| !m.voxels.is_empty())?;
    let sx = model.size.x as usize;
    let sy = model.size.y as usize;
    let sz = model.size.z as usize;
    if sx == 0 || sy == 0 || sz == 0 {
        return None;
    }

    // 3D grid of palette indices; 0 means "air".
    // `voxel.i = 0` is treated as solid (palette[0] is a valid color); we use
    // an `Option<u8>` so we can distinguish "no voxel" from "voxel of palette
    // index 0".
    let mut grid: Vec<Option<u8>> = vec![None; sx * sy * sz];
    let idx = |x: usize, y: usize, z: usize| (z * sy + y) * sx + x;
    for v in &model.voxels {
        let (x, y, z) = (v.x as usize, v.y as usize, v.z as usize);
        if x < sx && y < sy && z < sz {
            grid[idx(x, y, z)] = Some(v.i);
        }
    }

    let solid = |x: i32, y: i32, z: i32| -> bool {
        if x < 0 || y < 0 || z < 0 {
            return false;
        }
        let (x, y, z) = (x as usize, y as usize, z as usize);
        if x >= sx || y >= sy || z >= sz {
            return false;
        }
        grid[idx(x, y, z)].is_some()
    };

    let palette = &data.palette;
    let color_of = |pal_idx: u8| -> [f32; 4] {
        let c = palette.get(pal_idx as usize).copied().unwrap_or(dot_vox::Color {
            r: 255,
            g: 0,
            b: 255,
            a: 255,
        });
        let lin = bevy::color::LinearRgba::from(Color::srgba_u8(c.r, c.g, c.b, c.a));
        [lin.red, lin.green, lin.blue, lin.alpha]
    };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Emit a face quad in MagicaVoxel-space (Z-up) coordinates. The 4 corners
    // are written CCW when viewed from outside the voxel so culling works.
    let mut emit_face = |corners: [[f32; 3]; 4],
                         normal: [f32; 3],
                         color: [f32; 4],
                         positions: &mut Vec<[f32; 3]>,
                         normals: &mut Vec<[f32; 3]>,
                         colors: &mut Vec<[f32; 4]>,
                         indices: &mut Vec<u32>| {
        let base = positions.len() as u32;
        for c in &corners {
            positions.push(*c);
            normals.push(normal);
            colors.push(color);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    for z in 0..sz {
        for y in 0..sy {
            for x in 0..sx {
                let Some(pal) = grid[idx(x, y, z)] else {
                    continue;
                };
                let color = color_of(pal);
                let fx = x as f32;
                let fy = y as f32;
                let fz = z as f32;

                // +X face: neighbor at x+1 is air → render
                if !solid(x as i32 + 1, y as i32, z as i32) {
                    emit_face(
                        [
                            [fx + 1.0, fy, fz],
                            [fx + 1.0, fy + 1.0, fz],
                            [fx + 1.0, fy + 1.0, fz + 1.0],
                            [fx + 1.0, fy, fz + 1.0],
                        ],
                        [1.0, 0.0, 0.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // -X face
                if !solid(x as i32 - 1, y as i32, z as i32) {
                    emit_face(
                        [
                            [fx, fy + 1.0, fz],
                            [fx, fy, fz],
                            [fx, fy, fz + 1.0],
                            [fx, fy + 1.0, fz + 1.0],
                        ],
                        [-1.0, 0.0, 0.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // +Y face
                if !solid(x as i32, y as i32 + 1, z as i32) {
                    emit_face(
                        [
                            [fx + 1.0, fy + 1.0, fz],
                            [fx, fy + 1.0, fz],
                            [fx, fy + 1.0, fz + 1.0],
                            [fx + 1.0, fy + 1.0, fz + 1.0],
                        ],
                        [0.0, 1.0, 0.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // -Y face
                if !solid(x as i32, y as i32 - 1, z as i32) {
                    emit_face(
                        [
                            [fx, fy, fz],
                            [fx + 1.0, fy, fz],
                            [fx + 1.0, fy, fz + 1.0],
                            [fx, fy, fz + 1.0],
                        ],
                        [0.0, -1.0, 0.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // +Z face (Z-up "top")
                if !solid(x as i32, y as i32, z as i32 + 1) {
                    emit_face(
                        [
                            [fx, fy, fz + 1.0],
                            [fx + 1.0, fy, fz + 1.0],
                            [fx + 1.0, fy + 1.0, fz + 1.0],
                            [fx, fy + 1.0, fz + 1.0],
                        ],
                        [0.0, 0.0, 1.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
                // -Z face (Z-up "bottom")
                if !solid(x as i32, y as i32, z as i32 - 1) {
                    emit_face(
                        [
                            [fx, fy + 1.0, fz],
                            [fx + 1.0, fy + 1.0, fz],
                            [fx + 1.0, fy, fz],
                            [fx, fy, fz],
                        ],
                        [0.0, 0.0, -1.0],
                        color,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
            }
        }
    }

    if positions.is_empty() {
        return None;
    }

    // MagicaVoxel Z-up → Bevy Y-up: (x, y, z) → (x, z, -y).
    for p in &mut positions {
        let (px, py, pz) = (p[0], p[1], p[2]);
        *p = [px, pz, -py];
    }
    for n in &mut normals {
        let (nx, ny, nz) = (n[0], n[1], n[2]);
        *n = [nx, nz, -ny];
    }

    // Bbox in Bevy coords for normalization.
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for p in &positions {
        let v = Vec3::from_array(*p);
        min = min.min(v);
        max = max.max(v);
    }
    let extent = max - min;
    let tallest = extent.x.max(extent.y).max(extent.z).max(1e-6);
    let scale = target_height / tallest;
    let pivot = Vec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5);
    for p in &mut positions {
        let v = (Vec3::from_array(*p) - pivot) * scale;
        *p = [v.x, v.y, v.z];
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

/// Stream foliage tiles in/out as the player moves. Out-of-ring tiles despawn
/// recursively (which cascades to every prop spawned under them).
fn stream_foliage(
    mut commands: Commands,
    assets: Option<Res<FoliageAssets>>,
    terrain: Res<Terrain>,
    mut field: ResMut<FoliageField>,
    settings: Res<QualitySettings>,
    player_q: Query<&Transform, With<FollowTarget>>,
    camera_q: Query<&Projection, With<IsoCamera>>,
) {
    let Some(assets) = assets else {
        return;
    };
    if !settings.foliage_enabled {
        if !field.tiles.is_empty() {
            for (_, entity) in field.tiles.drain() {
                commands.entity(entity).despawn();
            }
        }
        return;
    }
    let Ok(player) = player_q.single() else {
        return;
    };
    let center = IVec2::new(
        (player.translation.x / TILE as f32).floor() as i32,
        (player.translation.z / TILE as f32).floor() as i32,
    );

    let ring = compute_ring(camera_q.single().ok(), settings.foliage_ring_max);

    for dx in -ring..=ring {
        for dz in -ring..=ring {
            let tile = center + IVec2::new(dx, dz);
            if field.tiles.contains_key(&tile) {
                continue;
            }
            let entity = spawn_tile(&mut commands, &assets, &terrain, tile);
            field.tiles.insert(tile, entity);
        }
    }

    field.tiles.retain(|tile, entity| {
        let in_ring = (tile.x - center.x).abs() <= ring && (tile.y - center.y).abs() <= ring;
        if !in_ring {
            commands.entity(*entity).despawn();
        }
        in_ring
    });
}

fn compute_ring(projection: Option<&Projection>, quality_ring_max: i32) -> i32 {
    let max = quality_ring_max.max(RING_MIN);
    let raw = match projection {
        Some(Projection::Orthographic(ortho)) => match ortho.scaling_mode {
            ScalingMode::FixedVertical { viewport_height } => {
                let blocks = viewport_height * RING_ZOOM_COVERAGE;
                let tiles = (blocks / TILE as f32).ceil();
                if tiles.is_finite() {
                    tiles as i32
                } else {
                    RING_FALLBACK
                }
            }
            _ => RING_FALLBACK,
        },
        _ => RING_FALLBACK,
    };
    raw.clamp(RING_MIN, max)
}

fn spawn_tile(
    commands: &mut Commands,
    assets: &FoliageAssets,
    terrain: &Terrain,
    tile: IVec2,
) -> Entity {
    let seed = (tile.x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (tile.y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    let mut rng = StdRng::seed_from_u64(seed);

    let base_x = tile.x * TILE;
    let base_z = tile.y * TILE;

    let parent = commands
        .spawn((
            Transform::default(),
            Visibility::default(),
            Name::new(format!("FoliageTile {},{}", tile.x, tile.y)),
            FoliageTile,
        ))
        .id();

    for lx in 0..TILE {
        for lz in 0..TILE {
            let x = base_x + lx;
            let z = base_z + lz;
            if !terrain.is_land(x, z) {
                continue;
            }
            let pos = terrain.stand_pos(x, z);
            let yaw = snap_yaw(&mut rng);

            if !assets.trees.is_empty() && rng.random::<f32>() < TREE_DENSITY {
                let variant = rng.random_range(0..assets.trees.len());
                spawn_prop(
                    commands,
                    parent,
                    assets.trees[variant].clone(),
                    assets.material.clone(),
                    pos,
                    yaw,
                    Some(PropKind::Tree),
                );
                continue;
            }
            if !assets.rocks.is_empty() && rng.random::<f32>() < ROCK_DENSITY {
                let variant = rng.random_range(0..assets.rocks.len());
                spawn_prop(
                    commands,
                    parent,
                    assets.rocks[variant].clone(),
                    assets.material.clone(),
                    pos,
                    yaw,
                    Some(PropKind::Rock),
                );
                continue;
            }
            if !assets.grass.is_empty() && rng.random::<f32>() < GRASS_DENSITY {
                let variant = rng.random_range(0..assets.grass.len());
                spawn_prop(
                    commands,
                    parent,
                    assets.grass[variant].clone(),
                    assets.material.clone(),
                    pos,
                    yaw,
                    None,
                );
            }
        }
    }

    parent
}

#[derive(Clone, Copy)]
enum PropKind {
    Tree,
    Rock,
}

fn spawn_prop(
    commands: &mut Commands,
    parent: Entity,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    pos: Vec3,
    yaw: f32,
    kind: Option<PropKind>,
) {
    let mut entity = commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
        Visibility::default(),
        ChildOf(parent),
    ));
    match kind {
        Some(PropKind::Tree) => {
            entity.insert(Tree);
        }
        Some(PropKind::Rock) => {
            entity.insert(Rock);
        }
        None => {}
    }
}

fn snap_yaw(rng: &mut StdRng) -> f32 {
    let q: u32 = rng.random_range(0..4);
    q as f32 * std::f32::consts::FRAC_PI_2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ortho_proj(vh: f32) -> Projection {
        Projection::Orthographic(bevy::camera::OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: vh,
            },
            ..bevy::camera::OrthographicProjection::default_3d()
        })
    }

    #[test]
    fn compute_ring_falls_back_when_no_camera() {
        assert_eq!(compute_ring(None, 8), RING_FALLBACK);
    }

    #[test]
    fn compute_ring_respects_minimum() {
        let proj = ortho_proj(4.0);
        assert_eq!(compute_ring(Some(&proj), 8), RING_MIN);
    }

    #[test]
    fn compute_ring_respects_quality_cap() {
        let proj = ortho_proj(400.0);
        assert_eq!(compute_ring(Some(&proj), 4), 4);
    }

    #[test]
    fn compute_ring_scales_with_zoom() {
        let proj = ortho_proj(90.0);
        assert_eq!(compute_ring(Some(&proj), 8), 6);
        let proj = ortho_proj(44.0);
        assert_eq!(compute_ring(Some(&proj), 8), 3);
    }

    #[test]
    fn snap_yaw_returns_cardinal_only() {
        let mut rng = StdRng::seed_from_u64(0xCAFEBABE);
        let valid = [
            0.0,
            std::f32::consts::FRAC_PI_2,
            std::f32::consts::PI,
            std::f32::consts::FRAC_PI_2 * 3.0,
        ];
        for _ in 0..256 {
            let y = snap_yaw(&mut rng);
            assert!(
                valid.iter().any(|v| (y - v).abs() < 1e-5),
                "non-cardinal yaw {y}"
            );
        }
    }

    #[test]
    fn build_voxel_mesh_handles_single_voxel() {
        let data = dot_vox::DotVoxData {
            version: 150,
            models: vec![dot_vox::Model {
                size: dot_vox::Size { x: 1, y: 1, z: 1 },
                voxels: vec![dot_vox::Voxel {
                    x: 0,
                    y: 0,
                    z: 0,
                    i: 0,
                }],
            }],
            palette: vec![dot_vox::Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }],
            materials: vec![],
            scenes: vec![],
            layers: vec![],
        };
        let mesh = build_voxel_mesh(&data, 1.0).expect("mesh");
        // 6 visible faces × 4 verts = 24, × 6 indices/face = 36.
        let pos = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| match a {
                bevy::mesh::VertexAttributeValues::Float32x3(v) => Some(v),
                _ => None,
            })
            .expect("positions");
        assert_eq!(pos.len(), 24);
        match mesh.indices().expect("indices") {
            Indices::U32(v) => assert_eq!(v.len(), 36),
            Indices::U16(v) => assert_eq!(v.len(), 36),
        }
    }

    #[test]
    fn build_voxel_mesh_culls_interior_faces() {
        // 2×1×1 of solid voxels — interior +X/-X faces between them are culled.
        let data = dot_vox::DotVoxData {
            version: 150,
            models: vec![dot_vox::Model {
                size: dot_vox::Size { x: 2, y: 1, z: 1 },
                voxels: vec![
                    dot_vox::Voxel { x: 0, y: 0, z: 0, i: 0 },
                    dot_vox::Voxel { x: 1, y: 0, z: 0, i: 0 },
                ],
            }],
            palette: vec![dot_vox::Color { r: 0, g: 255, b: 0, a: 255 }],
            materials: vec![],
            scenes: vec![],
            layers: vec![],
        };
        let mesh = build_voxel_mesh(&data, 1.0).expect("mesh");
        let pos = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| match a {
                bevy::mesh::VertexAttributeValues::Float32x3(v) => Some(v),
                _ => None,
            })
            .expect("positions");
        // Each voxel has 6 faces, but 1 face per voxel is shared interior →
        // culled. So 2 × 5 = 10 faces × 4 verts = 40.
        assert_eq!(pos.len(), 40);
    }
}
