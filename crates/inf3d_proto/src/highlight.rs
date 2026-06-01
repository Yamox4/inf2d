//! Voxel hover highlight: a translucent cube that snaps to the voxel under the
//! cursor each frame, hidden when the cursor isn't over any voxel.

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_voxel_world::prelude::*;

use crate::camera::IsoCamera;
use crate::world::MainWorld;

/// Slightly larger than a unit voxel so the overlay doesn't z-fight the surface.
const HIGHLIGHT_SCALE: f32 = 1.04;

#[derive(Component)]
struct VoxelHighlight;

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
            .add_systems(Startup, spawn_highlight)
            .add_systems(Update, update_highlight);
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
