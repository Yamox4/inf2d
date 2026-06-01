#![deny(unsafe_code)]
//! Health / damage primitives shared by player and mobs.

use bevy::prelude::*;

/// Per-entity HP component. `current` is decremented by [`apply_damage`] as
/// [`DamageEvent`] messages are consumed; `max` is the entity's authored cap.
/// Use [`Health::full`] to construct a freshly-spawned entity at full HP.
#[derive(Component, Reflect, Debug, Clone, Copy)]
#[reflect(Component)]
pub struct Health {
    pub current: f32,
    pub max: f32,
}

impl Health {
    /// Construct a [`Health`] at full HP: `current == max == max`.
    pub fn full(max: f32) -> Self {
        Self { current: max, max }
    }

    /// HP fraction in `[0, 1]`. Saturates on both ends so transient over- /
    /// under-fills (e.g. lethal damage in a single hit) still produce a sane
    /// value for HP bars.
    pub fn fraction(&self) -> f32 {
        (self.current / self.max).clamp(0.0, 1.0)
    }

    /// `true` once `current <= 0`.
    pub fn is_dead(&self) -> bool {
        self.current <= 0.0
    }
}

/// Message: deliver `amount` damage to `victim`. Spawned by attacks; consumed
/// by [`apply_damage`] which decrements `Health` and emits [`DeathEvent`] on
/// lethal.
#[derive(Message, Debug, Clone, Copy)]
pub struct DamageEvent {
    pub victim: Entity,
    pub amount: f32,
}

/// Message: fired when a [`DamageEvent`] drops `victim`'s HP to zero or below.
/// Consumed by [`despawn_dead`] in this plugin, but also available for other
/// listeners (loot drops, kill counters, ...).
#[derive(Message, Debug, Clone, Copy)]
pub struct DeathEvent {
    pub victim: Entity,
}

/// Consume [`DamageEvent`]s, subtract from each victim's [`Health`], and emit
/// a [`DeathEvent`] when a hit pushes HP to zero. Already-dead entities are
/// skipped so a single over-kill frame can't produce duplicate deaths.
pub fn apply_damage(
    mut events: MessageReader<DamageEvent>,
    mut healths: Query<&mut Health>,
    mut deaths: MessageWriter<DeathEvent>,
) {
    for ev in events.read() {
        let Ok(mut hp) = healths.get_mut(ev.victim) else {
            continue;
        };
        if hp.is_dead() {
            continue;
        }
        hp.current = (hp.current - ev.amount).max(0.0);
        if hp.is_dead() {
            deaths.write(DeathEvent { victim: ev.victim });
        }
    }
}

/// Despawn every entity that died this frame. `try_despawn` swallows the case
/// where the entity was already removed by another system (e.g. a parent
/// despawn that cascaded through the child relationship).
pub fn despawn_dead(
    mut commands: Commands,
    mut deaths: MessageReader<DeathEvent>,
) {
    for ev in deaths.read() {
        commands.entity(ev.victim).try_despawn();
    }
}

/// Plugin: registers [`Health`] for reflection, declares the [`DamageEvent`] /
/// [`DeathEvent`] message channels, and schedules [`apply_damage`] followed by
/// [`despawn_dead`] in `Update`.
pub struct StatsPlugin;

impl Plugin for StatsPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Health>()
            .add_message::<DamageEvent>()
            .add_message::<DeathEvent>()
            .add_systems(Update, (apply_damage, despawn_dead).chain());
    }
}
