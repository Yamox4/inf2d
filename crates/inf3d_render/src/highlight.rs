//! Voxel hover highlight: a translucent cube that snaps to the voxel under the
//! cursor each frame, hidden when the cursor isn't over any voxel. Plus a
//! distinct persistent **destination** marker that sits on the clicked
//! click-to-move target cell until the player arrives there.

use bevy::{prelude::*, window::PrimaryWindow};
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
/// cell. Distinct (cyan/green) from the yellow hover [`VoxelHighlight`].
#[derive(Component)]
struct TargetHighlight;

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
        .and_then(|(cursor, (camera, cam_gtf))| {
            camera.viewport_to_world(cam_gtf, cursor).ok()
        })
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
/// target exists. A cyan/green emissive cube, distinct from the yellow hover
/// [`VoxelHighlight`], so the player can tell "where I'm pointing" from "where
/// I told the character to go".
fn spawn_target_highlight(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(Cuboid::from_length(HIGHLIGHT_SCALE));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.30, 1.0, 0.70, 0.30),
        emissive: LinearRgba::rgb(0.25, 1.30, 0.85),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    });

    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
        TargetHighlight,
    ));
}

/// Snap the destination marker onto the active [`PathTarget`] cell — sitting on
/// that column's top surface voxel (same center convention as the hover
/// highlight: integer corner + 0.5) — and show it. Hidden whenever the player is
/// idle / has arrived (`PathTarget` is `None`). The cell's surface height comes
/// from the [`Terrain`] oracle, so the marker is correct even for columns whose
/// chunk hasn't streamed in yet.
fn update_target_highlight(
    target: Res<PathTarget>,
    terrain: Res<Terrain>,
    mut marker: Query<(&mut Transform, &mut Visibility), With<TargetHighlight>>,
) {
    let Ok((mut transform, mut visibility)) = marker.single_mut() else {
        return;
    };

    match target.0 {
        Some(cell) => {
            let surface_y = terrain.surface_y(cell.x, cell.y);
            transform.translation = Vec3::new(
                cell.x as f32 + 0.5,
                surface_y as f32 + 0.5,
                cell.y as f32 + 0.5,
            );
            *visibility = Visibility::Visible;
        }
        None => *visibility = Visibility::Hidden,
    }
}
