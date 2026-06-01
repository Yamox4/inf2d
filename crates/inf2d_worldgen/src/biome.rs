#![deny(unsafe_code)]
//! Whittaker-style biome generator: two independent fBm fields (elevation + moisture)
//! classified into the six [`TileKind`] values from `inf2d_world`.

use inf2d_core::{
    chunk_rng, mix_seed, rng::stream, ChunkPos, LocalTilePos, CHUNK_SIZE, CHUNK_TILES,
};
use inf2d_world::{ChunkData, Generator, Tile, TileKind};
use noise::{Fbm, MultiFractal, NoiseFn, Perlin};
use rand::Rng;

use crate::params::BiomeParams;

/// Distinct stream tags used only to derive the two `noise` seeds. They live in this crate
/// because the [`inf2d_core::rng::stream`] tags are aimed at chunk-local RNG, and worldgen
/// wants whole-world noise seeds that don't alias with the per-chunk streams.
const STREAM_ELEVATION_SEED: u32 = 0x1001;
const STREAM_MOISTURE_SEED: u32 = 0x1002;

/// Probability of converting a tile next to a 2-step gradient into a stair
/// landing. Tuned so stair landings appear "occasionally" rather than carpet
/// every gradient — at `0.4`, roughly two-fifths of qualifying tiles flip,
/// which threads enough ramps through hilly terrain to noticeably shorten
/// pathfinding routes without homogenizing the silhouette.
const STAIR_SPAWN_PROBABILITY: f64 = 0.4;

/// Deterministic procedural generator that drives every tile from two noise fields. Holds
/// the prebuilt `Fbm` sources so each `generate` call is just sampling, not setup.
///
/// Instances are `Send + Sync`; `noise::Fbm` is plain data once configured. Build one per
/// `BiomeParams` and feed it to [`inf2d_world::ActiveGenerator`].
pub struct BiomeGenerator {
    params: BiomeParams,
    elevation: Fbm<Perlin>,
    moisture: Fbm<Perlin>,
}

impl BiomeGenerator {
    /// Build the noise sources from `params`. Seeds for the two fields are derived through
    /// `mix_seed` so they avalanche independently of the world seed and of each other.
    pub fn new(params: BiomeParams) -> Self {
        let elev_seed =
            mix_seed(params.world_seed, ChunkPos::new(0, 0), STREAM_ELEVATION_SEED) as u32;
        let moist_seed =
            mix_seed(params.world_seed, ChunkPos::new(0, 0), STREAM_MOISTURE_SEED) as u32;

        let elevation = Fbm::<Perlin>::new(elev_seed)
            .set_octaves(params.elevation_octaves)
            .set_frequency(params.elevation_frequency)
            .set_lacunarity(params.elevation_lacunarity)
            .set_persistence(params.elevation_persistence);

        // Moisture reuses the elevation lacunarity/persistence — those shape the spectrum,
        // not the field identity, and only the seed + frequency need to decorrelate.
        let moisture = Fbm::<Perlin>::new(moist_seed)
            .set_octaves(params.moisture_octaves)
            .set_frequency(params.moisture_frequency)
            .set_lacunarity(params.elevation_lacunarity)
            .set_persistence(params.elevation_persistence);

        Self {
            params,
            elevation,
            moisture,
        }
    }

    /// Borrow the parameters this generator was built with.
    #[inline]
    pub fn params(&self) -> &BiomeParams {
        &self.params
    }
}

impl Generator for BiomeGenerator {
    fn generate(&self, chunk: ChunkPos) -> ChunkData {
        let mut data = ChunkData::default();
        let p = &self.params;
        {
            let tiles = data.raw_mut();

            for i in 0..CHUNK_TILES {
                let local = LocalTilePos::from_index(i);
                let world = local.to_world(chunk);
                let wx = world.x as f64;
                let wy = world.y as f64;

                let e = sample_field(&self.elevation, wx, wy, p.elevation_redistribution);
                let m = sample_field(&self.moisture, wx, wy, 1.0);

                let kind = classify_biome(e, m, p);
                let height = height_for(e, p);
                tiles[i] = Tile::with_height(kind, height);
            }
        }

        place_stair_landings(&mut data, chunk, p.world_seed);

        data
    }
}

/// Second pass over a freshly-generated chunk: convert select tiles into
/// [`TileKind::Stairs`] landings that bridge a 2-step elevation drop to one of
/// their in-chunk neighbors. Cross-chunk edges are skipped — a landing only
/// fires when both endpoints belong to the same chunk so the decision stays
/// local and deterministic per chunk.
///
/// A tile qualifies when at least one of its four cardinal neighbors sits
/// exactly two steps above or below it. When multiple neighbors qualify, the
/// first one in (north, east, south, west) order wins — the iteration order
/// is fixed so the RNG draw is reproducible.
///
/// The landing's new height is the midpoint of the two endpoints (rounded
/// toward zero), so a `5 → 3` drop produces a landing at height `4`. The
/// pathfinder's stair rule (see `inf2d_pathfinding::is_edge_walkable`) then
/// admits both `5 ↔ 4` and `4 ↔ 3` edges through the landing because either
/// endpoint being a stair widens the allowed step to 2.
///
/// Determinism: keyed on `chunk_rng(world_seed, chunk, stream::STAIRS)` so the
/// same chunk always gets the same landings, but the placement is decorrelated
/// from the terrain/moisture/scatter streams.
fn place_stair_landings(data: &mut ChunkData, chunk: ChunkPos, world_seed: u64) {
    // Snapshot heights and kinds up-front so the placement decision reads the
    // *original* terrain — mutating tiles in place during the same pass would
    // make the result depend on iteration order beyond what the RNG controls.
    let snapshot: Vec<(TileKind, i8)> =
        data.raw().iter().map(|t| (t.kind, t.height)).collect();

    let mut rng = chunk_rng(world_seed, chunk, stream::STAIRS);
    let size = CHUNK_SIZE as i32;
    // Cardinal neighbor offsets in (N, E, S, W) order. The first qualifying
    // neighbor in this order is the one whose height is used for the midpoint
    // — fixed order so the RNG draw remains deterministic.
    const NEIGHBORS: [(i32, i32); 4] = [(0, 1), (1, 0), (0, -1), (-1, 0)];

    for i in 0..CHUNK_TILES {
        let (kind, h_t) = snapshot[i];

        // Only convert non-solid land tiles. Water/Stone tiles stay impassable
        // (a stair sticking out of the sea would read as a glitch), and an
        // existing stair has nothing to convert.
        if kind.is_solid() || kind == TileKind::Stairs {
            continue;
        }

        let local = LocalTilePos::from_index(i);
        let mut found_neighbor_height: Option<i8> = None;

        for (dx, dy) in NEIGHBORS {
            let nx = local.x as i32 + dx;
            let ny = local.y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= size || ny >= size {
                // Cross-chunk neighbor — skip per design.
                continue;
            }
            let nb_index = LocalTilePos::new(nx as u32, ny as u32).index();
            let (nb_kind, h_n) = snapshot[nb_index];
            // Don't try to bridge into water or stone — those tiles aren't
            // walkable destinations even with a landing in front of them.
            if nb_kind.is_solid() {
                continue;
            }
            if (h_t as i32 - h_n as i32).abs() == 2 {
                found_neighbor_height = Some(h_n);
                break;
            }
        }

        let Some(h_n) = found_neighbor_height else {
            continue;
        };

        // Single RNG draw per qualifying tile — drawing only when a candidate
        // exists keeps the RNG sequence aligned with the (kind, height) world,
        // so flipping STAIR_SPAWN_PROBABILITY doesn't desynchronize chunks.
        if !rng.random_bool(STAIR_SPAWN_PROBABILITY) {
            continue;
        }

        let midpoint = ((h_t as i32 + h_n as i32) / 2) as i8;
        data.set(local, Tile::with_height(TileKind::Stairs, midpoint));
    }
}

/// Convert a normalized elevation sample (`0..1`) into a discrete `i8` height step using the
/// thresholds in `params`. Water tiles snap to `params.water_height` (typically a slight
/// recess so the shoreline doesn't read as a cliff); land tiles bucket into
/// `0..=max_height_steps` based on how far `e` sits above `water_level`.
///
/// The mapping is deliberately monotonic in elevation: a higher noise sample always yields
/// a height step that is `>=` a lower one, so adjacent tiles step up gradually instead of
/// flickering between layers as you walk along a gradient.
#[inline]
fn height_for(e: f64, params: &BiomeParams) -> i8 {
    if e < params.water_level {
        return params.water_height;
    }
    let span = (1.0 - params.water_level).max(f64::EPSILON);
    let normalized = ((e - params.water_level) / span).clamp(0.0, 1.0);
    let steps = (normalized * params.max_height_steps as f64).floor();
    // Clamp into the `i8` land range so a degenerate `max_height_steps` can't overflow.
    let clamped = steps.clamp(0.0, params.max_height_steps as f64);
    clamped as i8
}

/// Sample an fBm field at `(wx, wy)` in world-tile space, renormalize the `~[-1, 1]` Perlin
/// output to `[0, 1]`, and apply the Red-Blob redistribution exponent.
///
/// The exponent is applied as `value.powf(exp)` — for `exp > 1` this pushes mass toward
/// zero (low elevations dominate), for `exp < 1` it does the opposite. Frequency baked into
/// the `Fbm` already scales coords, so the caller passes raw world-tile coordinates.
#[inline]
fn sample_field(fbm: &Fbm<Perlin>, wx: f64, wy: f64, redistribution: f64) -> f64 {
    let raw = fbm.get([wx, wy]);
    let n = ((raw + 1.0) * 0.5).clamp(0.0, 1.0);
    if redistribution == 1.0 {
        n
    } else {
        n.powf(redistribution)
    }
}

/// Walk the elevation thresholds bottom-up, then disambiguate the mid-band by moisture.
/// Order matters: water/beach short-circuit before snow/stone so the classifier degrades
/// gracefully if a user pushes thresholds close together.
#[inline]
fn classify_biome(e: f64, m: f64, p: &BiomeParams) -> TileKind {
    if e < p.water_level {
        TileKind::Water
    } else if e < p.water_level + p.beach_band {
        TileKind::Sand
    } else if e > p.snow_level {
        TileKind::Snow
    } else if e > p.mountain_level {
        TileKind::Stone
    } else if m < 0.3 {
        TileKind::Sand
    } else if m > 0.7 {
        TileKind::Dirt
    } else {
        TileKind::Grass
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inf2d_world::Generator as _;

    #[test]
    fn default_params_produce_variation() {
        let gen = BiomeGenerator::new(BiomeParams::default());
        let data = gen.generate(ChunkPos::new(0, 0));

        let raw = data.raw();
        let first = raw[0].kind;
        let any_different = raw.iter().any(|t| t.kind != first);
        assert!(
            any_different,
            "expected at least one non-default tile in a fresh chunk; got all {:?}",
            first
        );
    }

    #[test]
    fn generation_is_deterministic() {
        let gen = BiomeGenerator::new(BiomeParams::default());
        let a = gen.generate(ChunkPos::new(3, -7));
        let b = gen.generate(ChunkPos::new(3, -7));
        assert_eq!(a.raw(), b.raw());
    }

    #[test]
    fn seed_changes_output() {
        let mut p1 = BiomeParams::default();
        let mut p2 = BiomeParams::default();
        p1.world_seed = 1;
        p2.world_seed = 2;
        let g1 = BiomeGenerator::new(p1);
        let g2 = BiomeGenerator::new(p2);
        let a = g1.generate(ChunkPos::new(0, 0));
        let b = g2.generate(ChunkPos::new(0, 0));
        assert_ne!(a.raw(), b.raw(), "different seeds must yield different chunks");
    }

    #[test]
    fn water_tiles_use_configured_water_height() {
        let p = BiomeParams::default();
        let below = p.water_level * 0.5;
        assert_eq!(height_for(below, &p), p.water_height);
    }

    #[test]
    fn land_height_is_monotonic_in_elevation() {
        let p = BiomeParams::default();
        let lo = height_for(p.water_level + 0.001, &p);
        let mid = height_for((p.water_level + 1.0) * 0.5, &p);
        let hi = height_for(0.999, &p);
        assert!(lo <= mid, "low={lo} mid={mid}");
        assert!(mid <= hi, "mid={mid} hi={hi}");
    }

    #[test]
    fn land_height_capped_at_max_steps() {
        let p = BiomeParams::default();
        let top = height_for(1.0, &p);
        assert!(top <= p.max_height_steps as i8);
    }

    #[test]
    fn generated_chunk_carries_height_field() {
        let gen = BiomeGenerator::new(BiomeParams::default());
        let data = gen.generate(ChunkPos::new(0, 0));
        let raw = data.raw();
        let any_nonzero = raw.iter().any(|t| t.height != 0);
        assert!(any_nonzero, "expected at least one tile with non-zero height in a generated chunk");
    }

    #[test]
    fn stair_pass_bridges_two_step_gap() {
        // Build a tiny synthetic chunk: a `0 ↔ 2` step pair flanked by flats.
        // After the stair pass, the lower-of-the-pair tile (or its partner)
        // should sometimes become a Stairs landing at height `1`. Run enough
        // independent seeds that the 0.4 probability has effectively no
        // chance of producing zero conversions across all of them.
        let mut converted_any = false;
        for seed in 0..16u64 {
            let mut data = ChunkData::default();
            {
                let tiles = data.raw_mut();
                // Half the chunk at height 0, the other half at height 2 —
                // the seam between them gives every tile on the seam a
                // qualifying neighbor.
                for i in 0..CHUNK_TILES {
                    let local = LocalTilePos::from_index(i);
                    let h: i8 = if local.x < CHUNK_SIZE / 2 { 0 } else { 2 };
                    tiles[i] = Tile::with_height(TileKind::Grass, h);
                }
            }
            place_stair_landings(&mut data, ChunkPos::new(0, 0), seed);
            if data.raw().iter().any(|t| t.kind == TileKind::Stairs) {
                converted_any = true;
                // Every stair must sit at the midpoint of its neighbor pair.
                for t in data.raw().iter() {
                    if t.kind == TileKind::Stairs {
                        assert_eq!(
                            t.height, 1,
                            "stair landing on a 0↔2 seam must have height 1, got {}",
                            t.height
                        );
                    }
                }
                break;
            }
        }
        assert!(
            converted_any,
            "across 16 seeds, the stair pass produced no landings on a 0↔2 seam — RNG broken?"
        );
    }

    #[test]
    fn stair_pass_is_deterministic() {
        let p = BiomeParams::default();
        let mut a = ChunkData::default();
        let mut b = ChunkData::default();
        {
            let ta = a.raw_mut();
            let tb = b.raw_mut();
            for i in 0..CHUNK_TILES {
                let local = LocalTilePos::from_index(i);
                let h: i8 = if local.x < CHUNK_SIZE / 2 { 0 } else { 2 };
                ta[i] = Tile::with_height(TileKind::Grass, h);
                tb[i] = Tile::with_height(TileKind::Grass, h);
            }
        }
        place_stair_landings(&mut a, ChunkPos::new(4, -3), p.world_seed);
        place_stair_landings(&mut b, ChunkPos::new(4, -3), p.world_seed);
        assert_eq!(a.raw(), b.raw(), "stair placement must be deterministic");
    }

    #[test]
    fn stair_pass_leaves_flat_chunks_alone() {
        // A perfectly flat chunk has no 2-step gradients → no stair landings.
        let mut data = ChunkData::default();
        {
            let tiles = data.raw_mut();
            for i in 0..CHUNK_TILES {
                tiles[i] = Tile::with_height(TileKind::Grass, 0);
            }
        }
        place_stair_landings(&mut data, ChunkPos::new(0, 0), 0xDEAD_BEEF);
        assert!(
            data.raw().iter().all(|t| t.kind == TileKind::Grass),
            "flat chunk must not gain any stair tiles"
        );
    }

    #[test]
    fn classify_water_then_beach_then_land() {
        let p = BiomeParams::default();
        assert_eq!(classify_biome(0.0, 0.5, &p), TileKind::Water);
        assert_eq!(
            classify_biome(p.water_level + p.beach_band * 0.5, 0.5, &p),
            TileKind::Sand
        );
        assert_eq!(classify_biome(0.5, 0.5, &p), TileKind::Grass);
        assert_eq!(classify_biome(0.5, 0.1, &p), TileKind::Sand);
        assert_eq!(classify_biome(0.5, 0.9, &p), TileKind::Dirt);
        assert_eq!(classify_biome(p.mountain_level + 0.01, 0.5, &p), TileKind::Stone);
        assert_eq!(classify_biome(p.snow_level + 0.01, 0.5, &p), TileKind::Snow);
    }
}
