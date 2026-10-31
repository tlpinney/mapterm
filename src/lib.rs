//! mapterm: a map in your terminal. This is the library behind the `mapterm` viewer.
//!
//! - [`source::Source`] reads tiles from PMTiles (file or HTTP range requests), XYZ URLs or
//!   TileJSON, with timeouts, size caps and a disk cache.
//! - [`store::prepare`] decodes MVT and styles features for the Protomaps or OpenMapTiles schema.
//! - [`render::MapWidget`] draws them with ratatui: braille lines, filled areas, labels and
//!   host overlays (markers, polylines).
//! - [`interact::Controller`] MapLibre-style interaction: wheel zoom, drag with inertia,
//!   double-click and box zoom, eased keyboard moves, and click reporting for inspection.
//! - [`smooth`] opt-in pixel smooth vertical panning for the Ghostty smooth-scroll fork.

pub mod interact;
pub mod mvt;
pub mod pbf;
pub mod pmtiles;
pub mod render;
pub mod smooth;
pub mod source;
pub mod store;
pub mod style;

pub use interact::{Click, Controller};
pub use render::{Camera, MapWidget, Overlay, Viewport};
pub use source::Source;
pub use store::{TileId, TileStore, prepare};
pub use style::{Style, Theme};
