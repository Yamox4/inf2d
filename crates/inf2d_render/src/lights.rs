#![deny(unsafe_code)]
//! 2D additive point lights. Each light is a quad rendered with a custom `Material2d`
//! whose fragment shader does `out = tint * radial_falloff(uv) * intensity` with
//! additive blending, producing a soft glow that brightens whatever's underneath.
//!
//! Lighting is purely cosmetic — no shadows, no occlusion. The trick scales: 50
//! lights on screen at 60 fps is comfortable on integrated GPUs.
//!
//! ## Wiring
//!
//! Lights are components, not entities-with-a-bundle. Spawn anything with a
//! [`PointLight2D`] and a `Transform`; the [`attach_light_visuals`] system
//! adds the rendered quad as a child the next frame.
//!
//! ## Why additive
//!
//! Standard alpha blending darkens the destination by `(1 - src_alpha)`, which
//! makes overlapping lights *replace* each other instead of summing. For torch
//! glow you want sums — two torches next to each other should be brighter than
//! one. The [`specialize`](Material2d::specialize) callback below overrides the
//! pipeline's color-target blend to `BlendOperation::Add` with `One`/`One`
//! factors. The fragment shader premultiplies RGB by alpha so the source value
//! already carries its own intensity weight — no extra `SrcAlpha` factor needed.
//!
//! [`Material2d::specialize`]: bevy::sprite_render::Material2d::specialize

use bevy::asset::{embedded_asset, RenderAssetUsages};
use bevy::image::ImageSampler;
use bevy::math::primitives::Rectangle;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Extent3d,
    RenderPipelineDescriptor, SpecializedMeshPipelineError, TextureDimension, TextureFormat,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey, Material2dPlugin};

use crate::layers::RenderLayer;
use inf2d_core::{tile_to_world, WorldTile};

/// Z value lights render at — above tiles, decals, and entities (`RenderLayer::ENTITY`),
/// below the day/night overlay (`RenderLayer::DAYNIGHT`) and screen-space UI.
pub const LIGHTS_Z: f32 = RenderLayer::ENTITY + 1.0;

/// Edge length, in pixels, of the procedurally-generated falloff texture.
const FALLOFF_SIZE: u32 = 128;

/// A 2D point light. Add as a component to any entity (typically alongside a
/// `Transform` at the world position you want to illuminate). A child quad with
/// the light material is spawned automatically by [`attach_light_visuals`].
#[derive(Component, Reflect, Debug, Clone, Copy)]
#[reflect(Component)]
pub struct PointLight2D {
    /// RGB color of the light. Alpha is ignored — [`intensity`](Self::intensity)
    /// is the strength knob.
    pub color: Color,
    /// Overall brightness multiplier. 1.0 is a normal torch; 4.0 is a bonfire.
    pub intensity: f32,
    /// World-unit radius. The light sprite is `radius * 2` on a side, so the
    /// falloff texture's outer ring lands at exactly this distance.
    pub radius: f32,
}

impl Default for PointLight2D {
    fn default() -> Self {
        Self {
            color: Color::srgb(1.0, 0.78, 0.45),
            intensity: 1.5,
            radius: 192.0,
        }
    }
}

/// Marker placed on the rendered child quad so [`attach_light_visuals`] doesn't
/// re-spawn one every frame.
#[derive(Component, Debug)]
struct PointLight2DVisual;

/// Marker on demo-spawned torches so an inspector tab can list and toggle them.
#[derive(Component, Debug)]
pub struct DemoTorch;

/// The custom material used by every light. Two fields share `#[uniform(0)]` so
/// `AsBindGroup` packs them into a single uniform struct (matching the WGSL).
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct PointLight2DMaterial {
    /// Linear-space tint applied to the falloff texture sample.
    #[uniform(0)]
    pub tint: LinearRgba,
    /// Brightness multiplier from [`PointLight2D::intensity`].
    #[uniform(0)]
    pub intensity: f32,
    /// Shared 128x128 RGBA radial falloff (white RGB, alpha rolls off to 0 at edge).
    #[texture(1)]
    #[sampler(2)]
    pub falloff: Handle<Image>,
}

impl Material2d for PointLight2DMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://inf2d_render/light.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        // Requesting `Blend` is what gets us into the transparent render phase
        // and assigns a non-`None` `BlendState` in the default pipeline. We
        // then overwrite that blend state in `specialize` to make it additive.
        AlphaMode2d::Blend
    }

    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Real additive blending of premultiplied source: the fragment shader
        // already outputs `rgb * a` so the source RGB carries its own alpha
        // weight — `One * src.rgb + One * dst.rgb` is the textbook "add light
        // to whatever's there" formula. Alpha channel is summed too so later
        // passes see an accumulated coverage value.
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Shared GPU assets every light reuses: one quad mesh, one falloff texture.
/// Material assets stay per-light so each light can carry its own tint/intensity
/// in its uniform block.
#[derive(Resource, Clone)]
pub struct LightAssets {
    /// Unit (1x1) quad centered on the origin; each light's child entity scales
    /// it by `Transform::with_scale(Vec3::splat(diameter))`.
    pub mesh: Handle<Mesh>,
    /// Procedurally-generated 128x128 radial falloff (white RGB, smooth alpha).
    pub falloff: Handle<Image>,
}

/// Plugin entry point — registers the material plugin, builds the falloff
/// texture at `Startup`, attaches child visuals during `Update`, and spawns a
/// few demo torches so the system is visible the first time you run the app.
pub struct PointLightsPlugin;

impl Plugin for PointLightsPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "light.wgsl");

        app.add_plugins(Material2dPlugin::<PointLight2DMaterial>::default())
            .register_type::<PointLight2D>()
            .add_systems(Startup, build_light_assets)
            .add_systems(Update, attach_light_visuals);
    }
}

/// Build the shared mesh + falloff texture and stash them in [`LightAssets`].
///
/// Run before [`spawn_demo_torches`] so the demo lights can read the resource;
/// run before [`attach_light_visuals`] so any user-spawned lights find their
/// visuals on the very first `Update` tick.
fn build_light_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
) {
    let mesh = meshes.add(Mesh::from(Rectangle::new(1.0, 1.0)));
    let falloff = images.add(build_falloff_image());
    commands.insert_resource(LightAssets { mesh, falloff });
}

/// Synthesize the radial falloff texture in CPU memory. Each pixel's alpha is
/// `smoothstep(1, 0, r)` squared, where `r` is the normalized distance from the
/// texture's center. RGB stays white; the per-light `tint` uniform colors the
/// light.
fn build_falloff_image() -> Image {
    let size = FALLOFF_SIZE;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let center = size as f32 * 0.5;
    let max_r = center;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let r = (dx * dx + dy * dy).sqrt() / max_r;
            // smoothstep(1, 0, r) == 1 - smoothstep(0, 1, r)
            let t = (1.0 - r).clamp(0.0, 1.0);
            let smooth = t * t * (3.0 - 2.0 * t);
            // Square the curve so the bright core is small and the falloff tail long.
            let a = (smooth * smooth * 255.0).round().clamp(0.0, 255.0) as u8;
            let off = ((y * size + x) * 4) as usize;
            buf[off] = 255;
            buf[off + 1] = 255;
            buf[off + 2] = 255;
            buf[off + 3] = a;
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        buf,
        // Use a linear (non-sRGB) format so the alpha curve we wrote above is
        // sampled verbatim — sRGB-encoded textures get gamma-applied to the
        // RGB channels and would lighten the white core unnecessarily.
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    // Linear filtering keeps the falloff smooth as the camera zooms in/out;
    // nearest-neighbor would reveal the 128px raster as chunky concentric rings.
    image.sampler = ImageSampler::linear();
    image
}

/// Walk every entity that has a [`PointLight2D`] but no child
/// [`PointLight2DVisual`] yet, and spawn that visual as a child.
///
/// Re-runs each frame so newly-spawned lights pick up their visuals automatically,
/// but is a no-op for already-attached lights (the marker check is `O(children)`).
pub fn attach_light_visuals(
    mut commands: Commands,
    assets: Option<Res<LightAssets>>,
    mut materials: ResMut<Assets<PointLight2DMaterial>>,
    lights: Query<(Entity, &PointLight2D, Option<&Children>)>,
    visuals: Query<&PointLight2DVisual>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, light, children) in &lights {
        let already_attached = children
            .map(|c| c.iter().any(|child| visuals.get(child).is_ok()))
            .unwrap_or(false);
        if already_attached {
            continue;
        }

        let material = materials.add(PointLight2DMaterial {
            tint: light.color.to_linear(),
            intensity: light.intensity,
            falloff: assets.falloff.clone(),
        });
        let diameter = light.radius * 2.0;

        commands.entity(entity).with_children(|parent| {
            parent.spawn((
                PointLight2DVisual,
                Mesh2d(assets.mesh.clone()),
                MeshMaterial2d(material),
                Transform::from_xyz(0.0, 0.0, LIGHTS_Z)
                    .with_scale(Vec3::new(diameter, diameter, 1.0)),
                Visibility::default(),
                Name::new("PointLight2DVisual"),
            ));
        });
    }
}

/// Spawn a handful of demo torches at known world coords. Not wired into
/// `PointLightsPlugin` — call manually from app code if you want a visible
/// lighting test before real props exist.
#[allow(dead_code)]
fn spawn_demo_torches(mut commands: Commands) {
    let torches: [(WorldTile, Color, f32, f32, &str); 5] = [
        (
            WorldTile::new(0, 0),
            Color::srgb(1.0, 0.78, 0.45),
            2.0,
            220.0,
            "DemoTorch:Warm",
        ),
        (
            WorldTile::new(5, 5),
            Color::srgb(1.0, 0.55, 0.20),
            3.5,
            300.0,
            "DemoTorch:Bonfire",
        ),
        (
            WorldTile::new(-3, 4),
            Color::srgb(0.35, 0.55, 1.0),
            1.8,
            240.0,
            "DemoTorch:CoolMagic",
        ),
        (
            WorldTile::new(4, -2),
            Color::srgb(0.45, 1.0, 0.55),
            1.6,
            220.0,
            "DemoTorch:GreenPoison",
        ),
        (
            WorldTile::new(-5, -5),
            Color::srgb(0.95, 0.30, 0.85),
            2.2,
            260.0,
            "DemoTorch:ArcanePink",
        ),
    ];

    for (tile, color, intensity, radius, name) in torches {
        let pos = tile_to_world(tile);
        commands.spawn((
            DemoTorch,
            PointLight2D {
                color,
                intensity,
                radius,
            },
            Transform::from_xyz(pos.x, pos.y, 0.0),
            Visibility::default(),
            Name::new(name),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_torch_is_warm_and_bright() {
        let torch = PointLight2D::default();
        assert!(torch.intensity > 1.0);
        assert!(torch.radius > 0.0);
        // Warm tint: red >= green > blue.
        let srgba = torch.color.to_srgba();
        assert!(srgba.red >= srgba.green);
        assert!(srgba.green > srgba.blue);
    }

    #[test]
    fn falloff_image_is_correct_size() {
        let img = build_falloff_image();
        let ext = img.texture_descriptor.size;
        assert_eq!(ext.width, FALLOFF_SIZE);
        assert_eq!(ext.height, FALLOFF_SIZE);
        assert_eq!(ext.depth_or_array_layers, 1);
        assert_eq!(img.texture_descriptor.format, TextureFormat::Rgba8Unorm);
    }

    #[test]
    fn falloff_center_is_bright_and_edges_are_transparent() {
        let img = build_falloff_image();
        let data = img.data.as_ref().expect("falloff has cpu data");
        let stride = FALLOFF_SIZE * 4;
        let cx = FALLOFF_SIZE / 2;
        let cy = FALLOFF_SIZE / 2;
        let center_alpha = data[(cy * stride + cx * 4 + 3) as usize];
        let corner_alpha = data[3];
        assert!(center_alpha > 240, "center should be near-opaque, got {center_alpha}");
        assert_eq!(corner_alpha, 0, "corners should be fully transparent");
    }
}
