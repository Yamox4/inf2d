//! Block place / break.
//!
//! In [`EditMode::Build`], a **left-click** places a block on the hovered face and
//! a **right-click** removes the hovered voxel — both written into the shared
//! [`VoxelOverrides`] store, which then marks the affected chunk(s) [`NeedsRemesh`]
//! so the change becomes visible. Because the store is the single source of truth
//! (the mesher snapshots it, the [`Terrain`] oracle consults it), the physics
//! ground picks the edit up for free — no extra wiring.
//!
//! Targeting reuses the [`Hover`] resource, written by `highlight::update_hover` in
//! the same Input phase (ordered before this system) so a click acts on THIS frame's
//! crosshair voxel. The editor only runs in [`EditMode::Build`]; [`EditMode::Walk`]
//! clicks are free for other uses.

use bevy::prelude::*;
use bevy_voxel_world::prelude::{Chunk, NeedsRemesh};

use inf3d_core::{EditMode, FollowTarget, GameSet, SelectedMaterial};
use inf3d_physics::PLAYER_DIMS;
use inf3d_world::{MainWorld, TerrainMaterialId};
use inf3d_worldgen::VoxelOverrides;

use crate::dust::DustBurst;
use crate::Hover;

/// Voxel side length of a chunk, matching `bevy_voxel_world` (chunks are 32³).
const CHUNK: i32 = 32;

/// Lifetime of the place "materialize" pop and the break "crumble", in seconds.
const PLACE_FX_LIFE: f32 = 0.26;
const BREAK_FX_LIFE: f32 = 0.34;

/// Emitted whenever the player edits a voxel. Downstream sinks react to it without
/// depending on the editor's internals: the foliage streamer reads [`cell`](Self::cell)
/// to drop grass off edited cells, and `inf3d_audio` reads [`placed`](Self::placed)
/// to play the place vs. break SFX. Public + registered here (the source), mirroring
/// how `inf3d_gameplay::Footstep` is owned by its emitter and consumed by the audio
/// sink.
#[derive(Message)]
pub struct BlockEdited {
    /// The edited column `(x, z)` — the cell whose grass blade should clear.
    pub cell: IVec2,
    /// `true` = a block was placed, `false` = a block was removed.
    pub placed: bool,
    /// Material involved: the placed material on a place, or the removed voxel's
    /// material on a break (the terrain fallback index `0` if it can't be resolved).
    /// Lets sinks vary the effect per material later (the audio sink only uses
    /// [`placed`](Self::placed) today).
    pub material: u8,
}

pub struct EditPlugin;

impl Plugin for EditPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<BlockEdited>()
            .add_systems(Startup, init_block_fx_assets)
            // Input phase, ordered AFTER the crosshair raycast so we consume this
            // frame's `Hover`, not last frame's (which mis-targeted while orbiting).
            .add_systems(
                Update,
                block_edit
                    .in_set(GameSet::Input)
                    .after(crate::highlight::update_hover),
            )
            // The transient place/break cubes animate in the Fx phase.
            .add_systems(Update, update_block_fx.in_set(GameSet::Fx));
    }
}

/// Shared cube mesh for the transient place/break effect cubes (materials are
/// per-instance so they can be tinted to the block and fade independently).
#[derive(Resource)]
struct BlockFxAssets {
    cube: Handle<Mesh>,
}

fn init_block_fx_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(BlockFxAssets {
        cube: meshes.add(Cuboid::from_length(1.0)),
    });
}

/// A short-lived cube that animates a block edit: a spring "pop-in" on place, a
/// shrinking, sinking "crumble" on break. Fades out as the real (re-meshed) chunk
/// geometry takes over.
#[derive(Component)]
struct BlockFx {
    age: f32,
    life: f32,
    /// `true` = place (pop-in), `false` = break (crumble).
    place: bool,
}

// The old startup "placeholder pillar" lived here; the flat test world's stamper
// in `inf3d_menu` now owns all the seeded test structures, so New Game starts from
// a clean slate and stamps them deterministically.

/// In Build mode, place (left-click) or break (right-click) the hovered voxel and
/// re-mesh.
fn block_edit(
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<EditMode>,
    selected: Res<SelectedMaterial>,
    hover: Res<Hover>,
    overrides: Res<VoxelOverrides>,
    interactions: Query<&Interaction>,
    chunks: Query<(Entity, &Chunk<MainWorld>)>,
    player_q: Query<&GlobalTransform, With<FollowTarget>>,
    fx_assets: Res<BlockFxAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut dust: MessageWriter<DustBurst>,
    mut edited_events: MessageWriter<BlockEdited>,
    mut commands: Commands,
) {
    // Editing only happens in Build mode.
    if *mode != EditMode::Build {
        return;
    }
    // Left-click places, right-click breaks. Left wins if both land on one frame.
    let place = mouse.just_pressed(MouseButton::Left);
    let break_ = mouse.just_pressed(MouseButton::Right);
    if !place && !break_ {
        return;
    }
    // Ignore clicks that landed on a UI widget (e.g. the mode buttons), so
    // switching mode doesn't also edit the world behind the cursor.
    if interactions.iter().any(|i| !matches!(i, Interaction::None)) {
        return;
    }
    let Some(voxel) = hover.voxel else {
        return;
    };

    // Edit the store and gather what the effect needs: the touched cell, the
    // block's color (to tint the puff/cube), whether it was a place or break, and
    // the material involved (for the edit message's downstream sinks).
    let (edited, color, place, material) = if place {
        // Place into the cell on the hovered face. Without a normal there's no
        // unambiguous side to build on, so do nothing.
        let Some(normal) = hover.normal else {
            return;
        };
        let target = voxel + normal;
        // Don't let the player place a solid block inside their own body. The
        // controller reads ground analytically from the override store, so a block
        // dropped into the player's column would pop or wedge them. Silently ignore
        // such a placement (like an out-of-reach click).
        if let Ok(player) = player_q.single() {
            if cell_overlaps_player(target, player.translation()) {
                return;
            }
        }
        // The player's currently-picked build material (defaults to BuiltStone;
        // set by the picker / number keys in `inf3d_ui`).
        let mat = selected.0;
        overrides.place(target, mat);
        let color = TerrainMaterialId::from_index(mat)
            .map(|id| id.color())
            .unwrap_or(NEUTRAL_DEBRIS);
        (target, color, true, mat)
    } else {
        // Right-click: remove the hovered voxel.
        overrides.remove(voxel);
        let mat = hover.material.unwrap_or(0);
        let color = TerrainMaterialId::from_index(mat)
            .map(|id| id.color())
            .unwrap_or(NEUTRAL_DEBRIS);
        (voxel, color, false, mat)
    };

    mark_chunks_dirty(&mut commands, &chunks, edited);
    // Notify downstream sinks: the foliage streamer clears grass off this cell,
    // the audio sink plays the place/break SFX.
    edited_events.write(BlockEdited {
        cell: IVec2::new(edited.x, edited.z),
        placed: place,
        material,
    });

    // Juice: a tinted pop-in (place) / crumble (break) cube + a dust puff — the
    // same particle system as footsteps, a bigger cloud on break.
    let center = edited.as_vec3() + Vec3::splat(0.5);
    spawn_block_fx(
        &mut commands,
        &mut materials,
        &fx_assets.cube,
        center,
        color,
        place,
    );
    // Place: the block center is now solid, so a puff there is hidden inside it.
    // Emit from UNDER the block and fling it harder so it scatters out past the
    // edges and stays visible around the base. Break: the cell is now air, so a
    // bigger cloud right at the center reads fine.
    let (dust_pos, amount, speed) = if place {
        (center - Vec3::Y * 0.5, 14, 2.7)
    } else {
        (center, 30, 2.9)
    };
    dust.write(DustBurst {
        pos: dust_pos,
        amount,
        speed,
    });
}

/// Fallback debris color when a block's material can't be resolved (a stray index).
const NEUTRAL_DEBRIS: [u8; 3] = [0x8a, 0x8a, 0x8a];

/// Whether world voxel cell `cell` overlaps the player's capsule, so placing a solid
/// block there would trap/clip them. `player_center` is the player entity's
/// translation, which is the capsule CENTRE (see [`inf3d_physics::PLAYER_DIMS`]): the
/// body spans `±(half_height + radius)` vertically and a disc of `radius` horizontally.
fn cell_overlaps_player(cell: IVec3, player_center: Vec3) -> bool {
    let radius = PLAYER_DIMS.radius;
    let half_extent = PLAYER_DIMS.half_height + PLAYER_DIMS.radius; // centre → feet/head
    // Vertical: cell box [cell.y, cell.y+1] vs body [centre.y - half, centre.y + half].
    let cy = cell.y as f32;
    if cy >= player_center.y + half_extent || cy + 1.0 <= player_center.y - half_extent {
        return false;
    }
    // Horizontal: closest point of the cell's XZ square to the player centre, within
    // `radius` (capsule cross-section is a disc).
    let closest_x = player_center.x.clamp(cell.x as f32, cell.x as f32 + 1.0);
    let closest_z = player_center.z.clamp(cell.z as f32, cell.z as f32 + 1.0);
    let dx = player_center.x - closest_x;
    let dz = player_center.z - closest_z;
    dx * dx + dz * dz < radius * radius
}

/// Spawn the transient place/break effect cube, tinted to the block's color.
fn spawn_block_fx(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    mesh: &Handle<Mesh>,
    center: Vec3,
    color: [u8; 3],
    place: bool,
) {
    let [r, g, b] = color;
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb_u8(r, g, b),
        // A soft glow on placement (reads through Bloom as a "materialize" flash);
        // a plain tinted cube for the break crumble.
        emissive: if place {
            LinearRgba::rgb(0.45, 0.5, 0.6)
        } else {
            LinearRgba::BLACK
        },
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    // Place pops up from tiny; break starts at full size and crumbles away.
    let start_scale = if place { 0.12 } else { 1.0 };
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material),
        Transform::from_translation(center).with_scale(Vec3::splat(start_scale)),
        BlockFx {
            age: 0.0,
            life: if place { PLACE_FX_LIFE } else { BREAK_FX_LIFE },
            place,
        },
    ));
}

/// Animate and retire the place/break effect cubes.
fn update_block_fx(
    time: Res<Time>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut q: Query<(
        Entity,
        &mut Transform,
        &MeshMaterial3d<StandardMaterial>,
        &mut BlockFx,
    )>,
) {
    let dt = time.delta_secs();
    for (e, mut t, mat, mut fx) in &mut q {
        fx.age += dt;
        if fx.age >= fx.life {
            commands.entity(e).despawn();
            continue;
        }
        let f = (fx.age / fx.life).clamp(0.0, 1.0);

        if fx.place {
            // Spring up to ~1.06 with overshoot, then fade out so the real meshed
            // block (which arrives a frame or two after the remesh) shows through.
            let s = (1.06 * ease_out_back(f)).max(0.05);
            t.scale = Vec3::splat(s);
            set_alpha(&mut materials, mat, 0.85 * (1.0 - f * f));
        } else {
            // Crumble: shrink, sink, and slowly spin away.
            let s = (1.0 - f) * 1.02;
            t.scale = Vec3::splat(s.max(0.0));
            t.translation.y -= 0.5 * dt;
            t.rotate_y(2.2 * dt);
            set_alpha(&mut materials, mat, 1.0 - f);
        }
    }
}

fn set_alpha(
    materials: &mut Assets<StandardMaterial>,
    handle: &MeshMaterial3d<StandardMaterial>,
    alpha: f32,
) {
    if let Some(m) = materials.get_mut(&handle.0) {
        m.base_color.set_alpha(alpha.clamp(0.0, 1.0));
    }
}

/// Ease-out-back: overshoots slightly past 1.0 before settling — the springy
/// "pop" for a placed block.
fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    let p = t - 1.0;
    1.0 + C3 * p * p * p + C1 * p * p
}

/// Mark the chunk holding `voxel`, plus every neighbor chunk whose padded mesh
/// samples it, as [`NeedsRemesh`]. The library then re-runs our voxel-lookup
/// delegate for those chunks, which re-reads the override store — so the edit
/// (and any newly exposed/hidden face across a chunk border) is re-meshed.
///
/// Chunks carry a 1-voxel padding ring, and the mesher reads it for BOTH face
/// culling and ambient occlusion (AO samples the 8 diagonal neighbours of each
/// face corner). So an edit on a chunk edge/corner can change up to 8 chunks'
/// meshes — not just the 6 face-neighbours. A voxel `v` is inside chunk `C`'s padded
/// region exactly when `v ∈ [C*32 - 1, C*32 + 32]` per axis, i.e. the affected set is
/// `chunk_of(v + d)` for `d ∈ {-1,0,1}³` (≤ 8 distinct chunks). Marking only the 6
/// faces previously left a stale AO seam / phantom skirt face on diagonal neighbours.
fn mark_chunks_dirty(
    commands: &mut Commands,
    chunks: &Query<(Entity, &Chunk<MainWorld>)>,
    voxel: IVec3,
) {
    let mut dirty: Vec<IVec3> = Vec::with_capacity(8);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let c = chunk_of(voxel + IVec3::new(dx, dy, dz));
                if !dirty.contains(&c) {
                    dirty.push(c);
                }
            }
        }
    }
    for (entity, chunk) in chunks.iter() {
        if dirty.contains(&chunk.position) {
            commands.entity(entity).try_insert(NeedsRemesh);
        }
    }
}

/// Chunk coordinate containing world voxel `v` (chunk origin = `position * 32`,
/// so this is floor division — `div_euclid` handles negatives correctly).
fn chunk_of(v: IVec3) -> IVec3 {
    IVec3::new(
        v.x.div_euclid(CHUNK),
        v.y.div_euclid(CHUNK),
        v.z.div_euclid(CHUNK),
    )
}
