//! inf3d_audio — game sound.
//!
//! A read-only downstream **sink**, exactly like `inf3d_monitor` / `inf3d_ui`: it
//! only *reads* gameplay events and *plays* sounds, so nothing depends on it (the
//! acyclic crate graph is preserved) and it adds no coupling to the systems it
//! listens to. Its systems run in `GameSet::Fx` (presentation, end of frame).
//!
//! Sounds live in `crates/inf3d_app/assets/audio/` (see that folder's README for
//! the structure + naming). Bevy's `AssetServer` resolves paths relative to the
//! app's asset root, so `"audio/sfx/footsteps/…"` finds the file regardless of
//! which crate loads it.
//!
//! ## What's wired
//! - **Footsteps** — one per [`inf3d_gameplay::Footstep`] message (emitted on each
//!   walk-hop landing / ground touch), played with a slight random pitch + volume
//!   variation so repeated steps don't sound identical/robotic. A single
//!   "all surfaces" clip is used today; per-surface clips can be selected later
//!   from the `Footstep` data without touching the trigger.

use bevy::audio::Volume; // not in bevy::prelude (AudioPlayer/PlaybackSettings are)
use bevy::prelude::*;
use rand::Rng;

use inf3d_core::GameSet;
use inf3d_gameplay::Footstep;

/// Footstep clip, relative to the app asset root (`crates/inf3d_app/assets/`).
/// This is a TRIMMED `.ogg` (≈0.22 s) produced from the original `.mp3`, which had
/// ~526 ms of leading silence baked in — that gap made every step play ~half a
/// second late and feel out of sync. Keep footstep clips gap-trimmed (no leading
/// silence) so the sound lands exactly on the visual step. Drop more / per-surface
/// variants in that folder; see its README.
const FOOTSTEP_CLIP: &str = "audio/sfx/footsteps/player_footstep.ogg";
/// Base footstep volume (linear; 1.0 = source level). Lower if footsteps drown
/// out everything; raise if too quiet. The single tuning knob for loudness.
const FOOTSTEP_VOLUME: f32 = 0.7;
/// Playback-speed (= pitch) range applied per step. Deliberately narrow — a
/// *slight* variation so steps feel organic without sounding warbly.
const FOOTSTEP_SPEED_MIN: f32 = 0.95;
const FOOTSTEP_SPEED_MAX: f32 = 1.05;
/// Per-step volume jitter as a ± fraction of [`FOOTSTEP_VOLUME`] (0.1 = ±10%).
const FOOTSTEP_VOLUME_JITTER: f32 = 0.1;

/// Loaded sound handles, kept resident so steps play instantly (no per-step load).
#[derive(Resource)]
struct AudioAssets {
    footstep: Handle<AudioSource>,
}

/// Plays game sound. Add once in the app (downstream of gameplay).
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_audio)
            // Presentation: react to gameplay events at the end of the frame.
            .add_systems(Update, play_footsteps.in_set(GameSet::Fx));
    }
}

/// Load sound handles once at startup so they're resident before the first step.
fn load_audio(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(AudioAssets {
        footstep: assets.load(FOOTSTEP_CLIP),
    });
}

/// Spawn a one-shot footstep for each hop-landing this frame, each with a slight
/// random pitch + volume so they don't sound identical. `PlaybackSettings::DESPAWN`
/// removes the entity when the clip finishes, so these transient audio entities
/// never accumulate.
fn play_footsteps(
    mut commands: Commands,
    mut steps: MessageReader<Footstep>,
    audio: Option<Res<AudioAssets>>,
) {
    let Some(audio) = audio else {
        return;
    };
    let mut rng = rand::rng();
    for _ in steps.read() {
        let speed = rng.random_range(FOOTSTEP_SPEED_MIN..FOOTSTEP_SPEED_MAX);
        let jitter = rng.random_range(-FOOTSTEP_VOLUME_JITTER..FOOTSTEP_VOLUME_JITTER);
        let volume = (FOOTSTEP_VOLUME * (1.0 + jitter)).max(0.0);
        // Start from DESPAWN (self-cleans the entity when the clip ends) and set
        // the two knobs — mutate rather than struct-literal so we don't depend on
        // every PlaybackSettings field being public from this crate.
        let mut settings = PlaybackSettings::DESPAWN;
        settings.speed = speed;
        settings.volume = Volume::Linear(volume);
        commands.spawn((AudioPlayer(audio.footstep.clone()), settings));
    }
}
