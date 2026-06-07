//! The editor's working color palette.
//!
//! A [`Palette`] is an ordered list of [`PaletteColor`]s; a sub-voxel stores a
//! `u8` index into it (see [`Voxel`](crate::volume::Voxel)). The default palette
//! is seeded from the **in-game** [`TerrainMaterialId`] block colors (via
//! `inf3d_world`) so a model painted in the editor reads in the same hues the
//! game renders its blocks in — the requested fidelity link. The user can add
//! custom colors on top; the palette is persisted in the `.ron` sidecar and the
//! colors are written into the exported `.vox` RGBA chunk.

use inf3d_world::{TerrainMaterialId, BUILDABLE};

/// Maximum palette entries. The MagicaVoxel `.vox` format has 256 palette slots
/// and dot_vox maps voxel color index `c` to `palette[c-1]`, leaving 255 usable
/// — we cap well under that.
pub const MAX_COLORS: usize = 255;

/// One palette entry: an sRGB color plus a label for the UI.
///
/// This is the in-memory form. The persisted form lives in
/// [`crate::io::rig`] (a serde struct with an owned `String` name), so the
/// runtime palette can keep a cheap `&'static str` label and not fight serde's
/// borrowed-lifetime rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PaletteColor {
    /// sRGB red/green/blue, `0..=255`.
    pub rgb: [u8; 3],
    /// Short display name (e.g. the source block name, or `"Custom"`).
    pub name: &'static str,
}

/// An ordered palette. Index `0` is always a valid color so a freshly created
/// model has something to paint with.
#[derive(Clone, Debug)]
pub struct Palette {
    colors: Vec<PaletteColor>,
}

impl Palette {
    /// The default palette: every in-game buildable block color first (so editor
    /// models match the game's hues), then a few neutral greys for shading. The
    /// labels come straight from the game's [`TerrainMaterialId::label`].
    pub fn default_game() -> Self {
        let mut colors: Vec<PaletteColor> = BUILDABLE
            .iter()
            .map(|&m| PaletteColor {
                rgb: m.color(),
                name: block_label(m),
            })
            .collect();
        // A handful of neutral ramps for free-form modeling on top of the block
        // palette (skin/cloth/metal get mixed from these + custom colors).
        for (rgb, name) in [
            ([0xff, 0xff, 0xff], "White"),
            ([0xc8, 0xc8, 0xc8], "Light Grey"),
            ([0x80, 0x80, 0x80], "Grey"),
            ([0x40, 0x40, 0x40], "Dark Grey"),
            ([0x12, 0x12, 0x12], "Near Black"),
            ([0xc8, 0x8a, 0x5a], "Skin"),
            ([0x8a, 0x2a, 0x2a], "Red"),
            ([0x2a, 0x6a, 0x2a], "Green"),
            ([0x2a, 0x3a, 0x8a], "Blue"),
            ([0xd8, 0xb0, 0x30], "Gold"),
        ] {
            colors.push(PaletteColor { rgb, name });
        }
        Self { colors }
    }

    /// Rebuild a palette from persisted sRGB triples (on load). Entries that
    /// match a known game-block color keep that block's label; the rest are
    /// labeled `"Loaded"`. Always yields at least one color so the result is a
    /// valid palette even from an empty/short input.
    pub fn from_rgbs(rgbs: &[[u8; 3]]) -> Self {
        let mut colors: Vec<PaletteColor> = rgbs
            .iter()
            .map(|&rgb| PaletteColor {
                rgb,
                name: game_label_for(rgb).unwrap_or("Loaded"),
            })
            .collect();
        if colors.is_empty() {
            colors.push(PaletteColor {
                rgb: [0xff, 0xff, 0xff],
                name: "White",
            });
        }
        Self { colors }
    }

    /// The palette as a flat list of sRGB triples, in index order — the form the
    /// `.ron` sidecar persists and the `.vox` exporter writes into its RGBA chunk.
    pub fn rgbs(&self) -> Vec<[u8; 3]> {
        self.colors.iter().map(|c| c.rgb).collect()
    }

    /// Number of colors in the palette. A valid palette is never empty (it always
    /// keeps at least one color), so there is deliberately no `is_empty`.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.colors.len()
    }

    /// Borrow the color at `index`, if in range.
    pub fn get(&self, index: u8) -> Option<PaletteColor> {
        self.colors.get(index as usize).copied()
    }

    /// The sRGB of a color index, falling back to magenta for an out-of-range
    /// index so a bad reference is visible rather than silent.
    pub fn rgb(&self, index: u8) -> [u8; 3] {
        self.get(index).map(|c| c.rgb).unwrap_or([0xff, 0x00, 0xff])
    }

    /// Iterate `(index, color)` pairs for the UI swatch grid.
    pub fn iter(&self) -> impl Iterator<Item = (u8, PaletteColor)> + '_ {
        self.colors
            .iter()
            .enumerate()
            .map(|(i, &c)| (i as u8, c))
    }

    /// Append a custom color, returning its new index, or `None` if the palette
    /// is full.
    pub fn push_custom(&mut self, rgb: [u8; 3]) -> Option<u8> {
        if self.colors.len() >= MAX_COLORS {
            return None;
        }
        self.colors.push(PaletteColor {
            rgb,
            name: "Custom",
        });
        Some((self.colors.len() - 1) as u8)
    }

    /// Overwrite the sRGB of an existing entry (the UI color picker edits in
    /// place). No-op for an out-of-range index.
    pub fn set_rgb(&mut self, index: u8, rgb: [u8; 3]) {
        if let Some(c) = self.colors.get_mut(index as usize) {
            c.rgb = rgb;
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::default_game()
    }
}

/// Map a buildable material to its `'static` label. The game's
/// [`TerrainMaterialId::label`] already returns `&'static str`, so this is just a
/// thin forwarder that keeps the palette's color labels in sync with the game.
fn block_label(m: TerrainMaterialId) -> &'static str {
    m.label()
}

/// If `rgb` exactly matches an in-game buildable block color, return that
/// block's `'static` label so a loaded palette re-labels known game colors;
/// otherwise `None`. Used by [`Palette::from_rgbs`].
fn game_label_for(rgb: [u8; 3]) -> Option<&'static str> {
    BUILDABLE
        .iter()
        .find(|m| m.color() == rgb)
        .map(|&m| block_label(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_starts_with_game_blocks() {
        let p = Palette::default_game();
        // First entries mirror the in-game BUILDABLE set in order.
        assert!(p.len() >= BUILDABLE.len());
        for (i, &m) in BUILDABLE.iter().enumerate() {
            assert_eq!(p.rgb(i as u8), m.color());
        }
    }

    #[test]
    fn push_and_edit_custom() {
        let mut p = Palette::default_game();
        let before = p.len();
        let idx = p.push_custom([1, 2, 3]).expect("room");
        assert_eq!(p.len(), before + 1);
        assert_eq!(p.rgb(idx), [1, 2, 3]);
        p.set_rgb(idx, [9, 9, 9]);
        assert_eq!(p.rgb(idx), [9, 9, 9]);
    }

    #[test]
    fn out_of_range_rgb_is_magenta() {
        let p = Palette::default_game();
        assert_eq!(p.rgb(250), [0xff, 0x00, 0xff]);
    }
}
