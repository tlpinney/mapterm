//! mapterm: a map in your terminal. This is the library behind the `mapterm` viewer: a
//! [`Map`] holds named sources and an ordered list of style layers, and draws as a
//! [ratatui](https://ratatui.rs) widget with braille lines, cell-color fills and
//! collision-free labels.
//!
//! ```
//! use mapterm::{Camera, Map, MapSource, StyleLayer};
//! use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
//! use serde_json::json;
//!
//! let mut map = Map::new();
//! map.add_source("sites", MapSource::geojson(&json!({
//!     "type": "Feature", "properties": {"name": "Washington Monument"},
//!     "geometry": {"type": "Point", "coordinates": [-77.0353, 38.8895]}
//! }))?)?;
//! map.add_layer(StyleLayer::symbol("sites", "sites").glyph("◆").text("{name}").color("cyan"))?;
//!
//! // Draw it 60x10 cells, centered on the monument at zoom 15.
//! let area = Rect::new(0, 0, 60, 10);
//! let mut buf = Buffer::empty(area);
//! map.view(&Camera::new(-77.0353, 38.8895, 15.0)).render(area, &mut buf);
//! let row: String = (0..60).map(|x| buf[(x, 5)].symbol().to_string()).collect();
//! assert!(row.contains("◆ Washington Monument"));
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! For a basemap, [`Map::from_style`] loads any MapLibre style (`"liberty"` and the other
//! OpenFreeMap styles by name, a URL or a file), or [`Map::terminal`] draws a vector tile
//! [`Source`] with mapterm's built-in terminal style. Tiles load with [`Map::load_tiles`], or
//! in the background with [`Map::tile_requests`] and [`Map::insert_loaded`].
//! `examples/hello_map.rs` is a complete pan-and-zoom app in about 60 lines.
//!
//! ## The map
//!
//! - [`map`]: [`Map`], its sources ([`MapSource`]: vector tiles, GeoJSON, raster), style and
//!   custom layers, feature state, [`Map::query_rendered_features`] and
//!   [`Map::query_features`] (every feature under a spot, drawn or not, as [`Pick`]s).
//! - [`spec`]: [`StyleLayer`] and the style properties a terminal can show (a table in its docs).
//! - [`expr`]: MapLibre expressions and legacy filters.
//! - [`stylejson`]: MapLibre style documents and the built-in terminal styles.
//! - [`layer`]: custom layers drawing through a [`DrawContext`]; [`Markers`] is one.
//! - [`icons`]: style icons as one-cell symbols.
//!
//! ## Interaction
//!
//! - [`interact`]: [`Controller`], MapLibre-style mouse and keyboard handling (wheel zoom,
//!   drag with inertia, double-click and box zoom, eased keys, `fly_to`, `fit_bounds`).
//! - [`events`]: layer click, enter and leave events, and hover highlights with feature state.
//! - [`geocode`]: place search with Photon or Nominatim.
//!
//! ## Data
//!
//! - [`source`]: vector tiles from PMTiles (file or HTTP range requests), XYZ URLs or
//!   TileJSON, with timeouts, size caps and a disk cache.
//! - [`raster`]: image tile sources (XYZ, TMS, PMTiles, named providers) for raster layers,
//!   drawn as [`kitty`] graphics ([`Map::kitty_commands`]) or as cell colors.
//! - [`geojson`]: GeoJSON files or URLs, and ready-made overlay style layers for them.
//! - [`formats`]: CSV, GPX, KML, FlatGeobuf ([`fgb`]) and Shapefiles ([`shp`]) as GeoJSON.
//! - [`index`] and [`cluster`]: the spatial index and point clustering behind GeoJSON sources.
//!
//! ## Output and the rest
//!
//! - [`export`]: a drawn map as plain text, ANSI-colored text or HTML.
//! - [`smooth`]: opt-in pixel-smooth panning for the Ghostty smooth-scroll fork.
//! - [`render`]: [`Camera`], [`Viewport`] and Web Mercator helpers.
//! - Lower level: [`mvt`] (vector tile decoding), [`pbf`] (protobuf), [`pmtiles`] (the
//!   archive format), [`store`] (decoded features), [`pick`] (hit testing and measurements).

#![warn(missing_docs)]

pub mod export;
pub mod cluster;
pub mod events;
pub mod expr;
pub mod fgb;
pub mod formats;
pub mod geocode;
pub mod geojson;
pub mod icons;
pub mod index;
pub mod interact;
pub mod kitty;
pub mod layer;
pub mod map;
pub mod mapbox;
pub mod mvt;
pub mod pbf;
pub mod pick;
pub mod pmtiles;
pub mod raster;
pub mod render;
pub mod shp;
pub mod smooth;
pub mod source;
pub mod spec;
pub mod store;
pub mod stylejson;

pub use interact::{Click, Controller};
pub use pick::Pick;
pub use layer::{CustomLayer, DrawContext, Markers, PickContext};
pub use map::{KittyState, LabelPlacement, Map, MapSource, MapView, RasterMode, TileRequest};
pub use spec::StyleLayer;
pub use render::{Camera, Overlay, Viewport};
pub use source::Source;
pub use store::TileId;
