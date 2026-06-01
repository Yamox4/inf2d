#![deny(unsafe_code)]
//! Audio plugin: master volume mixer, SFX cue trigger, music start/stop.
//!
//! Slice 1 has no audio assets shipped. The plugin is silent-but-correct: it
//! reserves the audio sink components, exposes the cue/music API, and is ready
//! to receive sound assets once they exist. Volumes are persistent via
//! [`MasterVolumes`] (serde-derived).
//!
//! ### Bevy 0.18 audio anchors
//!
//! - [`bevy::audio::AudioPlayer`] is a generic tuple struct
//!   `AudioPlayer<Source = AudioSource>(pub Handle<Source>)`. The non-generic
//!   default takes a `Handle<AudioSource>`.
//! - [`bevy::audio::PlaybackSettings`] has fields `mode`, `volume`, `speed`,
//!   `paused`, `muted`, `spatial`, `spatial_scale`, `start_position`,
//!   `duration`.
//! - [`bevy::audio::Volume`] has variants `Linear(f32)` and `Decibels(f32)`.
//! - [`bevy::audio::GlobalVolume::new`] takes a `Volume`, *not* an `f32`.

use bevy::audio::{
    AudioPlayer, AudioPlugin as BevyAudioPlugin, AudioSource, GlobalVolume, PlaybackMode,
    PlaybackSettings, Volume,
};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Persistent mixer state. Each channel is a `0.0..=1.0` linear multiplier.
///
/// The `master` value is mirrored into Bevy's [`GlobalVolume`] resource every
/// time it changes, so any audio entity in the world reflects it. `sfx` and
/// `music` are applied at spawn time by the [`handle_play_sfx`] and
/// [`handle_play_music`] systems respectively — already-playing sinks do
/// **not** retroactively pick up a new channel volume.
#[derive(Resource, Reflect, Debug, Clone, Copy, Serialize, Deserialize)]
#[reflect(Resource)]
pub struct MasterVolumes {
    /// Master multiplier on everything. `0.0..=1.0`.
    pub master: f32,
    /// SFX channel multiplier. `0.0..=1.0`.
    pub sfx: f32,
    /// Music channel multiplier. `0.0..=1.0`.
    pub music: f32,
}

impl Default for MasterVolumes {
    fn default() -> Self {
        Self {
            master: 0.8,
            sfx: 1.0,
            music: 0.7,
        }
    }
}

/// Fire-and-forget SFX cue. Spawns a despawn-on-finish audio entity.
#[derive(Message, Debug, Clone)]
pub struct PlaySfx {
    /// The audio source asset to play.
    pub handle: Handle<AudioSource>,
    /// Linear per-cue volume scalar (combined with [`MasterVolumes::sfx`]).
    pub volume: f32,
    /// Playback rate: `1.0` = normal, `0.5` = half-speed (pitch shift).
    pub speed: f32,
}

/// Start playing a music track, replacing any currently-playing one.
///
/// The `fade_in_secs` field is reserved for a future crossfade implementation;
/// today the swap is instantaneous.
#[derive(Message, Debug, Clone)]
pub struct PlayMusic {
    /// The looping music source to play.
    pub handle: Handle<AudioSource>,
    /// Crossfade-in duration in seconds. `0.0` = instant cut (currently the
    /// only behavior; non-zero values are accepted but ignored).
    pub fade_in_secs: f32,
}

/// Stop the currently-playing music track.
#[derive(Message, Debug, Clone)]
pub struct StopMusic {
    /// Crossfade-out duration in seconds. `0.0` = instant cut (currently the
    /// only behavior; non-zero values are accepted but ignored).
    pub fade_out_secs: f32,
}

/// Marker on the currently-playing music entity so it can be stopped / replaced.
#[derive(Component, Debug)]
pub struct CurrentMusic;

/// Audio plugin: registers messages + mixer resource, ensures Bevy's
/// [`BevyAudioPlugin`] is installed, and wires the SFX / music dispatch systems.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<BevyAudioPlugin>() {
            app.add_plugins(BevyAudioPlugin::default());
        }

        let defaults = MasterVolumes::default();
        app.init_resource::<MasterVolumes>()
            .register_type::<MasterVolumes>()
            .add_message::<PlaySfx>()
            .add_message::<PlayMusic>()
            .add_message::<StopMusic>()
            .insert_resource(GlobalVolume::new(Volume::Linear(defaults.master)))
            .add_systems(
                Update,
                (
                    handle_play_sfx,
                    handle_play_music,
                    handle_stop_music,
                    sync_global_volume,
                ),
            );
    }
}

/// Mirror [`MasterVolumes::master`] into Bevy's [`GlobalVolume`] whenever the
/// mixer resource is mutated (e.g. by a settings UI).
fn sync_global_volume(volumes: Res<MasterVolumes>, mut global: ResMut<GlobalVolume>) {
    if volumes.is_changed() {
        *global = GlobalVolume::new(Volume::Linear(volumes.master));
    }
}

/// Spawn a one-shot audio entity per [`PlaySfx`] event. The entity despawns
/// itself when playback completes ([`PlaybackMode::Despawn`]).
fn handle_play_sfx(
    mut commands: Commands,
    mut events: MessageReader<PlaySfx>,
    volumes: Res<MasterVolumes>,
) {
    for ev in events.read() {
        commands.spawn((
            AudioPlayer(ev.handle.clone()),
            PlaybackSettings {
                mode: PlaybackMode::Despawn,
                volume: Volume::Linear(ev.volume * volumes.sfx),
                speed: ev.speed,
                ..default()
            },
        ));
    }
}

/// Stop any existing [`CurrentMusic`] entity and spawn a new looping one per
/// [`PlayMusic`] event. Crossfade is reserved for future work.
fn handle_play_music(
    mut commands: Commands,
    mut events: MessageReader<PlayMusic>,
    volumes: Res<MasterVolumes>,
    existing: Query<Entity, With<CurrentMusic>>,
) {
    for ev in events.read() {
        for e in &existing {
            commands.entity(e).despawn();
        }
        commands.spawn((
            CurrentMusic,
            AudioPlayer(ev.handle.clone()),
            PlaybackSettings {
                mode: PlaybackMode::Loop,
                volume: Volume::Linear(volumes.music),
                ..default()
            },
            Name::new("CurrentMusic"),
        ));
    }
}

/// Despawn all [`CurrentMusic`] entities per [`StopMusic`] event.
fn handle_stop_music(
    mut commands: Commands,
    mut events: MessageReader<StopMusic>,
    q: Query<Entity, With<CurrentMusic>>,
) {
    for _ in events.read() {
        for e in &q {
            commands.entity(e).despawn();
        }
    }
}
