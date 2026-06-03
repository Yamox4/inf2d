//! Main-thread replay of [`ScatterItem`]s into real entities.
//!
//! The worker ([`super::scatter`]) decides *what* goes *where*; this module owns
//! the ECS side: spawning the per-tile parent and each prop's mesh + material +
//! collider request. Solid props (trees, rocks) get a [`SolidPropCollider`] that
//! the physics crate turns into a real collider; grass gets none, so the player
//! walks through it.

use bevy::prelude::*;

use inf3d_core::{Rock, Tree};
use inf3d_physics::SolidPropCollider;

use super::{footprint_radius, FoliageAssets, FoliageVariant, ScatterCategory, ScatterItem};

/// Marker on a per-tile parent entity. Despawning it cascades to every prop
/// scattered under that tile (the streamer relies on this for unloading).
#[derive(Component)]
struct FoliageTile;

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
        None => {}
    }
}
