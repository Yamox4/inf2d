//! Isolated cyberpunk city voxel backend.
//!
//! This crate is deliberately pure/deterministic: given `(x, y, z)` it returns the
//! same base city voxel forever, so the main game only has to save player edits.
//! It owns no Bevy systems and has no dependency on the rest of `inf3d`.

use bevy::prelude::*;

pub const CITY_SURFACE_Y: i32 = 8;
pub const CITY_SURFACE_HEIGHT: f64 = CITY_SURFACE_Y as f64 + 0.5;

pub const MAT_ASPHALT: u8 = 4;
pub const MAT_CONCRETE: u8 = 5;
pub const MAT_DARK_GLASS: u8 = 6;
pub const MAT_NEON_CYAN: u8 = 7;
pub const MAT_NEON_MAGENTA: u8 = 8;
pub const MAT_NEON_YELLOW: u8 = 9;

const BLOCK: i32 = 48;
const ROAD: i32 = 6;
const SIDEWALK: i32 = 2;
const FLOOR_H: i32 = 5;

#[derive(Clone, Copy)]
struct Lot {
    x0: i32,
    x1: i32,
    z0: i32,
    z1: i32,
    height: i32,
    construction: bool,
    neon: u8,
}

/// Base cyberpunk city voxel at `pos`, before player edits. `None` means air.
pub fn voxel_at(pos: IVec3) -> Option<u8> {
    let bx = div_floor(pos.x, BLOCK);
    let bz = div_floor(pos.z, BLOCK);
    let lx = pos.x.rem_euclid(BLOCK);
    let lz = pos.z.rem_euclid(BLOCK);

    if pos.y < CITY_SURFACE_Y - 5 {
        return Some(MAT_CONCRETE);
    }

    let road = lx < ROAD || lz < ROAD;
    let sidewalk = !road && (lx < ROAD + SIDEWALK || lz < ROAD + SIDEWALK);
    let alley = is_alley(lx, lz, bx, bz);

    if pos.y <= CITY_SURFACE_Y {
        return Some(if road || alley {
            MAT_ASPHALT
        } else {
            MAT_CONCRETE
        });
    }
    if road || sidewalk || alley {
        return None;
    }

    let lot = lot_for(lx, lz, bx, bz)?;
    if pos.y > CITY_SURFACE_Y + lot.height {
        return None;
    }

    if lot.construction {
        return construction_voxel(pos, lx, lz, lot);
    }
    tower_voxel(pos, lx, lz, lot)
}

fn tower_voxel(pos: IVec3, lx: i32, lz: i32, lot: Lot) -> Option<u8> {
    let ry = pos.y - CITY_SURFACE_Y - 1;
    let shell = lx == lot.x0 || lx == lot.x1 || lz == lot.z0 || lz == lot.z1;
    let roof = pos.y == CITY_SURFACE_Y + lot.height;
    let floor = ry % FLOOR_H == 0;
    if roof || floor {
        return Some(MAT_CONCRETE);
    }
    if !shell {
        return None;
    }

    let pillar = (lx - lot.x0).rem_euclid(6) == 0 || (lz - lot.z0).rem_euclid(6) == 0;
    let sign_band = ry > FLOOR_H && ry < lot.height - FLOOR_H && (lx + lz + ry).rem_euclid(11) == 0;
    if sign_band {
        return Some(lot.neon);
    }
    if pillar || ry % FLOOR_H == 1 {
        Some(MAT_CONCRETE)
    } else {
        Some(MAT_DARK_GLASS)
    }
}

fn construction_voxel(pos: IVec3, lx: i32, lz: i32, lot: Lot) -> Option<u8> {
    let ry = pos.y - CITY_SURFACE_Y - 1;
    let floor = ry % FLOOR_H == 0;
    let edge = lx == lot.x0 || lx == lot.x1 || lz == lot.z0 || lz == lot.z1;
    let frame_x = (lx - lot.x0).rem_euclid(5) == 0;
    let frame_z = (lz - lot.z0).rem_euclid(5) == 0;
    if floor && pos.y <= CITY_SURFACE_Y + lot.height - FLOOR_H {
        return Some(MAT_CONCRETE);
    }
    if edge && (frame_x || frame_z) {
        return Some(MAT_NEON_YELLOW);
    }
    if (frame_x && frame_z) && ry < lot.height - FLOOR_H {
        return Some(MAT_CONCRETE);
    }
    None
}

fn lot_for(lx: i32, lz: i32, bx: i32, bz: i32) -> Option<Lot> {
    let split = BLOCK / 2 + 1;
    let qx = if lx < split { 0 } else { 1 };
    let qz = if lz < split { 0 } else { 1 };
    let h = hash3(bx, bz, qx + qz * 2);
    let margin = 1 + (h % 3) as i32;
    let (x0, x1) = if qx == 0 {
        (ROAD + SIDEWALK + margin, split - 4 - margin)
    } else {
        (split + 3 + margin, BLOCK - 3 - margin)
    };
    let (z0, z1) = if qz == 0 {
        (ROAD + SIDEWALK + margin, split - 4 - margin)
    } else {
        (split + 3 + margin, BLOCK - 3 - margin)
    };
    if lx < x0 || lx > x1 || lz < z0 || lz > z1 {
        return None;
    }
    let floors = 3 + (h % 11) as i32;
    let neon = match h % 3 {
        0 => MAT_NEON_CYAN,
        1 => MAT_NEON_MAGENTA,
        _ => MAT_NEON_YELLOW,
    };
    Some(Lot {
        x0,
        x1,
        z0,
        z1,
        height: floors * FLOOR_H,
        construction: h % 7 == 0,
        neon,
    })
}

fn is_alley(lx: i32, lz: i32, bx: i32, bz: i32) -> bool {
    let h = hash3(bx, bz, 99);
    let mid = BLOCK / 2 + 1;
    let vertical = h & 1 == 0 && (lx - mid).abs() <= 1 && lz >= ROAD + SIDEWALK;
    let horizontal = h & 2 == 0 && (lz - mid).abs() <= 1 && lx >= ROAD + SIDEWALK;
    vertical || horizontal
}

fn div_floor(v: i32, d: i32) -> i32 {
    let q = v / d;
    let r = v % d;
    if r != 0 && (r > 0) != (d > 0) {
        q - 1
    } else {
        q
    }
}

fn hash3(a: i32, b: i32, c: i32) -> u32 {
    let mut x = (a as u32).wrapping_mul(0x9e37_79b9)
        ^ (b as u32).wrapping_mul(0x85eb_ca6b)
        ^ (c as u32).wrapping_mul(0xc2b2_ae35)
        ^ 0x51ed_270b;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roads_and_towers_are_deterministic() {
        assert_eq!(
            voxel_at(IVec3::new(0, CITY_SURFACE_Y, 0)),
            Some(MAT_ASPHALT)
        );
        let a = voxel_at(IVec3::new(14, CITY_SURFACE_Y + 8, 14));
        let b = voxel_at(IVec3::new(14, CITY_SURFACE_Y + 8, 14));
        assert_eq!(a, b);
    }
}
