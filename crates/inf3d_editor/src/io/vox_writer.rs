//! A minimal, correct MagicaVoxel `.vox` writer.
//!
//! `dot_vox` 5.x is a **loader only** — it exposes `load`/`load_bytes` but no
//! serializer (verified against its docs/source). So Phase 1 ships its own
//! writer here, emitting the small RIFF-like chunk format MagicaVoxel uses:
//!
//! ```text
//! "VOX " <version:i32>
//! MAIN (content 0, children = SIZE + XYZI + RGBA)
//!   SIZE  <x:i32> <y:i32> <z:i32>
//!   XYZI  <numVoxels:i32> then per-voxel (x, y, z, colorIndex) bytes
//!   RGBA  256 × (r, g, b, a) bytes
//! ```
//! (Format per the official `MagicaVoxel-file-format-vox.txt`.)
//!
//! **Coordinate frame.** The game's loader (`inf3d_render::foliage::vox_mesh`)
//! reads `dot_vox` voxels as `(x, y, z)` and maps them to Bevy `(x, z, -y)` — so
//! MagicaVoxel **Z is up**. The editor's model space is Bevy-style **Y up**. The
//! caller therefore converts editor cells `(ex, ey, ez)` into `.vox` voxels with
//! `(vx, vy, vz) = (ex, ez, ey)` before handing them here (the inverse of the
//! loader's map, dropping the negation that the loader re-applies as a flip),
//! which is exactly what [`VoxScene::from_model`](crate::io::vox_writer) does.
//!
//! **Palette indexing.** dot_vox normalizes a file color index `c` to the
//! in-memory index `c - 1` (`i: i.saturating_sub(1)` in its parser) and indexes
//! `palette[i]`. So a voxel that should show palette slot `s` (0-based, the
//! editor's `u8` color index) must be written with file color index `s + 1`,
//! and slot `s`'s RGBA goes at file palette position `s`. This module enforces
//! that mapping so exports round-trip through both dot_vox AND MagicaVoxel.

/// One voxel in **MagicaVoxel (Z-up) space**, with a 0-based editor palette slot.
#[derive(Clone, Copy, Debug)]
pub struct VoxVoxel {
    /// MagicaVoxel x (`0..size.x`).
    pub x: u8,
    /// MagicaVoxel y (`0..size.y`).
    pub y: u8,
    /// MagicaVoxel z (up, `0..size.z`).
    pub z: u8,
    /// 0-based palette slot this voxel paints (the editor's color index).
    pub palette_slot: u8,
}

/// Everything needed to serialize one `.vox` file: the model dimensions in
/// MagicaVoxel space, its voxels, and the sRGB palette (slot order).
pub struct VoxScene {
    /// Model size on MagicaVoxel x/y/z. Each `<= 255` (the format stores size as
    /// i32 but voxel coords are single bytes, so a model can't exceed 256/axis).
    pub size: [u32; 3],
    /// The voxels (already in MagicaVoxel space, see module docs).
    pub voxels: Vec<VoxVoxel>,
    /// sRGB palette, slot order. `palette[s]` is the color for `palette_slot s`.
    pub palette: Vec<[u8; 3]>,
}

/// `.vox` format version we stamp (matches the de-facto standard MagicaVoxel
/// emits and dot_vox's tests).
const VOX_VERSION: i32 = 150;

/// Serialize a [`VoxScene`] into `.vox` file bytes.
///
/// Returns `None` if the scene is unrepresentable (any axis `> 256`, since voxel
/// coordinates are single bytes). Callers crop/clamp before serializing.
pub fn write_vox(scene: &VoxScene) -> Option<Vec<u8>> {
    if scene.size.iter().any(|&d| d == 0 || d > 256) {
        return None;
    }

    // ── SIZE chunk content ────────────────────────────────────────────────
    let mut size_content = Vec::with_capacity(12);
    for &d in &scene.size {
        size_content.extend_from_slice(&(d as i32).to_le_bytes());
    }

    // ── XYZI chunk content ────────────────────────────────────────────────
    let mut xyzi_content = Vec::with_capacity(4 + scene.voxels.len() * 4);
    xyzi_content.extend_from_slice(&(scene.voxels.len() as i32).to_le_bytes());
    for v in &scene.voxels {
        // File color index = slot + 1 (dot_vox subtracts 1 on read; MagicaVoxel
        // palette index 0 is reserved). Saturate so slot 255 still fits a byte.
        let file_index = v.palette_slot.saturating_add(1);
        xyzi_content.extend_from_slice(&[v.x, v.y, v.z, file_index]);
    }

    // ── RGBA chunk content (256 entries) ──────────────────────────────────
    // File palette position `s` holds the color for editor slot `s` (which is
    // referenced by file color index `s + 1`). Fully opaque; unused tail is
    // left transparent-black, which MagicaVoxel/dot_vox both tolerate.
    let mut rgba_content = vec![0u8; 256 * 4];
    for (s, rgb) in scene.palette.iter().enumerate() {
        if s >= 256 {
            break;
        }
        let base = s * 4;
        rgba_content[base] = rgb[0];
        rgba_content[base + 1] = rgb[1];
        rgba_content[base + 2] = rgb[2];
        rgba_content[base + 3] = 0xff;
    }

    // ── Assemble children, then MAIN, then the file ───────────────────────
    let mut children = Vec::new();
    push_chunk(&mut children, b"SIZE", &size_content, &[]);
    push_chunk(&mut children, b"XYZI", &xyzi_content, &[]);
    push_chunk(&mut children, b"RGBA", &rgba_content, &[]);

    let mut out = Vec::new();
    out.extend_from_slice(b"VOX ");
    out.extend_from_slice(&VOX_VERSION.to_le_bytes());
    // MAIN has empty content and all the above as its children block.
    push_chunk(&mut out, b"MAIN", &[], &children);
    Some(out)
}

/// Append one chunk (`id`, content bytes, children bytes) to `out` in the
/// format's header layout: id, content length, children length, content,
/// children. Lengths are little-endian i32.
fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], content: &[u8], children: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(content.len() as i32).to_le_bytes());
    out.extend_from_slice(&(children.len() as i32).to_le_bytes());
    out.extend_from_slice(content);
    out.extend_from_slice(children);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_scene() -> VoxScene {
        VoxScene {
            size: [2, 1, 1],
            voxels: vec![
                VoxVoxel {
                    x: 0,
                    y: 0,
                    z: 0,
                    palette_slot: 0,
                },
                VoxVoxel {
                    x: 1,
                    y: 0,
                    z: 0,
                    palette_slot: 1,
                },
            ],
            palette: vec![[10, 20, 30], [40, 50, 60]],
        }
    }

    #[test]
    fn header_and_main_present() {
        let bytes = write_vox(&sample_scene()).expect("write");
        assert_eq!(&bytes[0..4], b"VOX ");
        assert_eq!(i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]), 150);
        // MAIN chunk id immediately follows the 8-byte file header.
        assert_eq!(&bytes[8..12], b"MAIN");
    }

    #[test]
    fn rejects_oversize() {
        let mut s = sample_scene();
        s.size = [257, 1, 1];
        assert!(write_vox(&s).is_none());
    }

    /// The whole point of the writer: a file we emit must read back through
    /// `dot_vox` (the game's loader) with the right coords AND the right colors.
    #[test]
    fn roundtrips_through_dot_vox() {
        let bytes = write_vox(&sample_scene()).expect("write");
        let data = dot_vox::load_bytes(&bytes).expect("dot_vox parse");
        let model = &data.models[0];
        assert_eq!(model.size.x, 2);
        assert_eq!(model.size.y, 1);
        assert_eq!(model.size.z, 1);
        assert_eq!(model.voxels.len(), 2);

        // Voxel at x=0 was written with palette_slot 0 → file index 1 → dot_vox
        // in-memory i = 0 → palette[0] == our first color.
        let v0 = model.voxels.iter().find(|v| v.x == 0).expect("v0");
        let c0 = data.palette[v0.i as usize];
        assert_eq!((c0.r, c0.g, c0.b), (10, 20, 30));

        // Voxel at x=1 → slot 1 → file index 2 → i = 1 → palette[1].
        let v1 = model.voxels.iter().find(|v| v.x == 1).expect("v1");
        let c1 = data.palette[v1.i as usize];
        assert_eq!((c1.r, c1.g, c1.b), (40, 50, 60));
    }
}
