//! mapterm: a map in your terminal. This is the library behind the `mapterm` viewer.
//!
//! - [`source::Source`] reads tiles from PMTiles (file or HTTP range requests), XYZ URLs or
//!   TileJSON, with timeouts, size caps and a disk cache.
//! - [`store::prepare`] decodes MVT and styles features for the Protomaps or OpenMapTiles schema.
//! - [`layer`] a Leaflet-style layer stack: [`MapView`] draws a camera and `&[&dyn Layer]`;
//!   [`VectorTiles`], [`geojson::GeoJson`] and [`Markers`] are layers, and so is anything that
//!   implements [`Layer`]. [`pick_all`] finds features under a spot, top layer first.
//! - [`interact::Controller`] MapLibre-style interaction: wheel zoom, drag with inertia,
//!   double-click and box zoom, eased keyboard moves, and click reporting for inspection.
//! - [`pick::pick`] finds the features under a spot (drawn or not) with their properties and
//!   measurements, for inspection and analysis.
//! - [`raster`] PNG/JPEG XYZ or TMS layers under the vector map, drawn as images with the
//!   [`kitty`] graphics protocol or as cell colors in any terminal.
//! - [`geojson`] GeoJSON files or URLs as overlay layers of points, lines and polygons.
//! - [`smooth`] opt-in pixel smooth vertical panning for the Ghostty smooth-scroll fork.

pub mod geojson;
pub mod interact;
pub mod kitty;
pub mod layer;
pub mod mvt;
pub mod pbf;
pub mod pick;
pub mod pmtiles;
pub mod raster;
pub mod render;
pub mod smooth;
pub mod source;
pub mod store;
pub mod style;

pub use interact::{Click, Controller};
pub use pick::{Pick, pick, pick_dot, pick_lonlat};
pub use layer::{DrawContext, Layer, MapView, Markers, PickContext, VectorTiles, pick_all};
pub use render::{Camera, Overlay, Viewport};
pub use source::Source;
pub use store::{TileId, TileStore, prepare};
pub use style::{Style, Theme};
