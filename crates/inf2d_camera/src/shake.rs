use bevy::prelude::*;

use crate::rig::CameraRig;

/// Fire-and-forget camera-shake request. Any system can write one of these to
/// kick the camera; the strongest currently-pending request wins (we don't
/// compound, but we accept a larger amplitude mid-shake).
///
/// `amplitude` is the peak displacement in world units, `duration` how long
/// (seconds) until it's fully decayed via an ease-out envelope, and
/// `frequency` the oscillation rate in Hz.
#[derive(Message, Debug, Clone)]
pub struct ShakeRequest {
    /// Peak displacement in world units.
    pub amplitude: f32,
    /// Seconds before the shake is fully decayed.
    pub duration: f32,
    /// Shake oscillations per second (Hz).
    pub frequency: f32,
}

impl ShakeRequest {
    /// Tiny tick — footsteps, UI clicks. ~1 px for ~120 ms.
    pub fn subtle() -> Self {
        Self { amplitude: 1.0, duration: 0.12, frequency: 25.0 }
    }
    /// Medium kick — small explosions, melee hits. ~3.5 px for 250 ms.
    pub fn moderate() -> Self {
        Self { amplitude: 3.5, duration: 0.25, frequency: 22.0 }
    }
    /// Big rumble — boss landings, large explosions. ~9 px for half a second.
    pub fn heavy() -> Self {
        Self { amplitude: 9.0, duration: 0.5, frequency: 18.0 }
    }
}

/// Per-rig shake state — the currently-active envelope being driven each frame.
/// Idle when `remaining <= 0.0`. Spawned alongside [`CameraRig`] via
/// `CameraRigBundle::default()`.
#[derive(Component, Default, Debug, Clone, Copy)]
pub struct ActiveShake {
    /// Peak amplitude of the active envelope, in world units.
    pub amplitude: f32,
    /// Seconds left until the envelope is fully decayed.
    pub remaining: f32,
    /// Total duration of the active envelope; used to normalize the envelope.
    pub duration: f32,
    /// Oscillation frequency of the active envelope, in Hz.
    pub frequency: f32,
    /// Seconds since the envelope began; drives the sine phase.
    pub elapsed: f32,
}

/// Consume [`ShakeRequest`] messages into the rig's [`ActiveShake`] state.
///
/// Strongest-wins: a new request replaces the active envelope only if its
/// `amplitude` is larger than what's currently playing, or if the active
/// envelope has already decayed. This avoids stomping a heavy boss-rumble with
/// a stream of subtle footstep ticks while still letting the camera respond to
/// a fresh hit mid-shake.
pub fn process_shake_requests(
    mut events: MessageReader<ShakeRequest>,
    mut q: Query<&mut ActiveShake, With<CameraRig>>,
) {
    let Ok(mut shake) = q.single_mut() else { return; };
    for req in events.read() {
        // Strongest request wins — don't compound, but accept the larger amplitude.
        if req.amplitude > shake.amplitude || shake.remaining <= 0.0 {
            shake.amplitude = req.amplitude;
            shake.duration = req.duration;
            shake.remaining = req.duration;
            shake.frequency = req.frequency;
            shake.elapsed = 0.0;
        }
    }
}

/// Advance the active shake envelope and write the resulting offset to
/// [`CameraRig::shake`], which the pan / follow systems add to the camera
/// transform downstream. Uses two phase-offset sines (deterministic, no RNG)
/// and an ease-out (envelope squared) so the shake tapers smoothly to zero.
pub fn drive_shake(
    time: Res<Time>,
    mut q: Query<(&mut CameraRig, &mut ActiveShake)>,
) {
    let dt = time.delta_secs();
    let Ok((mut rig, mut shake)) = q.single_mut() else { return; };
    if shake.remaining <= 0.0 {
        rig.shake = Vec2::ZERO;
        return;
    }
    shake.remaining = (shake.remaining - dt).max(0.0);
    shake.elapsed += dt;
    let t = shake.elapsed * shake.frequency * std::f32::consts::TAU;
    // 1.0 -> 0.0 over `duration`, squared for an ease-out feel.
    let envelope = shake.remaining / shake.duration;
    let amp = shake.amplitude * envelope * envelope;
    // Deterministic noise — two phase-offset sines for x and y.
    rig.shake.x = t.sin() * amp;
    rig.shake.y = (t * 1.37 + 1.7).sin() * amp;
}
