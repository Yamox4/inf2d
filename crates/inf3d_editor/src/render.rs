//! Build the visible scene from [`EditorState`]: the voxel mesh, the solid
//! reference platform, the sub-voxel / per-block grids, the reference-block
//! outlines, and the part-pivot gizmos.
//!
//! The voxel mesh is a cull-face mesher — for every solid sub-voxel it emits a
//! quad on each face whose neighbor is empty, with per-vertex color from the
//! palette (sRGB→linear, matching the game's `vox_mesh` so the look is
//! consistent). The mesh is rebuilt only when [`EditorState::dirty`] is set, so
//! painting a single voxel doesn't re-tessellate every frame for free.
//!
//! The **reference platform** is a real, solid slab mesh at the base of the
//! build volume (`y = 0`), sized to the current block extent and rebuilt only
//! when that extent changes ([`update_platform`]). It is the unmistakable
//! "ground you build on": the bright per-block grid, the fine sub-voxel grid,
//! and a cyan footprint border are drawn on its top face so the user always sees
//! exactly which cell the first layer will land in. The grids, outlines, and
//! pivots are immediate-mode [`Gizmos`] redrawn each frame (cheap line work).

use bevy::asset::RenderAssetUsages;
use bevy::color::palettes::css;
// `Alpha` brings `.with_alpha` into scope for the css `Srgba` constants.
use bevy::color::Alpha;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::state::EditorState;
use crate::volume::{Cell, VoxelModel};

/// Marker for the single entity that holds the rebuilt voxel mesh.
#[derive(Component)]
struct VoxelMeshTag;

/// Marker for the single solid reference-platform slab entity (the visible
/// "ground" the user builds on). Its mesh + transform are rebuilt to match the
/// current block extent / sub-voxel resolution.
#[derive(Component)]
struct PlatformTag;

/// Thickness of the reference-platform slab, in world units. Thin enough to read
/// as a floor surface, thick enough to be unmistakably solid. The slab sits just
/// *below* `y = 0` so the first voxel layer (cells with `y = 0`, occupying world
/// `[0, sub_voxel_size]`) rests directly on top of it.
const PLATFORM_THICKNESS: f32 = 0.06;

/// A shared white material; per-vertex colors carry the palette, so one material
/// renders the whole model and Bevy can batch it (same trick the game uses).
#[derive(Resource)]
struct VoxelMaterial(Handle<StandardMaterial>);

/// Caches the platform's mesh handle + the block extent it was last built for, so
/// the slab is only re-tessellated when the block count actually changes (its
/// size depends only on `blocks`; the sub-voxel grid is drawn as gizmos).
#[derive(Resource)]
struct PlatformCache {
    /// The slab mesh handle (its data is swapped in place on a rebuild).
    mesh: Handle<Mesh>,
    /// The `blocks` extent the current slab was built for; `None` until the first
    /// build so the slab is created on the first frame.
    built_for: Option<u32>,
}

/// Plugin: spawns the mesh holder + material + platform and runs the rebuild +
/// overlays.
pub struct EditorRenderPlugin;

impl Plugin for EditorRenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(
            Update,
            (rebuild_mesh, update_platform, draw_overlays),
        );
    }
}

/// Create the shared material, the (initially empty) mesh entity, and the
/// reference-platform slab.
fn setup(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.9,
        // Per-vertex COLOR drives the look; the base color just modulates it.
        ..default()
    });
    commands.insert_resource(VoxelMaterial(material.clone()));

    commands.spawn((
        Mesh3d(meshes.add(empty_mesh())),
        MeshMaterial3d(material),
        Transform::default(),
        VoxelMeshTag,
    ));

    // The reference platform: a solid, clearly-lit slab the user builds on. A
    // distinct matte material (not the per-vertex voxel one) so it reads as the
    // ground rather than as geometry. Its mesh + transform are sized to the build
    // volume by `update_platform` (which runs on the first frame).
    let platform_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.32, 0.34, 0.40),
        perceptual_roughness: 1.0,
        metallic: 0.0,
        ..default()
    });
    let platform_mesh = meshes.add(Mesh::from(Cuboid::new(1.0, PLATFORM_THICKNESS, 1.0)));
    commands.insert_resource(PlatformCache {
        mesh: platform_mesh.clone(),
        built_for: None,
    });
    commands.spawn((
        Mesh3d(platform_mesh),
        MeshMaterial3d(platform_mat),
        Transform::default(),
        PlatformTag,
    ));
}

/// Resize + reposition the reference platform to span the current build-volume
/// footprint, rebuilding its mesh only when the block count changed. The slab
/// spans `[0, blocks]` in X/Z and sits just below `y = 0` so the first layer
/// lands on top of it.
fn update_platform(
    state: Res<EditorState>,
    mut cache: ResMut<PlatformCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut platform: Query<&mut Transform, With<PlatformTag>>,
) {
    let blocks = state.model.blocks();
    if cache.built_for == Some(blocks) {
        return;
    }

    let span = blocks as f32;
    if let Some(mesh) = meshes.get_mut(&cache.mesh) {
        *mesh = Mesh::from(Cuboid::new(span, PLATFORM_THICKNESS, span));
    }
    // Center the slab over the footprint, its TOP face flush with `y = 0`.
    if let Ok(mut transform) = platform.single_mut() {
        transform.translation = Vec3::new(span * 0.5, -PLATFORM_THICKNESS * 0.5, span * 0.5);
    }
    cache.built_for = Some(blocks);
}

/// Rebuild the voxel mesh when the model changed. Reuses the existing mesh
/// asset handle (swaps its data) so we don't leak a new asset each edit.
fn rebuild_mesh(
    mut state: ResMut<EditorState>,
    mesh_q: Query<&Mesh3d, With<VoxelMeshTag>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !state.dirty {
        return;
    }
    let Ok(mesh3d) = mesh_q.single() else {
        return;
    };
    let Some(mesh) = meshes.get_mut(&mesh3d.0) else {
        return;
    };
    *mesh = build_voxel_mesh(&state.model, &state.palette);
    state.dirty = false;
}

/// An empty triangle-list mesh with all the attributes the mesher fills, so the
/// asset's vertex layout is stable from spawn.
fn empty_mesh() -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, Vec::<[f32; 4]>::new());
    mesh.insert_indices(Indices::U32(Vec::new()));
    mesh
}

/// One cube face: neighbor offset to test for emptiness, outward normal, and the
/// four CCW corners (so back-face culling keeps the outward face).
struct Face {
    neighbor: [i32; 3],
    normal: [f32; 3],
    corners: [[f32; 3]; 4],
}

/// The six axis-aligned faces, in Bevy's Y-up frame (the editor's native space —
/// no axis swap here; the swap happens only on `.vox` export).
#[rustfmt::skip]
const FACES: [Face; 6] = [
    Face { neighbor: [1, 0, 0], normal: [1.0, 0.0, 0.0],
        corners: [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [1.0, 0.0, 1.0]] },
    Face { neighbor: [-1, 0, 0], normal: [-1.0, 0.0, 0.0],
        corners: [[0.0, 1.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0]] },
    Face { neighbor: [0, 1, 0], normal: [0.0, 1.0, 0.0],
        corners: [[1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 1.0], [1.0, 1.0, 1.0]] },
    Face { neighbor: [0, -1, 0], normal: [0.0, -1.0, 0.0],
        corners: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]] },
    Face { neighbor: [0, 0, 1], normal: [0.0, 0.0, 1.0],
        corners: [[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0]] },
    Face { neighbor: [0, 0, -1], normal: [0.0, 0.0, -1.0],
        corners: [[0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0]] },
];

/// Cull-face mesh of every solid sub-voxel, each scaled to the model's sub-voxel
/// size and placed at its cell. Per-vertex color is the palette color
/// (sRGB→linear).
fn build_voxel_mesh(model: &VoxelModel, palette: &crate::palette::Palette) -> Mesh {
    let s = model.sub_voxel_size();
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for (cell, voxel) in model.iter() {
        let rgb = palette.rgb(voxel.color);
        let lin = LinearRgba::from(Color::srgb_u8(rgb[0], rgb[1], rgb[2]));
        let color = [lin.red, lin.green, lin.blue, 1.0];
        let base_pos = Vec3::new(cell.x as f32, cell.y as f32, cell.z as f32) * s;

        for face in &FACES {
            let n = Cell::new(
                cell.x + face.neighbor[0],
                cell.y + face.neighbor[1],
                cell.z + face.neighbor[2],
            );
            if model.is_solid(n) {
                continue; // interior face → culled
            }
            let base = positions.len() as u32;
            for corner in &face.corners {
                let p = base_pos + Vec3::from_array(*corner) * s;
                positions.push([p.x, p.y, p.z]);
                normals.push(face.normal);
                colors.push(color);
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Draw the platform grids, reference-block outlines, and pivot gizmos each frame
/// from the current [`EditorState`] toggles. The solid slab is a real mesh
/// ([`update_platform`]); these lines sit on its top face (`y = 0`) to show the
/// exact per-block and sub-voxel divisions where placement will land.
fn draw_overlays(mut gizmos: Gizmos, state: Res<EditorState>) {
    let blocks_i = state.model.blocks() as i32;
    let span = state.model.blocks() as f32;

    // ── Sub-voxel grid on the platform surface ────────────────────────────
    // A fine lattice on `y = 0` at sub-voxel spacing, so the user sees exactly
    // which cell the first layer drops into. Drawn just above the slab top so it
    // reads on the surface; gated by the Grid toggle.
    if state.show_grid {
        let res = state.model.resolution() as i32 * blocks_i;
        let s = state.model.sub_voxel_size();
        // Lift a hair above the slab top so the lines aren't z-fought by it.
        let y = 0.001;
        let fine: Color = css::LIGHT_GRAY.with_alpha(0.35).into();
        for i in 0..=res {
            // Skip the per-block lines here — they're drawn bolder below.
            if i % state.model.resolution() as i32 == 0 {
                continue;
            }
            let p = i as f32 * s;
            gizmos.line(Vec3::new(p, y, 0.0), Vec3::new(p, y, span), fine);
            gizmos.line(Vec3::new(0.0, y, p), Vec3::new(span, y, p), fine);
        }
    }

    // ── Bold per-block (1×1×1) grid on the platform surface ───────────────
    // The reference-block divisions, always drawn (independent of the fine grid)
    // so the user always sees the in-game-voxel cells the model spans, and a
    // bright border framing the whole buildable footprint.
    let y = 0.002;
    let block_line: Color = css::WHITE.with_alpha(0.85).into();
    for i in 0..=blocks_i {
        let p = i as f32;
        gizmos.line(Vec3::new(p, y, 0.0), Vec3::new(p, y, span), block_line);
        gizmos.line(Vec3::new(0.0, y, p), Vec3::new(span, y, p), block_line);
    }
    // A bright cyan border so the platform edge is unmistakable.
    let border: Color = css::AQUA.into();
    let border_y = 0.003;
    gizmos.line(Vec3::new(0.0, border_y, 0.0), Vec3::new(span, border_y, 0.0), border);
    gizmos.line(Vec3::new(span, border_y, 0.0), Vec3::new(span, border_y, span), border);
    gizmos.line(Vec3::new(span, border_y, span), Vec3::new(0.0, border_y, span), border);
    gizmos.line(Vec3::new(0.0, border_y, span), Vec3::new(0.0, border_y, 0.0), border);

    // ── Reference-block outlines (the build volume's 3D extent) ───────────
    // One box per reference block so the user sees how the model spans 1..N
    // in-game voxels in height/depth too. The anchor block is highlighted.
    if state.show_reference {
        for bx in 0..blocks_i {
            for by in 0..blocks_i {
                for bz in 0..blocks_i {
                    let min = Vec3::new(bx as f32, by as f32, bz as f32);
                    let is_anchor = bx == 0 && by == 0 && bz == 0;
                    let color = if is_anchor {
                        css::ORANGE
                    } else {
                        css::DIM_GRAY
                    };
                    draw_box(&mut gizmos, min, min + Vec3::ONE, color.into());
                }
            }
        }
    }

    // ── Part pivots (rig preview) ─────────────────────────────────────────
    if state.show_pivots {
        for part in state.tree.iter() {
            let is_active = part.id == state.active_part;
            let color: Color = if is_active { css::AQUA } else { css::YELLOW }.into();
            let r = 0.05 * span.max(1.0);
            gizmos.sphere(Isometry3d::from_translation(part.pivot), r, color);
            // A short bone line to the parent pivot to show the hierarchy.
            if let Some(parent) = part.parent.and_then(|pid| state.tree.get(pid)) {
                let bone: Color = css::WHITE.with_alpha(0.5).into();
                gizmos.line(part.pivot, parent.pivot, bone);
            }
        }
    }
}

/// Draw the 12 edges of an axis-aligned box from `min` to `max`.
fn draw_box(gizmos: &mut Gizmos, min: Vec3, max: Vec3, color: Color) {
    let c = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(max.x, max.y, max.z),
        Vec3::new(min.x, max.y, max.z),
    ];
    // bottom loop, top loop, verticals
    let edges = [
        (0, 1), (1, 2), (2, 3), (3, 0),
        (4, 5), (5, 6), (6, 7), (7, 4),
        (0, 4), (1, 5), (2, 6), (3, 7),
    ];
    for (a, b) in edges {
        gizmos.line(c[a], c[b], color);
    }
}
