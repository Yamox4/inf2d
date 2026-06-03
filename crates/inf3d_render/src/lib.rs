//! Rendering & visual-effects crate: water, fog, dust, hover highlight, foliage,
//! custom cursor.
mod cursor;
mod dust;
mod fog;
mod foliage;
mod highlight;
mod water;

pub use cursor::CursorPlugin;
pub use dust::{DustBurst, DustPlugin};
pub use fog::FogPlugin;
pub use foliage::{FoliagePlugin, FoliageTile};
pub use highlight::{HighlightPlugin, Hover};
pub use water::WaterPlugin;
