//! Generic vertical sprite stack — N child quads layered with 1px Y offsets
//! to produce a fake-3D voxel-cube look from a single 2D rendering setup.
//!
//! Add a [`SpriteStack`] component to any entity (alongside Transform / Visibility).
//! [`spawn_stack_children`] picks up entities that don't yet have a stack child
//! tree and spawns one. The child sprites are pure children of the marked entity —
//! they inherit the parent's Transform, so the parent moves them as one.

use bevy::prelude::*;

/// Configuration for a Brigador-style vertical sprite stack. Attach to any
/// entity that already has a `Transform` + `Visibility`; the plugin spawns the
/// slice children on the next `Update`.
#[derive(Component, Reflect, Debug, Clone)]
#[reflect(Component)]
pub struct SpriteStack {
    /// Number of slices. 8–16 is the sweet spot for "small character / prop";
    /// 24+ reads as a tower.
    pub slices: u32,
    /// Sprite dimensions per slice, in world units.
    pub slice_size: Vec2,
    /// Vertical separation between slice centers, in world units. Usually `1.0`
    /// for pixel-art (1px per slice).
    pub slice_spacing: f32,
    /// Color at the **bottom** slice. Gradient is interpolated from bottom → top.
    pub base_color: Color,
    /// Color at the **top** slice.
    pub top_color: Color,
}

impl Default for SpriteStack {
    fn default() -> Self {
        Self {
            slices: 12,
            slice_size: Vec2::new(18.0, 10.0),
            slice_spacing: 1.0,
            base_color: Color::srgb(0.4, 0.05, 0.02),
            top_color: Color::srgb(1.0, 0.42, 0.28),
        }
    }
}

/// Marker on a slice child so [`spawn_stack_children`] can detect "already
/// populated" stacks and skip them (idempotent for hot-reloaded inspector
/// edits, future component additions, etc.).
#[derive(Component, Debug)]
pub struct SpriteStackSlice;

/// `Update` system: for every entity carrying [`SpriteStack`] that has not
/// yet been populated, spawn the slice children with a base→top color
/// gradient and a small per-slice Z-step so depth-sorting keeps the stack
/// coherent.
pub fn spawn_stack_children(
    mut commands: Commands,
    stacks: Query<(Entity, &SpriteStack, Option<&Children>)>,
    existing_slices: Query<&SpriteStackSlice>,
) {
    for (entity, stack, children) in &stacks {
        let already = children
            .map(|c| c.iter().any(|child| existing_slices.get(child).is_ok()))
            .unwrap_or(false);
        if already {
            continue;
        }

        for i in 0..stack.slices {
            let t = if stack.slices > 1 {
                i as f32 / (stack.slices - 1) as f32
            } else {
                0.0
            };
            let rgb = lerp_color(stack.base_color, stack.top_color, t);
            let slice = commands
                .spawn((
                    SpriteStackSlice,
                    Sprite {
                        color: rgb,
                        custom_size: Some(stack.slice_size),
                        ..default()
                    },
                    Transform::from_xyz(
                        0.0,
                        i as f32 * stack.slice_spacing,
                        i as f32 * 0.001,
                    ),
                    Visibility::default(),
                    Name::new(format!("Stack({i})")),
                ))
                .id();
            commands.entity(entity).add_child(slice);
        }
    }
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let la = a.to_linear();
    let lb = b.to_linear();
    Color::linear_rgba(
        la.red + (lb.red - la.red) * t,
        la.green + (lb.green - la.green) * t,
        la.blue + (lb.blue - la.blue) * t,
        la.alpha + (lb.alpha - la.alpha) * t,
    )
}

/// Plugin: registers [`SpriteStack`] for reflection and schedules
/// [`spawn_stack_children`] on `Update`.
pub struct SpriteStackPlugin;

impl Plugin for SpriteStackPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<SpriteStack>()
            .add_systems(Update, spawn_stack_children);
    }
}
