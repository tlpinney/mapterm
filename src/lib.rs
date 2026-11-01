//! mapterm: a map in your terminal. This is the library behind the `mapterm` viewer.
//!
//! - [`source::Source`] reads tiles from PMTiles (file or HTTP range requests), XYZ URLs or
//!   TileJSON, with timeouts, size caps and a disk cache.
//! - [`stylejson`] loads MapLibre style documents and has built-in terminal styles for the
//!   OpenMapTiles and Protomaps schemas.
//! - [`map::Map`] a map, MapLibre GL style: named sources (vector tiles, GeoJSON) drawn by an
//!   ordered list of [`spec::StyleLayer`]s whose paint and filters are [`expr`] expressions,
//!   plus [`CustomLayer`]s for anything else. [`Map::view`] draws it and
//!   [`Map::query_rendered_features`] says what's under a spot.
//! - [`events`] layer events (click, enter, leave) and hover highlights with feature state.
//! - [`layer`] custom layers: draw anything through a [`DrawContext`]; [`Markers`] is one.
//! - [`interact::Controller`] MapLibre-style interaction: wheel zoom, drag with inertia,
//!   double-click and box zoom, eased keyboard moves, and click reporting for inspection.
//! - [`Map::query_features`] finds every feature under a spot (drawn or not) with its
//!   properties and measurements ([`Pick`]), for inspection and analysis.
//! - [`raster`] PNG/JPEG/WebP XYZ, TMS or PMTiles sources for raster layers, drawn as images
//!   with the [`kitty`] graphics protocol ([`Map::kitty_commands`]) or as cell colors.
//! - [`geocode`] place search with Photon or Nominatim (OpenStreetMap geocoders).
//! - [`geojson`] GeoJSON files or URLs, and ready-made overlay style layers for them.
//! - [`export`] a drawn map as plain text, ANSI-colored text or HTML, for snapshots.
//! - [`smooth`] opt-in pixel smooth vertical panning for the Ghostty smooth-scroll fork.

pub mod export;
pub mod cluster;
pub mod events;
pub mod expr;
pub mod formats;
pub mod geocode;
pub mod geojson;
pub mod icons;
pub mod index;
pub mod interact;
pub mod kitty;
pub mod layer;
pub mod map;
pub mod mvt;
pub mod pbf;
pub mod pick;
pub mod pmtiles;
pub mod raster;
pub mod render;
pub mod smooth;
pub mod source;
pub mod spec;
pub mod store;
pub mod stylejson;

pub use interact::{Click, Controller};
pub use pick::Pick;
pub use layer::{CustomLayer, DrawContext, Markers, PickContext};
pub use map::{KittyState, Map, MapSource, MapView, RasterMode, TileRequest};
pub use spec::StyleLayer;
pub use render::{Camera, Overlay, Viewport};
pub use source::Source;
pub use store::TileId;
