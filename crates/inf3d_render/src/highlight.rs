//! Voxel hover highlight: a translucent cube that snaps to the voxel under the
//! cursor each frame, hidden when the cursor isn't over any voxel. Plus a
//! distinct persistent **destination** marker that sits on the clicked
//! click-to-move target cell until the player arrives there — a thick glowing
//! gold tile outline (RuneScape-style), distinct from the yellow hover cube.

use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    window::PrimaryWindow,
};
use bevy_voxel_world::prelude::*;

use inf3d_camera::IsoCamera;
use inf3d_core::{GameSet, PathTarget};
use inf3d_world::MainWorld;
use inf3d_worldgen::Terrain;

/// Slightly larger than a unit voxel so the overlay doesn't z-fight the surface.
const HIGHLIGHT_SCALE: f32 = 1.04;

#[derive(Component)]
struct VoxelHighlight;

/// The persistent destination marker shown on the active click-to-move target
/// cell: a thick glowing gold tile outline (RuneScape-style) that frames the
/// destination column's top face, distinct from the translucent yellow hover
/// [`VoxelHighlight`] cube.
#[derive(Component)]
struct TargetHighlight;

/// Per-marker animation clock, reset to 0 on every fresh click so the outline
/// replays its "stamp-in" pop and then settles into a cozy idle breathe/glow.
#[derive(Component, Default)]
struct TargetAnim {
    elapsed: f32,
}

/// Half-extent of the outline frame (tile is 1×1, so the outer edge sits exactly
/// on the cell border).
const OUTLINE_OUTER: f32 = 0.5;
/// Thickness of the gold border band, in world units. "Thick" on purpose.
const OUTLINE_THICKNESS: f32 = 0.16;

/// Hovered voxel exposed for the HUD: the integer voxel position and its
/// material id (if the hovered voxel is solid).
#[derive(Resource, Default)]
pub struct Hover {
    pub voxel: Option<IVec3>,
    pub material: Option<u8>,
}

pub struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Hover>()
            .add_systems(Startup, (spawn_highlight, spawn_target_highlight))
            .add_systems(
                Update,
                (update_highlight, update_target_highlight).in_set(GameSet::Fx),
            );
    }
}

fn spawn_highlight(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(Cuboid::from_length(HIGHLIGHT_SCALE));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.95, 0.45, 0.30),
        emissive: LinearRgba::rgb(1.4, 1.25, 0.35),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    });

    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
        VoxelHighlight,
    ));
}

/// Raycast the cursor into the voxel world and move/show the highlight on the
/// hovered voxel; hide it when nothing is under the cursor.
fn update_highlight(
    window: Query<&Window, With<PrimaryWindow>>,
    cam: Query<(&Camera, &GlobalTransform), With<IsoCamera>>,
    voxel_world: VoxelWorld<MainWorld>,
    mut highlight: Query<(&mut Transform, &mut Visibility), With<VoxelHighlight>>,
    mut hover: ResMut<Hover>,
) {
    let Ok((mut transform, mut visibility)) = highlight.single_mut() else {
        return;
    };

    let hit = window
        .single()
        .ok()
        .and_then(|w| w.cursor_position())
        .zip(cam.single().ok())
        .and_then(|(cursor, (camera, cam_gtf))| camera.viewport_to_world(cam_gtf, cursor).ok())
        .and_then(|ray| voxel_world.raycast(ray, &|(_p, _v)| true));

    match hit {
        Some(hit) => {
            // `voxel_pos` is the integer voxel corner; center the cube on it.
            transform.translation = hit.voxel_pos().as_vec3() + Vec3::splat(0.5);
            *visibility = Visibility::Visible;
            hover.voxel = Some(hit.voxel_pos());
            hover.material = match hit.voxel {
                WorldVoxel::Solid(m) => Some(m),
                _ => None,
            };
        }
        None => {
            *visibility = Visibility::Hidden;
            hover.voxel = None;
            hover.material = None;
        }
    }
}

/// Spawn the persistent click-to-move destination marker, hidden until a path
/// target exists. A thick glowing gold tile outline (RuneScape-style), distinct
/// from the yellow hover [`VoxelHighlight`] cube, so the player can tell "where
/// I'm pointing" from "where I told the character to go". The high emissive makes
/// it catch the Bloom post-FX for a soft glow.
fn spawn_target_highlight(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(tile_outline_mesh());
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.82, 0.10),
        emissive: LinearRgba::rgb(3.2, 2.3, 0.25),
        unlit: true,
        // Flat frame seen from above; render both faces so an orbit never hides it.
        double_sided: true,
        cull_mode: None,
        ..default()
    });

    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
        TargetHighlight,
        TargetAnim::default(),
    ));
}

/// Build a flat square-ring ("frame") mesh in the XZ plane, centered on the
/// origin: a [`OUTLINE_THICKNESS`]-wide gold band tracing the perimeter of a
/// unit tile. Four quads (one per edge), normals up. Laid flat on a cell's top
/// face by [`update_target_highlight`].
fn tile_outline_mesh() -> Mesh {
    let o = OUTLINE_OUTER;
    let i = OUTLINE_OUTER - OUTLINE_THICKNESS;

    // Outer / inner square corners (clockwise from the -X/-Z corner), y = 0.
    let oa = [-o, 0.0, -o];
    let ob = [o, 0.0, -o];
    let oc = [o, 0.0, o];
    let od = [-o, 0.0, o];
    let ia = [-i, 0.0, -i];
    let ib = [i, 0.0, -i];
    let ic = [i, 0.0, i];
    let id = [-i, 0.0, i];

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(16);
    let mut indices: Vec<u32> = Vec::with_capacity(24);
    let mut push_quad = |a, b, c, d| {
        let base = positions.len() as u32;
        positions.extend_from_slice(&[a, b, c, d]);
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    push_quad(oa, ob, ib, ia); // -Z edge
    push_quad(ob, oc, ic, ib); // +X edge
    push_quad(oc, od, id, ic); // +Z edge
    push_quad(od, oa, ia, id); // -X edge

    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    let uvs = vec![[0.0, 0.0]; positions.len()];

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Snap the destination outline onto the active [`PathTarget`] cell — lying flat
/// on that column's top face (voxel `surface_y`'s top is at `surface_y + 1`, plus
/// a small lift to avoid z-fighting the surface) and centered on the cell — then
/// show it. Hidden whenever the player is idle / has arrived (`PathTarget` is
/// `None`). The cell's surface height comes from the [`Terrain`] oracle, so the
/// marker is correct even for columns whose chunk hasn't streamed in yet.
fn update_target_highlight(
    target: Res<PathTarget>,
    terrain: Res<Terrain>,
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut marker: Query<
        (
            &mut Transform,
            &mut Visibility,
            &mut TargetAnim,
            &MeshMaterial3d<StandardMaterial>,
        ),
        With<TargetHighlight>,
    >,
) {
    let Ok((mut transform, mut visibility, mut anim, material)) = marker.single_mut() else {
        return;
    };

    let Some(cell) = target.0 else {
        *visibility = Visibility::Hidden;
        return;
    };

    // Fresh click → restart the pop. `PathTarget` is only written when a route is
    // found, so `is_changed()` fires exactly once per click (same tile included).
    if target.is_changed() {
        anim.elapsed = 0.0;
    } else {
        anim.elapsed += time.delta_secs();
    }

    let surface_y = terrain.surface_y(cell.x, cell.y);
    transform.translation = Vec3::new(
        cell.x as f32 + 0.5,
        surface_y as f32 + 1.0 + 0.02,
        cell.y as f32 + 0.5,
    );
    *visibility = Visibility::Visible;

    // Stamp-in: the ring lands a bit oversized and eases down to tile size with a
    // small overshoot (ease-out-back) over ~0.35 s — a satisfying "thunk".
    let pop = (anim.elapsed / 0.35).clamp(0.0, 1.0);
    let stamp = 1.55 - 0.55 * ease_out_back(pop); // 1.55 → ~0.97 → 1.0
                                                  // Cozy idle breathing once it has settled.
    let breathe = 1.0 + 0.035 * (anim.elapsed * 2.6).sin();
    transform.scale = Vec3::splat(stamp * breathe);

    // A bright flash on impact that decays into a gentle, slow glow pulse — reads
    // beautifully through the Bloom post-FX.
    if let Some(mat) = materials.get_mut(&material.0) {
        let flash = 1.0 + 2.2 * (1.0 - pop);
        let pulse = 1.0 + 0.18 * (anim.elapsed * 2.6).sin();
        let k = flash * pulse;
        mat.emissive = LinearRgba::rgb(3.2 * k, 2.3 * k, 0.25 * k);
    }
}

/// Ease-out-back: overshoots slightly past 1.0 before settling, giving the
/// stamp-in its springy "bounce".
fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    let p = t - 1.0;
    1.0 + C3 * p * p * p + C1 * p * p
}
