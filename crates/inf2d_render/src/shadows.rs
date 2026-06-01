#![deny(unsafe_code)]
//! Drop shadows: a soft dark ellipse rendered just under any entity carrying both
//! [`IsoAnchor`] and [`DropShadow`]. The shadow is a separate child entity at
//! [`RenderLayer::SHADOW`], so y-sort naturally places it between the ground and
//! the entity's body.
//!
//! ## Architectural commitment: sprites are visual, [`IsoAnchor`] is logical
//!
//! Every iso-aware entity carries an [`IsoAnchor`] whose value is the
//! ground-anchored world position the entity *logically occupies*. The
//! `Transform` on the entity itself can drift upward in screen space (think of
//! a tall tree whose sprite stretches well above its trunk, or a sprite-stack
//! used to fake a tower's height), but every gameplay system — picking,
//! pathfinding, AI targeting, shadow placement, physics — resolves the entity's
//! position via [`IsoAnchor`], never via `Transform.xy`.
//!
//! Shadows specifically render at the anchor, which is what makes a tall tree's
//! shadow correctly sit at the foot of its trunk even though the foliage sprite
//! reaches far above. Gameplay code can opt out of auto-sync (see
//! [`IsoAnchor::auto_sync`]) when it needs to detach a sprite's visual offset
//! from its logical footprint.

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::layers::RenderLayer;

/// Edge dimensions of the procedurally-generated soft-ellipse shadow texture.
/// The aspect (2:1) matches the iso tile so the squashed ellipse looks correct
/// before per-instance `squash` further compresses it.
const SHADOW_TEX_WIDTH: u32 = 64;
const SHADOW_TEX_HEIGHT: u32 = 32;

/// Local Z offset applied to the shadow child entity. The parent sits at
/// [`RenderLayer::ENTITY`] (`2.0`), so a local offset of `SHADOW - ENTITY`
/// (`-0.5`) places the child at the absolute [`RenderLayer::SHADOW`] (`1.5`)
/// — between the ground/decal layer and the entity body.
const SHADOW_LOCAL_Z: f32 = RenderLayer::SHADOW - RenderLayer::ENTITY;

/// "Where on the ground this entity actually stands." Required on every
/// iso-aware entity. The entity's `Transform` may live anywhere (a tall tree's
/// sprite center is well above its base; a sprite-stacked tower's apex sits at
/// roof height), but `IsoAnchor.world` is the canonical ground point used by
/// picking, shadow placement, pathfinding, and physics.
///
/// By default `auto_sync` is `true`: each frame
/// [`sync_iso_anchor`] copies `Transform.translation.xy()` into
/// `world`. Gameplay code that needs to decouple the sprite's visual position
/// from the entity's logical footprint (e.g. a hopping unit whose sprite bobs
/// in screen space while its tile occupancy stays fixed) sets `auto_sync =
/// false` and writes `world` directly.
#[derive(Component, Reflect, Debug, Clone, Copy)]
#[reflect(Component)]
pub struct IsoAnchor {
    /// Ground-anchored world position (XY plane). Read by every system that
    /// needs the entity's *logical* footprint rather than its sprite center.
    pub world: Vec2,
    /// When `true` (default), [`sync_iso_anchor`] overwrites `world` from
    /// `Transform.translation.xy()` each frame. Flip to `false` when gameplay
    /// code is the source of truth for the anchor.
    pub auto_sync: bool,
}

impl Default for IsoAnchor {
    fn default() -> Self {
        Self {
            world: Vec2::ZERO,
            auto_sync: true,
        }
    }
}

/// Attach to any entity that should cast a soft elliptical drop shadow at its
/// [`IsoAnchor`]. The plugin spawns a single child [`Sprite`] carrying
/// [`DropShadowVisual`] the first frame both components are present; that
/// child is reused for the lifetime of the entity and recolored in place when
/// the [`DropShadow`] component changes.
#[derive(Component, Reflect, Debug, Clone)]
#[reflect(Component)]
pub struct DropShadow {
    /// Major-axis radius of the ellipse in world units (half the visible width).
    pub radius: f32,
    /// Vertical squash factor — ellipse height equals `radius * squash * 2`.
    /// Iso shadows are squashed to match the 2:1 tile aspect; default `0.5`.
    pub squash: f32,
    /// Color of the shadow at full opacity (the texture's per-texel alpha is
    /// multiplied on top to produce the soft falloff).
    pub color: Color,
}

impl Default for DropShadow {
    fn default() -> Self {
        Self {
            radius: 12.0,
            squash: 0.5,
            color: Color::srgba(0.0, 0.0, 0.0, 0.45),
        }
    }
}

/// Internal marker on the child entity that visually renders the shadow.
/// External code should never query for this directly; it exists so
/// [`attach_shadows`] can detect "this entity already has its shadow visual"
/// without scanning components.
#[derive(Component)]
struct DropShadowVisual;

/// Shared GPU asset for every drop shadow. One soft-ellipse texture is reused
/// across every entity; per-instance `DropShadow::color` and
/// `Sprite::custom_size` make each shadow unique without spawning extra images.
#[derive(Resource, Clone)]
pub struct ShadowAssets {
    /// 64x32 RGBA soft ellipse, white RGB with `smoothstep(1, 0, r)^2` alpha
    /// falloff in normalized ellipse coordinates.
    pub texture: Handle<Image>,
}

/// Plugin entry point. Registers reflect types, builds the shared shadow
/// texture on `Startup`, and wires the three update systems into the inf2d
/// system-set ordering.
pub struct ShadowsPlugin;

impl Plugin for ShadowsPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<IsoAnchor>()
            .register_type::<DropShadow>()
            .add_systems(Startup, build_shadow_texture)
            .add_systems(Update, sync_iso_anchor.in_set(inf2d_core::CoreSet))
            .add_systems(
                Update,
                (attach_shadows, update_shadow_color).in_set(inf2d_core::RenderPrepSet),
            );
    }
}

/// Generate the shared soft-ellipse texture in CPU memory and stash it in
/// [`ShadowAssets`]. Alpha is `smoothstep(1, 0, r_normalized)^2` in elliptical
/// coordinates, RGB is pure white so per-instance `Sprite::color` is the only
/// tint knob.
fn build_shadow_texture(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let width = SHADOW_TEX_WIDTH;
    let height = SHADOW_TEX_HEIGHT;
    let mut buf = vec![0u8; (width * height * 4) as usize];
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let rx = cx;
    let ry = cy;

    for y in 0..height {
        for x in 0..width {
            // Normalize sample into unit-ellipse coordinates so the falloff
            // forms an ellipse rather than a circle.
            let dx = (x as f32 + 0.5 - cx) / rx;
            let dy = (y as f32 + 0.5 - cy) / ry;
            let r = (dx * dx + dy * dy).sqrt();
            let t = (1.0 - r).clamp(0.0, 1.0);
            // smoothstep(1, 0, r) == 1 - smoothstep(0, 1, r)
            let smooth = t * t * (3.0 - 2.0 * t);
            // Squared so the dark core is small and the falloff tail long.
            let a = (smooth * smooth * 255.0).round().clamp(0.0, 255.0) as u8;
            let off = ((y * width + x) * 4) as usize;
            buf[off] = 255;
            buf[off + 1] = 255;
            buf[off + 2] = 255;
            buf[off + 3] = a;
        }
    }

    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        buf,
        // Linear (non-sRGB) — the alpha curve we wrote samples verbatim.
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::linear();

    let texture = images.add(image);
    commands.insert_resource(ShadowAssets { texture });
}

/// Copy `Transform.translation.xy()` into [`IsoAnchor::world`] for every entity
/// that opts into auto-sync. Runs in [`inf2d_core::CoreSet`] so downstream
/// simulation/rendering systems see the freshest anchor each frame.
pub fn sync_iso_anchor(mut q: Query<(&Transform, &mut IsoAnchor)>) {
    for (transform, mut anchor) in &mut q {
        if anchor.auto_sync {
            anchor.world = transform.translation.xy();
        }
    }
}

/// Spawn the shadow-visual child for any entity that has both [`IsoAnchor`] and
/// [`DropShadow`] but does not yet own a [`DropShadowVisual`] descendant.
/// The child is parented via [`ChildOf`] so its position auto-tracks the
/// entity, and its local `Z` of [`SHADOW_LOCAL_Z`] resolves to the absolute
/// [`RenderLayer::SHADOW`] depth.
pub fn attach_shadows(
    mut commands: Commands,
    assets: Option<Res<ShadowAssets>>,
    candidates: Query<(Entity, &DropShadow, Option<&Children>), With<IsoAnchor>>,
    visuals: Query<&DropShadowVisual>,
) {
    let Some(assets) = assets else {
        // Texture not built yet on the first frame; try again next frame.
        return;
    };

    for (entity, shadow, children) in &candidates {
        let already_has = children
            .map(|cs| cs.iter().any(|child| visuals.get(child).is_ok()))
            .unwrap_or(false);
        if already_has {
            continue;
        }

        let size = Vec2::new(shadow.radius * 2.0, shadow.radius * 2.0 * shadow.squash);
        let sprite = Sprite {
            image: assets.texture.clone(),
            color: shadow.color,
            custom_size: Some(size),
            ..default()
        };

        commands.entity(entity).with_children(|parent| {
            parent.spawn((
                DropShadowVisual,
                sprite,
                Transform::from_xyz(0.0, 0.0, SHADOW_LOCAL_Z),
            ));
        });
    }
}

/// When a parent's [`DropShadow`] changes, rewrite the child visual's
/// `Sprite.color` and `custom_size` to match. Only touches entities whose
/// parent's [`DropShadow`] actually changed this frame.
pub fn update_shadow_color(
    parents: Query<(&DropShadow, &Children), Changed<DropShadow>>,
    mut visuals: Query<&mut Sprite, With<DropShadowVisual>>,
) {
    for (shadow, children) in &parents {
        for child in children.iter() {
            let Ok(mut sprite) = visuals.get_mut(child) else {
                continue;
            };
            sprite.color = shadow.color;
            sprite.custom_size = Some(Vec2::new(
                shadow.radius * 2.0,
                shadow.radius * 2.0 * shadow.squash,
            ));
        }
    }
}
