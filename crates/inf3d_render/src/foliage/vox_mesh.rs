//! Load `.vox` files and build cull-face Bevy meshes at startup.
//!
//! [`load_category`] enumerates the `.vox` files in one asset directory;
//! [`build_voxel_mesh`] turns a parsed model into a [`Mesh`] by emitting a face
//! quad for every solid voxel face whose neighbor is air. Per-vertex colors come
//! from the MagicaVoxel palette (sRGB→linear), so a single shared white material
//! reproduces the full per-voxel look and lets Bevy batch instances.

use std::fs;
use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::{FoliageVariant, ROCKS_DIR};

/// Filesystem root the dev-mode `AssetPlugin` reads from. Bevy resolves
/// `<CARGO_MANIFEST_DIR>/assets/foo` in dev. The render crate's manifest dir
/// is `crates/inf3d_render/`, so we hop up one level and back into `inf3d_app`.
const APP_ASSETS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../inf3d_app/assets");

/// One cube face for the cull-mesher: the offset to the neighbor we test for
/// air, the outward normal, and the four corner offsets (relative to the
/// voxel's `(x, y, z)` min corner) written CCW when viewed from outside so
/// back-face culling keeps the face. Coordinates are MagicaVoxel-space (Z-up);
/// the whole mesh is rotated into Bevy's Y-up frame after assembly.
struct Face {
    neighbor: [i32; 3],
    normal: [f32; 3],
    corners: [[f32; 3]; 4],
}

/// The six axis-aligned cube faces. Replaces six near-identical hand-written
/// emission blocks; the mesher loops this table per voxel.
#[rustfmt::skip]
const FACES: [Face; 6] = [
    // +X
    Face { neighbor: [1, 0, 0], normal: [1.0, 0.0, 0.0],
        corners: [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [1.0, 0.0, 1.0]] },
    // -X
    Face { neighbor: [-1, 0, 0], normal: [-1.0, 0.0, 0.0],
        corners: [[0.0, 1.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0]] },
    // +Y
    Face { neighbor: [0, 1, 0], normal: [0.0, 1.0, 0.0],
        corners: [[1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 1.0], [1.0, 1.0, 1.0]] },
    // -Y
    Face { neighbor: [0, -1, 0], normal: [0.0, -1.0, 0.0],
        corners: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]] },
    // +Z (Z-up "top")
    Face { neighbor: [0, 0, 1], normal: [0.0, 0.0, 1.0],
        corners: [[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0]] },
    // -Z (Z-up "bottom")
    Face { neighbor: [0, 0, -1], normal: [0.0, 0.0, -1.0],
        corners: [[0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0]] },
];

/// Subtle deterministic per-voxel brightness jitter so foliage surfaces read as
/// TEXTURED (matching the terrain's per-texel detail) instead of flat palette
/// blocks. Multiplies the (linear) color by ~0.90..1.10 from a hash of the voxel
/// position; alpha is untouched. Stable per model — same voxel → same shade.
fn jitter_color(c: [f32; 4], x: usize, y: usize, z: usize) -> [f32; 4] {
    let mut h = (x as u32).wrapping_mul(0x27d4_eb2d)
        ^ (y as u32).wrapping_mul(0x1656_67b1)
        ^ (z as u32).wrapping_mul(0x9e37_79b1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    let n = (h & 0xffff) as f32 / 65535.0; // 0..1
    let f = 0.90 + n * 0.20; // 0.90..1.10
    [c[0] * f, c[1] * f, c[2] * f, c[3]]
}

/// Enumerate `.vox` files under `<APP_ASSETS_ROOT>/<rel_dir>`, parse each
/// with `dot_vox`, build a cull-face mesh per file, and return mesh handles.
pub(super) fn load_category(
    rel_dir: &str,
    target_height: f32,
    meshes: &mut Assets<Mesh>,
) -> Vec<FoliageVariant> {
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
        // Rocks and dead tree stumps must fit inside a SINGLE voxel so their
        // texture never overlaps neighbouring voxels; trees/grass keep the
        // height-normalized scaling.
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let fit_unit = rel_dir == ROCKS_DIR || stem.contains("stump");
        let Some((mesh, size)) = build_voxel_mesh(&data, target_height, fit_unit) else {
            warn!(
                "foliage: empty/unsupported model in {}, skipping",
                path.display()
            );
            continue;
        };
        handles.push(FoliageVariant {
            // Store the file stem (already computed above for the `fit_unit`
            // test) as the variant name, so the per-biome scatter policy can
            // select tree variants by name substring (see `mod::biome_policy`).
            name: stem.to_string(),
            mesh: meshes.add(mesh),
            size,
        });
    }
    handles
}

/// Build a Bevy [`Mesh`] from the first non-empty model in a [`dot_vox::DotVoxData`].
///
/// Algorithm: scan every voxel, emit face quads for each of the 6 faces whose
/// neighbor in that direction is empty (see [`FACES`]). Per-vertex color is the
/// voxel's palette color converted from sRGB→linear (Bevy's pipeline does the
/// reverse on output, so this round-trips correctly).
///
/// MagicaVoxel uses **Z-up, right-handed**. Bevy uses **Y-up, right-handed**.
/// We apply `(x, y, z) -> (x, z, -y)` per vertex so the model's natural up
/// axis lines up with the world's. The mesh is then translated so the
/// **bottom-center** sits at `(0, 0, 0)` and uniform-scaled so its **vertical
/// (Y) extent** equals `target_height` (or, for `fit_unit_voxel`, so the whole
/// model fits a single 1×1×1 voxel).
///
/// Returns the built mesh together with its **post-scale** bounding box (width
/// in X, height in Y, depth in Z), which the collider/footprint logic uses.
fn build_voxel_mesh(
    data: &dot_vox::DotVoxData,
    target_height: f32,
    fit_unit_voxel: bool,
) -> Option<(Mesh, Vec3)> {
    let model = data.models.iter().find(|m| !m.voxels.is_empty())?;
    let sx = model.size.x as usize;
    let sy = model.size.y as usize;
    let sz = model.size.z as usize;
    if sx == 0 || sy == 0 || sz == 0 {
        return None;
    }

    // 3D grid of palette indices; `None` means "air".
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
        let c = palette
            .get(pal_idx as usize)
            .copied()
            .unwrap_or(dot_vox::Color {
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

    for z in 0..sz {
        for y in 0..sy {
            for x in 0..sx {
                let Some(pal) = grid[idx(x, y, z)] else {
                    continue;
                };
                let color = jitter_color(color_of(pal), x, y, z);
                let (fx, fy, fz) = (x as f32, y as f32, z as f32);

                // Emit each face whose neighbor is air. CCW corner order in the
                // table makes the quad front-face outward.
                for face in &FACES {
                    let (nx, ny, nz) = (
                        x as i32 + face.neighbor[0],
                        y as i32 + face.neighbor[1],
                        z as i32 + face.neighbor[2],
                    );
                    if solid(nx, ny, nz) {
                        continue;
                    }
                    let base = positions.len() as u32;
                    for corner in &face.corners {
                        positions.push([fx + corner[0], fy + corner[1], fz + corner[2]]);
                        normals.push(face.normal);
                        colors.push(color);
                    }
                    indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);
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
    let scale = if fit_unit_voxel {
        // Fit the WHOLE model inside a single 1x1x1 voxel (rocks, stumps) so its
        // texture never spills onto neighbouring voxels: scale the largest extent
        // to exactly 1.0, the others fall within. `target_height` is ignored.
        1.0 / extent.max_element().max(1e-6)
    } else {
        // Normalize by the *vertical* (Y) extent so `target_height` is the prop's
        // real height, never its width. Bottom-center pivot → sits on the surface.
        target_height / extent.y.max(1e-6)
    };
    let pivot = Vec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5);
    for p in &mut positions {
        let v = (Vec3::from_array(*p) - pivot) * scale;
        *p = [v.x, v.y, v.z];
    }

    // Post-scale bounding box: width (X), height (Y, == target_height), depth (Z).
    let scaled_size = extent * scale;

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    Some((mesh, scaled_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_voxel_mesh_handles_single_voxel() {
        let data = dot_vox::DotVoxData {
            version: 150,
            index_map: vec![],
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
        let (mesh, size) = build_voxel_mesh(&data, 1.0, false).expect("mesh");
        // A single 1×1×1 voxel scaled so its height (Y) == target 1.0 → unit box.
        assert!((size.y - 1.0).abs() < 1e-5, "height should equal target");
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
            index_map: vec![],
            models: vec![dot_vox::Model {
                size: dot_vox::Size { x: 2, y: 1, z: 1 },
                voxels: vec![
                    dot_vox::Voxel {
                        x: 0,
                        y: 0,
                        z: 0,
                        i: 0,
                    },
                    dot_vox::Voxel {
                        x: 1,
                        y: 0,
                        z: 0,
                        i: 0,
                    },
                ],
            }],
            palette: vec![dot_vox::Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }],
            materials: vec![],
            scenes: vec![],
            layers: vec![],
        };
        let (mesh, size) = build_voxel_mesh(&data, 1.0, false).expect("mesh");
        // 2-wide, 1-tall, 1-deep voxels: scaling normalizes the *height* to 1.0,
        // so the width (X) stays at twice the height (2.0) — the fix's whole
        // point (previously it would have been squashed to make width == 1.0).
        assert!((size.y - 1.0).abs() < 1e-5, "height should equal target");
        assert!(
            (size.x - 2.0).abs() < 1e-5,
            "width preserved relative to height"
        );
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
