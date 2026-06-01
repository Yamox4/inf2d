//! Z-layer constants used when placing chunk tilemaps, decals, entities, and UI sprites.
//!
//! Bevy 2D sorts by translation `z`; these constants keep ordering decisions in one
//! place so the renderer, future decal/entity sprite systems, and any debug overlays
//! all agree on layering.

/// Standard render-layer Z offsets, applied to a sprite/tilemap's local `Transform.z`
/// so it composites correctly against other things in the same chunk.
pub struct RenderLayer;

impl RenderLayer {
    /// Ground tilemap — the bottom-most layer; the chunk's diamond grid lives here.
    pub const GROUND: f32 = 0.0;
    /// Legacy flat water z-slice. **No longer used to position water shader quads.**
    ///
    /// `crate::water::spawn_water_quads` now spawns one merged shader quad per
    /// chunk at `z = -0.005`, sandwiched between the recessed water tilemap at
    /// `z = -0.01` and the ground plane at `z = 0.0`. A single flat `0.5` z
    /// would render every water quad above every elevated terrain — stone at
    /// height 5 sits at z ≈ 0.05, so a flat-z water quad would occlude it.
    /// The constant is retained for backward compatibility with any external
    /// code that still imports it.
    pub const WATER: f32 = 0.5;
    /// Decals painted on top of ground (cracks, footprints, blood, debug overlays).
    pub const DECAL: f32 = 1.0;
    /// Drop shadows cast under iso-anchored entities. Sits above decals so a tree's
    /// shadow occludes a footprint, but below [`ENTITY`](Self::ENTITY) so the
    /// sprite body always composites on top of its own shadow.
    pub const SHADOW: f32 = 1.5;
    /// Gameplay entities (units, props, items). Sorted further per-entity by world Y
    /// elsewhere, but this is their base layer.
    pub const ENTITY: f32 = 2.0;
    /// Fullscreen day/night color-grading overlay. Sits above gameplay sprites
    /// but below world-space UI so HUD markers remain unaffected by the tint.
    pub const DAYNIGHT: f32 = 5.0;
    /// Fullscreen LUT-based post-process tint. Sits above [`DAYNIGHT`](Self::DAYNIGHT)
    /// so the LUT colorization composites on top of the day/night flat overlay,
    /// and below [`UI`](Self::UI) so HUD elements stay untinted.
    pub const POSTFX: f32 = 7.0;
    /// World-space UI markers (selection rings, health bars) that should sit above
    /// all in-world geometry but below the screen-space UI.
    pub const UI: f32 = 10.0;
}
