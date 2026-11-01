//! Custom layers: code that draws through a [`DrawContext`] (lines, fills, markers, labels in
//! world or lon/lat coordinates) and, optionally, says which of its features are under a spot.
//! Add one to a [`crate::Map`] with `add_custom_layer` for anything style layers can't express,
//! the way MapLibre's `CustomLayerInterface` works.
//!
//! [`LayerStack`] draws a camera and a borrowed list of custom layers without a `Map`, which the
//! viewer still uses for its basemap ([`VectorTiles`]) until that moves onto style layers.

use std::collections::HashSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style as TStyle},
    widgets::Widget,
};
use unicode_width::UnicodeWidthStr;

use crate::{
    pick::Pick,
    render::{Camera, Canvas, Overlay, TILE_DOTS, Viewport, to_world},
    store::TileStore,
    style::Paint,
};

/// Something drawn on the map by your own code.
pub trait CustomLayer {
    /// A short name, shown when inspecting its features.
    fn name(&self) -> &str;

    /// Add this layer's lines, fills, markers and labels for the current view.
    fn draw(&self, ctx: &mut DrawContext);

    /// Features of this layer under `ctx.target`, most specific first.
    fn pick(&self, _ctx: &PickContext) -> Vec<Pick> {
        Vec::new()
    }

    /// Lon/lat bounds (west, south, east, north), for fitting the view to the layer.
    fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        None
    }
}

type MarkerItem = (i32, (f64, f64), String, TStyle, Option<(String, TStyle)>);
type LabelItem = (i32, String, TStyle, (f64, f64));

/// What a layer draws into. Positions are braille dots relative to the map area (2 per
/// column, 4 per row); helpers take world (Web Mercator 0..1) or lon/lat coordinates.
/// Lines and fills draw immediately, in layer order; markers and labels are collected and
/// placed after every layer, highest priority first, so they never overlap.
pub struct DrawContext<'a> {
    pub camera: &'a Camera,
    pub viewport: Viewport,
    canvas: &'a mut Canvas,
    /// [min_x, min_y, max_x, max_y] of the view in world coordinates.
    view: [f64; 4],
    size: (f64, f64),
    markers: Vec<MarkerItem>,
    labels: Vec<LabelItem>,
    /// Added to marker and label priorities, so higher layers win collisions.
    pub(crate) base: i32,
}

impl DrawContext<'_> {
    /// World coordinates -> dots.
    pub fn dot(&self, x: f64, y: f64) -> (f64, f64) {
        self.viewport.dot(x, y)
    }

    /// Whether a world-coordinate bounding box [min_x, min_y, max_x, max_y] overlaps the view.
    pub fn visible(&self, bbox: [f64; 4]) -> bool {
        !(bbox[2] < self.view[0] || bbox[0] > self.view[2] || bbox[3] < self.view[1] || bbox[1] > self.view[3])
    }

    /// The view in world coordinates [min_x, min_y, max_x, max_y].
    pub fn world_view(&self) -> [f64; 4] {
        self.view
    }

    /// Whether a dot position is on the map.
    pub fn inside(&self, p: (f64, f64)) -> bool {
        p.0 >= 0.0 && p.1 >= 0.0 && p.0 < self.size.0 && p.1 < self.size.1
    }

    /// A polyline in dots.
    pub fn line_dots(&mut self, pts: &[(f64, f64)], color: Color) {
        for s in pts.windows(2) {
            self.canvas.line(s[0], s[1], color);
        }
    }

    /// A polyline in world coordinates.
    pub fn line(&mut self, pts: &[(f64, f64)], color: Color) {
        let d: Vec<(f64, f64)> = pts.iter().map(|&(x, y)| self.dot(x, y)).collect();
        self.line_dots(&d, color);
    }

    /// A polyline in (lon, lat).
    pub fn line_lonlat(&mut self, pts: &[(f64, f64)], color: Color) {
        let d: Vec<(f64, f64)> = pts.iter().map(|&(lon, lat)| self.viewport.dot_ll(lon, lat)).collect();
        self.line_dots(&d, color);
    }

    /// Fill polygon rings (even-odd, holes allowed), in dots, into cell backgrounds.
    pub fn fill_dots(&mut self, rings: &[Vec<(f64, f64)>], color: Color) {
        self.canvas.fill(rings, color);
    }

    /// Fill polygon rings in world coordinates.
    pub fn fill(&mut self, rings: &[Vec<(f64, f64)>], color: Color) {
        let d: Vec<Vec<(f64, f64)>> = rings.iter().map(|r| r.iter().map(|&(x, y)| self.dot(x, y)).collect()).collect();
        self.fill_dots(&d, color);
    }

    /// A text marker at a dot position, with an optional label to its right.
    pub fn marker_dot(&mut self, p: (f64, f64), glyph: &str, style: TStyle, priority: i32, label: Option<(String, TStyle)>) {
        self.markers.push((self.base.saturating_add(priority), p, glyph.to_string(), style, label));
    }

    /// A text marker at (lon, lat).
    pub fn marker(&mut self, lon: f64, lat: f64, glyph: &str, style: TStyle, priority: i32, label: Option<(String, TStyle)>) {
        let p = self.viewport.dot_ll(lon, lat);
        self.marker_dot(p, glyph, style, priority, label);
    }

    /// A label centered on a dot position (skipped where it would overlap another).
    pub fn label_dot(&mut self, p: (f64, f64), text: &str, style: TStyle, priority: i32) {
        if self.inside(p) {
            self.labels.push((self.base.saturating_add(priority), text.to_string(), style, p));
        }
    }
}

/// Where to look for features: `target` is a dot position relative to `area`.
pub struct PickContext<'a> {
    pub camera: &'a Camera,
    pub area: Rect,
    pub viewport: Viewport,
    pub target: (f64, f64),
}

/// Features under `target` (dots relative to `area`) across `layers`, top layer first.
pub fn pick_all(layers: &[&dyn CustomLayer], camera: &Camera, area: Rect, target: (f64, f64)) -> Vec<Pick> {
    let ctx = PickContext { camera, area, viewport: Viewport::new(camera, area), target };
    layers.iter().rev().flat_map(|l| l.pick(&ctx)).collect()
}

/// Set up a canvas for `area`, let `draw` fill it, then place markers and labels (highest
/// priority first, never overlapping) and write the cells into `buf`.
pub(crate) fn render_with(camera: &Camera, area: Rect, buf: &mut Buffer, draw: impl FnOnce(&mut DrawContext)) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let mut canvas = Canvas::new(area.width as usize, area.height as usize);
    let (vw, vh) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
    let view = {
        let (cx, cy) = to_world(camera.lon, camera.lat);
        let s = TILE_DOTS * 2f64.powf(camera.zoom);
        let (hw, hh) = (vw / 2.0 / s, vh / 2.0 / s);
        [cx - hw, cy - hh, cx + hw, cy + hh]
    };
    let mut ctx = DrawContext {
        camera,
        viewport: Viewport::new(camera, area),
        canvas: &mut canvas,
        view,
        size: (vw, vh),
        markers: Vec::new(),
        labels: Vec::new(),
        base: 0,
    };
    draw(&mut ctx);
    let DrawContext { mut markers, mut labels, .. } = ctx;

    // Markers claim space first, then labels, highest priority first.
    let inside = |p: (f64, f64)| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < vw && p.1 < vh;
    markers.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p, glyph, st, label) in markers {
        if !inside(p) {
            continue;
        }
        let (col, row) = ((p.0 / 2.0) as i64, (p.1 / 4.0) as i64);
        if canvas.text_at(col, row, &glyph, st, false)
            && let Some((l, ls)) = label
        {
            canvas.text_at(col + glyph.width() as i64 + 1, row, &l, ls, true);
        }
    }
    labels.sort_by(|a, b| b.0.cmp(&a.0));
    let mut placed = HashSet::new();
    for (_, text, st, p) in labels {
        if placed.contains(&text) {
            continue;
        }
        let col = (p.0 / 2.0) as i64 - text.width() as i64 / 2;
        if canvas.text_at(col, (p.1 / 4.0) as i64, &text, st, true) {
            placed.insert(text);
        }
    }
    canvas.flush(area, buf);
}

/// Draws a camera and a borrowed stack of custom layers (first at the bottom).
pub struct LayerStack<'a> {
    pub camera: &'a Camera,
    pub layers: &'a [&'a dyn CustomLayer],
}

impl Widget for LayerStack<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_with(self.camera, area, buf, |ctx| {
            for layer in self.layers {
                layer.draw(ctx);
            }
        });
    }
}

/// The vector tile basemap (from a [`TileStore`]) as a layer.
pub struct VectorTiles<'a> {
    pub store: &'a TileStore,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// Area fills (water, parks, buildings); turn off over a raster to keep it visible.
    pub fills: bool,
    /// Roads, rivers, boundaries.
    pub lines: bool,
    pub labels: bool,
}

impl<'a> VectorTiles<'a> {
    pub fn new(store: &'a TileStore, min_zoom: u8, max_zoom: u8) -> Self {
        Self { store, min_zoom, max_zoom, fills: true, lines: true, labels: true }
    }
}

impl CustomLayer for VectorTiles<'_> {
    fn name(&self) -> &str {
        "basemap"
    }

    fn draw(&self, ctx: &mut DrawContext) {
        let vp = ctx.viewport;
        let tz = ctx.camera.tile_zoom(self.min_zoom, self.max_zoom);
        // Loaded tiles (or stand-in ancestors), each once, ancestors first so real tiles win.
        let mut seen = HashSet::new();
        let mut tiles = Vec::new();
        for id in vp.tiles(tz) {
            if let Some((aid, t)) = self.store.get_or_ancestor(id)
                && seen.insert(aid)
            {
                tiles.push((aid, t));
            }
        }
        tiles.sort_by_key(|(id, _)| id.0);
        let mut items: Vec<(u16, usize, usize)> = Vec::new();
        for (ti, (_, t)) in tiles.iter().enumerate() {
            for (ii, it) in t.items.iter().enumerate() {
                items.push((it.order, ti, ii));
            }
        }
        items.sort_by_key(|(o, _, _)| *o);
        for (_, ti, ii) in items {
            let ((z, tx, ty), t) = &tiles[ti];
            let n = (1u64 << z) as f64;
            let it = &t.items[ii];
            let to_dot = |p: &[f32; 2]| vp.dot((*tx as f64 + p[0] as f64) / n, (*ty as f64 + p[1] as f64) / n);
            match &it.paint {
                Paint::Fill(c) if self.fills => {
                    let rings: Vec<Vec<(f64, f64)>> = it.parts.iter().map(|r| r.iter().map(to_dot).collect()).collect();
                    ctx.fill_dots(&rings, *c);
                }
                Paint::Line(c) if self.lines => {
                    for part in it.parts.iter() {
                        let pts: Vec<(f64, f64)> = part.iter().map(to_dot).collect();
                        ctx.line_dots(&pts, *c);
                    }
                }
                Paint::Label { text, color, priority, bold } if self.labels => {
                    // Anchor at fixed vertices along the feature (not the middle of whatever is
                    // on screen), so labels stay put while the map pans.
                    let anchor = it.parts.iter().find_map(|part| {
                        let step = (part.len() / 3).max(1);
                        (step / 2..part.len()).step_by(step).map(|i| to_dot(&part[i])).find(|p| ctx.inside(*p))
                    });
                    if let Some(p) = anchor {
                        let mut st = TStyle::new().fg(*color);
                        if *bold {
                            st = st.add_modifier(Modifier::BOLD);
                        }
                        ctx.label_dot(p, text, st, *priority);
                    }
                }
                _ => {}
            }
        }
    }

    fn pick(&self, ctx: &PickContext) -> Vec<Pick> {
        crate::pick::pick_dot(self.store, ctx.camera, self.min_zoom, self.max_zoom, ctx.area, ctx.target)
    }
}

/// A list of [`Overlay`] markers and lines as a layer.
pub struct Markers<'a>(pub &'a [Overlay]);

impl CustomLayer for Markers<'_> {
    fn name(&self) -> &str {
        "markers"
    }

    fn draw(&self, ctx: &mut DrawContext) {
        for o in self.0 {
            match o {
                Overlay::Line { pts, color } => ctx.line_lonlat(pts, *color),
                Overlay::Marker { lon, lat, glyph, style, label, priority } => {
                    ctx.marker(*lon, *lat, glyph, *style, *priority, label.clone())
                }
            }
        }
    }
}
