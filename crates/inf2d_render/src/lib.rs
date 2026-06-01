#![deny(unsafe_code)]
//! Renders chunks as `bevy_ecs_tilemap` isometric tilemaps. Listens to `ChunkLoaded` /
//! `ChunkUnloaded` events from `inf2d_world` and manages a one-tilemap-per-chunk hierarchy.
//!
//! ## Wiring
//!
//! Add [`RenderPlugin`] after `inf2d_core::CorePlugin` and `inf2d_world::WorldPlugin`:
//!
//! ```ignore
//! app.add_plugins((CorePlugin, WorldPlugin, RenderPlugin));
//! ```
//!
//! ## Hierarchy
//!
//! ```text
//! Chunk entity (Transform at chunk_origin_world)
//! └── ChunkTilemap entity (Transform at (0, 0, GROUND))
//!     ├── Tile entity (TilePos { x: 0, y: 0 }, ...)
//!     ├── Tile entity (TilePos { x: 1, y: 0 }, ...)
//!     └── ... CHUNK_SIZE * CHUNK_SIZE tile entities
//! ```
//!
//! On `ChunkUnloaded`, the world streamer despawns the chunk entity; Bevy's `ChildOf`
//! relationship cascades the despawn through the tilemap and every tile entity, so no
//! manual GPU cleanup is required.

mod atlas;
mod cliffs;
mod daynight;
mod hlod;
mod hover;
mod layers;
mod lights;
mod lit_tile_material;
mod particles;
mod picking;
mod postfx;
mod shadows;
mod sprite_stack;
mod tilemap;
mod water;

use bevy::prelude::*;
use bevy_ecs_tilemap::TilemapPlugin;
use inf2d_camera::update_cursor_pick;
use inf2d_core::{CoreSet, RenderPrepSet};

pub use atlas::{build_tile_atlas_image, build_tile_normal_atlas_image, TileAtlas, BASE_COLOR};
pub use cliffs::{
    cliff_color, release_cliff_keys_on_unload, spawn_chunk_cliffs, ChunkCliff,
    ChunkCliffAssets, ChunkCliffMaterial, CliffFace, CliffSide, CliffsPlugin,
};
pub use daynight::{sun_strength_for_hour, DayNightConfig, DayNightOverlay, TimeOfDay};
pub use hlod::{bake_chunk_imposter_image, HlodBakeCache, HlodImposter, HlodPlugin};
pub use hover::{configure_hover_gizmo, draw_hover_highlight};
pub use layers::RenderLayer;
pub use lights::{DemoTorch, LightAssets, PointLight2D, PointLight2DMaterial, PointLightsPlugin};
pub use lit_tile_material::{
    LightingPlugin, LightingUniforms, LitTileMaterialHandle, LitTilemapMaterial, PackedLight,
    MAX_TILE_LIGHTS,
};
pub use particles::{
    Emitter, EmitterPreset, EmitterShape, Particle, ParticleAssets, ParticlesPlugin,
};
pub use picking::{EntityPick, EntityPickingPlugin};
pub use postfx::godrays::{GodRaysMaterial, GodRaysPlugin};
pub use postfx::heat::{HeatMaterial, HeatPlugin};
pub use postfx::vignette::{VignetteMaterial, VignettePlugin};
pub use postfx::{LutDriver, LutPalette, LutPostProcessPlugin, LutSettings};
pub use shadows::{DropShadow, IsoAnchor, ShadowAssets, ShadowsPlugin};
pub use sprite_stack::{SpriteStack, SpriteStackPlugin, SpriteStackSlice};
pub use tilemap::ChunkTilemap;
pub use water::{WaterAssets, WaterMaterial, WaterPlugin, WaterTileQuad};

/// Plugin: registers `TilemapPlugin`, builds the procedural atlas on `Startup`, and runs
/// the per-chunk spawn/despawn systems in `RenderPrepSet` so they always observe the final
/// per-frame simulation state.
pub struct RenderPlugin;

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TimeOfDay>()
            .init_resource::<DayNightConfig>()
            .register_type::<TimeOfDay>()
            .register_type::<DayNightConfig>();

        app.add_plugins(TilemapPlugin)
            .add_plugins(lit_tile_material::LightingPlugin)
            .add_plugins(lights::PointLightsPlugin)
            .add_plugins(particles::ParticlesPlugin)
            .add_plugins(water::WaterPlugin)
            .add_plugins(cliffs::CliffsPlugin)
            .add_plugins(shadows::ShadowsPlugin)
            .add_plugins(sprite_stack::SpriteStackPlugin)
            .add_plugins(picking::EntityPickingPlugin)
            .add_plugins(hlod::HlodPlugin)
            .add_plugins(postfx::LutPostProcessPlugin)
            .add_plugins(postfx::godrays::GodRaysPlugin)
            .add_plugins(postfx::heat::HeatPlugin)
            .add_plugins(postfx::vignette::VignettePlugin)
            .add_systems(Startup, atlas::setup_tile_atlas)
            .add_systems(Startup, daynight::spawn_overlay)
            .add_systems(Startup, hover::configure_hover_gizmo)
            .add_systems(
                Update,
                (tilemap::spawn_chunk_tilemap, tilemap::despawn_chunk_tilemap)
                    .in_set(RenderPrepSet),
            )
            // Chain `after(update_cursor_pick)` so the hover system always sees
            // the freshly-resolved `CursorPick` for the current frame, not last
            // frame's value. Both systems live in `RenderPrepSet`; the explicit
            // ordering closes the race.
            .add_systems(
                Update,
                hover::draw_hover_highlight
                    .in_set(RenderPrepSet)
                    .after(update_cursor_pick),
            )
            .add_systems(Update, daynight::advance_and_tint.in_set(CoreSet));
    }
}
