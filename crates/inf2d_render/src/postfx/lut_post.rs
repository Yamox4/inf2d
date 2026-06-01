#![deny(unsafe_code)]
//! Real read-from-scene 3D-LUT color grading post-process.
//!
//! This module replaces the historical "fullscreen LUT quad" path. Instead of
//! laying a translucent material on top of the rendered scene it registers a
//! [`bevy::render::render_graph::ViewNode`] in the 2D core pipeline. The node
//! runs after [`Node2d::Tonemapping`] and before [`Node2d::EndMainPassPostProcessing`],
//! samples the previous post-process color target through
//! [`ViewTarget::post_process_write`], applies the LUT lookup, and writes
//! back. This is the standard Bevy 0.18 post-process pattern; the canonical
//! reference is `examples/shader_advanced/custom_post_processing.rs`.
//!
//! # Layout
//!
//! - Per-camera [`LutSettings`] component holds `blend` + `strength`. Driven
//!   from [`TimeOfDay`] in the main world, extracted + uploaded as a UBO by
//!   [`ExtractComponentPlugin`] + [`UniformComponentPlugin`].
//! - Resource [`LutDriver`] holds the two `Handle<Image>`s for the active LUT
//!   pair. It is also driven from `TimeOfDay` in the main world and extracted
//!   via [`ExtractResourcePlugin`].
//! - In the render world, the `ViewNode` looks the handles up in
//!   [`RenderAssets<GpuImage>`], creates the bind group on the fly (mandatory
//!   for the ping-pong `post_process_write`), and dispatches a fullscreen
//!   triangle through [`FullscreenShader`].
//!
//! # Why post-tonemap
//!
//! The procedural LUTs in [`super::lut`] are defined over `[0, 1]^3` and
//! clamp inputs. Sampling them in HDR (pre-tonemap) space would crush highs
//! to the LUT's `b = 1.0` slice. Running after [`Node2d::Tonemapping`] keeps
//! the LUT's character intact and matches the way film LUTs are authored.

use bevy::asset::{embedded_asset, load_embedded_asset, AssetServer};
use bevy::core_pipeline::{
    core_2d::graph::{Core2d, Node2d},
    FullscreenShader,
};
use bevy::ecs::query::QueryItem;
use bevy::image::BevyDefault as _;
use bevy::prelude::*;
use bevy::render::{
    extract_component::{
        ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
        UniformComponentPlugin,
    },
    extract_resource::{ExtractResource, ExtractResourcePlugin},
    render_asset::RenderAssets,
    render_graph::{
        NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
    },
    render_resource::{
        binding_types::{sampler, texture_2d, uniform_buffer},
        BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
        CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, Operations,
        PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor,
        Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages, ShaderType,
        SpecializedRenderPipeline, SpecializedRenderPipelines, TextureFormat, TextureSampleType,
    },
    renderer::{RenderContext, RenderDevice},
    texture::GpuImage,
    view::{ExtractedView, ViewTarget},
    Render, RenderApp, RenderStartup, RenderSystems,
};
use bevy::shader::Shader;

use crate::daynight::TimeOfDay;

use super::lut::{select_lut_pair, strength_for_hour, LutPalette};

/// Per-camera component carrying the two scalars uploaded to the shader UBO.
///
/// The plugin auto-attaches a `disabled()` instance to every `Camera2d` via
/// [`bevy::app::App::register_required_components_with`], so user code never
/// needs to insert it manually. Setting `strength` to `0.0` cleanly bypasses
/// the effect on a per-camera basis (the shader still runs — the per-frame
/// fixed cost is one fullscreen triangle — but the output is the un-graded
/// source color).
#[derive(Component, ExtractComponent, ShaderType, Clone, Copy, Debug)]
pub struct LutSettings {
    /// Cross-fade between LUT A (0.0) and LUT B (1.0).
    pub blend: f32,
    /// Overall pass strength. 0 = fully bypass, 1 = fully graded.
    pub strength: f32,
    /// First padding slot — keeps the UBO at the 16-byte alignment WebGL2 requires.
    pub _pad0: f32,
    /// Second padding slot — keeps the UBO at the 16-byte alignment WebGL2 requires.
    pub _pad1: f32,
}

impl LutSettings {
    /// Settings that act as an explicit no-op: zero strength, neutral blend.
    pub fn disabled() -> Self {
        Self {
            blend: 0.0,
            strength: 0.0,
            _pad0: 0.0,
            _pad1: 0.0,
        }
    }
}

/// Render-world view of the active LUT pair handles. The main world rewrites
/// this from [`TimeOfDay`] every frame; [`ExtractResourcePlugin`] copies it
/// across into the render world for the [`LutPostProcessNode`] to read.
#[derive(Resource, ExtractResource, Clone)]
pub struct LutDriver {
    /// First LUT in the active pair. Sampled when `LutSettings::blend = 0`.
    pub lut_a: Handle<Image>,
    /// Second LUT in the active pair. Sampled when `LutSettings::blend = 1`.
    pub lut_b: Handle<Image>,
}

/// Render-graph label for the LUT post-process node.
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct LutPostProcessLabel;

/// Plugin: registers the render-graph node, the extract/uniform plumbing for
/// [`LutSettings`], and the main-world driver system that translates
/// [`TimeOfDay`] into uniform values + active LUT pair.
pub struct LutPostProcessPlugin;

impl Plugin for LutPostProcessPlugin {
    fn build(&self, app: &mut App) {
        // Embed the fragment shader at compile time — same trick the rest of
        // the crate uses for material shaders.
        embedded_asset!(app, "lut_post.wgsl");

        app.add_plugins((
            ExtractComponentPlugin::<LutSettings>::default(),
            UniformComponentPlugin::<LutSettings>::default(),
            ExtractResourcePlugin::<LutDriver>::default(),
        ))
        // Auto-attach `LutSettings` to every `Camera2d` (same pattern
        // `Core2dPlugin` uses for `Tonemapping`). Spawning a fresh camera mid-
        // game still gets graded without a separate attach system.
        .register_required_components_with::<Camera2d, LutSettings>(LutSettings::disabled)
        // Build the procedural LUT palette + seed the driver on Startup; the
        // `Update` driver then keeps both the per-camera UBO and the driver
        // resource current from `TimeOfDay`. `backfill_settings` covers
        // cameras spawned before this plugin was added (rare but valid).
        .add_systems(Startup, (super::build_palette, seed_driver).chain())
        .add_systems(Update, (backfill_settings, drive_from_time_of_day).chain());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .init_resource::<SpecializedRenderPipelines<LutPostProcessPipeline>>()
            .add_systems(RenderStartup, init_post_process_pipeline)
            .add_systems(
                Render,
                prepare_lut_post_process_pipelines.in_set(RenderSystems::Prepare),
            )
            .add_render_graph_node::<ViewNodeRunner<LutPostProcessNode>>(
                Core2d,
                LutPostProcessLabel,
            )
            .add_render_graph_edges(
                Core2d,
                (
                    Node2d::Tonemapping,
                    LutPostProcessLabel,
                    Node2d::EndMainPassPostProcessing,
                ),
            );
    }
}

/// Belt-and-braces Update system: register-required-components only fires
/// when `Camera2d` is *inserted*. Cameras that already existed before the
/// plugin was added would otherwise miss the settings; this picks them up
/// at the next Update tick. Almost always a no-op.
fn backfill_settings(
    mut commands: Commands,
    cameras: Query<Entity, (With<Camera2d>, Without<LutSettings>)>,
) {
    for entity in &cameras {
        commands.entity(entity).insert(LutSettings::disabled());
    }
}

/// Startup system: pre-populate [`LutDriver`] from the freshly-built
/// [`LutPalette`] so the very first extracted frame has valid handles.
fn seed_driver(mut commands: Commands, palette: Option<Res<LutPalette>>) {
    let Some(palette) = palette else {
        // `build_palette` failed (extremely unlikely — it just builds CPU
        // images and inserts the resource). Without LUTs there is nothing to
        // drive; the node short-circuits below.
        return;
    };
    commands.insert_resource(LutDriver {
        lut_a: palette.neutral.clone(),
        lut_b: palette.neutral.clone(),
    });
}

/// Per-frame driver: read [`TimeOfDay`], pick the active LUT pair and
/// strength, and stamp both the per-camera UBO settings and the global
/// driver resource. Both then ride [`ExtractComponentPlugin`] /
/// [`ExtractResourcePlugin`] across to the render world automatically.
fn drive_from_time_of_day(
    tod: Res<TimeOfDay>,
    palette: Option<Res<LutPalette>>,
    driver: Option<ResMut<LutDriver>>,
    mut cameras: Query<&mut LutSettings>,
) {
    let Some(palette) = palette else {
        return;
    };
    let (lut_a, lut_b, blend) = select_lut_pair(tod.hours, &palette);
    let strength = strength_for_hour(tod.hours);

    if let Some(mut driver) = driver {
        driver.lut_a = lut_a;
        driver.lut_b = lut_b;
    }

    for mut settings in &mut cameras {
        settings.blend = blend;
        settings.strength = strength;
    }
}

/// View-scoped render-graph node that performs the LUT lookup.
#[derive(Default)]
struct LutPostProcessNode;

impl ViewNode for LutPostProcessNode {
    type ViewQuery = (
        &'static ViewTarget,
        &'static LutSettings,
        &'static DynamicUniformIndex<LutSettings>,
        &'static LutPostProcessPipelineId,
    );

    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        (view_target, settings, settings_index, pipeline_id): QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        // Zero strength is the explicit "bypass" signal. Skip the entire pass
        // — including the bind-group build — for the noon-to-afternoon window
        // when `strength_for_hour` returns 0.0.
        if settings.strength <= 0.0 {
            return Ok(());
        }

        let pipeline_res = world.resource::<LutPostProcessPipeline>();
        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_id.0) else {
            return Ok(());
        };

        let settings_uniforms = world.resource::<ComponentUniforms<LutSettings>>();
        let Some(settings_binding) = settings_uniforms.uniforms().binding() else {
            return Ok(());
        };

        // The driver resource may not have been seeded yet on the very first
        // frame (palette build runs at Startup, driver write follows). Skip
        // cleanly if so — the un-graded scene already lives in the source.
        let Some(driver) = world.get_resource::<LutDriver>() else {
            return Ok(());
        };

        let images = world.resource::<RenderAssets<GpuImage>>();
        let (Some(lut_a), Some(lut_b)) = (images.get(&driver.lut_a), images.get(&driver.lut_b))
        else {
            // GPU upload hasn't completed yet (one-frame race on the first
            // tick). The source already holds the un-graded scene; do nothing.
            return Ok(());
        };

        let post_process = view_target.post_process_write();

        let bind_group = render_context.render_device().create_bind_group(
            Some("lut_post_process_bind_group"),
            &pipeline_cache.get_bind_group_layout(&pipeline_res.layout),
            &BindGroupEntries::sequential((
                post_process.source,
                &pipeline_res.scene_sampler,
                &lut_a.texture_view,
                &pipeline_res.lut_sampler,
                &lut_b.texture_view,
                &pipeline_res.lut_sampler,
                settings_binding.clone(),
            )),
        );

        let mut render_pass = render_context.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("lut_post_process_pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: post_process.destination,
                depth_slice: None,
                resolve_target: None,
                ops: Operations::default(),
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_render_pipeline(pipeline);
        render_pass.set_bind_group(0, &bind_group, &[settings_index.index()]);
        // Fullscreen triangle baked into `FullscreenShader`.
        render_pass.draw(0..3, 0..1);

        Ok(())
    }
}

/// Render-world resource carrying the static pipeline scaffold (bind-group
/// layout descriptor, fragment + vertex shader handles, samplers). The actual
/// `CachedRenderPipelineId` is per-view and lives on
/// [`LutPostProcessPipelineId`] because the color target format depends on
/// the view's HDR flag.
#[derive(Resource)]
struct LutPostProcessPipeline {
    layout: BindGroupLayoutDescriptor,
    scene_sampler: Sampler,
    lut_sampler: Sampler,
    fullscreen_shader: FullscreenShader,
    fragment_shader: Handle<Shader>,
}

/// Specialization key: just the HDR flag of the target view.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
struct LutPostProcessPipelineKey {
    hdr: bool,
}

impl SpecializedRenderPipeline for LutPostProcessPipeline {
    type Key = LutPostProcessPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("lut_post_process_pipeline".into()),
            layout: vec![self.layout.clone()],
            push_constant_ranges: vec![],
            vertex: self.fullscreen_shader.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: self.fragment_shader.clone(),
                shader_defs: vec![],
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState {
                    // HDR cameras keep the `Rgba16Float` ping-pong target
                    // even after tonemapping; non-HDR collapses to the swap-
                    // target's `bevy_default()` format.
                    format: if key.hdr {
                        ViewTarget::TEXTURE_FORMAT_HDR
                    } else {
                        TextureFormat::bevy_default()
                    },
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            zero_initialize_workgroup_memory: false,
        }
    }
}

/// Per-view cached pipeline id, written by [`prepare_lut_post_process_pipelines`]
/// during `RenderSystems::Prepare`.
#[derive(Component)]
struct LutPostProcessPipelineId(CachedRenderPipelineId);

/// `RenderStartup` system: build the bind-group layout, samplers, and store
/// the shared shader handles in a [`LutPostProcessPipeline`] resource.
fn init_post_process_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    asset_server: Res<AssetServer>,
    fullscreen_shader: Res<FullscreenShader>,
) {
    let layout_entries = BindGroupLayoutEntries::sequential(
        ShaderStages::FRAGMENT,
        (
            // Scene color target (read-from-scene input).
            texture_2d(TextureSampleType::Float { filterable: true }),
            sampler(SamplerBindingType::Filtering),
            // LUT A.
            texture_2d(TextureSampleType::Float { filterable: true }),
            sampler(SamplerBindingType::Filtering),
            // LUT B.
            texture_2d(TextureSampleType::Float { filterable: true }),
            sampler(SamplerBindingType::Filtering),
            // Settings UBO. `true` = dynamic offset (one entry per view).
            uniform_buffer::<LutSettings>(true),
        ),
    );
    let layout =
        BindGroupLayoutDescriptor::new("lut_post_process_bind_group_layout", &layout_entries);

    let scene_sampler = render_device.create_sampler(&SamplerDescriptor::default());
    let lut_sampler = render_device.create_sampler(&SamplerDescriptor::default());

    let fragment_shader: Handle<Shader> =
        load_embedded_asset!(asset_server.as_ref(), "lut_post.wgsl");

    commands.insert_resource(LutPostProcessPipeline {
        layout,
        scene_sampler,
        lut_sampler,
        fullscreen_shader: fullscreen_shader.clone(),
        fragment_shader,
    });
}

/// `Render`/`Prepare` system: for every view with [`LutSettings`], specialize
/// the post-process pipeline for that view's HDR state and stash the result
/// as a [`LutPostProcessPipelineId`] component for the node to pick up.
fn prepare_lut_post_process_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<LutPostProcessPipeline>>,
    pipeline: Res<LutPostProcessPipeline>,
    views: Query<(Entity, &ExtractedView), With<LutSettings>>,
) {
    for (entity, view) in &views {
        let pipeline_id = pipelines.specialize(
            &pipeline_cache,
            &pipeline,
            LutPostProcessPipelineKey { hdr: view.hdr },
        );
        commands
            .entity(entity)
            .insert(LutPostProcessPipelineId(pipeline_id));
    }
}
