#![deny(unsafe_code)]
//! Tunables for the biome generator. Held as a Bevy `Resource` so it can be hot-edited
//! through the inspector and re-fed to a new `BiomeGenerator` if desired.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Default world seed. Picked once; arbitrary except that swapping it must visibly change
/// the world, so we use a 64-bit constant with good bit diversity.
pub const DEFAULT_WORLD_SEED: u64 = 0xCAFE_F00D_D15E_A5E5;

/// All knobs that drive [`crate::biome::BiomeGenerator`]. Two noise fields (elevation +
/// moisture) plus the Whittaker thresholds that split them into the six [`inf2d_world::TileKind`]
/// values.
///
/// Frequencies are in *cycles per tile* — `1.0 / 256.0` means one noise period spans 256
/// tiles, which is roughly 8 chunks at the current `CHUNK_SIZE = 32`. Octaves stack to
/// add detail; `lacunarity` is the per-octave frequency multiplier and `persistence` is the
/// per-octave amplitude multiplier — the standard fBm formulation.
///
/// `elevation_redistribution` is the Red-Blob exponent applied to the normalized elevation:
/// values `> 1` push mass toward the low end (more flat lowlands, fewer mountains).
#[derive(Resource, Reflect, Clone, Debug, Serialize, Deserialize)]
#[reflect(Resource)]
pub struct BiomeParams {
    pub world_seed: u64,

    pub elevation_frequency: f64,
    pub elevation_octaves: usize,
    pub elevation_lacunarity: f64,
    pub elevation_persistence: f64,
    pub elevation_redistribution: f64,

    pub moisture_frequency: f64,
    pub moisture_octaves: usize,

    pub water_level: f64,
    pub beach_band: f64,
    pub mountain_level: f64,
    pub snow_level: f64,

    /// Maximum elevation step assigned to non-water tiles. Land tiles are bucketed into
    /// `0..=max_height_steps` based on normalized elevation; bumping this gives taller
    /// mountains at the cost of a wider per-chunk height-layer fan-out (more tilemaps per
    /// chunk). `6` keeps mountain peaks distinctly tall while staying within the renderer's
    /// per-chunk layer budget.
    pub max_height_steps: u8,
    /// Step assigned to tiles classified as water. Slight recess (`-1`) reads as "pond
    /// bottom" without making coastlines look like cliffs. Set to `0` to render water flush
    /// with the beach.
    pub water_height: i8,
}

impl Default for BiomeParams {
    fn default() -> Self {
        Self {
            world_seed: DEFAULT_WORLD_SEED,

            elevation_frequency: 1.0 / 256.0,
            elevation_octaves: 5,
            elevation_lacunarity: 2.0,
            elevation_persistence: 0.5,
            elevation_redistribution: 1.8,

            moisture_frequency: 1.0 / 200.0,
            moisture_octaves: 4,

            water_level: 0.35,
            beach_band: 0.05,
            mountain_level: 0.78,
            snow_level: 0.92,

            max_height_steps: 6,
            water_height: -1,
        }
    }
}

impl BiomeParams {
    /// Reject parameter sets that would produce degenerate or empty biome bands.
    ///
    /// The classifier walks the elevation thresholds in ascending order, so any inversion
    /// (e.g. `mountain_level <= water_level + beach_band`) silently swallows whole biomes.
    /// Catching it here lets the caller surface a useful error before chunks start streaming.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(self.elevation_frequency.is_finite() && self.elevation_frequency > 0.0) {
            return Err("elevation_frequency must be finite and > 0");
        }
        if !(self.moisture_frequency.is_finite() && self.moisture_frequency > 0.0) {
            return Err("moisture_frequency must be finite and > 0");
        }
        if self.elevation_octaves == 0 {
            return Err("elevation_octaves must be >= 1");
        }
        if self.moisture_octaves == 0 {
            return Err("moisture_octaves must be >= 1");
        }
        if !(self.elevation_lacunarity.is_finite() && self.elevation_lacunarity > 1.0) {
            return Err("elevation_lacunarity must be finite and > 1");
        }
        if !(self.elevation_persistence.is_finite()
            && self.elevation_persistence > 0.0
            && self.elevation_persistence < 1.0)
        {
            return Err("elevation_persistence must be in (0, 1)");
        }
        if !(self.elevation_redistribution.is_finite() && self.elevation_redistribution > 0.0) {
            return Err("elevation_redistribution must be finite and > 0");
        }
        if !(0.0..=1.0).contains(&self.water_level) {
            return Err("water_level must be in [0, 1]");
        }
        if !(0.0..=1.0).contains(&self.mountain_level) {
            return Err("mountain_level must be in [0, 1]");
        }
        if !(0.0..=1.0).contains(&self.snow_level) {
            return Err("snow_level must be in [0, 1]");
        }
        if self.beach_band < 0.0 || self.beach_band > 1.0 {
            return Err("beach_band must be in [0, 1]");
        }
        if self.water_level + self.beach_band >= self.mountain_level {
            return Err("water_level + beach_band must be < mountain_level");
        }
        if self.mountain_level >= self.snow_level {
            return Err("mountain_level must be < snow_level");
        }
        if self.max_height_steps == 0 {
            return Err("max_height_steps must be >= 1");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_params_validate() {
        BiomeParams::default().validate().expect("defaults must validate");
    }

    #[test]
    fn inverted_thresholds_rejected() {
        let mut p = BiomeParams::default();
        p.mountain_level = 0.10;
        assert!(p.validate().is_err());
    }

    #[test]
    fn nonpositive_frequency_rejected() {
        let mut p = BiomeParams::default();
        p.elevation_frequency = 0.0;
        assert!(p.validate().is_err());
        p = BiomeParams::default();
        p.moisture_frequency = -1.0;
        assert!(p.validate().is_err());
    }
}
