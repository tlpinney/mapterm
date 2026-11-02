//! A map, MapLibre GL style: named sources (vector tiles, GeoJSON) plus an ordered list of
//! style layers that say how to draw them, with custom layers for anything else. The app
//! changes the map with `set_data`, `set_paint`, `set_filter` and friends, draws it with
//! [`Map::view`], and asks what's under the pointer with [`Map::query_rendered_features`].
//!
//! ```no_run
//! # use mapterm::{Camera, Map, MapSource, Source, StyleLayer, source::OPENFREEMAP};
//! # async fn demo(mut term: ratatui::DefaultTerminal, route_json: serde_json::Value) -> anyhow::Result<()> {
//! # let cam = Camera::new(-77.03, 38.89, 13.0);
//! let mut map = Map::new();
//! map.add_source("basemap", MapSource::vector(Source::open(OPENFREEMAP).await?))?;
//! map.add_source("route", MapSource::geojson(&route_json)?)?;
//! map.add_layer(StyleLayer::background("bg").color("#101418"))?;
//! map.add_layer(StyleLayer::line("roads", "basemap").source_layer("transportation").color("#666"))?;
//! map.add_layer(StyleLayer::line("route", "route").color("#00e5ff"))?;
//! loop {
//!     let area = term.get_frame().area();
//!     map.load_tiles(&cam, area).await?;
//!     term.draw(|frame| frame.render_widget(map.view(&cam), area))?;
//!     // What's under a spot: braille dots in the map area (2 per column, 4 per row).
//!     for f in map.query_rendered_features(&cam, area, (40.0, 40.0), Some(&["route"])) {
//!         println!("{}", f.title());
//!     }
//! #   break;
//! }
//! # Ok(())
//! # }
//! ```

use std::{
    any::Any,
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use anyhow::{Result, anyhow, bail};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style as TStyle},
    widgets::Widget,
};
use serde_json::Value as Json;
use unicode_width::UnicodeWidthStr;

use crate::{
    expr::{Ctx, Rgba, Val},
    geojson,
    layer::{CustomLayer, DrawContext, PickContext},
    mvt::GeomType,
    pick::{Pick, hit, stats},
    raster::{RasterSource, RasterStore, RasterTile},
    render::{Camera, Viewport, to_lonlat, to_world},
    source::Source,
    spec::{Kind, StyleLayer},
    store::{self, TileId},
};

/// How many decoded tiles a vector source keeps.
const TILE_CACHE: usize = 512;

/// Priority room per layer: symbols in higher layers win collisions, then lower sort keys.
const LAYER_PRIORITY: i32 = 10_000;

/// A source of features for style layers.
pub enum MapSource {
    /// Vector tiles (MVT), from PMTiles, XYZ or TileJSON.
    Vector(VectorSource),
    /// GeoJSON (or CSV, GPX, KML, FlatGeobuf turned into GeoJSON).
    GeoJson(GeoJsonSource),
    /// Image tiles for raster layers.
    Raster(RasterTiles),
}

impl MapSource {
    /// Vector tiles fetched from `src` by [`Map::load_tiles`].
    pub fn vector(src: Source) -> Self {
        let attribution = src.attribution.as_deref().map(crate::source::plain_attribution);
        MapSource::Vector(VectorSource {
            min_zoom: src.min_zoom,
            max_zoom: src.max_zoom,
            attribution,
            promote_id: None,
            fetch: Some(Arc::new(src)),
            tiles: Tiles::default(),
        })
    }

    /// Vector tiles the app supplies itself with [`Map::insert_tile`].
    pub fn vector_tiles(min_zoom: u8, max_zoom: u8) -> Self {
        MapSource::Vector(VectorSource { min_zoom, max_zoom, attribution: None, promote_id: None, fetch: None, tiles: Tiles::default() })
    }

    /// The property used as feature id, if set (MapLibre's `promoteId`).
    pub fn promote_id(&self) -> Option<&str> {
        match self {
            MapSource::Vector(v) => v.promote_id.as_deref(),
            MapSource::GeoJson(g) => g.promote_id.as_deref(),
            MapSource::Raster(_) => None,
        }
    }

    /// Cluster a GeoJSON source's points (MapLibre's `cluster: true`, `clusterRadius`,
    /// `clusterMaxZoom`). Cluster features have `cluster`, `cluster_id`, `point_count` and
    /// `point_count_abbreviated` properties.
    pub fn with_cluster(mut self, options: crate::cluster::ClusterOptions) -> Self {
        if let MapSource::GeoJson(g) = &mut self {
            g.set_cluster(Some(options));
        }
        self
    }

    /// Use a property as the feature id (MapLibre's `promoteId`), for feature state.
    pub fn with_promote_id(mut self, prop: impl Into<String>) -> Self {
        match &mut self {
            MapSource::Vector(v) => v.promote_id = Some(prop.into()),
            MapSource::GeoJson(g) => g.promote_id = Some(prop.into()),
            MapSource::Raster(_) => {}
        }
        self
    }

    /// The source's credit as plain text, if it has one.
    pub fn attribution(&self) -> Option<&str> {
        match self {
            MapSource::Vector(v) => v.attribution.as_deref(),
            MapSource::GeoJson(g) => g.attribution.as_deref(),
            MapSource::Raster(r) => Some(r.credit.as_deref().unwrap_or(r.src.attribution.as_str())),
        }
        .filter(|a| !a.trim().is_empty())
    }

    /// A GeoJSON document (FeatureCollection, Feature or geometry).
    pub fn geojson(doc: &Json) -> Result<Self> {
        Ok(MapSource::GeoJson(GeoJsonSource::parse(doc)?))
    }

    /// Image tiles (PNG, JPEG, WebP) for raster layers.
    pub fn raster(src: Arc<RasterSource>) -> Self {
        MapSource::Raster(RasterTiles { src, tiles: RasterStore::default(), credit: None })
    }
}

/// A raster source and its decoded tiles.
pub struct RasterTiles {
    /// Where the tiles come from.
    pub src: Arc<RasterSource>,
    tiles: RasterStore,
    /// Credit set with [`Map::set_attribution`], over the source's own (which can't change
    /// once the source is shared).
    credit: Option<String>,
}

/// How labels and markers are placed from one frame to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LabelPlacement {
    /// Symbols shown last frame are placed first and keep their spot when it still fits, so
    /// labels don't flicker or hop between spots while the map moves (as in MapLibre).
    #[default]
    Sticky,
    /// Every frame is placed from scratch, by priority alone.
    Fresh,
}

/// How raster layers are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum RasterMode {
    /// One averaged color per cell background; works in any terminal.
    #[default]
    Cells,
    /// Real images with the kitty graphics protocol, placed under the text by
    /// [`Map::kitty_commands`]. `cell_px` is the cell size in pixels.
    Kitty {
        /// Cell width and height in pixels.
        cell_px: (f64, f64),
    },
}

/// A tile a view needs, to load in the background with [`TileRequest::load`] and hand back
/// with [`Map::insert_loaded`].
pub struct TileRequest {
    /// The source id.
    pub source: String,
    /// The tile.
    pub id: TileId,
    fetch: Fetch,
}

enum Fetch {
    Vector(Arc<Source>),
    Raster(Arc<RasterSource>),
}

/// A fetched and decoded tile.
pub struct Loaded(LoadedTile);

enum LoadedTile {
    Vector(Vec<store::Feature>),
    Raster(Option<RasterTile>),
}

impl TileRequest {
    /// Whether it is an image tile (image servers often ask for fewer connections).
    pub fn is_raster(&self) -> bool {
        matches!(self.fetch, Fetch::Raster(_))
    }

    /// Fetch and decode the tile (decoding runs on a blocking thread).
    pub async fn load(&self) -> Result<Loaded> {
        let (z, x, y) = self.id;
        Ok(Loaded(match &self.fetch {
            Fetch::Vector(src) => match src.tile(z, x, y).await? {
                Some(b) => LoadedTile::Vector(tokio::task::spawn_blocking(move || store::decode_features(&b)).await??),
                None => LoadedTile::Vector(Vec::new()),
            },
            Fetch::Raster(src) => match src.tile(z, x, y).await? {
                Some(b) => LoadedTile::Raster(Some(tokio::task::spawn_blocking(move || crate::raster::decode(&b)).await??)),
                None => LoadedTile::Raster(None),
            },
        }))
    }
}

/// The images [`Map::kitty_commands`] has sent to the terminal: their decoded size and the
/// last frame that used each.
///
/// Terminals keep a limited amount of image memory, counted in decoded RGBA bytes (herdr: 64
/// MiB per pane; Ghostty: 320 MB) and evict on their own when it runs out, which can drop a
/// tile still on screen that mapterm believes is loaded. So mapterm keeps under `budget`
/// itself, freeing the least recently used images first.
pub struct KittyState {
    sent: HashMap<u32, (usize, u64)>,
    frame: u64,
    /// Most decoded image bytes to keep in the terminal (default 48 MiB, under herdr's 64 MiB;
    /// `MAPTERM_KITTY_BUDGET_MB` overrides it in the viewer).
    pub budget: usize,
}

impl Default for KittyState {
    fn default() -> Self {
        Self { sent: HashMap::new(), frame: 0, budget: 48 << 20 }
    }
}

impl KittyState {
    /// Decoded bytes currently held by the terminal.
    pub fn bytes(&self) -> usize {
        self.sent.values().map(|(b, _)| b).sum()
    }
}

impl From<geojson::GeoJson> for MapSource {
    fn from(g: geojson::GeoJson) -> Self {
        MapSource::GeoJson(GeoJsonSource::new(g.features, g.bbox))
    }
}

/// A vector tile source and its loaded tiles.
pub struct VectorSource {
    /// Lowest zoom with tiles.
    pub min_zoom: u8,
    /// Highest zoom with tiles (higher zooms reuse these).
    pub max_zoom: u8,
    /// A property to use as the feature id (MapLibre's `promoteId`), for feature state.
    pub promote_id: Option<String>,
    /// Credit to show while the source is drawn (plain text).
    pub attribution: Option<String>,
    fetch: Option<Arc<Source>>,
    tiles: Tiles,
}

/// A GeoJSON source: features, a spatial index, and clusters when on.
pub struct GeoJsonSource {
    /// The features, with a spatial index so drawing and picking look only at those in view.
    data: crate::cluster::Level,
    /// [min_x, min_y, max_x, max_y] in world coordinates.
    pub bbox: [f64; 4],
    /// Credit to show while the source is drawn (plain text).
    pub attribution: Option<String>,
    /// A property to use as the feature id (MapLibre's `promoteId`), for feature state.
    pub promote_id: Option<String>,
    /// Point clusters by zoom, when clustering is on.
    clusters: Option<crate::cluster::Clusters>,
}

impl GeoJsonSource {
    /// A source of these features (`bbox` in world coordinates).
    pub fn new(features: Vec<geojson::Feature>, bbox: [f64; 4]) -> Self {
        Self { data: crate::cluster::Level::new(features), bbox, attribution: None, promote_id: None, clusters: None }
    }

    /// Parse a GeoJSON document.
    pub fn parse(doc: &Json) -> Result<Self> {
        let (features, bbox) = geojson::features(doc)?;
        Ok(Self::new(features, bbox))
    }

    /// The features as given.
    pub fn features(&self) -> &[geojson::Feature] {
        &self.data.features
    }

    /// Cluster nearby points (MapLibre's `cluster: true`); `None` turns it off.
    pub fn set_cluster(&mut self, options: Option<crate::cluster::ClusterOptions>) {
        self.clusters = options.map(|o| crate::cluster::Clusters::build(&self.data.features, o));
    }

    /// The clustering settings, if on.
    pub fn cluster_options(&self) -> Option<crate::cluster::ClusterOptions> {
        self.clusters.as_ref().map(|c| c.options)
    }

    /// The features to draw at `zoom` (clusters when on) and their index.
    fn at(&self, zoom: f64) -> &crate::cluster::Level {
        self.clusters.as_ref().and_then(|c| c.at(zoom)).unwrap_or(&self.data)
    }

    /// Features (of [`GeoJsonSource::at`]) whose boxes overlap `q` (world coordinates).
    fn near(&self, zoom: f64, q: crate::index::BBox) -> impl Iterator<Item = &geojson::Feature> {
        let level = self.at(zoom);
        level.index.search(q).into_iter().map(move |i| &level.features[i])
    }
}

/// What a style layer resolved to for one feature.
#[derive(Clone, Debug)]
enum Resolved {
    Fill {
        color: Option<Color>,
        outline: Option<Color>,
    },
    /// `width` in CSS pixels; `dash` is `line-dasharray`: on/off lengths in line widths.
    Line {
        color: Color,
        width: f64,
        dash: Option<Vec<f64>>,
    },
    /// A glyph at each point, with an optional label beside it.
    Point {
        glyph: String,
        style: TStyle,
        label: Option<(String, TStyle)>,
        priority: i32,
    },
    /// `anchors` are tried in order (`text-variable-anchor`); `offset` is `text-offset` in
    /// dots; `spacing` is `symbol-spacing` in dots, for labels repeated along lines.
    Text {
        text: String,
        style: TStyle,
        anchors: Vec<(i8, i8)>,
        offset: (f64, f64),
        along_line: bool,
        spacing: f64,
        priority: i32,
    },
}

struct Item {
    layer: u16,
    feature: u32,
    paint: Resolved,
}

/// One decoded vector tile: its features and what the style draws of them, by layer.
struct MapTile {
    features: Arc<Vec<store::Feature>>,
    /// What layers whose style ignores the zoom draw, sorted by layer.
    items: Vec<Item>,
    /// Feature indices by source-layer name.
    by_layer: HashMap<Arc<str>, Vec<u32>>,
    /// What zoom-dependent layers draw, by layer, for the zoom they were evaluated at.
    zoomed: std::sync::Mutex<ZoomedItems>,
}

/// Items of zoom-dependent layers by layer index, with the zoom they were evaluated at.
type ZoomedItems = HashMap<u16, (ZoomKey, u64, Arc<Vec<Item>>)>;

/// The zooms a zoom-dependent layer was evaluated at: filters and layout at the whole zoom
/// level, paint at the camera zoom (to 1/20 of a level).
type ZoomKey = (i64, i64);

fn zoom_key(zoom: f64) -> ZoomKey {
    (zoom.floor() as i64, (zoom * 20.0).round() as i64)
}

impl MapTile {
    fn new(features: Arc<Vec<store::Feature>>, items: Vec<Item>) -> Self {
        let mut by_layer: HashMap<Arc<str>, Vec<u32>> = HashMap::new();
        for (i, f) in features.iter().enumerate() {
            by_layer.entry(f.layer.clone()).or_default().push(i as u32);
        }
        Self { features, items, by_layer, zoomed: Default::default() }
    }

    fn items_of(&self, layer: usize) -> &[Item] {
        let lo = self.items.partition_point(|i| (i.layer as usize) < layer);
        let hi = self.items.partition_point(|i| (i.layer as usize) <= layer);
        &self.items[lo..hi]
    }

    /// What style layer `i` draws from this tile at `zoom`. Layers that ignore the zoom were
    /// evaluated when the tile loaded; the others are evaluated here, as MapLibre does:
    /// filters and layout at the whole zoom level (past the source's last tile zoom too),
    /// paint at the exact camera zoom. Kept until the zoom changes.
    /// Layers reading feature state are evaluated here too, again when the state changes.
    fn layer_items(&self, i: usize, l: &StyleLayer, zoom: f64, bg: Option<Rgba>, env: StateEnv) -> LayerItems<'_> {
        let stateful = l.uses_state();
        if !stateful && !l.uses_zoom() {
            return LayerItems::Cached(self.items_of(i));
        }
        let key = zoom_key(zoom);
        let version = if stateful { env.version } else { 0 };
        let mut cache = self.zoomed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, v, items)) = cache.get(&(i as u16))
            && *k == key
            && *v == version
        {
            return LayerItems::Zoomed(items.clone());
        }
        let mut items = Vec::new();
        if let Some(fs) = l.source_layer.as_deref().and_then(|s| self.by_layer.get(s)) {
            for &fi in fs {
                let f = &self.features[fi as usize];
                let c = Ctx { zoom: zoom.floor(), geom: f.geom, props: &f.props, state: env.of(&f.id, &f.props) };
                if let Some(paint) = resolve(l, &c, zoom, bg) {
                    items.push(Item { layer: i as u16, feature: fi, paint });
                }
            }
        }
        let items = Arc::new(items);
        cache.insert(i as u16, (key, version, items.clone()));
        LayerItems::Zoomed(items)
    }
}

enum LayerItems<'a> {
    Cached(&'a [Item]),
    Zoomed(Arc<Vec<Item>>),
}

impl std::ops::Deref for LayerItems<'_> {
    type Target = [Item];
    fn deref(&self) -> &[Item] {
        match self {
            LayerItems::Cached(s) => s,
            LayerItems::Zoomed(v) => v,
        }
    }
}

#[derive(Default)]
struct Tiles {
    map: HashMap<TileId, Arc<MapTile>>,
    order: VecDeque<TileId>,
}

impl Tiles {
    fn insert(&mut self, id: TileId, t: MapTile) {
        if self.map.insert(id, Arc::new(t)).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > TILE_CACHE {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    /// The tile, or its nearest loaded ancestor (up to 4 levels) to stand in while it loads.
    fn get_or_ancestor(&self, id: TileId) -> Option<(TileId, Arc<MapTile>)> {
        let (z, x, y) = id;
        (0..=4u8.min(z)).find_map(|k| {
            let a = (z - k, x >> k, y >> k);
            self.map.get(&a).map(|t| (a, t.clone()))
        })
    }

    /// Whether every tile the view needs at `z` is loaded itself (no ancestor standing in).
    fn complete(&self, vp: &Viewport, z: u8) -> bool {
        vp.tiles(z).iter().all(|id| self.map.contains_key(id))
    }

    /// Loaded tiles covering the view (ancestors standing in), lowest zoom first.
    fn visible(&self, vp: &Viewport, z: u8) -> Vec<(TileId, Arc<MapTile>)> {
        let mut seen = HashSet::new();
        let mut out: Vec<(TileId, Arc<MapTile>)> =
            vp.tiles(z).into_iter().filter_map(|id| self.get_or_ancestor(id)).filter(|(id, _)| seen.insert(*id)).collect();
        out.sort_by_key(|(id, _)| id.0);
        out
    }
}

/// A custom layer the map owns, which can be found again by type.
/// (`Send` so a whole map can be built on another task, e.g. while a style loads.)
trait OwnedCustom: CustomLayer + Any + Send {}
impl<T: CustomLayer + Any + Send> OwnedCustom for T {}

enum Entry {
    Style(StyleLayer),
    Custom { id: String, layer: Box<dyn OwnedCustom> },
}

impl Entry {
    fn id(&self) -> &str {
        match self {
            Entry::Style(l) => &l.id,
            Entry::Custom { id, .. } => id,
        }
    }
}

#[derive(Default)]
/// A map: sources, style and custom layers, feature state, and how raster layers draw. See the [module docs](self).
pub struct Map {
    sources: Vec<(String, MapSource)>,
    layers: Vec<Entry>,
    warnings: Vec<String>,
    /// How raster layers draw (cell colors unless set to kitty images).
    pub raster_mode: RasterMode,
    /// Pixel mode: the terminal's cell size in pixels, to draw fills and lines as an image
    /// (take each frame with [`Map::take_pixels`] and show it with kitty graphics) instead of
    /// braille. Labels and markers stay text. None (the default) draws braille.
    pub pixel_mode: Option<(f64, f64)>,
    /// The last frame drawn in pixel mode, until taken.
    pixels: std::sync::Mutex<Option<crate::pixels::PixFrame>>,
    /// Pixel mode's tile images.
    pixcache: std::sync::Mutex<crate::pixels::PixCache>,
    /// How labels and markers are placed frame to frame (sticky by default).
    pub label_placement: LabelPlacement,
    /// Where symbols were placed last frame.
    label_memory: std::sync::Mutex<crate::layer::LabelMemory>,
    /// The starting view (lon, lat, zoom) a style suggests.
    pub center: Option<(f64, f64, f64)>,
    /// When to ask again for tiles that failed to load.
    pub retry: RetryPolicy,
    /// Tiles whose last load failed: attempts so far and when to try again.
    failed: HashMap<(String, TileId), (u32, std::time::Instant)>,
    /// Feature state by source, then feature id (as text), then key.
    states: HashMap<String, HashMap<String, Vec<(String, Val)>>>,
    /// Bumped whenever feature state changes, so layers that read it are re-evaluated.
    state_version: u64,
}

/// The id a source gives a feature: its `promoteId` property when set, else the feature's id.
fn feature_id(promote: Option<&str>, id: &Option<crate::mvt::Value>, props: &[(String, crate::mvt::Value)]) -> Option<crate::mvt::Value> {
    match promote {
        Some(p) => props.iter().find(|(k, _)| k == p).map(|(_, v)| v.clone()),
        None => id.clone(),
    }
}

/// What layers reading feature state need: the source's states and `promoteId`, and the
/// state version (part of the cache key).
#[derive(Clone, Copy)]
struct StateEnv<'a> {
    states: Option<&'a HashMap<String, Vec<(String, Val)>>>,
    promote: Option<&'a str>,
    version: u64,
}

impl<'a> StateEnv<'a> {
    fn of(&self, id: &Option<crate::mvt::Value>, props: &[(String, crate::mvt::Value)]) -> &'a [(String, Val)] {
        let Some(states) = self.states else { return &[] };
        feature_id(self.promote, id, props).and_then(|id| states.get(&id.to_string())).map_or(&[], |v| v.as_slice())
    }
}

/// Retrying tiles that failed to load (timeouts, network or server errors). A tile that
/// doesn't exist ("no tile here") is not a failure: it is stored empty at once.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// Wait after the first failure; doubles with each further one.
    pub base: std::time::Duration,
    /// Longest wait between attempts.
    pub max: std::time::Duration,
    /// After this many failures in a row the tile is stored empty and not requested again
    /// (until it leaves the cache). Rate-limited tiles (HTTP 429) are never given up.
    pub attempts: u32,
}

/// Whether a tile error is the provider asking for fewer requests (HTTP 429): such tiles are
/// retried without limit, and apps can tell it apart from a real failure.
pub fn is_rate_limited(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("HTTP 429")
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { base: std::time::Duration::from_secs(2), max: std::time::Duration::from_secs(60), attempts: 5 }
    }
}

impl Map {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last frame drawn in [`Map::pixel_mode`]: the style's fills and lines as tile
    /// images (drawn once each, about 512 pixels across) and where they go, to show under the
    /// text with kitty graphics. None until a frame is drawn in pixel mode, and after it was
    /// taken.
    pub fn take_pixels(&self) -> Option<crate::pixels::PixFrame> {
        self.pixels.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Whether every vector tile a view needs is loaded (none standing in).
    fn tiles_complete(&self, cam: &Camera, area: Rect) -> bool {
        let vp = Viewport::new(cam, area);
        self.sources.iter().all(|(_, s)| match s {
            MapSource::Vector(v) => v.tiles.complete(&vp, cam.tile_zoom(v.min_zoom, v.max_zoom)),
            _ => true,
        })
    }

    /// What decides how tiles look besides their data and the zoom: the layers and which are
    /// switched on. Not their zoom ranges, so tiles from other zooms can stand in.
    fn pixel_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for e in &self.layers {
            e.id().hash(&mut h);
            if let Entry::Style(l) = e {
                l.visible.hash(&mut h);
            }
        }
        for (id, _) in &self.sources {
            id.hash(&mut h);
        }
        h.finish()
    }

    /// Pixel mode: draw (or reuse) the grid tiles over the view and plan where each goes.
    fn pixel_tiles(&self, cam: &Camera, area: Rect, cpx: (f64, f64)) -> crate::pixels::PixFrame {
        use crate::pixels::{BACKDROP_SCALE, CachedTile, DRAW_BUDGET, PIX_BUDGET, Pix, PixFrame, PixPlacement, next_image_id};
        let mut frame = PixFrame::default();
        let (cw, ch) = (cpx.0.max(1.0), cpx.1.max(1.0));
        let (sx, sy) = (cw / 2.0, ch / 4.0);
        let mut cache = self.pixcache.lock().unwrap_or_else(|e| e.into_inner());
        cache.frame += 1;
        let now = cache.frame;
        // Feature state changed (hover highlights): tiles with those features are stale. They
        // stay up until redrawn, so a highlight never blanks a tile.
        let changed = std::mem::take(&mut cache.changed);
        if !changed.is_empty() {
            for t in cache.tiles.values_mut() {
                if changed.iter().any(|(src, id)| match id {
                    Some(id) => t.touched.contains(&(src.clone(), id.clone())),
                    None => t.touched.iter().any(|(s, _)| s == src),
                }) {
                    t.stale = true;
                }
            }
        }
        let started = std::time::Instant::now();
        let zoom = cam.zoom;
        let imagery = self.layers.iter().any(|e| matches!(e, Entry::Style(l) if l.kind == Kind::Raster && l.shown_at(zoom)));
        let bg_rgb = if imagery { None } else { self.background(zoom).map(|c| c.to_u8()) };
        // A grid fine enough that each tile is about 512 pixels across (braille's tiles would
        // be thousands of pixels wide on a Retina screen).
        let gz = (cam.zoom + (sx / 2.0).log2()).floor().clamp(0.0, 24.0) as u8;
        let zkey = (cam.zoom * 64.0).round() as i64;
        let sig = self.pixel_signature();
        let vp = Viewport::new(cam, area);
        let s = crate::render::TILE_DOTS * 2f64.powf(cam.zoom);
        let n = (1u64 << gz) as f64;
        let d = s / n;
        let (aw, ah) = (area.width as f64 * cw, area.height as f64 * ch);
        // Tiles drawn at other zooms (same style), nearest zoom last, for stand-ins.
        // (distance in zoom, grid tile, its zoom, pixmap)
        type Other = (f64, (u8, u32, u32), f64, std::sync::Arc<tiny_skia::Pixmap>);
        let mut others: Vec<Other> =
            cache.tiles.iter().filter(|(k, _)| k.2 == sig).map(|(k, t)| ((k.1 as f64 / 64.0 - zoom).abs(), k.0, k.1 as f64 / 64.0, t.pixmap.clone())).collect();
        others.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (z, tx, ty) in vp.tiles(gz) {
            let key = ((z, tx, ty), zkey, sig);
            let budget = cache.budget.unwrap_or(DRAW_BUDGET);
            let image = match cache.tiles.get_mut(&key) {
                Some(t) if !t.stale || started.elapsed() >= budget => {
                    t.used = now;
                    t.image.clone()
                }
                stale_or_missing => {
                    let old = stale_or_missing.map(|t| t.image.clone());
                    // A camera that puts this tile's top-left corner at dot (0, 0).
                    let (x0, y0) = (tx as f64 / n, ty as f64 / n);
                    let (cols, rows) = ((d / 2.0).ceil() as u16 + 1, (d / 4.0).ceil() as u16 + 1);
                    let (lon, lat) = crate::render::to_lonlat(x0 + cols as f64 / s, y0 + 2.0 * rows as f64 / s);
                    let tcam = Camera::new(lon, lat, cam.zoom);
                    let rect = Rect::new(0, 0, cols, rows);
                    let (w, h) = ((d * sx).ceil() as u32, (d * sy).ceil() as u32);
                    if started.elapsed() < budget && self.tiles_complete(&tcam, rect) {
                        let Some(mut pix) = Pix::sized(w, h, cpx) else { continue };
                        let mut scratch = Buffer::empty(rect);
                        let mut touched = HashSet::new();
                        draw_layers(self, &tcam, rect, &mut scratch, Pass::Tile { pix: &mut pix, touched: &mut touched });
                        let (img, pixmap) = pix.finish(next_image_id());
                        frame.new.push(img.clone());
                        cache.bytes += img.rgba.len() + pixmap.data().len();
                        // The terminal gets the pixels once; the cache keeps the pixmap.
                        let mut kept = img.clone();
                        kept.rgba = Default::default();
                        if let Some(old) =
                            cache.tiles.insert(key, CachedTile { image: kept, pixmap: std::sync::Arc::new(pixmap), used: now, touched, stale: false })
                        {
                            cache.bytes -= (old.image.width * old.image.height * 4) as usize * 2;
                            frame.freed.push(old.image.id);
                        }
                        img
                    } else if let Some(old) = old {
                        // Stale, its data not complete: the old image stays up.
                        old
                    } else {
                        // Not drawn yet (out of time this frame, center tiles first, or its
                        // data is still loading): the backdrop shows under it.
                        frame.pending += 1;
                        continue;
                    }
                }
            };
            // Where it goes, in pixels from the map area's corner, cut to the area.
            let (dx, dy) = vp.dot(tx as f64 / n, ty as f64 / n);
            let (px, py) = (dx * sx, dy * sy);
            let (ax0, ay0) = (px.max(0.0), py.max(0.0));
            let (ax1, ay1) = ((px + image.width as f64).min(aw), (py + image.height as f64).min(ah));
            if ax1 - ax0 < 1.0 || ay1 - ay0 < 1.0 {
                continue;
            }
            let (col, row) = ((ax0 / cw).floor(), (ay0 / ch).floor());
            frame.placed.push(PixPlacement {
                id: image.id,
                col: area.x + col as u16,
                row: area.y + row as u16,
                x_off: ((ax0 - col * cw).round() as u32).min(cw as u32 - 1),
                y_off: ((ay0 - row * ch).round() as u32).min(ch as u32 - 1),
                src: ((ax0 - px).round() as u32, (ay0 - py).round() as u32, (ax1 - ax0).round().max(1.0) as u32, (ay1 - ay0).round().max(1.0) as u32),
            });
        }
        // Tiles missing: a backdrop of the whole area at low resolution, from the map's
        // background and every cached tile (any zoom, nearest last) scaled into place, for the
        // terminal to stretch over the area's cells under the tiles.
        if frame.pending > 0 {
            let (bw, bh) = ((aw * BACKDROP_SCALE).ceil().max(1.0), (ah * BACKDROP_SCALE).ceil().max(1.0));
            let (fx, fy) = (bw / aw, bh / ah);
            if let Some(mut pix) = Pix::sized(bw as u32, bh as u32, cpx) {
                if let Some(bg) = bg_rgb {
                    pix.clear(bg);
                }
                let view = {
                    let (vx0, vy0) = (vp.dot_to_lonlat(0.0, 0.0), vp.dot_to_lonlat(area.width as f64 * 2.0, area.height as f64 * 4.0));
                    let (a, b) = (crate::render::to_world(vx0.0, vx0.1), crate::render::to_world(vy0.0, vy0.1));
                    [a.0, a.1, b.0, b.1]
                };
                for (_, (oz, ox, oy), ozoom, pm) in &others {
                    let on = (1u64 << oz) as f64;
                    let (sx0, sy0, size) = (*ox as f64 / on, *oy as f64 / on, 1.0 / on);
                    if sx0 > view[2] || sy0 > view[3] || sx0 + size < view[0] || sy0 + size < view[1] {
                        continue;
                    }
                    let (dx, dy) = vp.dot(sx0, sy0);
                    let k = 2f64.powf(zoom - ozoom);
                    // Pixels per dot differ across and down (cells aren't 2:4), so scale by
                    // the backdrop's two factors; tiles were drawn with the same aspect.
                    pix.blit_xy(pm, (dx * sx * fx) as f32, (dy * sy * fy) as f32, (k * fx) as f32, (k * fy) as f32);
                }
                let (img, _) = pix.finish(next_image_id());
                frame.backdrop = Some(img);
            }
        }
        // Over budget: the least recently used tiles this frame didn't use go.
        if cache.bytes > PIX_BUDGET {
            let mut old: Vec<(u64, crate::pixels::PixKey)> = cache.tiles.iter().filter(|(_, t)| t.used < now).map(|(k, t)| (t.used, *k)).collect();
            old.sort_by_key(|(u, _)| *u);
            for (_, k) in old {
                if cache.bytes <= PIX_BUDGET {
                    break;
                }
                if let Some(t) = cache.tiles.remove(&k) {
                    cache.bytes -= (t.image.width * t.image.height * 4) as usize * 2;
                    frame.freed.push(t.image.id);
                }
            }
        }
        frame
    }

    /// The labels and markers placed in the last frame drawn: (text, or glyph and label, with
    /// lon, lat). For tests and measuring how labels move between frames.
    #[doc(hidden)]
    pub fn placed_labels(&self) -> Vec<(String, f64, f64)> {
        self.label_memory.lock().unwrap_or_else(|e| e.into_inner()).placed.clone()
    }

    /// The label and marker keys offered for placement in the last frame (placed or not).
    #[doc(hidden)]
    pub fn offered_labels(&self) -> Vec<String> {
        self.label_memory.lock().unwrap_or_else(|e| e.into_inner()).offered.clone()
    }

    // ---- sources ----

    /// Add a source under `id` (an error if the id is taken).
    pub fn add_source(&mut self, id: impl Into<String>, src: MapSource) -> Result<()> {
        let id = id.into();
        if self.source(&id).is_some() {
            bail!("source \"{id}\" already exists");
        }
        self.sources.push((id, src));
        self.restyle();
        Ok(())
    }

    /// Move another map's sources and layers into this one, its layers just below `before`
    /// (or on top): e.g. a style loaded with [`Map::from_style`] on another task, swapped in
    /// under your own layers. Returns the ids of the layers added. Nothing changes if a source
    /// or layer id is already taken.
    pub fn add_map(&mut self, other: Map, before: Option<&str>) -> Result<Vec<String>> {
        for (id, _) in &other.sources {
            if self.source(id).is_some() {
                bail!("source \"{id}\" already exists");
            }
        }
        for e in &other.layers {
            if self.index(e.id()).is_some() {
                bail!("layer \"{}\" already exists", e.id());
            }
        }
        let at = match before {
            Some(b) => self.index(b).ok_or_else(|| anyhow!("no layer \"{b}\" to add before"))?,
            None => self.layers.len(),
        };
        if self.layers.len() + other.layers.len() >= u16::MAX as usize {
            bail!("too many layers");
        }
        let ids: Vec<String> = other.layers.iter().map(|e| e.id().to_string()).collect();
        self.sources.extend(other.sources);
        self.layers.splice(at..at, other.layers);
        self.warnings.extend(other.warnings);
        self.restyle();
        Ok(ids)
    }

    /// Remove a source; layers that use it stay but draw nothing.
    pub fn remove_source(&mut self, id: &str) -> bool {
        let n = self.sources.len();
        self.sources.retain(|(s, _)| s != id);
        n != self.sources.len()
    }

    /// Every source with its id, in the order added.
    pub fn sources(&self) -> impl Iterator<Item = (&str, &MapSource)> {
        self.sources.iter().map(|(id, s)| (id.as_str(), s))
    }

    /// A source by id.
    pub fn source(&self, id: &str) -> Option<&MapSource> {
        self.sources.iter().find(|(s, _)| s == id).map(|(_, s)| s)
    }

    /// Set a source's credit (plain text, or HTML that [`crate::source::plain_attribution`]
    /// turns into text); empty removes it. Can change any time, e.g. as credits that depend
    /// on the view (Google's) are updated.
    pub fn set_attribution(&mut self, id: &str, text: &str) -> Result<()> {
        let text = crate::source::plain_attribution(text);
        let value = (!text.is_empty()).then_some(text);
        match self.sources.iter_mut().find(|(s, _)| s == id) {
            Some((_, MapSource::Vector(v))) => v.attribution = value,
            Some((_, MapSource::GeoJson(g))) => g.attribution = value,
            Some((_, MapSource::Raster(r))) => r.credit = Some(value.unwrap_or_default()),
            None => bail!("no source \"{id}\""),
        }
        Ok(())
    }

    /// Set feature state (MapLibre's `setFeatureState`): the keys of `state` (a JSON object)
    /// are merged into the feature's state, which paint expressions read with
    /// `["feature-state", key]`. `id` is the feature's id, or its `promoteId` property.
    pub fn set_feature_state(&mut self, source: &str, id: &crate::mvt::Value, state: &Json) -> Result<()> {
        if self.source(source).is_none() {
            bail!("no source \"{source}\"");
        }
        let Some(obj) = state.as_object() else { bail!("feature state must be a JSON object") };
        let entry = self.states.entry(source.to_string()).or_default().entry(id.to_string()).or_default();
        for (k, v) in obj {
            let v = crate::expr::Expr::parse(&serde_json::json!(["literal", v]))
                .map(|e| e.eval(&Ctx { zoom: 0.0, geom: GeomType::Unknown, props: &[], state: &[] }))?;
            match entry.iter_mut().find(|(key, _)| key == k) {
                Some(slot) => slot.1 = v,
                None => entry.push((k.clone(), v)),
            }
        }
        self.state_version += 1;
        self.note_state_change(source, Some(id.to_string()));
        Ok(())
    }

    /// Remove feature state: one key of one feature, all of one feature's state, or (with no
    /// id) all state of the source.
    pub fn remove_feature_state(&mut self, source: &str, id: Option<&crate::mvt::Value>, key: Option<&str>) {
        let Some(by_id) = self.states.get_mut(source) else { return };
        match (id, key) {
            (None, _) => {
                self.states.remove(source);
            }
            (Some(id), None) => {
                by_id.remove(&id.to_string());
            }
            (Some(id), Some(k)) => {
                if let Some(st) = by_id.get_mut(&id.to_string()) {
                    st.retain(|(key, _)| key != k);
                }
            }
        }
        self.state_version += 1;
        self.note_state_change(source, id.map(|i| i.to_string()));
    }

    /// A feature's state as a JSON object (empty when none is set).
    pub fn feature_state(&self, source: &str, id: &crate::mvt::Value) -> Json {
        let mut m = serde_json::Map::new();
        if let Some(st) = self.states.get(source).and_then(|s| s.get(&id.to_string())) {
            for (k, v) in st {
                m.insert(k.clone(), val_json(v));
            }
        }
        Json::Object(m)
    }

    /// Pixel mode: remember a feature's (or with None, a whole source's) state changed, so
    /// the next frame redraws the tile images it's in.
    fn note_state_change(&mut self, source: &str, id: Option<String>) {
        if self.pixel_mode.is_some() {
            self.pixcache.get_mut().unwrap_or_else(|e| e.into_inner()).changed.push((source.to_string(), id));
        }
    }

    fn state_env(&self, source: &str) -> StateEnv<'_> {
        StateEnv { states: self.states.get(source), promote: self.source(source).and_then(MapSource::promote_id), version: self.state_version }
    }

    /// The credits of the sources that the layers shown at `zoom` draw from, in source order
    /// and without repeats, as MapLibre's attribution control lists them.
    pub fn attributions(&self, zoom: f64) -> Vec<String> {
        let used: HashSet<&str> = self
            .layers
            .iter()
            .filter_map(|e| match e {
                Entry::Style(l) if l.shown_at(zoom) && l.kind != Kind::Background => l.source.as_deref(),
                _ => None,
            })
            .collect();
        let mut out: Vec<String> = Vec::new();
        for (id, s) in &self.sources {
            if let Some(a) = s.attribution().filter(|_| used.contains(id.as_str()))
                && !out.iter().any(|o| o == a)
            {
                out.push(a.to_string());
            }
        }
        out
    }

    /// The zoom at which a cluster of a clustered GeoJSON source splits into its parts
    /// (MapLibre's `getClusterExpansionZoom`): fly there when a cluster is clicked.
    pub fn cluster_expansion_zoom(&self, source: &str, cluster_id: u64) -> Option<f64> {
        match self.source(source)? {
            MapSource::GeoJson(g) => g.clusters.as_ref()?.expansion_zoom(cluster_id).map(f64::from),
            _ => None,
        }
    }

    /// Replace a GeoJSON source's data (MapLibre's `setData`).
    pub fn set_data(&mut self, id: &str, doc: &Json) -> Result<()> {
        let mut data = GeoJsonSource::parse(doc)?;
        match self.sources.iter_mut().find(|(s, _)| s == id) {
            Some((_, MapSource::GeoJson(g))) => {
                data.attribution = g.attribution.take();
                data.promote_id = g.promote_id.take();
                data.set_cluster(g.cluster_options());
                *g = data;
            }
            Some(_) => bail!("source \"{id}\" is not a GeoJSON source"),
            None => bail!("no source \"{id}\""),
        }
        Ok(())
    }

    /// Lon/lat bounds (west, south, east, north) of a GeoJSON source, for fitting the view.
    pub fn source_bounds(&self, id: &str) -> Option<(f64, f64, f64, f64)> {
        match self.source(id)? {
            MapSource::GeoJson(g) if !g.features().is_empty() => {
                let (w, n) = to_lonlat(g.bbox[0], g.bbox[1]);
                let (e, s) = to_lonlat(g.bbox[2], g.bbox[3]);
                Some((w, s, e, n))
            }
            _ => None,
        }
    }

    /// Tiles the view needs that aren't loaded yet, for sources that shown layers use and the
    /// map can fetch itself (vector sources from [`MapSource::vector`], raster sources).
    pub fn tile_requests(&self, cam: &Camera, area: Rect) -> Vec<TileRequest> {
        let vp = Viewport::new(cam, area);
        let used: HashSet<&str> = self
            .layers
            .iter()
            .filter_map(|e| match e {
                Entry::Style(l) if l.shown_at(cam.zoom) => l.source.as_deref(),
                _ => None,
            })
            .collect();
        let mut out = Vec::new();
        for (id, src) in &self.sources {
            if !used.contains(id.as_str()) {
                continue;
            }
            match src {
                MapSource::Vector(v) => {
                    let Some(f) = &v.fetch else { continue };
                    let z = cam.tile_zoom(v.min_zoom, v.max_zoom);
                    out.extend(vp.tiles(z).into_iter().filter(|t| !v.tiles.map.contains_key(t) && self.due(id, t)).map(|t| TileRequest {
                        source: id.clone(),
                        id: t,
                        fetch: Fetch::Vector(f.clone()),
                    }));
                }
                MapSource::Raster(r) => {
                    let rz = self.raster_zoom(&r.src, cam);
                    // Zoomed out past what the service renders: ask for nothing.
                    if rz < r.src.min_zoom {
                        continue;
                    }
                    out.extend(crate::raster::tiles_for(cam, area, rz).into_iter().filter(|t| !r.tiles.contains(t) && self.due(id, t)).map(|t| TileRequest {
                        source: id.clone(),
                        id: t,
                        fetch: Fetch::Raster(r.src.clone()),
                    }));
                }
                MapSource::GeoJson(_) => {}
            }
        }
        out
    }

    /// Raster sources this view is zoomed out too far for (past their
    /// [`RasterSource::min_zoom`], so no tiles are requested): each one's name and the zoom it
    /// starts at, for telling the user to zoom in.
    pub fn rasters_below_min_zoom(&self, cam: &Camera) -> Vec<(String, u8)> {
        // Only sources a layer shows at this zoom: a layer that starts at a higher zoom
        // (NAIP over Sentinel-2) is simply not on yet.
        let shown: HashSet<&str> = self
            .layers
            .iter()
            .filter_map(|e| match e {
                Entry::Style(l) if l.shown_at(cam.zoom) => l.source.as_deref(),
                _ => None,
            })
            .collect();
        self.sources
            .iter()
            .filter_map(|(id, s)| match s {
                MapSource::Raster(r) if shown.contains(id.as_str()) && self.raster_zoom(&r.src, cam) < r.src.min_zoom => {
                    Some((r.src.name.clone(), r.src.min_zoom))
                }
                _ => None,
            })
            .collect()
    }

    /// The zoom to fetch a raster source at: about 2 screen pixels per image pixel as kitty
    /// images (like a browser on a Retina screen), one cell per image pixel as cell colors.
    fn raster_zoom(&self, src: &RasterSource, cam: &Camera) -> u8 {
        let dots_per_px = match self.raster_mode {
            RasterMode::Kitty { cell_px } => 2.0 / (cell_px.0 / 2.0).max(0.5),
            RasterMode::Cells => 2.0,
        };
        crate::raster::raster_zoom(cam, src.tile_px, dots_per_px, src.max_zoom)
    }

    /// Store a tile loaded with [`TileRequest::load`]. A failed load returns its error and the
    /// tile is requested again after a growing wait ([`Map::retry`]); after the last attempt
    /// it is stored empty. Lower-zoom tiles stand in until then.
    pub fn insert_loaded(&mut self, source: &str, id: TileId, loaded: Result<Loaded>) -> Result<()> {
        let is_raster = matches!(self.source(source), Some(MapSource::Raster(_)));
        let key = (source.to_string(), id);
        let (tile, err) = match loaded {
            Ok(Loaded(t)) => {
                self.failed.remove(&key);
                (t, None)
            }
            Err(e) => {
                // A provider saying "too many requests" means later, not never: keep asking,
                // with the growing wait, rather than giving the tile up.
                let throttled = is_rate_limited(&e);
                let e = e.context(format!("{source} tile {}/{}/{}", id.0, id.1, id.2));
                let attempts = self.failed.get(&key).map_or(0, |f| f.0) + 1;
                let attempts = if throttled { attempts.min(self.retry.attempts.saturating_sub(1).max(1)) } else { attempts };
                if throttled || attempts < self.retry.attempts {
                    // Ask again later; lower-zoom tiles stand in meanwhile.
                    let wait = self.retry.base.saturating_mul(1 << (attempts - 1).min(16)).min(self.retry.max);
                    self.failed.insert(key, (attempts, std::time::Instant::now() + wait));
                    return Err(e);
                }
                // Given up: store it empty so it isn't requested again and again.
                self.failed.remove(&key);
                (if is_raster { LoadedTile::Raster(None) } else { LoadedTile::Vector(Vec::new()) }, Some(e))
            }
        };
        match tile {
            LoadedTile::Vector(features) => self.insert_features(source, id, features)?,
            LoadedTile::Raster(t) => match self.sources.iter_mut().find(|(s, _)| s == source) {
                Some((_, MapSource::Raster(r))) => r.tiles.insert(id, t),
                _ => bail!("no raster source \"{source}\""),
            },
        }
        err.map_or(Ok(()), Err)
    }

    /// Whether a tile may be requested now (it hasn't failed recently).
    fn due(&self, source: &str, id: &TileId) -> bool {
        self.failed.get(&(source.to_string(), *id)).is_none_or(|(_, at)| std::time::Instant::now() >= *at)
    }

    /// Fetch the missing tiles one at a time (simple apps; see [`Map::tile_requests`] to load
    /// in the background). Failed tiles are retried later (see [`Map::retry`]) and the first
    /// error is returned.
    pub async fn load_tiles(&mut self, cam: &Camera, area: Rect) -> Result<()> {
        let mut first_err = None;
        for req in self.tile_requests(cam, area) {
            let loaded = req.load().await;
            if let Err(e) = self.insert_loaded(&req.source, req.id, loaded) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// The zoom range of the vector sources (min of minimums, max of maximums).
    pub fn zoom_range(&self) -> (u8, u8) {
        self.sources
            .iter()
            .filter_map(|(_, s)| match s {
                MapSource::Vector(v) => Some((v.min_zoom, v.max_zoom)),
                _ => None,
            })
            .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
            .unwrap_or((0, 14))
    }

    /// Kitty graphics commands that place the shown raster layers under the text for this
    /// frame (images are sent once, then only moved). Write them after the frame's cells, in
    /// the same synchronized update. Empty unless [`Map::raster_mode`] is kitty.
    pub fn kitty_commands(&mut self, cam: &Camera, area: Rect, state: &mut KittyState) -> String {
        use crate::kitty;
        let RasterMode::Kitty { .. } = self.raster_mode else { return String::new() };
        state.frame += 1;
        let mut cmds = String::new();
        // Images the store dropped are freed from the terminal too.
        for (_, s) in &mut self.sources {
            if let MapSource::Raster(r) = s {
                for id in r.tiles.take_evicted() {
                    if state.sent.remove(&id).is_some() {
                        cmds.push_str(&kitty::free(id));
                    }
                }
            }
        }
        // This frame's pieces, bottom layer first. Pieces no bigger than 32x16 cells, which
        // herdr relays (it drops placements stretched over very large areas).
        let mut pieces = Vec::new();
        for (i, e) in self.layers.iter().enumerate() {
            let Entry::Style(l) = e else { continue };
            if l.kind != Kind::Raster || !l.shown_at(cam.zoom) {
                continue;
            }
            let Some(MapSource::Raster(r)) = l.source.as_deref().and_then(|s| self.source(s)) else { continue };
            let rz = self.raster_zoom(&r.src, cam);
            pieces.extend(crate::raster::layout_max(&r.tiles, cam, area, rz, (32, 16)).into_iter().map(|b| (i, b)));
        }
        if pieces.is_empty() && state.sent.is_empty() {
            return cmds;
        }
        // Stay under the budget before sending anything new: free the least recently used
        // images this frame doesn't show, so the terminal never evicts one it does.
        let needed: HashSet<u32> = pieces.iter().map(|(_, b)| b.tile.id).collect();
        let mut new_bytes = 0;
        let mut counted = HashSet::new();
        for (_, b) in &pieces {
            if !state.sent.contains_key(&b.tile.id) && counted.insert(b.tile.id) {
                let (w, h) = b.tile.size();
                new_bytes += w as usize * h as usize * 4;
            }
        }
        let mut total = state.bytes();
        if total + new_bytes > state.budget {
            let mut old: Vec<(u64, u32, usize)> = state.sent.iter().filter(|(id, _)| !needed.contains(id)).map(|(id, (b, f))| (*f, *id, *b)).collect();
            old.sort_unstable();
            for (_, id, b) in old {
                if total + new_bytes <= state.budget {
                    break;
                }
                state.sent.remove(&id);
                cmds.push_str(&kitty::free(id));
                total -= b;
            }
        }
        cmds.push_str(kitty::CLEAR_PLACEMENTS);
        // Higher layers stack above lower ones, all under the text.
        for (placement, (i, b)) in pieces.iter().enumerate() {
            let (w, h) = b.tile.size();
            let frame = state.frame;
            match state.sent.get_mut(&b.tile.id) {
                Some(entry) => entry.1 = frame,
                None => {
                    state.sent.insert(b.tile.id, (w as usize * h as usize * 4, frame));
                    cmds.push_str(&kitty::transmit(b.tile.id, &b.tile.png));
                }
            }
            cmds.push_str(&kitty::place(b.tile.id, placement as u32 + 1, b.cells.x, b.cells.y, b.src, b.cells.width, b.cells.height, -1000 + *i as i32));
        }
        cmds
    }

    /// Add a vector tile (MVT bytes, or `None` for an empty tile) to a vector source.
    pub fn insert_tile(&mut self, source: &str, id: TileId, bytes: Option<&[u8]>) -> Result<()> {
        let features = match bytes {
            Some(b) => store::decode_features(b),
            None => Ok(Vec::new()),
        };
        let (features, err) = match features {
            Ok(f) => (f, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        self.insert_features(source, id, features)?;
        err.map_or(Ok(()), |e| Err(e.context(format!("tile {}/{}/{}", id.0, id.1, id.2))))
    }

    fn insert_features(&mut self, source: &str, id: TileId, features: Vec<store::Feature>) -> Result<()> {
        let features = Arc::new(features);
        let items = style_tile(&self.layers, source, &features, id.0 as f64);
        match self.sources.iter_mut().find(|(s, _)| s == source) {
            Some((_, MapSource::Vector(v))) => v.tiles.insert(id, MapTile::new(features, items)),
            _ => bail!("no vector source \"{source}\""),
        }
        Ok(())
    }

    // ---- layers ----

    /// Add a style layer on top.
    pub fn add_layer(&mut self, layer: StyleLayer) -> Result<()> {
        self.insert_entry(Entry::Style(layer), None)
    }

    /// Add a style layer just below `before`.
    pub fn add_layer_before(&mut self, layer: StyleLayer, before: &str) -> Result<()> {
        self.insert_entry(Entry::Style(layer), Some(before))
    }

    /// Add a custom layer (see [`CustomLayer`]) on top, or below `before`.
    pub fn add_custom_layer<T: CustomLayer + Any + Send>(&mut self, id: impl Into<String>, layer: T, before: Option<&str>) -> Result<()> {
        self.insert_entry(Entry::Custom { id: id.into(), layer: Box::new(layer) }, before)
    }

    /// Add several style layers at once (on top, or below `before`), restyling loaded tiles
    /// only once. Nothing is added if any id is taken.
    pub fn add_layers(&mut self, layers: Vec<StyleLayer>, before: Option<&str>) -> Result<()> {
        let mut ids = HashSet::new();
        for l in &layers {
            if self.index(&l.id).is_some() || !ids.insert(l.id.as_str()) {
                bail!("layer \"{}\" already exists", l.id);
            }
        }
        if self.layers.len() + layers.len() >= u16::MAX as usize {
            bail!("too many layers");
        }
        let at = match before {
            Some(b) => self.index(b).ok_or_else(|| anyhow!("no layer \"{b}\" to add before"))?,
            None => self.layers.len(),
        };
        self.layers.splice(at..at, layers.into_iter().map(Entry::Style));
        self.restyle();
        Ok(())
    }

    /// Remove several layers at once, restyling only once. Returns how many were removed.
    pub fn remove_layers(&mut self, ids: &[&str]) -> usize {
        let n = self.layers.len();
        self.layers.retain(|e| !ids.contains(&e.id()));
        let removed = n - self.layers.len();
        if removed > 0 {
            self.restyle();
        }
        removed
    }

    fn insert_entry(&mut self, e: Entry, before: Option<&str>) -> Result<()> {
        if self.index(e.id()).is_some() {
            bail!("layer \"{}\" already exists", e.id());
        }
        if self.layers.len() >= u16::MAX as usize {
            bail!("too many layers");
        }
        let at = match before {
            Some(b) => self.index(b).ok_or_else(|| anyhow!("no layer \"{b}\" to add before"))?,
            None => self.layers.len(),
        };
        self.layers.insert(at, e);
        self.restyle();
        Ok(())
    }

    /// Remove a layer; false if there was none.
    pub fn remove_layer(&mut self, id: &str) -> bool {
        let Some(i) = self.index(id) else { return false };
        self.layers.remove(i);
        self.restyle();
        true
    }

    /// Move a layer below `before`, or to the top.
    pub fn move_layer(&mut self, id: &str, before: Option<&str>) -> Result<()> {
        let i = self.index(id).ok_or_else(|| anyhow!("no layer \"{id}\""))?;
        let e = self.layers.remove(i);
        let at = match before {
            Some(b) => match self.index(b) {
                Some(at) => at,
                None => {
                    self.layers.insert(i, e);
                    bail!("no layer \"{b}\"");
                }
            },
            None => self.layers.len(),
        };
        self.layers.insert(at, e);
        self.restyle();
        Ok(())
    }

    /// Layer ids, bottom first.
    pub fn layer_ids(&self) -> Vec<&str> {
        self.layers.iter().map(Entry::id).collect()
    }

    /// A style layer by id (`None` for custom layers).
    pub fn layer(&self, id: &str) -> Option<&StyleLayer> {
        match &self.layers[self.index(id)?] {
            Entry::Style(l) => Some(l),
            Entry::Custom { .. } => None,
        }
    }

    /// A custom layer by id and type, to change its state.
    pub fn custom_layer_mut<T: CustomLayer + Any + Send>(&mut self, id: &str) -> Option<&mut T> {
        let i = self.index(id)?;
        match &mut self.layers[i] {
            Entry::Custom { layer, .. } => (&mut **layer as &mut dyn Any).downcast_mut::<T>(),
            Entry::Style(_) => None,
        }
    }

    /// Set a paint property of a style layer (MapLibre's `setPaintProperty`).
    pub fn set_paint(&mut self, id: &str, name: &str, v: &Json) -> Result<()> {
        self.style_mut(id)?.set_paint(name, v)?;
        self.restyle();
        Ok(())
    }

    /// Set a layout property of a style layer (MapLibre's `setLayoutProperty`).
    pub fn set_layout(&mut self, id: &str, name: &str, v: &Json) -> Result<()> {
        self.style_mut(id)?.set_layout(name, v)?;
        self.restyle();
        Ok(())
    }

    /// Replace a layer's filter (`None` draws every feature).
    pub fn set_filter(&mut self, id: &str, f: Option<&Json>) -> Result<()> {
        let e = f.map(crate::expr::Expr::parse).transpose()?;
        self.style_mut(id)?.filter = e;
        self.restyle();
        Ok(())
    }

    /// Show or hide a layer.
    pub fn set_visibility(&mut self, id: &str, visible: bool) -> Result<()> {
        self.style_mut(id)?.visible = visible;
        Ok(())
    }

    /// Draw a layer only from `min` up to (not including) `max` zoom.
    pub fn set_zoom_range(&mut self, id: &str, min: f64, max: f64) -> Result<()> {
        let l = self.style_mut(id)?;
        (l.min_zoom, l.max_zoom) = (min, max);
        Ok(())
    }

    /// Problems met while building the style (unsupported expressions and the like).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn warn(&mut self, w: String) {
        self.warnings.push(w);
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.layers.iter().position(|e| e.id() == id)
    }

    fn style_mut(&mut self, id: &str) -> Result<&mut StyleLayer> {
        let i = self.index(id).ok_or_else(|| anyhow!("no layer \"{id}\""))?;
        match &mut self.layers[i] {
            Entry::Style(l) => Ok(l),
            Entry::Custom { .. } => bail!("\"{id}\" is a custom layer"),
        }
    }

    /// Re-evaluate the style for every loaded vector tile.
    fn restyle(&mut self) {
        let layers = &self.layers;
        for (sid, src) in &mut self.sources {
            if let MapSource::Vector(v) = src {
                let ids: Vec<TileId> = v.tiles.map.keys().copied().collect();
                for id in ids {
                    let features = v.tiles.map[&id].features.clone();
                    let items = style_tile(layers, sid, &features, id.0 as f64);
                    v.tiles.map.insert(id, Arc::new(MapTile::new(features, items)));
                }
            }
        }
    }

    // ---- drawing and querying ----

    /// A widget that draws the map with this camera.
    pub fn view<'a>(&'a self, camera: &'a Camera) -> MapView<'a> {
        MapView { map: self, camera }
    }

    /// Features drawn under `target` (braille dots relative to `area`: 2 per column, 4 per
    /// row), top layer first and, within a layer, points before lines before polygons.
    /// `layers` limits the search to those layer ids. Custom layers answer with their `pick`.
    pub fn query_rendered_features(&self, cam: &Camera, area: Rect, target: (f64, f64), layers: Option<&[&str]>) -> Vec<Pick> {
        let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
        if target.0 < 0.0 || target.1 < 0.0 || target.0 >= w || target.1 >= h {
            return Vec::new();
        }
        let vp = Viewport::new(cam, area);
        let bg = self.background(cam.zoom);
        let mut out = Vec::new();
        for (i, entry) in self.layers.iter().enumerate().rev() {
            if layers.is_some_and(|ids| !ids.contains(&entry.id())) {
                continue;
            }
            let l = match entry {
                Entry::Style(l) => l,
                Entry::Custom { id, layer } => {
                    let ctx = PickContext { camera: cam, area, viewport: vp, target };
                    for mut p in layer.pick(&ctx) {
                        p.style_layer.get_or_insert_with(|| Arc::from(id.as_str()));
                        out.push(p);
                    }
                    continue;
                }
            };
            if !l.shown_at(cam.zoom) || matches!(l.kind, Kind::Background | Kind::Raster | Kind::Other(_)) {
                continue;
            }
            let Some(sid) = l.source.as_deref() else { continue };
            let env = self.state_env(sid);
            let style_layer: Arc<str> = Arc::from(l.id.as_str());
            let source: Arc<str> = Arc::from(sid);
            let edges = l.kind == Kind::Line;
            let mut hits: Vec<(u8, f64, Pick)> = Vec::new();
            let mut push = |geom: GeomType,
                            dots: &[Vec<(f64, f64)>],
                            lonlat: Vec<Vec<(f64, f64)>>,
                            layer: Arc<str>,
                            props: &[(String, crate::mvt::Value)],
                            tile: Option<TileId>,
                            id: Option<crate::mvt::Value>| {
                if let Some((rank, score)) = hit(geom, dots, target, edges) {
                    let st = stats(geom, &lonlat);
                    let score = if geom == GeomType::Polygon { st.area_km2.unwrap_or(0.0) } else { score };
                    let pick = Pick {
                        tile,
                        layer,
                        geom,
                        props: props.to_vec(),
                        lonlat,
                        drawn: true,
                        stats: st,
                        source: Some(source.clone()),
                        style_layer: Some(style_layer.clone()),
                        id,
                    };
                    hits.push((rank, score, pick));
                }
            };
            match self.source(sid) {
                Some(MapSource::Vector(v)) => {
                    let z = cam.tile_zoom(v.min_zoom, v.max_zoom);
                    let n = (1u64 << z) as f64;
                    let (wx, wy) = {
                        let (lon, lat) = vp.dot_to_lonlat(target.0, target.1);
                        to_world(lon, lat)
                    };
                    let id: TileId = (z, (wx * n).floor().clamp(0.0, n - 1.0) as u32, (wy * n).floor().clamp(0.0, n - 1.0) as u32);
                    let Some((tid, tile)) = v.tiles.get_or_ancestor(id) else { continue };
                    let tn = (1u64 << tid.0) as f64;
                    let world = |p: &[f32; 2]| ((tid.1 as f64 + p[0] as f64) / tn, (tid.2 as f64 + p[1] as f64) / tn);
                    let mut seen = HashSet::new();
                    for it in tile.layer_items(i, l, cam.zoom, bg, env).iter() {
                        let f = &tile.features[it.feature as usize];
                        let dots: Vec<Vec<(f64, f64)>> = f
                            .parts
                            .iter()
                            .map(|part| {
                                part.iter()
                                    .map(|p| {
                                        let (x, y) = world(p);
                                        vp.dot(x, y)
                                    })
                                    .collect()
                            })
                            .collect();
                        if hit(f.geom, &dots, target, edges).is_none() || !seen.insert(format!("{:?}{:?}", f.geom, f.props)) {
                            continue;
                        }
                        let lonlat = f
                            .parts
                            .iter()
                            .map(|part| {
                                part.iter()
                                    .map(|p| {
                                        let (x, y) = world(p);
                                        to_lonlat(x, y)
                                    })
                                    .collect()
                            })
                            .collect();
                        push(f.geom, &dots, lonlat, f.layer.clone(), &f.props, Some(tid), feature_id(env.promote, &f.id, &f.props));
                    }
                }
                Some(MapSource::GeoJson(g)) => {
                    for f in g.near(cam.zoom, near_box(&vp, target)) {
                        let c = Ctx { zoom: cam.zoom.floor(), geom: f.geom, props: &f.props, state: env.of(&f.id, &f.props) };
                        if resolve(l, &c, cam.zoom, bg).is_none() {
                            continue;
                        }
                        let dots: Vec<Vec<(f64, f64)>> = f.parts.iter().map(|p| p.iter().map(|&(x, y)| vp.dot(x, y)).collect()).collect();
                        let lonlat = f.parts.iter().map(|p| p.iter().map(|&(x, y)| to_lonlat(x, y)).collect()).collect();
                        push(f.geom, &dots, lonlat, source.clone(), &f.props, None, feature_id(env.promote, &f.id, &f.props));
                    }
                }
                _ => {}
            }
            hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
            out.extend(hits.into_iter().map(|(_, _, p)| p));
        }
        out
    }

    /// Every feature of every source under `target` (dots relative to `area`), drawn or not,
    /// for inspecting data. Sources come in the order of their topmost layer, top first;
    /// within a source, points, then lines, then polygons from smallest to largest. Each pick's
    /// `style_layer` is the top shown layer that draws it, and `drawn` says whether one does.
    pub fn query_features(&self, cam: &Camera, area: Rect, target: (f64, f64)) -> Vec<Pick> {
        let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
        if target.0 < 0.0 || target.1 < 0.0 || target.0 >= w || target.1 >= h {
            return Vec::new();
        }
        let vp = Viewport::new(cam, area);
        let bg = self.background(cam.zoom);
        let top_layer = |sid: &str| self.layers.iter().rposition(|e| matches!(e, Entry::Style(l) if l.source.as_deref() == Some(sid)));
        let mut order: Vec<&(String, MapSource)> = self.sources.iter().collect();
        order.sort_by_key(|(id, _)| std::cmp::Reverse(top_layer(id).map_or(-1, |i| i as i64)));
        let mut out = Vec::new();
        for (sid, src) in order {
            let source: Arc<str> = Arc::from(sid.as_str());
            let env = self.state_env(sid);
            let mut hits: Vec<(u8, f64, Pick)> = Vec::new();
            let mut push = |geom: GeomType,
                            rank: u8,
                            score: f64,
                            lonlat: Vec<Vec<(f64, f64)>>,
                            layer: Arc<str>,
                            props: &[(String, crate::mvt::Value)],
                            tile: Option<TileId>,
                            by: Option<usize>,
                            id: Option<crate::mvt::Value>| {
                let st = stats(geom, &lonlat);
                let score = if geom == GeomType::Polygon { st.area_km2.unwrap_or(0.0) } else { score };
                let style_layer = by.map(|i| Arc::from(self.layers[i].id()));
                let pick =
                    Pick { tile, layer, geom, props: props.to_vec(), lonlat, drawn: by.is_some(), stats: st, source: Some(source.clone()), style_layer, id };
                hits.push((rank, score, pick));
            };
            match src {
                MapSource::Vector(v) => {
                    let z = cam.tile_zoom(v.min_zoom, v.max_zoom);
                    let n = (1u64 << z) as f64;
                    let (wx, wy) = {
                        let (lon, lat) = vp.dot_to_lonlat(target.0, target.1);
                        to_world(lon, lat)
                    };
                    let id: TileId = (z, (wx * n).floor().clamp(0.0, n - 1.0) as u32, (wy * n).floor().clamp(0.0, n - 1.0) as u32);
                    let Some((tid, tile)) = v.tiles.get_or_ancestor(id) else { continue };
                    // The top shown layer drawing each feature.
                    let mut drawn_by: HashMap<u32, usize> = HashMap::new();
                    for (i, e) in self.layers.iter().enumerate() {
                        if let Entry::Style(l) = e
                            && l.source.as_deref() == Some(sid.as_str())
                            && l.shown_at(cam.zoom)
                        {
                            for it in tile.layer_items(i, l, cam.zoom, bg, env).iter() {
                                drawn_by.insert(it.feature, i);
                            }
                        }
                    }
                    let tn = (1u64 << tid.0) as f64;
                    let world = |p: &[f32; 2]| ((tid.1 as f64 + p[0] as f64) / tn, (tid.2 as f64 + p[1] as f64) / tn);
                    let mut seen = HashSet::new();
                    for (fi, f) in tile.features.iter().enumerate() {
                        let dots: Vec<Vec<(f64, f64)>> = f
                            .parts
                            .iter()
                            .map(|part| {
                                part.iter()
                                    .map(|p| {
                                        let (x, y) = world(p);
                                        vp.dot(x, y)
                                    })
                                    .collect()
                            })
                            .collect();
                        let Some((rank, score)) = hit(f.geom, &dots, target, false) else { continue };
                        // A feature cut by tile edges can appear more than once; keep one.
                        if !seen.insert(format!("{}|{:?}|{:?}", f.layer, f.geom, f.props)) {
                            continue;
                        }
                        let lonlat = f
                            .parts
                            .iter()
                            .map(|part| {
                                part.iter()
                                    .map(|p| {
                                        let (x, y) = world(p);
                                        to_lonlat(x, y)
                                    })
                                    .collect()
                            })
                            .collect();
                        push(
                            f.geom,
                            rank,
                            score,
                            lonlat,
                            f.layer.clone(),
                            &f.props,
                            Some(tid),
                            drawn_by.get(&(fi as u32)).copied(),
                            feature_id(env.promote, &f.id, &f.props),
                        );
                    }
                }
                MapSource::GeoJson(g) => {
                    for f in g.near(cam.zoom, near_box(&vp, target)) {
                        let dots: Vec<Vec<(f64, f64)>> = f.parts.iter().map(|p| p.iter().map(|&(x, y)| vp.dot(x, y)).collect()).collect();
                        let Some((rank, score)) = hit(f.geom, &dots, target, true) else { continue };
                        let c = Ctx { zoom: cam.zoom.floor(), geom: f.geom, props: &f.props, state: env.of(&f.id, &f.props) };
                        let by = self.layers.iter().rposition(|e| {
                            matches!(e, Entry::Style(l) if l.source.as_deref() == Some(sid.as_str()) && l.shown_at(cam.zoom) && resolve(l, &c, cam.zoom, bg).is_some())
                        });
                        let lonlat = f.parts.iter().map(|p| p.iter().map(|&(x, y)| to_lonlat(x, y)).collect()).collect();
                        push(f.geom, rank, score, lonlat, source.clone(), &f.props, None, by, feature_id(env.promote, &f.id, &f.props));
                    }
                }
                MapSource::Raster(_) => {}
            }
            hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
            out.extend(hits.into_iter().map(|(_, _, p)| p));
        }
        out
    }

    /// The color of the top shown background layer, used to blend translucent colors.
    fn background(&self, zoom: f64) -> Option<Rgba> {
        self.layers.iter().rev().find_map(|e| match e {
            Entry::Style(l) if l.kind == Kind::Background && l.shown_at(zoom) => {
                let c = Ctx { zoom, geom: GeomType::Unknown, props: &[], state: &[] };
                l.paint_expr("background-color").and_then(|e| e.eval(&c).as_color())
            }
            _ => None,
        })
    }
}

/// Evaluate every style layer that draws from `source` against a tile's features.
fn style_tile(layers: &[Entry], source: &str, features: &[store::Feature], zoom: f64) -> Vec<Item> {
    let mut by_layer: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in features.iter().enumerate() {
        by_layer.entry(&f.layer).or_default().push(i);
    }
    let bg = layers.iter().rev().find_map(|e| match e {
        Entry::Style(l) if l.kind == Kind::Background && l.shown_at(zoom) => {
            l.paint_expr("background-color").and_then(|e| e.eval(&Ctx { zoom, geom: GeomType::Unknown, props: &[], state: &[] }).as_color())
        }
        _ => None,
    });
    let mut items = Vec::new();
    for (li, e) in layers.iter().enumerate() {
        let Entry::Style(l) = e else { continue };
        if l.source.as_deref() != Some(source) {
            continue;
        }
        // Layers whose style depends on the zoom or feature state are evaluated when drawn.
        if l.uses_zoom() || l.uses_state() {
            continue;
        }
        let Some(fs) = l.source_layer.as_deref().and_then(|s| by_layer.get(s)) else { continue };
        for &fi in fs {
            let f = &features[fi];
            if let Some(paint) = resolve(l, &Ctx { zoom, geom: f.geom, props: &f.props, state: &[] }, zoom, bg) {
                items.push(Item { layer: li as u16, feature: fi as u32, paint });
            }
        }
    }
    items
}

/// A color with its opacity, blended over the background when there is one. Nearly
/// transparent colors aren't drawn.
fn term_color(c: Rgba, opacity: f64, bg: Option<Rgba>) -> Option<Color> {
    let alpha = c.a * opacity;
    if alpha <= 0.05 {
        return None;
    }
    let c = match bg {
        Some(b) if alpha < 0.999 => c.over(b, alpha),
        _ => c,
    };
    let (r, g, b) = c.to_u8();
    Some(Color::Rgb(r, g, b))
}

const BLACK: Rgba = Rgba::rgb(0, 0, 0);

/// What one style layer draws for one feature, or `None`. The filter and layout use `c`
/// (its zoom is the whole zoom level); paint uses `paint_zoom`.
fn resolve(l: &StyleLayer, c: &Ctx, paint_zoom: f64, bg: Option<Rgba>) -> Option<Resolved> {
    if let Some(f) = &l.filter
        && !f.eval(c).as_bool()
    {
        return None;
    }
    let cp = Ctx { zoom: paint_zoom, geom: c.geom, props: c.props, state: c.state };
    let color = |prop: &str, opacity: &str, default: Option<Rgba>| -> Option<Color> {
        let rgba = l.paint_expr(prop).and_then(|e| e.eval(&cp).as_color()).or(default)?;
        let o = l.paint_expr(opacity).and_then(|e| e.eval(&cp).as_num()).unwrap_or(1.0);
        term_color(rgba, o, bg)
    };
    let layout_str = |name: &str| l.layout_expr(name).map(|e| e.eval(c).to_text()).filter(|s| !s.is_empty());
    let sort = || {
        let k = l.layout_expr("symbol-sort-key").and_then(|e| e.eval(c).as_num()).unwrap_or(0.0);
        LAYER_PRIORITY / 2 - (k.round() as i32).clamp(-(LAYER_PRIORITY / 2 - 1), LAYER_PRIORITY / 2 - 1)
    };
    match l.kind {
        Kind::Fill if c.geom == GeomType::Polygon => {
            let fill = color("fill-color", "fill-opacity", Some(BLACK));
            let outline = color("fill-outline-color", "fill-opacity", None);
            (fill.is_some() || outline.is_some()).then_some(Resolved::Fill { color: fill, outline })
        }
        Kind::Line if c.geom != GeomType::Point => {
            let color = color("line-color", "line-opacity", Some(BLACK))?;
            // Pixels to dots: a braille dot is about 4 px, so thin lines stay one dot and wide
            // roads get two or three (no wider: braille can't show an outline around a fill).
            let px = l.paint_expr("line-width").and_then(|e| e.eval(&cp).as_num()).unwrap_or(1.0);
            if px <= 0.0 {
                return None;
            }
            // Dash lengths are in line widths. All gaps zero means a solid line.
            let dash = match l.paint_expr("line-dasharray").map(|e| e.eval(&cp)) {
                Some(crate::expr::Val::Array(a)) => {
                    let v: Vec<f64> = a.iter().filter_map(|x| x.as_num()).collect();
                    let gaps = a.iter().skip(1).step_by(2).any(|x| x.as_num().is_some_and(|n| n > 0.0));
                    (v.len() >= 2 && gaps).then_some(v)
                }
                _ => None,
            };
            Some(Resolved::Line { color, width: px, dash })
        }
        Kind::Circle if c.geom == GeomType::Point => {
            let fg = color("circle-color", "circle-opacity", Some(BLACK))?;
            let glyph = layout_str("mapterm:glyph").unwrap_or_else(|| "●".into());
            Some(Resolved::Point { glyph, style: TStyle::new().fg(fg), label: None, priority: sort() })
        }
        Kind::Symbol => {
            let fg = color("text-color", "text-opacity", Some(BLACK))?;
            let mut style = TStyle::new().fg(fg);
            let font = l.layout_expr("text-font").map(|e| e.eval(c).to_text().to_lowercase()).unwrap_or_default();
            if font.contains("bold") {
                style = style.add_modifier(Modifier::BOLD);
            }
            if font.contains("italic") {
                style = style.add_modifier(Modifier::ITALIC);
            }
            // Terminal labels are one line: the first non-empty line of the text.
            let text = layout_str("text-field").and_then(|t| t.lines().map(str::trim).find(|s| !s.is_empty()).map(str::to_string)).map(|t| {
                match layout_str("text-transform").as_deref() {
                    Some("uppercase") => t.to_uppercase(),
                    Some("lowercase") => t.to_lowercase(),
                    _ => t,
                }
            });
            // A point's glyph: `mapterm:glyph`, else the style's icon as a one-cell symbol.
            let glyph = layout_str("mapterm:glyph")
                .or_else(|| (c.geom == GeomType::Point).then(|| layout_str("icon-image")).flatten().and_then(|n| crate::icons::glyph(&n)).map(str::to_string));
            let priority = sort();
            match (glyph, text) {
                (Some(glyph), text) if c.geom == GeomType::Point => {
                    let icon = color("icon-color", "icon-opacity", None).map_or(style, |c| TStyle::new().fg(c));
                    Some(Resolved::Point { glyph, style: icon, label: text.map(|t| (t, style)), priority })
                }
                (_, Some(text)) => {
                    // Where the text sits relative to its point: text-variable-anchor (tried
                    // in order), else text-anchor.
                    let variable: Vec<(i8, i8)> = match l.layout_expr("text-variable-anchor").map(|e| e.eval(c)) {
                        Some(crate::expr::Val::Array(a)) => a.iter().map(|v| anchor_of(&v.to_text())).collect(),
                        _ => Vec::new(),
                    };
                    let anchors = if variable.is_empty() { vec![anchor_of(layout_str("text-anchor").as_deref().unwrap_or("center"))] } else { variable };
                    // Ems to dots: an em is about two columns wide (4 dots) and a row tall.
                    let offset = match l.layout_expr("text-offset").map(|e| e.eval(c)) {
                        Some(crate::expr::Val::Array(a)) if a.len() == 2 => (a[0].as_num().unwrap_or(0.0) * 4.0, a[1].as_num().unwrap_or(0.0) * 4.0),
                        _ => (0.0, 0.0),
                    };
                    // CSS pixels to dots: a dot is about 4 px.
                    let spacing = (l.layout_expr("symbol-spacing").and_then(|e| e.eval(c).as_num()).unwrap_or(250.0) / 4.0).max(8.0);
                    let along_line = c.geom != GeomType::Point && layout_str("symbol-placement").is_some_and(|p| p.starts_with("line"));
                    Some(Resolved::Text { text, style, anchors, offset, along_line, spacing, priority })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Draw one resolved feature whose parts are already in dots.
fn draw_resolved(ctx: &mut DrawContext, r: &Resolved, geom: GeomType, dots: &[Vec<(f64, f64)>]) {
    match r {
        Resolved::Fill { color, outline } => {
            if let Some(c) = color {
                ctx.fill_dots(dots, *c);
            }
            if let Some(c) = outline {
                for ring in dots {
                    ctx.line_dots(ring, *c);
                }
            }
        }
        Resolved::Line { color, width, dash } => {
            for part in dots {
                ctx.line_px(part, *color, *width, dash.as_deref());
            }
        }
        Resolved::Point { glyph, style, label, priority } => {
            for &p in dots.iter().flatten() {
                ctx.marker_dot(p, glyph, *style, *priority, label.clone());
            }
        }
        Resolved::Text { text, style, anchors, offset, along_line, spacing, priority } => {
            // Candidate positions around a point, one per anchor: half the text's width in
            // dots (2 per column) plus a column of room sideways, a row up or down.
            let place = |p: (f64, f64)| -> Vec<(f64, f64)> {
                let half = text.width() as f64 + 2.0;
                anchors.iter().map(|a| (p.0 + a.0 as f64 * half + offset.0, p.1 + a.1 as f64 * 4.0 + offset.1)).collect()
            };
            if geom == GeomType::Point {
                for &p in dots.iter().flatten() {
                    ctx.label_dots(place(p), text, *style, *priority);
                }
            } else if *along_line || geom == GeomType::Line {
                // Repeat along the line every `spacing`, measured from the start of each part
                // at the whole zoom level, so labels stay put while the map pans and zooms
                // within a level (MapLibre's symbol-spacing). Short parts get one, mid-way.
                // The same name keeps most of a spacing apart, so a road made of many short
                // pieces is still labelled about once per spacing.
                let scale = 2f64.powf(ctx.camera.zoom - ctx.camera.zoom.floor());
                let repeat = (spacing * scale * 0.8).max(crate::layer::REPEAT_DOTS);
                for part in dots {
                    for p in along(part, spacing * scale) {
                        if ctx.inside(p) {
                            ctx.label_spaced(place(p), text, *style, *priority, repeat);
                        }
                    }
                }
            } else {
                // Polygons: the middle of the outer ring's bounding box.
                if let Some(ring) = dots.first().filter(|r| !r.is_empty()) {
                    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                    for &(x, y) in ring {
                        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                    }
                    ctx.label_dots(place(((x0 + x1) / 2.0, (y0 + y1) / 2.0)), text, *style, *priority);
                }
            }
        }
    }
}

/// A `text-anchor` name as a direction: (-1 left .. 1 right, -1 up .. 1 down) from the
/// point to the text's center.
fn anchor_of(name: &str) -> (i8, i8) {
    match name {
        "left" => (1, 0),
        "right" => (-1, 0),
        "top" => (0, 1),
        "bottom" => (0, -1),
        "top-left" => (1, 1),
        "top-right" => (-1, 1),
        "bottom-left" => (1, -1),
        "bottom-right" => (-1, -1),
        _ => (0, 0),
    }
}

/// Points every `spacing` along a polyline, starting half a spacing in; a line shorter than
/// one spacing gets its middle.
fn along(part: &[(f64, f64)], spacing: f64) -> Vec<(f64, f64)> {
    let lens: Vec<f64> = part.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).collect();
    let total: f64 = lens.iter().sum();
    if total <= 0.0 || part.len() < 2 {
        return part.first().copied().into_iter().collect();
    }
    let mut targets = Vec::new();
    if total < spacing {
        targets.push(total / 2.0);
    } else {
        let mut s = spacing / 2.0;
        while s < total {
            targets.push(s);
            s += spacing;
        }
    }
    let mut out = Vec::with_capacity(targets.len());
    let (mut seg, mut start) = (0usize, 0.0);
    for t in targets {
        while seg < lens.len() - 1 && start + lens[seg] < t {
            start += lens[seg];
            seg += 1;
        }
        let k = if lens[seg] > 0.0 { ((t - start) / lens[seg]).clamp(0.0, 1.0) } else { 0.0 };
        let (a, b) = (part[seg], part[seg + 1]);
        out.push((a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k));
    }
    out
}

/// A box (world coordinates) around a spot in dots, wide enough for the hit tolerances.
fn near_box(vp: &Viewport, target: (f64, f64)) -> crate::index::BBox {
    let tol = 6.0;
    let (w, n) = vp.dot_to_lonlat(target.0 - tol, target.1 - tol);
    let (e, s) = vp.dot_to_lonlat(target.0 + tol, target.1 + tol);
    let (x0, y0) = to_world(w, n);
    let (x1, y1) = to_world(e, s);
    [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]
}

/// An expression value as JSON.
fn val_json(v: &Val) -> Json {
    match v {
        Val::Null => Json::Null,
        Val::Bool(b) => Json::Bool(*b),
        Val::Num(n) => serde_json::Number::from_f64(*n).map_or(Json::Null, Json::Number),
        Val::Str(s) => Json::String(s.clone()),
        Val::Color(c) => Json::String(Val::Color(*c).to_text()),
        Val::Array(a) => Json::Array(a.iter().map(val_json).collect()),
    }
}

/// Draws a [`Map`] with a camera. Make one with [`Map::view`].
pub struct MapView<'a> {
    /// The map.
    pub map: &'a Map,
    /// The camera to draw with.
    pub camera: &'a Camera,
}

impl Widget for MapView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (map, cam) = (self.map, self.camera);
        match map.pixel_mode {
            Some(cpx) => {
                let frame = map.pixel_tiles(cam, area, cpx);
                *map.pixels.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
                // Labels, markers and custom layers; the tiles hold the style's geometry.
                draw_layers(map, cam, area, buf, Pass::Frame { labels: &map.label_memory, geometry_off: true });
            }
            None => {
                draw_layers(map, cam, area, buf, Pass::Frame { labels: &map.label_memory, geometry_off: false });
            }
        }
    }
}

/// What a [`draw_layers`] pass is for.
enum Pass<'a> {
    /// The frame on screen: labels and markers into the buffer, remembered for the next
    /// frame; fills and lines as braille, or (with `geometry_off`, pixel mode) not at all for
    /// style layers.
    Frame { labels: &'a std::sync::Mutex<crate::layer::LabelMemory>, geometry_off: bool },
    /// A pixel-mode tile image: fills and lines into `pix`. `touched` collects the (source,
    /// feature id) of features in layers that read feature state, so a state change redraws
    /// only the tiles they're in.
    Tile { pix: &'a mut crate::pixels::Pix, touched: &'a mut HashSet<(String, String)> },
}

/// Draw the map's layers for `cam` into `area`, for a [`Pass`].
fn draw_layers(map: &Map, cam: &Camera, area: Rect, buf: &mut Buffer, pass: Pass) {
    let (pix, labels, geometry_off, mut touched) = match pass {
        Pass::Frame { labels, geometry_off } => (None, Some(labels), geometry_off, None),
        Pass::Tile { pix, touched } => (Some(pix), None, false, Some(touched)),
    };
    {
        let vp = Viewport::new(cam, area);
        let bg = map.background(cam.zoom);
        // Loaded tiles per vector source, looked up once per frame.
        let tiles: HashMap<&str, Vec<(TileId, Arc<MapTile>)>> = map
            .sources
            .iter()
            .filter_map(|(id, s)| match s {
                MapSource::Vector(v) => Some((id.as_str(), v.tiles.visible(&vp, cam.tile_zoom(v.min_zoom, v.max_zoom)))),
                _ => None,
            })
            .collect();
        // Imagery under the map shows through the image, so the background isn't painted.
        let imagery = map.layers.iter().any(|e| matches!(e, Entry::Style(l) if l.kind == Kind::Raster && l.shown_at(cam.zoom)));
        let skip_bg = pix.is_some() && imagery;
        crate::layer::render_labelled(cam, area, buf, pix, labels, map.label_placement == LabelPlacement::Sticky, |ctx| {
            for (i, entry) in map.layers.iter().enumerate() {
                ctx.base = i as i32 * LAYER_PRIORITY;
                let l = match entry {
                    Entry::Style(l) => l,
                    Entry::Custom { layer, .. } => {
                        ctx.geometry_off = false;
                        layer.draw(ctx);
                        continue;
                    }
                };
                ctx.geometry_off = geometry_off;
                if !l.shown_at(cam.zoom) {
                    continue;
                }
                if l.kind == Kind::Background {
                    if skip_bg {
                        continue;
                    }
                    if let Some(c) = l
                        .paint_expr("background-color")
                        .and_then(|e| e.eval(&Ctx { zoom: cam.zoom, geom: GeomType::Unknown, props: &[], state: &[] }).as_color())
                        && let Some(c) = term_color(
                            c,
                            l.paint_expr("background-opacity")
                                .and_then(|e| e.eval(&Ctx { zoom: cam.zoom, geom: GeomType::Unknown, props: &[], state: &[] }).as_num())
                                .unwrap_or(1.0),
                            None,
                        )
                    {
                        let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
                        ctx.fill_dots(&[vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h), (0.0, 0.0)]], c);
                    }
                    continue;
                }
                let Some(sid) = l.source.as_deref() else { continue };
                let env = map.state_env(sid);
                let stateful = touched.is_some() && l.uses_state();
                // Pixel tiles note the features whose look can change with feature state (all
                // of a stateful layer's, drawn or not: a state can make a hidden one show).
                let mut touch = |id: &Option<crate::mvt::Value>, props: &[(String, crate::mvt::Value)]| {
                    if let Some(t) = touched.as_deref_mut()
                        && let Some(id) = feature_id(env.promote, id, props)
                    {
                        t.insert((sid.to_string(), id.to_string()));
                    }
                };
                match map.source(sid) {
                    Some(MapSource::Vector(_)) => {
                        for ((z, tx, ty), tile) in tiles.get(sid).into_iter().flatten() {
                            let n = (1u64 << z) as f64;
                            if stateful && let Some(fs) = l.source_layer.as_deref().and_then(|s| tile.by_layer.get(s)) {
                                for &fi in fs {
                                    let f = &tile.features[fi as usize];
                                    touch(&f.id, &f.props);
                                }
                            }
                            for it in tile.layer_items(i, l, cam.zoom, bg, env).iter() {
                                let f = &tile.features[it.feature as usize];
                                let dots: Vec<Vec<(f64, f64)>> = f
                                    .parts
                                    .iter()
                                    .map(|part| part.iter().map(|p| vp.dot((*tx as f64 + p[0] as f64) / n, (*ty as f64 + p[1] as f64) / n)).collect())
                                    .collect();
                                draw_resolved(ctx, &it.paint, f.geom, &dots);
                            }
                        }
                    }
                    Some(MapSource::GeoJson(g)) => {
                        for f in g.near(cam.zoom, ctx.world_view()) {
                            if !ctx.visible(f.bbox) {
                                continue;
                            }
                            if stateful {
                                touch(&f.id, &f.props);
                            }
                            let c = Ctx { zoom: cam.zoom.floor(), geom: f.geom, props: &f.props, state: env.of(&f.id, &f.props) };
                            let Some(r) = resolve(l, &c, cam.zoom, bg) else { continue };
                            let dots: Vec<Vec<(f64, f64)>> = f.parts.iter().map(|p| p.iter().map(|&(x, y)| vp.dot(x, y)).collect()).collect();
                            draw_resolved(ctx, &r, f.geom, &dots);
                        }
                    }
                    Some(MapSource::Raster(r)) if map.raster_mode == RasterMode::Cells => {
                        // raster-opacity: blended with the style's background (the terminal's
                        // own background color isn't known); nearly transparent isn't drawn.
                        let opacity = l
                            .paint_expr("raster-opacity")
                            .and_then(|e| e.eval(&Ctx { zoom: cam.zoom, geom: GeomType::Unknown, props: &[], state: &[] }).as_num())
                            .unwrap_or(1.0)
                            .clamp(0.0, 1.0);
                        if opacity <= 0.05 {
                            continue;
                        }
                        let blend = |c: Color| match (c, bg) {
                            (Color::Rgb(r, g, b), Some(under)) if opacity < 0.999 => {
                                let (r, g, b) = Rgba::rgb(r, g, b).over(under, opacity).to_u8();
                                Color::Rgb(r, g, b)
                            }
                            (c, _) => c,
                        };
                        let rz = map.raster_zoom(&r.src, cam);
                        for b in crate::raster::layout(&r.tiles, cam, area, rz) {
                            let (sx, sy, sw, sh) = b.src;
                            let (cw, ch) = (sw as f64 / b.cells.width as f64, sh as f64 / b.cells.height as f64);
                            for row in 0..b.cells.height {
                                for col in 0..b.cells.width {
                                    let (x, y) = (sx + (col as f64 * cw) as u32, sy + (row as f64 * ch) as u32);
                                    let c = b.tile.average(x, y, cw.ceil() as u32, ch.ceil() as u32);
                                    ctx.cell_bg(b.cells.x - area.x + col, b.cells.y - area.y + row, blend(c));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::DrawContext;
    use serde_json::json;

    fn sample() -> Map {
        let mut map = Map::new();
        map.add_source(
            "sites",
            MapSource::geojson(&json!({"type": "FeatureCollection", "features": [
                {"type": "Feature", "properties": {"name": "Box", "kind": "zone"},
                 "geometry": {"type": "Polygon", "coordinates": [[[-77.05, 38.88], [-77.02, 38.88], [-77.02, 38.90], [-77.05, 38.90], [-77.05, 38.88]]]}},
                {"type": "Feature", "properties": {"name": "Pin", "kind": "site"},
                 "geometry": {"type": "Point", "coordinates": [-77.0353, 38.8895]}}
            ]}))
            .unwrap(),
        )
        .unwrap();
        map.add_layer(StyleLayer::background("bg").color("#000010")).unwrap();
        map.add_layer(StyleLayer::fill("zones", "sites").filter(json!(["==", ["get", "kind"], "zone"])).color("#204060")).unwrap();
        map.add_layer(StyleLayer::symbol("pins", "sites").filter(json!(["==", "kind", "site"])).glyph("◆").text(json!(["get", "name"])).color("#ff0")).unwrap();
        map
    }

    fn render(map: &Map, cam: &Camera, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        map.view(cam).render(area, &mut buf);
        buf
    }

    #[test]
    fn draws_geojson_through_style_layers() {
        let map = sample();
        let cam = Camera::new(-77.0353, 38.8895, 15.0);
        let area = Rect::new(0, 0, 60, 20);
        let buf = render(&map, &cam, area);
        let text: String =
            (0..area.height).map(|y| (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("◆ Pin"), "{text}");
        assert_eq!(buf[(30, 10)].symbol(), "◆");
        assert_eq!(buf[(30, 10)].fg, Color::Rgb(255, 255, 0));
        assert_eq!(buf[(2, 2)].bg, Color::Rgb(0x20, 0x40, 0x60), "inside the zone");
        // Hide the zones: the background shows instead.
        let mut map = map;
        map.set_visibility("zones", false).unwrap();
        assert_eq!(render(&map, &cam, area)[(2, 2)].bg, Color::Rgb(0, 0, 0x10));
    }

    #[test]
    fn queries_by_layer_top_first() {
        let map = sample();
        let cam = Camera::new(-77.0353, 38.8895, 15.0);
        let area = Rect::new(0, 0, 60, 20);
        let center = (60.0, 40.0);
        let ids: Vec<(String, String)> =
            map.query_rendered_features(&cam, area, center, None).iter().map(|p| (p.style_layer.as_deref().unwrap().to_string(), p.title())).collect();
        assert_eq!(ids, vec![("pins".into(), "Pin".into()), ("zones".into(), "Box".into())]);
        let only = map.query_rendered_features(&cam, area, center, Some(&["zones"]));
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].source.as_deref(), Some("sites"));
    }

    #[test]
    fn layer_order_and_set_data() {
        let mut map = sample();
        assert_eq!(map.layer_ids(), vec!["bg", "zones", "pins"]);
        map.move_layer("pins", Some("zones")).unwrap();
        assert_eq!(map.layer_ids(), vec!["bg", "pins", "zones"]);
        assert!(map.add_layer(StyleLayer::line("zones", "sites")).is_err(), "duplicate id");
        assert!(map.add_layer_before(StyleLayer::line("x", "sites"), "nope").is_err());
        map.set_data("sites", &json!({"type": "Point", "coordinates": [0, 0]})).unwrap();
        let (w, s, e, n) = map.source_bounds("sites").unwrap();
        assert!(w.abs() < 1e-9 && s.abs() < 1e-9 && e.abs() < 1e-9 && n.abs() < 1e-9);
        assert!(!map.remove_layer("x") && map.remove_layer("zones"));
    }

    struct Counter {
        hits: usize,
    }

    impl CustomLayer for Counter {
        fn name(&self) -> &str {
            "counter"
        }
        fn draw(&self, ctx: &mut DrawContext) {
            ctx.marker(-77.0353, 38.8895, "X", TStyle::new(), 0, None);
        }
    }

    #[test]
    fn custom_layers_draw_in_order_and_downcast() {
        let mut map = sample();
        map.add_custom_layer("count", Counter { hits: 0 }, None).unwrap();
        map.custom_layer_mut::<Counter>("count").unwrap().hits += 1;
        assert_eq!(map.custom_layer_mut::<Counter>("count").unwrap().hits, 1);
        assert!(map.custom_layer_mut::<Counter>("pins").is_none());
        // The custom layer is on top, so its marker wins the cell over the pin.
        let cam = Camera::new(-77.0353, 38.8895, 15.0);
        let buf = render(&map, &cam, Rect::new(0, 0, 60, 20));
        assert_eq!(buf[(30, 10)].symbol(), "X");
    }

    #[test]
    fn vector_tiles_are_styled_on_insert_and_restyled() {
        // A one-feature MVT: layer "water", a square polygon covering the tile.
        let mvt = crate::mvt::tests_support::square_tile("water", 4096);
        let mut map = Map::new();
        map.add_source("v", MapSource::vector_tiles(0, 14)).unwrap();
        map.add_layer(StyleLayer::fill("water", "v").source_layer("water").color("#0000ff")).unwrap();
        map.insert_tile("v", (0, 0, 0), Some(&mvt)).unwrap();
        let cam = Camera::new(0.0, 0.0, 0.0);
        let area = Rect::new(0, 0, 40, 20);
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(0, 0, 255));
        map.set_paint("water", "fill-color", &json!("#ff0000")).unwrap();
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(255, 0, 0));
        map.set_filter("water", Some(&json!(["==", "class", "lake"]))).unwrap();
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Reset);
        assert!(map.tile_requests(&cam, area).is_empty(), "app-supplied tiles are never requested");
    }

    fn raster_map() -> Map {
        let mut map = Map::new();
        let src = Arc::new(crate::raster::RasterSource::new("https://t.example/{z}/{x}/{y}.png", false, 19).unwrap());
        map.add_source("img", MapSource::raster(src)).unwrap();
        map.add_layer(StyleLayer::raster("img", "img")).unwrap();
        map
    }

    fn solid_tile(r: u8, g: u8, b: u8) -> RasterTile {
        let rgba = image::RgbaImage::from_pixel(256, 256, image::Rgba([r, g, b, 255]));
        RasterTile { id: 4242 + r as u32, png: vec![1, 2, 3], rgba }
    }

    #[test]
    fn raster_layers_draw_cells_or_kitty_images() {
        let mut map = raster_map();
        let cam = Camera::new(0.0, 0.0, 0.0);
        let area = Rect::new(0, 0, 40, 20);
        let reqs = map.tile_requests(&cam, area);
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].is_raster() && reqs[0].id == (0, 0, 0));
        map.insert_loaded("img", (0, 0, 0), Ok(Loaded(LoadedTile::Raster(Some(solid_tile(10, 200, 30)))))).unwrap();
        assert!(map.tile_requests(&cam, area).is_empty());
        // Cells: every cell takes the image color.
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(10, 200, 30));
        // Kitty: nothing in the cells; the image is sent once, then only placed.
        map.raster_mode = RasterMode::Kitty { cell_px: (10.0, 20.0) };
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Reset);
        let mut k = KittyState::default();
        let first = map.kitty_commands(&cam, area, &mut k);
        assert!(first.starts_with(crate::kitty::CLEAR_PLACEMENTS) && first.contains("a=t,f=100") && first.contains("z=-1000"));
        let second = map.kitty_commands(&cam, area, &mut k);
        assert!(!second.contains("a=t,") && second.contains("a=p,"));
        map.set_visibility("img", false).unwrap();
        assert!(!map.kitty_commands(&cam, area, &mut k).contains("a=p,"), "hidden layers aren't placed");
    }

    #[test]
    fn kitty_images_stay_under_the_budget() {
        let mut map = raster_map();
        map.raster_mode = RasterMode::Kitty { cell_px: (10.0, 20.0) };
        let area = Rect::new(0, 0, 40, 20);
        // Four zoom 1 tiles, each 256 KiB decoded; room for only two at a time.
        for (i, id) in [(1, 0, 0), (1, 1, 0), (1, 0, 1), (1, 1, 1)].into_iter().enumerate() {
            let mut t = solid_tile(10, 20, 30);
            t.id = 9000 + i as u32;
            map.insert_loaded("img", id, Ok(Loaded(LoadedTile::Raster(Some(t))))).unwrap();
        }
        let mut k = KittyState { budget: 2 * 256 * 1024, ..KittyState::default() };
        // Zoomed in on the north-west quarter: one tile shown, sent.
        let nw = Camera::new(-90.0, 45.0, 3.0);
        let c1 = map.kitty_commands(&nw, area, &mut k);
        assert_eq!(c1.matches("a=t,").count(), 1);
        // Pan to the north-east: a second image fits without freeing anything.
        let ne = Camera::new(90.0, 45.0, 3.0);
        let c2 = map.kitty_commands(&ne, area, &mut k);
        assert!(c2.matches("a=t,").count() == 1 && !c2.contains("a=d,d=I"));
        // A third doesn't fit: the least recently used (north-west, not on screen) is freed
        // before the new one is sent, and the budget holds.
        let sw = Camera::new(-90.0, -45.0, 3.0);
        let c3 = map.kitty_commands(&sw, area, &mut k);
        assert!(c3.contains("a=d,d=I,i=9000,"), "{c3}");
        assert!(c3.find("a=d,d=I").unwrap() < c3.find("a=t,").unwrap(), "freed before sending");
        assert!(k.bytes() <= k.budget);
        // Back to the north-west: it is sent again rather than assumed loaded.
        assert_eq!(map.kitty_commands(&nw, area, &mut k).matches("a=t,").count(), 1);
    }

    #[test]
    fn failed_loads_are_retried_then_given_up() {
        use std::time::Duration;
        let mut map = raster_map();
        let cam = Camera::new(0.0, 0.0, 0.0);
        let area = Rect::new(0, 0, 40, 20);
        let fail = |map: &mut Map| map.insert_loaded("img", (0, 0, 0), Err(anyhow!("HTTP 500"))).unwrap_err();
        // A long wait: not requested again until it passes.
        map.retry = RetryPolicy { base: Duration::from_secs(3600), max: Duration::from_secs(3600), attempts: 3 };
        assert!(fail(&mut map).to_string().contains("img tile 0/0/0"));
        assert!(map.tile_requests(&cam, area).is_empty(), "waiting before the retry");
        // No wait: requested again right away, still nothing drawn.
        map.retry.base = Duration::ZERO;
        map.failed.values_mut().for_each(|f| f.1 = std::time::Instant::now());
        assert_eq!(map.tile_requests(&cam, area).len(), 1);
        fail(&mut map);
        assert_eq!(map.tile_requests(&cam, area).len(), 1);
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Reset);
        // The last attempt gives up: stored empty, never requested again.
        fail(&mut map);
        assert!(map.tile_requests(&cam, area).is_empty());
        // A success clears the failure record.
        let mut map = raster_map();
        map.retry.base = Duration::ZERO;
        fail(&mut map);
        map.insert_loaded("img", (0, 0, 0), Ok(Loaded(LoadedTile::Raster(Some(solid_tile(1, 2, 3)))))).unwrap();
        assert!(map.failed.is_empty() && map.tile_requests(&cam, area).is_empty());
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn rate_limited_loads_are_never_given_up() {
        use std::time::Duration;
        let mut map = raster_map();
        let cam = Camera::new(0.0, 0.0, 0.0);
        let area = Rect::new(0, 0, 40, 20);
        map.retry = RetryPolicy { base: Duration::ZERO, max: Duration::ZERO, attempts: 2 };
        // Far more 429s than attempts: still asked for again each time.
        for _ in 0..6 {
            let e = map.insert_loaded("img", (0, 0, 0), Err(anyhow!("https://x/0/0/0: HTTP 429 Too Many Requests"))).unwrap_err();
            assert!(is_rate_limited(&e));
            assert_eq!(map.tile_requests(&cam, area).len(), 1, "still wanted");
        }
        assert!(!is_rate_limited(&anyhow!("HTTP 500")));
    }

    #[test]
    fn vector_failures_are_retried_too() {
        let mut map = Map::new();
        map.retry.base = std::time::Duration::ZERO;
        let src = crate::source::Source::xyz_for_tests();
        map.add_source("v", MapSource::vector(src)).unwrap();
        map.add_layer(StyleLayer::fill("water", "v").source_layer("water").color("#00f")).unwrap();
        let (cam, area) = (Camera::new(0.0, 0.0, 0.0), Rect::new(0, 0, 40, 20));
        let req = map.tile_requests(&cam, area).remove(0);
        assert!(map.insert_loaded(&req.source, req.id, Err(anyhow!("timed out"))).is_err());
        assert_eq!(map.tile_requests(&cam, area).len(), 1, "asked for again, not stored empty");
    }

    #[test]
    fn query_features_lists_drawn_and_undrawn() {
        let mvt = crate::mvt::tests_support::square_tile("water", 4096);
        let mut map = sample();
        // A z0-only source, so the camera's z15 view uses this tile.
        map.add_source("v", MapSource::vector_tiles(0, 0)).unwrap();
        map.add_layers(vec![StyleLayer::fill("lakes", "v").source_layer("water").filter(json!(["==", "class", "lake"])).color("#00f")], Some("zones")).unwrap();
        map.insert_tile("v", (0, 0, 0), Some(&mvt)).unwrap();
        let cam = Camera::new(-77.0353, 38.8895, 15.0);
        let area = Rect::new(0, 0, 60, 20);
        let picks = map.query_features(&cam, area, (60.0, 40.0));
        let got: Vec<(String, Option<String>, bool)> = picks.iter().map(|p| (p.title(), p.style_layer.as_deref().map(str::to_string), p.drawn)).collect();
        assert_eq!(
            got,
            vec![("Pin".into(), Some("pins".into()), true), ("Box".into(), Some("zones".into()), true), ("water".into(), None, false),],
            "GeoJSON (top layers) first; the unstyled water square is listed but not drawn"
        );
        map.set_visibility("pins", false).unwrap();
        let pin = &map.query_features(&cam, area, (60.0, 40.0))[0];
        assert!(pin.title() == "Pin" && !pin.drawn, "hidden layers don't count as drawing");
    }

    #[test]
    fn geojson_overlay_layers() {
        let mut map = Map::new();
        map.add_source(
            "mine",
            MapSource::geojson(&json!({"type": "FeatureCollection", "features": [
            {"type": "Feature", "properties": {"name": "Pin"}, "geometry": {"type": "Point", "coordinates": [-77.0353, 38.8895]}}]}))
            .unwrap(),
        )
        .unwrap();
        map.add_layers(crate::geojson::overlay_layers("mine", Color::Rgb(255, 99, 132), true), None).unwrap();
        assert_eq!(map.layer_ids(), vec!["mine-fill", "mine-line", "mine-point", "mine-cluster", "mine-label"]);
        let cam = Camera::new(-77.0353, 38.8895, 15.0);
        let buf = render(&map, &cam, Rect::new(0, 0, 60, 20));
        let row: String = (0..60).map(|x| buf[(x, 10)].symbol().to_string()).collect();
        assert!(row.contains("● Pin"), "{row}");
        assert_eq!(buf[(30, 10)].fg, Color::Rgb(255, 99, 132));
    }

    fn water_map(max_zoom: u8, tile: TileId, layer: StyleLayer) -> Map {
        let mvt = crate::mvt::tests_support::square_tile("water", 4096);
        let mut map = Map::new();
        map.add_source("v", MapSource::vector_tiles(0, max_zoom)).unwrap();
        map.add_layer(layer).unwrap();
        map.insert_tile("v", tile, Some(&mvt)).unwrap();
        map
    }

    #[test]
    fn paint_follows_the_fractional_camera_zoom() {
        // Black at z2 fading to white at z4, drawn from a z2-only source.
        let layer = StyleLayer::fill("water", "v").source_layer("water").color(json!(["interpolate", ["linear"], ["zoom"], 2, "#000000", 4, "#ffffff"]));
        let map = water_map(2, (2, 1, 1), layer);
        // The middle of tile 2/1/1 (lon -45, lat ~40.98).
        let at = |z: f64| render(&map, &Camera::new(-45.0, 40.979898, z), Rect::new(0, 0, 40, 20))[(20, 10)].bg;
        assert_eq!(at(2.0), Color::Rgb(0, 0, 0));
        assert_eq!(at(2.5), Color::Rgb(64, 64, 64), "a quarter of the way at z2.5, not the tile's z2");
        assert_eq!(at(3.0), Color::Rgb(128, 128, 128));
        assert_eq!(at(4.0), Color::Rgb(255, 255, 255), "past the source's last zoom");
    }

    #[test]
    fn filters_use_the_whole_zoom_level_past_the_source_max() {
        // Only drawn from z16, from a source whose tiles stop at z14.
        let layer = StyleLayer::fill("water", "v").source_layer("water").filter(json!([">=", ["zoom"], 16])).color("#0000ff");
        let map = water_map(14, (14, 4686, 6268), layer);
        let center = |z: f64| Camera::new(-77.025146484375, 38.882481197550796, z);
        let area = Rect::new(0, 0, 40, 20);
        assert_eq!(render(&map, &center(15.9), area)[(20, 10)].bg, Color::Reset, "z15.9 is level 15");
        assert_eq!(render(&map, &center(16.0), area)[(20, 10)].bg, Color::Rgb(0, 0, 255));
        assert_eq!(map.query_rendered_features(&center(16.2), area, (40.0, 40.0), None).len(), 1);
        assert!(map.query_rendered_features(&center(15.2), area, (40.0, 40.0), None).is_empty());
        let picks = map.query_features(&center(15.2), area, (40.0, 40.0));
        assert!(picks.len() == 1 && !picks[0].drawn, "listed by inspect, but not drawn at z15");
    }

    #[test]
    fn attributions_of_drawn_sources() {
        let mut map = sample();
        assert!(map.attributions(15.0).is_empty(), "no credits given");
        map.set_attribution("sites", "<a href='https://example.org'>&copy; Example Sites</a>").unwrap();
        map.add_source("other", MapSource::geojson(&json!({"type": "Point", "coordinates": [0, 0]})).unwrap()).unwrap();
        map.set_attribution("other", "© Example Sites").unwrap();
        map.add_layer(StyleLayer::circle("other-dots", "other").min_zoom(16.0)).unwrap();
        assert_eq!(map.attributions(15.0), vec!["© Example Sites"]);
        assert_eq!(map.attributions(16.0), vec!["© Example Sites"], "the same credit once");
        map.set_attribution("other", "Other Data").unwrap();
        assert_eq!(map.attributions(16.0), vec!["© Example Sites", "Other Data"]);
        assert_eq!(map.attributions(15.0), vec!["© Example Sites"], "layers not shown at this zoom don't credit");
        for id in ["zones", "pins"] {
            map.set_visibility(id, false).unwrap();
        }
        assert_eq!(map.attributions(16.0), vec!["Other Data"], "hidden layers don't credit");
        map.set_data("other", &json!({"type": "Point", "coordinates": [1, 1]})).unwrap();
        assert_eq!(map.attributions(16.0), vec!["Other Data"], "new data keeps the credit");
        assert!(map.set_attribution("nope", "x").is_err());
    }

    #[test]
    fn pixel_backdrop_while_zooming() {
        // A blue square over a grey background, in pixel mode.
        let mut map = Map::new();
        map.add_source(
            "sq",
            MapSource::geojson(&json!({"type": "Polygon", "coordinates": [[[-40, -30], [40, -30], [40, 30], [-40, 30], [-40, -30]]]})).unwrap(),
        )
        .unwrap();
        map.add_layer(StyleLayer::background("bg").color("#808080")).unwrap();
        map.add_layer(StyleLayer::fill("fill", "sq").color("#0000ff")).unwrap();
        // Shown only up to zoom 2.2: zoom ranges mustn't stop zoom 2's tiles standing in.
        map.add_layer(StyleLayer::fill("upto", "sq").color("#0000ff").max_zoom(2.2)).unwrap();
        map.pixel_mode = Some((10.0, 20.0));
        // Time enough to draw every tile, however slow the machine (debug builds in CI).
        map.pixcache.lock().unwrap().budget = Some(std::time::Duration::from_secs(60));
        let area = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(area);
        let blue_at_center = |f: &crate::pixels::PixFrame| {
            // The topmost image placed over the view's center pixel (neighbours overlap by
            // a pixel), and that pixel's color.
            let (cx, cy) = (200u32, 200u32);
            f.placed.iter().rev().find_map(|p| {
                let (x0, y0) = (p.col as u32 * 10 + p.x_off, p.row as u32 * 20 + p.y_off);
                let (sx, sy, sw, sh) = p.src;
                if cx < x0 || cy < y0 || cx >= x0 + sw || cy >= y0 + sh {
                    return None;
                }
                let img = f.new.iter().find(|i| i.id == p.id)?;
                let o = (((cy - y0 + sy) * img.width) + cx - x0 + sx) as usize * 4;
                Some(img.rgba[o..o + 4].to_vec())
            })
        };
        map.view(&Camera::new(0.0, 0.0, 2.0)).render(area, &mut buf);
        let f = map.take_pixels().unwrap();
        assert!(f.pending == 0 && !f.new.is_empty(), "everything drawn at zoom 2");
        assert_eq!(blue_at_center(&f), Some(vec![0, 0, 255, 255]));
        // No time to draw: zooming in and out, the backdrop shows zoom 2's tiles scaled
        // into place over the whole area, with nothing see-through.
        map.pixcache.lock().unwrap().budget = Some(std::time::Duration::ZERO);
        for z in [2.5, 1.5] {
            map.view(&Camera::new(0.0, 0.0, z)).render(area, &mut buf);
            let f = map.take_pixels().unwrap();
            assert!(f.pending > 0 && f.placed.is_empty(), "z{z}: no tiles drawn");
            let b = f.backdrop.as_ref().expect("a backdrop");
            assert!(b.rgba.chunks(4).all(|p| p[3] == 255), "z{z}: backdrop see-through");
            let at = |x: u32, y: u32| b.rgba[((y * b.width + x) * 4) as usize..((y * b.width + x) * 4 + 4) as usize].to_vec();
            assert_eq!(at(b.width / 2, b.height / 2), vec![0, 0, 255, 255], "z{z}: the square in the middle");
        }
    }

    #[test]
    fn pixel_tiles_redraw_on_feature_state() {
        // Two squares with ids, red while hovered; one sits inside a single grid tile.
        let sq = |id: u32, x0: f64, y0: f64, d: f64| {
            json!({"type": "Feature", "id": id, "properties": {},
                "geometry": {"type": "Polygon", "coordinates": [[[x0, y0], [x0 + d, y0], [x0 + d, y0 + d], [x0, y0 + d], [x0, y0]]]}})
        };
        let mut map = Map::new();
        map.add_source(
            "sq",
            MapSource::geojson(&json!({"type": "FeatureCollection", "features": [sq(1, -12.0, 4.0, 6.0), sq(2, 20.0, -40.0, 30.0)]})).unwrap(),
        )
        .unwrap();
        map.add_layer(StyleLayer::background("bg").color("#808080")).unwrap();
        map.add_layer(StyleLayer::fill("fill", "sq").color(json!(["case", ["boolean", ["feature-state", "hover"], false], "#ff0000", "#0000ff"]))).unwrap();
        map.pixel_mode = Some((10.0, 20.0));
        // Time enough to draw every tile, however slow the machine (debug builds in CI).
        let plenty = Some(std::time::Duration::from_secs(60));
        map.pixcache.lock().unwrap().budget = plenty;
        let (cam, area) = (Camera::new(0.0, 0.0, 2.0), Rect::new(0, 0, 40, 20));
        let mut buf = Buffer::empty(area);
        let red = |img: &crate::pixels::PixImage| img.rgba.chunks(4).any(|p| p == [255, 0, 0, 255]);
        map.view(&cam).render(area, &mut buf);
        let first = map.take_pixels().unwrap();
        assert!(first.pending == 0 && first.new.len() > 2, "all {} tiles drawn", first.new.len());
        assert!(!first.new.iter().any(red));
        // Hover square 1: only the tile it's in is redrawn (and red), its old image freed.
        map.set_feature_state("sq", &crate::mvt::Value::Num(1.0), &json!({"hover": true})).unwrap();
        map.view(&cam).render(area, &mut buf);
        let f = map.take_pixels().unwrap();
        assert_eq!(f.new.len(), 1, "one tile redrawn");
        assert!(red(&f.new[0]));
        assert_eq!(f.freed.len(), 1);
        assert!(first.new.iter().any(|i| i.id == f.freed[0]));
        assert_eq!(f.placed.len(), first.placed.len());
        // Nothing changed: nothing redrawn.
        map.view(&cam).render(area, &mut buf);
        assert!(map.take_pixels().unwrap().new.is_empty());
        // No time to redraw: the stale tile stays up (never blank), and is redrawn later.
        map.pixcache.lock().unwrap().budget = Some(std::time::Duration::ZERO);
        map.remove_feature_state("sq", None, None);
        map.view(&cam).render(area, &mut buf);
        let f = map.take_pixels().unwrap();
        assert!(f.new.is_empty() && f.pending == 0 && f.placed.len() == first.placed.len(), "old images stay placed");
        map.pixcache.lock().unwrap().budget = plenty;
        map.view(&cam).render(area, &mut buf);
        let f = map.take_pixels().unwrap();
        assert!(!f.new.is_empty() && !f.new.iter().any(red), "redrawn without the highlight");
    }

    #[test]
    fn zoom_in_hints_only_for_shown_layers() {
        // Sentinel-2 always on (its tiles from zoom 9) with NAIP over it from 12: NAIP's layer
        // isn't on below 12, so only Sentinel-2 asks to zoom in, and only below 9.
        let mut map = Map::new();
        for (id, name, min) in [("s2", "sentinel2", 0.0), ("naip", "naip", 12.0)] {
            let src = crate::raster::RasterSource::from_preset(crate::raster::preset(name).unwrap(), None).unwrap();
            map.add_source(id, MapSource::raster(Arc::new(src))).unwrap();
            map.add_layer(StyleLayer::raster(id, id).min_zoom(min)).unwrap();
        }
        let names = |z: f64| map.rasters_below_min_zoom(&Camera::new(0.0, 0.0, z)).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names(5.0), vec!["sentinel2"]);
        assert!(names(10.0).is_empty(), "{:?}", names(10.0));
        assert!(names(16.0).is_empty());
    }

    #[test]
    fn shared_raster_credits_can_change() {
        // Credits that follow the view (Google's) change after the source is shared.
        let src = Arc::new(crate::raster::RasterSource::from_preset(crate::raster::preset("osm").unwrap(), None).unwrap());
        let mut map = Map::new();
        map.add_source("img", MapSource::raster(src.clone())).unwrap();
        assert_eq!(map.source("img").and_then(|s| s.attribution()), Some("© OpenStreetMap contributors"));
        map.set_attribution("img", "Google Maps · Imagery ©2026 Example").unwrap();
        assert_eq!(map.source("img").and_then(|s| s.attribution()), Some("Google Maps · Imagery ©2026 Example"));
        map.set_attribution("img", "").unwrap();
        assert_eq!(map.source("img").and_then(|s| s.attribution()), None, "empty removes it");
        drop(src);
    }

    #[test]
    fn add_map_merges_sources_and_layers_below_before() {
        let mut map = sample();
        let mut other = Map::new();
        other.add_source("v", MapSource::vector_tiles(0, 14)).unwrap();
        other.add_layer(StyleLayer::fill("land", "v").source_layer("earth").color("#222")).unwrap();
        other.add_layer(StyleLayer::line("roads", "v").source_layer("roads").color("#888")).unwrap();
        let added = map.add_map(other, Some("zones")).unwrap();
        assert_eq!(added, vec!["land", "roads"]);
        assert_eq!(map.layer_ids(), vec!["bg", "land", "roads", "zones", "pins"]);
        assert!(map.source("v").is_some());
        let mut again = Map::new();
        again.add_source("v", MapSource::vector_tiles(0, 14)).unwrap();
        assert!(map.add_map(again, None).is_err(), "source id taken");
        // Map can move to another task (built while a style loads).
        fn is_send<T: Send>() {}
        is_send::<Map>();
    }

    fn text_rows(buf: &Buffer) -> Vec<String> {
        let a = buf.area;
        (a.y..a.bottom()).map(|y| (a.x..a.right()).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    }

    #[test]
    fn points_along_a_line() {
        let line = [(0.0, 0.0), (60.0, 0.0), (100.0, 0.0)];
        assert_eq!(along(&line, 30.0), vec![(15.0, 0.0), (45.0, 0.0), (75.0, 0.0)]);
        assert_eq!(along(&line, 300.0), vec![(50.0, 0.0)], "a short line gets its middle");
        assert_eq!(along(&[(0.0, 0.0), (0.0, 40.0)], 40.0), vec![(0.0, 20.0)]);
    }

    #[test]
    fn long_roads_repeat_their_name_and_close_copies_merge() {
        let mut map = Map::new();
        // A road across the whole view, and two same-named points: one close to the other,
        // one far away.
        map.add_source(
            "r",
            MapSource::geojson(&json!({"type": "FeatureCollection", "features": [
                {"type": "Feature", "properties": {"name": "Long Road"}, "geometry": {"type": "LineString", "coordinates": [[-0.5, 0], [0.5, 0]]}},
                {"type": "Feature", "properties": {"name": "Pump"}, "geometry": {"type": "Point", "coordinates": [-0.2, 0.1]}},
                {"type": "Feature", "properties": {"name": "Pump"}, "geometry": {"type": "Point", "coordinates": [-0.195, 0.1]}},
                {"type": "Feature", "properties": {"name": "Pump"}, "geometry": {"type": "Point", "coordinates": [0.25, 0.1]}}
            ]}))
            .unwrap(),
        )
        .unwrap();
        map.add_layer(StyleLayer::symbol("road", "r").filter(json!(["==", ["geometry-type"], "LineString"])).text("{name}").color("#fff")).unwrap();
        map.add_layer(StyleLayer::symbol("pumps", "r").filter(json!(["==", ["geometry-type"], "Point"])).text("{name}").color("#fff")).unwrap();
        let buf = render(&map, &Camera::new(0.0, 0.0, 9.0), Rect::new(0, 0, 160, 20));
        let all = text_rows(&buf).join("\n");
        let roads = all.matches("Long Road").count();
        assert!(roads >= 3, "repeated along the road: {roads}\n{all}");
        assert_eq!(all.matches("Pump").count(), 2, "the close copy merges, the far one stays\n{all}");
        // Wider spacing, fewer repeats.
        map.set_layout("road", "symbol-spacing", &json!(1000)).unwrap();
        let fewer = text_rows(&render(&map, &Camera::new(0.0, 0.0, 9.0), Rect::new(0, 0, 160, 20))).join("\n").matches("Long Road").count();
        assert!(fewer < roads, "{fewer} < {roads}");
    }

    #[test]
    fn variable_anchor_and_offset() {
        let mut map = Map::new();
        map.add_source(
            "p",
            MapSource::geojson(&json!({"type": "FeatureCollection", "features": [
                {"type": "Feature", "properties": {"name": "Blocker"}, "geometry": {"type": "Point", "coordinates": [0, 0]}},
                {"type": "Feature", "properties": {"name": "Here"}, "geometry": {"type": "Point", "coordinates": [0, 0]}}
            ]}))
            .unwrap(),
        )
        .unwrap();
        // "Blocker" (higher layer, placed first) sits on the point; "Here" wants the same spot
        // first, then tries below it.
        map.add_layer(
            StyleLayer::symbol("here", "p")
                .filter(json!(["==", "name", "Here"]))
                .text("{name}")
                .color("#fff")
                .layout("text-variable-anchor", json!(["center", "top"])),
        )
        .unwrap();
        map.add_layer(StyleLayer::symbol("blocker", "p").filter(json!(["==", "name", "Blocker"])).text("{name}").color("#fff")).unwrap();
        let area = Rect::new(0, 0, 40, 11);
        let rows = text_rows(&render(&map, &Camera::new(0.0, 0.0, 5.0), area));
        assert!(rows[5].contains("Blocker") && rows[6].contains("Here"), "{rows:#?}");
        // Without the alternative, it has nowhere to go.
        map.set_layout("here", "text-variable-anchor", &json!(["center"])).unwrap();
        let rows = text_rows(&render(&map, &Camera::new(0.0, 0.0, 5.0), area));
        assert!(!rows.join("").contains("Here"));
        // text-offset in ems: two ems down is two rows.
        map.set_layout("here", "text-offset", &json!([0, 2])).unwrap();
        let rows = text_rows(&render(&map, &Camera::new(0.0, 0.0, 5.0), area));
        assert!(rows[7].contains("Here"), "{rows:#?}");
    }

    /// How many braille dots a buffer shows.
    fn dot_count(buf: &Buffer) -> u32 {
        let a = buf.area;
        let mut n = 0;
        for y in a.y..a.bottom() {
            for x in a.x..a.right() {
                if let Some(ch) = buf[(x, y)].symbol().chars().next()
                    && ('\u{2800}'..='\u{28FF}').contains(&ch)
                {
                    n += (ch as u32 - 0x2800).count_ones();
                }
            }
        }
        n
    }

    #[test]
    fn line_widths_and_dashes() {
        let line_map = |layer: StyleLayer| {
            let mut map = Map::new();
            map.add_source("l", MapSource::geojson(&json!({"type": "LineString", "coordinates": [[-0.2, 0], [0.2, 0]]})).unwrap()).unwrap();
            map.add_layer(layer).unwrap();
            dot_count(&render(&map, &Camera::new(0.0, 0.0, 9.0), Rect::new(0, 0, 80, 10)))
        };
        let solid = line_map(StyleLayer::line("l", "l").color("#fff"));
        assert!(solid > 100, "{solid}");
        let dashed = line_map(StyleLayer::line("l", "l").color("#fff").paint("line-dasharray", json!([4, 4])));
        assert!(dashed * 10 > solid * 4 && dashed * 10 < solid * 6, "about half the dots: {dashed} of {solid}");
        let no_gaps = line_map(StyleLayer::line("l", "l").color("#fff").paint("line-dasharray", json!([12, 0])));
        assert_eq!(no_gaps, solid, "no gaps is a solid line");
        let thick = line_map(StyleLayer::line("l", "l").color("#fff").paint("line-width", 10));
        assert!(thick >= solid * 2 - 4, "two dots thick: {thick} vs {solid}");
        let thicker = line_map(StyleLayer::line("l", "l").color("#fff").paint("line-width", 40));
        assert!(thicker >= solid * 3 - 6 && thicker <= solid * 3 + 6, "three at most: {thicker}");
        assert_eq!(line_map(StyleLayer::line("l", "l").color("#fff").paint("line-width", 0)), 0, "zero width draws nothing");
        // Widths don't make a layer re-evaluate as the zoom changes.
        assert!(!StyleLayer::line("l", "l").paint("line-width", json!(["interpolate", ["linear"], ["zoom"], 5, 1, 15, 8])).uses_zoom());
    }

    #[test]
    fn raster_opacity_blends_with_the_background() {
        let mut map = raster_map();
        map.add_layer_before(StyleLayer::background("bg").color("#000000"), "img").unwrap();
        map.insert_loaded("img", (0, 0, 0), Ok(Loaded(LoadedTile::Raster(Some(solid_tile(200, 100, 0)))))).unwrap();
        let cam = Camera::new(0.0, 0.0, 0.0);
        let area = Rect::new(0, 0, 40, 20);
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(200, 100, 0));
        map.set_paint("img", "raster-opacity", &json!(0.5)).unwrap();
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(100, 50, 0));
        map.set_paint("img", "raster-opacity", &json!(0.0)).unwrap();
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(0, 0, 0), "invisible: the background shows");
    }

    #[test]
    fn feature_state_by_id_and_promote_id() {
        // A vector tile feature with id 42, styled by its state.
        let mvt = crate::mvt::tests_support::square_tile_with_id("water", 4096, Some(42));
        let mut map = Map::new();
        map.add_source("v", MapSource::vector_tiles(0, 14)).unwrap();
        map.add_layer(StyleLayer::fill("water", "v").source_layer("water").color(json!([
            "case",
            ["boolean", ["feature-state", "picked"], false],
            "#ff0000",
            "#0000ff"
        ])))
        .unwrap();
        map.insert_tile("v", (0, 0, 0), Some(&mvt)).unwrap();
        let (cam, area) = (Camera::new(0.0, 0.0, 0.0), Rect::new(0, 0, 40, 20));
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(0, 0, 255));
        let id = crate::mvt::Value::Num(42.0);
        map.set_feature_state("v", &id, &json!({"picked": true})).unwrap();
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(255, 0, 0), "restyled when the state changes");
        assert_eq!(map.feature_state("v", &id), json!({"picked": true}));
        assert_eq!(map.query_rendered_features(&cam, area, (40.0, 40.0), None)[0].id, Some(id.clone()));
        map.remove_feature_state("v", Some(&id), Some("picked"));
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(0, 0, 255));
        assert!(map.set_feature_state("nope", &id, &json!({})).is_err());

        // GeoJSON without ids, using a property instead (promoteId).
        let mut map = Map::new();
        map.add_source(
            "g",
            MapSource::geojson(&json!({"type": "Feature", "properties": {"code": "DC"},
            "geometry": {"type": "Polygon", "coordinates": [[[-1, -1], [1, -1], [1, 1], [-1, 1], [-1, -1]]]}}))
            .unwrap()
            .with_promote_id("code"),
        )
        .unwrap();
        map.add_layer(StyleLayer::fill("g", "g").color(json!(["case", ["boolean", ["feature-state", "on"], false], "#00ff00", "#000000"]))).unwrap();
        let cam = Camera::new(0.0, 0.0, 6.0);
        map.set_feature_state("g", &crate::mvt::Value::Str("DC".into()), &json!({"on": true})).unwrap();
        assert_eq!(render(&map, &cam, area)[(20, 10)].bg, Color::Rgb(0, 255, 0));
        assert_eq!(map.query_features(&cam, area, (40.0, 40.0))[0].id, Some(crate::mvt::Value::Str("DC".into())));
    }

    #[test]
    fn clustered_geojson_source() {
        // 50 points in a tight group plus one far away.
        let mut feats: Vec<Json> = (0..50)
            .map(|i| {
                json!({"type": "Feature", "properties": {"name": format!("p{i}")},
            "geometry": {"type": "Point", "coordinates": [-77.03 + i as f64 * 1e-5, 38.89]}})
            })
            .collect();
        feats.push(json!({"type": "Feature", "properties": {"name": "far"}, "geometry": {"type": "Point", "coordinates": [-76.0, 38.89]}}));
        let doc = json!({"type": "FeatureCollection", "features": feats});
        let mut map = Map::new();
        map.add_source("pts", MapSource::geojson(&doc).unwrap().with_cluster(crate::cluster::ClusterOptions::default())).unwrap();
        map.add_layers(crate::geojson::overlay_layers("pts", Color::Rgb(255, 99, 132), false), None).unwrap();
        let area = Rect::new(0, 0, 80, 20);
        let cam = Camera::new(-77.03, 38.89, 10.0);
        let rows = text_rows(&render(&map, &cam, area)).join("\n");
        assert!(rows.contains("◉ 50"), "one cluster of 50: {rows}");
        let picks = map.query_rendered_features(&cam, area, (80.0, 40.0), Some(&["pts-cluster"]));
        assert_eq!(picks.len(), 1);
        let Some(crate::mvt::Value::Num(id)) = picks[0].prop("cluster_id").cloned() else { panic!("{:?}", picks[0]) };
        let z = map.cluster_expansion_zoom("pts", id as u64).unwrap();
        assert!(z > 10.0, "{z}");
        // Past the cluster max zoom, the points themselves.
        let close = Camera::new(-77.03, 38.89, 15.5);
        assert!(!text_rows(&render(&map, &close, area)).join("").contains('◉'));
        // New data keeps clustering on.
        map.set_data("pts", &doc).unwrap();
        assert!(text_rows(&render(&map, &cam, area)).join("").contains("◉ 50"));
    }
}
