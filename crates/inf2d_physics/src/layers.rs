use avian2d::prelude::*;

/// Project-wide collision categories used to drive Avian's [`CollisionLayers`].
///
/// Each variant maps to one bit in the underlying `LayerMask`. `Default` reserves
/// bit 0 (Avian uses it as the implicit layer for entities without explicit
/// `CollisionLayers`); the remaining variants name the actual gameplay categories
/// the collision matrix is built from:
///
/// - `Terrain`   — static tile colliders (water, stone, cliffs).
/// - `Player`    — the single player-controlled kinematic body.
/// - `Mob`       — AI-driven entities (mobs, NPCs).
/// - `Projectile`— short-lived entities spawned by attacks / abilities.
///
/// Renamed from `PhysicsLayer` to `GameLayer` so it doesn't collide with the
/// `PhysicsLayer` *trait* re-exported by `avian2d::prelude` (the derive macro is
/// what we want to invoke; the trait shares its name).
///
/// New variants must be appended at the end so existing `to_bits()` values stay
/// stable across releases; reordering would silently invalidate any persisted
/// collision masks.
#[derive(PhysicsLayer, Default, Clone, Copy, Debug)]
pub enum GameLayer {
    #[default]
    Default,
    Terrain,
    Player,
    Mob,
    Projectile,
}
