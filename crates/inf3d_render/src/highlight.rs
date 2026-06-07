//! Voxel hover highlight: a translucent cube that snaps to the voxel under the
//! screen-center crosshair each frame (Build mode only), hidden when nothing is
//! targeted or while walking.
//!
//! Split into two systems so consumers see a FRESH hit:
//! - [`update_hover`] runs in [`GameSet::Input`] and is the single writer of the
//!   [`Hover`] resource (the crosshair → voxel targeting service). Being in the
//!   Input phase, same-frame consumers — the block editor and the HUD readout —
//!   read this frame's raycast, not last frame's. It is also gated with the rest of
//!   the Input phase, so it does NOT raycast in the menu / while paused.
//! - [`update_highlight`] runs in [`GameSet::Fx`] and is a pure VISUAL mirror of
//!   [`Hover`]: it just positions/shows/hides the translucent cube.

use bevy::prelude::*;
use bevy_voxel_world::prelude::*;

use inf3d_camera::OrbitCamera;
use inf3d_core::{EditMode, FollowTarget, GameSet};
use inf3d_world::MainWorld;

/// Slightly larger than a unit voxel so the overlay doesn't z-fight the surface.
const HIGHLIGHT_SCALE: f32 = 1.04;

/// Max distance (world units) from the player the build crosshair can target — your
/// "reach". A crosshair hit farther than this is ignored, so you place/break in the
/// space around you rather than across the map.
const BUILD_RANGE: f32 = 6.0;

#[derive(Component)]
struct VoxelHighlight;

/// Hovered voxel exposed for the HUD: the integer voxel position and its
/// material id (if the hovered voxel is solid).
#[derive(Resource, Default)]
pub struct Hover {
    pub voxel: Option<IVec3>,
    pub material: Option<u8>,
    /// Outward normal of the hovered face (the side the cursor is over), so the
    /// block-edit system knows which adjacent cell to fill when placing a block.
    pub normal: Option<IVec3>,
}

pub struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Hover>()
            .add_systems(Startup, spawn_highlight)
            // The crosshair raycast (writes `Hover`) runs in Input so same-frame
            // consumers (the editor, the HUD) get this frame's hit; the cube visual
            // mirrors it in Fx.
            .add_systems(Update, update_hover.in_set(GameSet::Input))
            .add_systems(Update, update_highlight.in_set(GameSet::Fx));
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

/// Raycast the screen-center crosshair into the voxel world and record the targeted
/// voxel (+ material + face normal) in [`Hover`]. The single writer of [`Hover`] and
/// the one source of crosshair targeting. Runs in [`GameSet::Input`] so the block
/// editor and HUD see THIS frame's hit (previously this ran in `Fx`, after the editor
/// in `Input`, so clicks acted on last frame's voxel — a visible mis-target while
/// orbiting). Gated to [`EditMode::Build`]; outside Build the hover is cleared.
pub(crate) fn update_hover(
    cam: Query<&GlobalTransform, With<OrbitCamera>>,
    player_q: Query<&GlobalTransform, With<FollowTarget>>,
    voxel_world: VoxelWorld<MainWorld>,
    mode: Res<EditMode>,
    mut hover: ResMut<Hover>,
) {
    // Only target voxels while building; outside Build clear the hover state.
    if *mode != EditMode::Build {
        hover.voxel = None;
        hover.material = None;
        hover.normal = None;
        return;
    }

    // Camera-forward = screen center for a centered viewport, so the crosshair and
    // the targeted voxel line up. Targets the first solid voxel along the ray.
    let hit = cam.single().ok().and_then(|cam_gtf| {
        let ray = Ray3d {
            origin: cam_gtf.translation(),
            direction: Dir3::new(cam_gtf.forward().as_vec3()).unwrap_or(Dir3::NEG_Z),
        };
        voxel_world.raycast(ray, &|(_coords, voxel)| matches!(voxel, WorldVoxel::Solid(_)))
    });

    // Build reach: ignore a crosshair hit farther than `BUILD_RANGE` from the player,
    // so you can only place/break within arm's reach in the space in front of you.
    let hit = hit.filter(|h| {
        player_q
            .single()
            .map(|p| (h.position - p.translation()).length() <= BUILD_RANGE)
            .unwrap_or(true)
    });

    match hit {
        Some(hit) => {
            hover.voxel = Some(hit.voxel_pos());
            hover.material = match hit.voxel {
                WorldVoxel::Solid(m) => Some(m),
                _ => None,
            };
            hover.normal = hit.voxel_normal();
        }
        None => {
            hover.voxel = None;
            hover.material = None;
            hover.normal = None;
        }
    }
}

/// Move/show the translucent highlight cube to match the current [`Hover`]; hide it
/// when nothing is targeted. Visual only — the hit itself is computed in
/// [`update_hover`] during the Input phase, so the cube tracks the same voxel the
/// editor will act on this frame.
fn update_highlight(
    hover: Res<Hover>,
    mut highlight: Query<(&mut Transform, &mut Visibility), With<VoxelHighlight>>,
) {
    let Ok((mut transform, mut visibility)) = highlight.single_mut() else {
        return;
    };
    match hover.voxel {
        Some(voxel) => {
            // `voxel` is the integer voxel corner; center the cube on it.
            transform.translation = voxel.as_vec3() + Vec3::splat(0.5);
            *visibility = Visibility::Visible;
        }
        None => {
            *visibility = Visibility::Hidden;
        }
    }
}
