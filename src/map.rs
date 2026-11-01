//! A map, MapLibre GL style: named sources (vector tiles, GeoJSON) plus an ordered list of
//! style layers that say how to draw them, with custom layers for anything else. The app
//! changes the map with `set_data`, `set_paint`, `set_filter` and friends, draws it with
//! [`Map::view`], and asks what's under the pointer with [`Map::query_rendered_features`].
//!
//! ```ignore
//! let mut map = Map::new();
//! map.add_source("basemap", MapSource::vector(Source::open(OPENFREEMAP).await?))?;
//! map.add_source("route", MapSource::geojson(&route_json)?)?;
//! map.add_layer(StyleLayer::background("bg").color("#101418"))?;
//! map.add_layer(StyleLayer::line("roads", "basemap").source_layer("transportation").color("#666"))?;
//! map.add_layer(StyleLayer::line("route", "route").color("#00e5ff"))?;
//! loop {
//!     map.load_tiles(&cam, area).await;
//!     frame.render_widget(map.view(&cam), area);
//!     for f in map.query_rendered_features(&cam, area, dot, Some(&["route"])) { .. }
//! }
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
    expr::{Ctx, Rgba},
    geojson,
    layer::{CustomLayer, DrawContext, PickContext, render_with},
    mvt::GeomType,
    pick::{Pick, hit, stats},
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
    Vector(VectorSource),
    GeoJson(GeoJsonSource),
}

impl MapSource {
    /// Vector tiles fetched from `src` by [`Map::load_tiles`].
    pub fn vector(src: Source) -> Self {
        MapSource::Vector(VectorSource { min_zoom: src.min_zoom, max_zoom: src.max_zoom, fetch: Some(Arc::new(src)), tiles: Tiles::default() })
    }

    /// Vector tiles the app supplies itself with [`Map::insert_tile`].
    pub fn vector_tiles(min_zoom: u8, max_zoom: u8) -> Self {
        MapSource::Vector(VectorSource { min_zoom, max_zoom, fetch: None, tiles: Tiles::default() })
    }

    /// A GeoJSON document (FeatureCollection, Feature or geometry).
    pub fn geojson(doc: &Json) -> Result<Self> {
        Ok(MapSource::GeoJson(GeoJsonSource::parse(doc)?))
    }
}

impl From<geojson::GeoJson> for MapSource {
    fn from(g: geojson::GeoJson) -> Self {
        MapSource::GeoJson(GeoJsonSource { features: g.features, bbox: g.bbox })
    }
}

pub struct VectorSource {
    pub min_zoom: u8,
    pub max_zoom: u8,
    fetch: Option<Arc<Source>>,
    tiles: Tiles,
}

pub struct GeoJsonSource {
    pub features: Vec<geojson::Feature>,
    /// [min_x, min_y, max_x, max_y] in world coordinates.
    pub bbox: [f64; 4],
}

impl GeoJsonSource {
    pub fn parse(doc: &Json) -> Result<Self> {
        let (features, bbox) = geojson::features(doc)?;
        Ok(Self { features, bbox })
    }
}

/// What a style layer resolved to for one feature.
#[derive(Clone, Debug)]
enum Resolved {
    Fill { color: Option<Color>, outline: Option<Color> },
    Line(Color),
    /// A glyph at each point, with an optional label beside it.
    Point { glyph: String, style: TStyle, label: Option<(String, TStyle)>, priority: i32 },
    Text { text: String, style: TStyle, anchor: (i8, i8), along_line: bool, priority: i32 },
}

struct Item {
    layer: u16,
    feature: u32,
    paint: Resolved,
}

/// One decoded vector tile: its features and what the style draws of them, by layer.
struct MapTile {
    features: Arc<Vec<store::Feature>>,
    /// Sorted by layer.
    items: Vec<Item>,
}

impl MapTile {
    fn items_of(&self, layer: usize) -> &[Item] {
        let lo = self.items.partition_point(|i| (i.layer as usize) < layer);
        let hi = self.items.partition_point(|i| (i.layer as usize) <= layer);
        &self.items[lo..hi]
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
trait OwnedCustom: CustomLayer + Any {}
impl<T: CustomLayer + Any> OwnedCustom for T {}

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
pub struct Map {
    sources: Vec<(String, MapSource)>,
    layers: Vec<Entry>,
    warnings: Vec<String>,
}

impl Map {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- sources ----

    pub fn add_source(&mut self, id: impl Into<String>, src: MapSource) -> Result<()> {
        let id = id.into();
        if self.source(&id).is_some() {
            bail!("source \"{id}\" already exists");
        }
        self.sources.push((id, src));
        self.restyle();
        Ok(())
    }

    /// Remove a source; layers that use it stay but draw nothing.
    pub fn remove_source(&mut self, id: &str) -> bool {
        let n = self.sources.len();
        self.sources.retain(|(s, _)| s != id);
        n != self.sources.len()
    }

    pub fn source(&self, id: &str) -> Option<&MapSource> {
        self.sources.iter().find(|(s, _)| s == id).map(|(_, s)| s)
    }

    /// Replace a GeoJSON source's data (MapLibre's `setData`).
    pub fn set_data(&mut self, id: &str, doc: &Json) -> Result<()> {
        let data = GeoJsonSource::parse(doc)?;
        match self.sources.iter_mut().find(|(s, _)| s == id) {
            Some((_, MapSource::GeoJson(g))) => *g = data,
            Some(_) => bail!("source \"{id}\" is not a GeoJSON source"),
            None => bail!("no source \"{id}\""),
        }
        Ok(())
    }

    /// Lon/lat bounds (west, south, east, north) of a GeoJSON source, for fitting the view.
    pub fn source_bounds(&self, id: &str) -> Option<(f64, f64, f64, f64)> {
        match self.source(id)? {
            MapSource::GeoJson(g) if !g.features.is_empty() => {
                let (w, n) = to_lonlat(g.bbox[0], g.bbox[1]);
                let (e, s) = to_lonlat(g.bbox[2], g.bbox[3]);
                Some((w, s, e, n))
            }
            _ => None,
        }
    }

    /// Vector tiles the view needs that aren't loaded yet, for sources that shown layers use.
    pub fn missing_tiles(&self, cam: &Camera, area: Rect) -> Vec<(String, TileId)> {
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
            if let MapSource::Vector(v) = src
                && used.contains(id.as_str())
            {
                let z = cam.tile_zoom(v.min_zoom, v.max_zoom);
                out.extend(vp.tiles(z).into_iter().filter(|t| !v.tiles.map.contains_key(t)).map(|t| (id.clone(), t)));
            }
        }
        out
    }

    /// Fetch the missing tiles of sources made with [`MapSource::vector`], one at a time.
    /// A tile that fails to load is stored empty (so it isn't retried every frame) and the
    /// first error is returned after the rest have loaded.
    pub async fn load_tiles(&mut self, cam: &Camera, area: Rect) -> Result<()> {
        let mut first_err = None;
        for (sid, id) in self.missing_tiles(cam, area) {
            let Some(MapSource::Vector(VectorSource { fetch: Some(src), .. })) = self.source(&sid) else { continue };
            let src = src.clone();
            let bytes = match src.tile(id.0, id.1, id.2).await {
                Ok(b) => b,
                Err(e) => {
                    first_err.get_or_insert(e);
                    None
                }
            };
            if let Err(e) = self.insert_tile(&sid, id, bytes.as_deref()) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
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
        let features = Arc::new(features);
        let items = style_tile(&self.layers, source, &features, id.0 as f64);
        match self.sources.iter_mut().find(|(s, _)| s == source) {
            Some((_, MapSource::Vector(v))) => v.tiles.insert(id, MapTile { features, items }),
            _ => bail!("no vector source \"{source}\""),
        }
        err.map_or(Ok(()), |e| Err(e.context(format!("tile {}/{}/{}", id.0, id.1, id.2))))
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
    pub fn add_custom_layer<T: CustomLayer + Any>(&mut self, id: impl Into<String>, layer: T, before: Option<&str>) -> Result<()> {
        self.insert_entry(Entry::Custom { id: id.into(), layer: Box::new(layer) }, before)
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

    pub fn layer(&self, id: &str) -> Option<&StyleLayer> {
        match &self.layers[self.index(id)?] {
            Entry::Style(l) => Some(l),
            Entry::Custom { .. } => None,
        }
    }

    /// A custom layer by id and type, to change its state.
    pub fn custom_layer_mut<T: CustomLayer + Any>(&mut self, id: &str) -> Option<&mut T> {
        let i = self.index(id)?;
        match &mut self.layers[i] {
            Entry::Custom { layer, .. } => (&mut **layer as &mut dyn Any).downcast_mut::<T>(),
            Entry::Style(_) => None,
        }
    }

    pub fn set_paint(&mut self, id: &str, name: &str, v: &Json) -> Result<()> {
        self.style_mut(id)?.set_paint(name, v)?;
        self.restyle();
        Ok(())
    }

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

    pub fn set_visibility(&mut self, id: &str, visible: bool) -> Result<()> {
        self.style_mut(id)?.visible = visible;
        Ok(())
    }

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
                    v.tiles.map.insert(id, Arc::new(MapTile { features, items }));
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
            let style_layer: Arc<str> = Arc::from(l.id.as_str());
            let source: Arc<str> = Arc::from(sid);
            let edges = l.kind == Kind::Line;
            let mut hits: Vec<(u8, f64, Pick)> = Vec::new();
            let mut push = |geom: GeomType, dots: &[Vec<(f64, f64)>], lonlat: Vec<Vec<(f64, f64)>>, layer: Arc<str>, props: &[(String, crate::mvt::Value)], tile: Option<TileId>| {
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
                    for it in tile.items_of(i) {
                        let f = &tile.features[it.feature as usize];
                        let dots: Vec<Vec<(f64, f64)>> = f.parts.iter().map(|part| part.iter().map(|p| { let (x, y) = world(p); vp.dot(x, y) }).collect()).collect();
                        if hit(f.geom, &dots, target, edges).is_none() || !seen.insert(format!("{:?}{:?}", f.geom, f.props)) {
                            continue;
                        }
                        let lonlat = f.parts.iter().map(|part| part.iter().map(|p| { let (x, y) = world(p); to_lonlat(x, y) }).collect()).collect();
                        push(f.geom, &dots, lonlat, f.layer.clone(), &f.props, Some(tid));
                    }
                }
                Some(MapSource::GeoJson(g)) => {
                    for f in &g.features {
                        let c = Ctx { zoom: cam.zoom, geom: f.geom, props: &f.props };
                        if resolve(l, &c, bg).is_none() {
                            continue;
                        }
                        let dots: Vec<Vec<(f64, f64)>> = f.parts.iter().map(|p| p.iter().map(|&(x, y)| vp.dot(x, y)).collect()).collect();
                        let lonlat = f.parts.iter().map(|p| p.iter().map(|&(x, y)| to_lonlat(x, y)).collect()).collect();
                        push(f.geom, &dots, lonlat, source.clone(), &f.props, None);
                    }
                }
                None => {}
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
                let c = Ctx { zoom, geom: GeomType::Unknown, props: &[] };
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
            l.paint_expr("background-color").and_then(|e| e.eval(&Ctx { zoom, geom: GeomType::Unknown, props: &[] }).as_color())
        }
        _ => None,
    });
    let mut items = Vec::new();
    for (li, e) in layers.iter().enumerate() {
        let Entry::Style(l) = e else { continue };
        if l.source.as_deref() != Some(source) {
            continue;
        }
        let Some(fs) = l.source_layer.as_deref().and_then(|s| by_layer.get(s)) else { continue };
        for &fi in fs {
            let f = &features[fi];
            if let Some(paint) = resolve(l, &Ctx { zoom, geom: f.geom, props: &f.props }, bg) {
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

/// What one style layer draws for one feature, or `None`.
fn resolve(l: &StyleLayer, c: &Ctx, bg: Option<Rgba>) -> Option<Resolved> {
    if let Some(f) = &l.filter
        && !f.eval(c).as_bool()
    {
        return None;
    }
    let color = |prop: &str, opacity: &str, default: Option<Rgba>| -> Option<Color> {
        let rgba = l.paint_expr(prop).and_then(|e| e.eval(c).as_color()).or(default)?;
        let o = l.paint_expr(opacity).and_then(|e| e.eval(c).as_num()).unwrap_or(1.0);
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
        Kind::Line if c.geom != GeomType::Point => color("line-color", "line-opacity", Some(BLACK)).map(Resolved::Line),
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
            let text = layout_str("text-field")
                .and_then(|t| t.lines().map(str::trim).find(|s| !s.is_empty()).map(str::to_string))
                .map(|t| match layout_str("text-transform").as_deref() {
                    Some("uppercase") => t.to_uppercase(),
                    Some("lowercase") => t.to_lowercase(),
                    _ => t,
                });
            let glyph = layout_str("mapterm:glyph");
            let priority = sort();
            match (glyph, text) {
                (Some(glyph), text) if c.geom == GeomType::Point => {
                    Some(Resolved::Point { glyph, style, label: text.map(|t| (t, style)), priority })
                }
                (_, Some(text)) => {
                    let anchor = match layout_str("text-anchor").as_deref() {
                        Some("left") => (1, 0),
                        Some("right") => (-1, 0),
                        Some("top") => (0, 1),
                        Some("bottom") => (0, -1),
                        Some("top-left") => (1, 1),
                        Some("top-right") => (-1, 1),
                        Some("bottom-left") => (1, -1),
                        Some("bottom-right") => (-1, -1),
                        _ => (0, 0),
                    };
                    let along_line = c.geom != GeomType::Point && layout_str("symbol-placement").is_some_and(|p| p.starts_with("line"));
                    Some(Resolved::Text { text, style, anchor, along_line, priority })
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
        Resolved::Line(c) => {
            for part in dots {
                ctx.line_dots(part, *c);
            }
        }
        Resolved::Point { glyph, style, label, priority } => {
            for &p in dots.iter().flatten() {
                ctx.marker_dot(p, glyph, *style, *priority, label.clone());
            }
        }
        Resolved::Text { text, style, anchor, along_line, priority } => {
            let shift = |p: (f64, f64)| {
                // Half the text's width in dots (2 per column), plus a column of room.
                let half = text.width() as f64 + 2.0;
                (p.0 + anchor.0 as f64 * half, p.1 + anchor.1 as f64 * 4.0)
            };
            if geom == GeomType::Point {
                for &p in dots.iter().flatten() {
                    ctx.label_dot(shift(p), text, *style, *priority);
                }
            } else if *along_line || geom == GeomType::Line {
                // Anchor at fixed vertices along the feature (not the middle of whatever is on
                // screen), so labels stay put while the map pans.
                let anchor = dots.iter().find_map(|part| {
                    let step = (part.len() / 3).max(1);
                    (step / 2..part.len()).step_by(step).map(|i| part[i]).find(|p| ctx.inside(*p))
                });
                if let Some(p) = anchor {
                    ctx.label_dot(shift(p), text, *style, *priority);
                }
            } else {
                // Polygons: the middle of the outer ring's bounding box.
                if let Some(ring) = dots.first().filter(|r| !r.is_empty()) {
                    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                    for &(x, y) in ring {
                        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                    }
                    ctx.label_dot(shift(((x0 + x1) / 2.0, (y0 + y1) / 2.0)), text, *style, *priority);
                }
            }
        }
    }
}

/// Draws a [`Map`] with a camera. Make one with [`Map::view`].
pub struct MapView<'a> {
    pub map: &'a Map,
    pub camera: &'a Camera,
}

impl Widget for MapView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (map, cam) = (self.map, self.camera);
        let vp = Viewport::new(cam, area);
        let bg = map.background(cam.zoom);
        // Loaded tiles per vector source, looked up once per frame.
        let tiles: HashMap<&str, Vec<(TileId, Arc<MapTile>)>> = map
            .sources
            .iter()
            .filter_map(|(id, s)| match s {
                MapSource::Vector(v) => Some((id.as_str(), v.tiles.visible(&vp, cam.tile_zoom(v.min_zoom, v.max_zoom)))),
                MapSource::GeoJson(_) => None,
            })
            .collect();
        render_with(cam, area, buf, |ctx| {
            for (i, entry) in map.layers.iter().enumerate() {
                ctx.base = i as i32 * LAYER_PRIORITY;
                let l = match entry {
                    Entry::Style(l) => l,
                    Entry::Custom { layer, .. } => {
                        layer.draw(ctx);
                        continue;
                    }
                };
                if !l.shown_at(cam.zoom) {
                    continue;
                }
                if l.kind == Kind::Background {
                    if let Some(c) = l.paint_expr("background-color").and_then(|e| e.eval(&Ctx { zoom: cam.zoom, geom: GeomType::Unknown, props: &[] }).as_color())
                        && let Some(c) = term_color(c, l.paint_expr("background-opacity").and_then(|e| e.eval(&Ctx { zoom: cam.zoom, geom: GeomType::Unknown, props: &[] }).as_num()).unwrap_or(1.0), None)
                    {
                        let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
                        ctx.fill_dots(&[vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h), (0.0, 0.0)]], c);
                    }
                    continue;
                }
                let Some(sid) = l.source.as_deref() else { continue };
                match map.source(sid) {
                    Some(MapSource::Vector(_)) => {
                        for ((z, tx, ty), tile) in tiles.get(sid).into_iter().flatten() {
                            let n = (1u64 << z) as f64;
                            for it in tile.items_of(i) {
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
                        for f in &g.features {
                            if !ctx.visible(f.bbox) {
                                continue;
                            }
                            let Some(r) = resolve(l, &Ctx { zoom: cam.zoom, geom: f.geom, props: &f.props }, bg) else { continue };
                            let dots: Vec<Vec<(f64, f64)>> = f.parts.iter().map(|p| p.iter().map(|&(x, y)| vp.dot(x, y)).collect()).collect();
                            draw_resolved(ctx, &r, f.geom, &dots);
                        }
                    }
                    None => {}
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
        let text: String = (0..area.height).map(|y| (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n");
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
        let ids: Vec<(String, String)> = map
            .query_rendered_features(&cam, area, center, None)
            .iter()
            .map(|p| (p.style_layer.as_deref().unwrap().to_string(), p.title()))
            .collect();
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
        assert!(map.missing_tiles(&cam, area).is_empty());
    }
}
