//! Main-thread replay of [`ScatterItem`]s into real entities.
//!
//! The scatter workers ([`super::scatter`]) decide *what* goes *where*; this
//! module owns the ECS side: spawning the per-tile parent and each prop's mesh +
//! material + collider request. It's shared by BOTH streaming layers — the solid
//! layer feeds it tree/rock items, the grass layer feeds it grass items — and the
//! per-item branch below routes each category correctly: solid props (trees,
//! rocks) get a [`SolidPropCollider`] the physics crate turns into a real
//! collider, while grass gets none so the player walks through it.

use bevy::prelude::*;

use inf3d_core::{Rock, Tree};
use inf3d_physics::SolidPropCollider;

use super::{footprint_radius, FoliageAssets, FoliageVariant, ScatterCategory, ScatterItem};

/// Marker on a per-tile parent entity. Despawning it cascades to every prop
/// scattered under that tile (the streamer relies on this for unloading).
///
/// Public + re-exported from the crate root so downstream sinks (e.g.
/// `inf3d_monitor`) can count foliage tiles with
/// `Query<(), With<inf3d_render::FoliageTile>>` instead of a fragile
/// `Name`-prefix scan. Attached to EVERY tile parent (both the solid and grass
/// layers go through [`spawn_tile_entities`]).
#[derive(Component)]
pub struct FoliageTile;

/// Marker on an individual grass blade, tagged with the voxel column it sits on.
/// Lets a block edit despawn exactly the blade on that cell (grass blades are
/// separate entities, so this touches nothing else in the tile).
#[derive(Component)]
pub(super) struct GrassBlade {
    pub cell: IVec2,
}

/// Which kind of static collider a solid prop requests. Grass is represented by
/// `None` at the call site and gets no collider at all.
#[derive(Clone, Copy)]
enum PropKind {
    Tree,
    Rock,
}

/// Spawn the tile parent and replay the worker's [`ScatterItem`]s into real
/// entities (meshes/materials/colliders), returning the parent entity so the
/// streamer can hold it for cascade-despawn.
pub(super) fn spawn_tile_entities(
    commands: &mut Commands,
    assets: &FoliageAssets,
    tile: IVec2,
    items: &[ScatterItem],
) -> Entity {
    let parent = commands
        .spawn((
            Transform::default(),
            Visibility::default(),
            Name::new(format!("FoliageTile {},{}", tile.x, tile.y)),
            FoliageTile,
        ))
        .id();

    for item in items {
        let (variant, kind) = match item.category {
            ScatterCategory::Tree => (&assets.trees[item.variant], Some(PropKind::Tree)),
            ScatterCategory::Rock => (&assets.rocks[item.variant], Some(PropKind::Rock)),
            ScatterCategory::Grass => (&assets.grass[item.variant], None),
        };
        spawn_prop(
            commands,
            parent,
            variant,
            assets.material.clone(),
            item.pos,
            item.yaw,
            kind,
        );
    }

    parent
}

fn spawn_prop(
    commands: &mut Commands,
    parent: Entity,
    variant: &FoliageVariant,
    material: Handle<StandardMaterial>,
    pos: Vec3,
    yaw: f32,
    kind: Option<PropKind>,
) {
    let mut entity = commands.spawn((
        Mesh3d(variant.mesh.clone()),
        MeshMaterial3d(material),
        Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
        Visibility::default(),
        ChildOf(parent),
    ));
    // Solid props (trees, rocks) get a static collider sized to their footprint
    // so the player is blocked by them and can stand on rocks. The physics crate
    // turns `SolidPropCollider` into the real `Collider` + `RigidBody::Static`
    // on the `Solid` collision layer.
    //
    // GRASS gets NO collider — it's intentionally left out of the physics layers
    // so the player walks straight through it (see `inf3d_physics::GameLayer`).
    match kind {
        Some(PropKind::Tree) => {
            let height = variant.size.y;
            let radius = (footprint_radius(variant.size) * 0.35).clamp(0.12, 0.6);
            entity.insert((Tree, SolidPropCollider::Tree { radius, height }));
        }
        Some(PropKind::Rock) => {
            entity.insert((
                Rock,
                SolidPropCollider::Rock {
                    half: variant.size * 0.5,
                },
            ));
        }
        // Grass: tag with its cell so a block edit can despawn just this blade.
        None => {
            entity.insert(GrassBlade {
                cell: IVec2::new(pos.x.floor() as i32, pos.z.floor() as i32),
            });
        }
    }
}
