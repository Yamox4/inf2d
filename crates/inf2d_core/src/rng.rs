use crate::coords::ChunkPos;

/// Streams that share a `(seed, chunk)` but should not correlate. Use distinct values
/// for terrain, scatter, structures, loot, etc., so changes in one don't ripple.
#[allow(dead_code)]
pub mod stream {
    /// Base terrain noise.
    pub const TERRAIN: u32 = 0;
    /// Moisture / biome humidity noise.
    pub const MOISTURE: u32 = 1;
    /// Prop scatter (trees, debris) inside a chunk.
    pub const SCATTER: u32 = 2;
    /// Structure placement (future).
    pub const STRUCTURE: u32 = 3;
    /// Loot rolls (future).
    pub const LOOT: u32 = 4;
    /// Stair / ramp landing placement at steep-gradient tile pairs. See
    /// `inf2d_worldgen::biome::BiomeGenerator::generate` for the consumer.
    pub const STAIRS: u32 = 5;
}

/// SplitMix64 — the canonical fast, statistically-sound bit mixer.
///
/// We use it both as a state-free hash for `(seed, chunk_x, chunk_y, stream)` and as an
/// RNG seed generator. It's the same mixer the reference Xoshiro authors recommend for
/// stretching a seed into the larger state of Xoshiro256++.
#[inline]
pub const fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Mix a world seed, chunk coordinates, and a stream tag into a deterministic 64-bit key.
///
/// Avalanche is good enough that flipping any input bit changes ~half of the output bits,
/// which is what we need for independent per-chunk RNGs.
#[inline]
pub fn mix_seed(world_seed: u64, chunk: ChunkPos, stream: u32) -> u64 {
    // Pack the i32 chunk coords into a single u64 and mix in the stream tag.
    let packed = ((chunk.x as u32 as u64) << 32) | (chunk.y as u32 as u64);
    let mut s = splitmix64(world_seed ^ packed);
    s = splitmix64(s ^ (stream as u64).wrapping_mul(0xD2B7_4407_B1CE_6E93));
    s
}

/// Build a fresh deterministic RNG keyed on `(world_seed, chunk, stream)`. The RNG is
/// position-independent: rebuilding it with the same inputs yields the same sequence.
#[inline]
pub fn chunk_rng(world_seed: u64, chunk: ChunkPos, stream: u32) -> rand_xoshiro::Xoshiro256PlusPlus {
    use rand_xoshiro::rand_core::SeedableRng;
    rand_xoshiro::Xoshiro256PlusPlus::seed_from_u64(mix_seed(world_seed, chunk, stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn determinism() {
        let a = mix_seed(0xDEAD_BEEF, ChunkPos::new(3, -7), stream::TERRAIN);
        let b = mix_seed(0xDEAD_BEEF, ChunkPos::new(3, -7), stream::TERRAIN);
        assert_eq!(a, b);
        let c = mix_seed(0xDEAD_BEEF, ChunkPos::new(3, -7), stream::SCATTER);
        assert_ne!(a, c);
    }

    #[test]
    fn neighbor_chunks_decorrelate() {
        let a = mix_seed(1, ChunkPos::new(0, 0), 0);
        let b = mix_seed(1, ChunkPos::new(1, 0), 0);
        // No deeper statistical test here; just guard against accidental aliasing.
        assert_ne!(a, b);
    }
}
