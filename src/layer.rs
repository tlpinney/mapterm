//! Custom layers: code that draws through a [`DrawContext`] (lines, fills, markers, labels in
//! world or lon/lat coordinates) and, optionally, says which of its features are under a spot.
//! Add one to a [`crate::Map`] with `add_custom_layer` for anything style layers can't express,
//! the way MapLibre's `CustomLayerInterface` works.
//!
//! [`Markers`] is a ready-made one for a list of [`Overlay`] markers and lines.

use std::collections::HashMap;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style as TStyle},

};
use unicode_width::UnicodeWidthStr;

use crate::{
    pick::Pick,
    render::{Camera, Canvas, Overlay, TILE_DOTS, Viewport, to_world},
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
/// A label: priority, text, style, candidate positions (tried in order), and how far (in
/// dots) it must be from a placed label with the same text.
type LabelItem = (i32, String, TStyle, Vec<(f64, f64)>, f64);

/// By default the same text isn't placed again within this many braille dots (20 columns or
/// 10 rows; a dot is about as tall as it is wide), so copies of one label from neighboring
/// tiles merge.
pub const REPEAT_DOTS: f64 = 40.0;

/// What a layer draws into. Positions are braille dots relative to the map area (2 per
/// column, 4 per row); helpers take world (Web Mercator 0..1) or lon/lat coordinates.
/// Lines and fills draw immediately, in layer order; markers and labels are collected and
/// placed after every layer, highest priority first, so they never overlap.
pub struct DrawContext<'a> {
    /// The camera being drawn.
    pub camera: &'a Camera,
    /// Maps world and lon/lat coordinates to dots.
    pub viewport: Viewport,
    canvas: &'a mut Canvas,
    /// Pixel mode: fills and lines go here instead of braille dots.
    pix: Option<&'a mut crate::pixels::Pix>,
    /// Skip fills and lines (pixel mode draws style layers' geometry into tile images; this
    /// pass only places their labels and markers).
    pub(crate) geometry_off: bool,
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

    /// Whether fills and lines are drawn as pixels (see [`crate::Map::pixel_mode`]).
    pub fn pixels(&self) -> bool {
        self.pix.is_some()
    }

    /// A polyline in dots, `width` CSS pixels wide, dashed by `dash` (on/off lengths in line
    /// widths). As pixels in pixel mode; otherwise braille, 1 to 3 dots wide.
    pub fn line_px(&mut self, pts: &[(f64, f64)], color: Color, width: f64, dash: Option<&[f64]>) {
        if self.geometry_off {
            return;
        }
        if let Some(p) = self.pix.as_deref_mut() {
            p.stroke(pts, color, width, dash);
            return;
        }
        let dots = if width < 6.0 { 1 } else if width < 14.0 { 2 } else { 3 };
        let dash: Option<Vec<f64>> = dash.map(|d| d.iter().map(|n| (n * width.max(1.0) / 4.0).max(1.0)).collect());
        self.line_styled(pts, color, dots, dash.as_deref());
    }

    /// A polyline in dots.
    pub fn line_dots(&mut self, pts: &[(f64, f64)], color: Color) {
        if self.geometry_off {
            return;
        }
        if let Some(p) = self.pix.as_deref_mut() {
            p.stroke(pts, color, 1.0, None);
            return;
        }
        for s in pts.windows(2) {
            self.canvas.line(s[0], s[1], color);
        }
    }

    /// A polyline in dots, `width` dots thick (1 to 3), dashed when `dash` gives on and off
    /// lengths in dots (the pattern runs on across the polyline's corners).
    pub fn line_styled(&mut self, pts: &[(f64, f64)], color: Color, width: u8, dash: Option<&[f64]>) {
        let offsets: &[f64] = match width {
            0 | 1 => &[0.0],
            2 => &[-0.5, 0.5],
            _ => &[-1.0, 0.0, 1.0],
        };
        for &k in offsets {
            let mut phase = 0.0;
            for s in pts.windows(2) {
                let (a, b) = (s[0], s[1]);
                let len = (b.0 - a.0).hypot(b.1 - a.1);
                // Shift sideways for thickness, along the segment's normal.
                let (nx, ny) = if len > 0.0 { (-(b.1 - a.1) / len * k, (b.0 - a.0) / len * k) } else { (0.0, 0.0) };
                let (a, b) = ((a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny));
                match dash {
                    Some(d) => self.canvas.line_dashed(a, b, color, d, phase),
                    None => self.canvas.line(a, b, color),
                }
                phase += len;
            }
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
        if self.geometry_off {
            return;
        }
        match self.pix.as_deref_mut() {
            Some(p) => p.fill(rings, color),
            None => self.canvas.fill(rings, color),
        }
    }

    /// Fill polygon rings in world coordinates.
    pub fn fill(&mut self, rings: &[Vec<(f64, f64)>], color: Color) {
        let d: Vec<Vec<(f64, f64)>> = rings.iter().map(|r| r.iter().map(|&(x, y)| self.dot(x, y)).collect()).collect();
        self.fill_dots(&d, color);
    }

    /// Set the background of one cell (column, row relative to the map area).
    pub fn cell_bg(&mut self, col: u16, row: u16, color: Color) {
        self.canvas.set_bg(col as usize, row as usize, color);
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
        self.label_dots(vec![p], text, style, priority);
    }

    /// A label with candidate positions (dots), tried in order until one fits, like
    /// MapLibre's `text-variable-anchor`. Positions off the map are dropped.
    pub fn label_dots(&mut self, candidates: Vec<(f64, f64)>, text: &str, style: TStyle, priority: i32) {
        self.label_spaced(candidates, text, style, priority, REPEAT_DOTS);
    }

    /// Like [`DrawContext::label_dots`], kept at least `repeat` dots from any placed label
    /// with the same text (road names repeated along a road).
    pub fn label_spaced(&mut self, mut candidates: Vec<(f64, f64)>, text: &str, style: TStyle, priority: i32, repeat: f64) {
        candidates.retain(|p| self.inside(*p));
        if !candidates.is_empty() {
            self.labels.push((self.base.saturating_add(priority), text.to_string(), style, candidates, repeat));
        }
    }
}

/// Where to look for features: `target` is a dot position relative to `area`.
pub struct PickContext<'a> {
    /// The camera.
    pub camera: &'a Camera,
    /// The map's area on screen.
    pub area: Rect,
    /// Maps world and lon/lat coordinates to dots.
    pub viewport: Viewport,
    /// The spot to look under, in dots relative to `area`.
    pub target: (f64, f64),
}

/// Set up a canvas for `area`, let `draw` fill it, then place markers and labels (highest
/// priority first, never overlapping) and write the cells into `buf`.
#[cfg(test)]
pub(crate) fn render_with(camera: &Camera, area: Rect, buf: &mut Buffer, draw: impl FnOnce(&mut DrawContext)) {
    render_into(camera, area, buf, None, draw)
}

/// [`render_with`], with fills and lines going to `pix` when given (pixel mode).
pub(crate) fn render_into(camera: &Camera, area: Rect, buf: &mut Buffer, pix: Option<&mut crate::pixels::Pix>, draw: impl FnOnce(&mut DrawContext)) {
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
        pix,
        geometry_off: false,
        view,
        size: (vw, vh),
        markers: Vec::new(),
        labels: Vec::new(),
        base: 0,
    };
    draw(&mut ctx);
    let DrawContext { mut markers, mut labels, .. } = ctx;

    // Markers and labels in one order, highest priority first (higher layers, then lower
    // sort keys), as MapLibre places symbols. A marker with a label shows both or neither,
    // unless the label would only run off the edge of the map.
    enum Sym {
        Marker((f64, f64), String, TStyle, Option<(String, TStyle)>),
        Label(String, TStyle, Vec<(f64, f64)>, f64),
    }
    let inside = |p: (f64, f64)| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < vw && p.1 < vh;
    let mut syms: Vec<(i32, usize, Sym)> = Vec::with_capacity(markers.len() + labels.len());
    for (pri, p, g, st, l) in markers.drain(..) {
        syms.push((pri, syms.len(), Sym::Marker(p, g, st, l)));
    }
    for (pri, t, st, p, repeat) in labels.drain(..) {
        syms.push((pri, syms.len(), Sym::Label(t, st, p, repeat)));
    }
    // Stable among equal priorities: markers before labels, then in the order added.
    syms.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut placed: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    for (_, _, sym) in syms {
        match sym {
            Sym::Marker(p, glyph, st, label) => {
                if !inside(p) {
                    continue;
                }
                let (col, row) = ((p.0 / 2.0) as i64, (p.1 / 4.0) as i64);
                let lcol = col + glyph.width() as i64 + 1;
                // A label running off the right edge is cut short with "…", as a browser
                // map clips it; with no room for a few characters the marker shows alone.
                let room = canvas.width() as i64 - lcol;
                let label = label.and_then(|(l, ls)| match l.width() as i64 {
                    w if w <= room => Some((l, ls)),
                    _ if room >= 4 => Some((clip(&l, room as usize - 1) + "…", ls)),
                    _ => None,
                });
                match label {
                    Some((l, ls)) => {
                        if canvas.fits(col, row, &glyph, false) && canvas.fits(lcol, row, &l, true) {
                            canvas.text_at(col, row, &glyph, st, false);
                            canvas.text_at(lcol, row, &l, ls, true);
                        }
                    }
                    None => {
                        canvas.text_at(col, row, &glyph, st, false);
                    }
                }
            }
            Sym::Label(text, st, cands, repeat) => {
                let near = |a: (f64, f64)| placed.get(&text).is_some_and(|ps| ps.iter().any(|b| (a.0 - b.0).hypot(a.1 - b.1) < repeat));
                if cands.first().is_none_or(|p| near(*p)) {
                    continue;
                }
                for p in cands {
                    let col = (p.0 / 2.0) as i64 - text.width() as i64 / 2;
                    if canvas.text_at(col, (p.1 / 4.0) as i64, &text, st, true) {
                        placed.entry(text).or_default().push(p);
                        break;
                    }
                }
            }
        }
    }
    canvas.flush(area, buf);
}

/// The longest start of `s` at most `cols` cells wide.
fn clip(s: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > cols {
            break;
        }
        w += cw;
        out.push(ch);
    }
    out
}

/// A list of [`Overlay`] markers and lines as a custom layer.
#[derive(Default)]
pub struct Markers(pub Vec<Overlay>);

impl CustomLayer for Markers {
    fn name(&self) -> &str {
        "markers"
    }

    fn draw(&self, ctx: &mut DrawContext) {
        for o in &self.0 {
            match o {
                Overlay::Line { pts, color } => ctx.line_lonlat(pts, *color),
                Overlay::Marker { lon, lat, glyph, style, label, priority } => {
                    ctx.marker(*lon, *lat, glyph, *style, *priority, label.clone())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Camera;

    /// Draw with `f` into a 30x5 map at zoom 0 and return the rows as text.
    fn draw(f: impl FnOnce(&mut DrawContext)) -> Vec<String> {
        let area = Rect::new(0, 0, 30, 5);
        let mut buf = Buffer::empty(area);
        render_with(&Camera::new(0.0, 0.0, 0.0), area, &mut buf, f);
        (0..5).map(|y| (0..30).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    }

    #[test]
    fn higher_priority_wins_whatever_its_kind() {
        let st = TStyle::new();
        // A label outranks a marker at the same spot, and the other way round.
        let rows = draw(|c| {
            c.marker_dot((20.0, 8.0), "M", st, 1, None);
            c.label_dot((20.0, 8.0), "LABEL", st, 5);
        });
        assert!(rows[2].contains("LABEL") && !rows[2].contains('M'), "{rows:?}");
        let rows = draw(|c| {
            c.marker_dot((20.0, 8.0), "M", st, 5, None);
            c.label_dot((20.0, 8.0), "LABEL", st, 1);
        });
        assert!(rows[2].contains('M') && !rows[2].contains("LABEL"), "{rows:?}");
    }

    #[test]
    fn a_marker_shows_with_its_label_or_not_at_all() {
        let st = TStyle::new();
        let rows = draw(|c| {
            c.label_dot((30.0, 8.0), "BLOCK", st, 9);
            // Its label would land on BLOCK: neither the glyph nor the label shows.
            c.marker_dot((10.0, 8.0), "◆", st, 1, Some(("name here".into(), st)));
            // Room for both on another row.
            c.marker_dot((10.0, 16.0), "◆", st, 1, Some(("ok".into(), st)));
        });
        assert!(!rows[2].contains('◆') && rows[2].contains("BLOCK"), "{rows:?}");
        assert!(rows[4].contains("◆ ok"), "{rows:?}");
    }

    #[test]
    fn labels_at_the_edge_are_cut_short() {
        let st = TStyle::new();
        let rows = draw(|c| c.marker_dot((48.0, 8.0), "◆", st, 1, Some(("a long name".into(), st))));
        assert!(rows[2].trim_end().ends_with("◆ a l…"), "{rows:?}");
        // No room for even a few characters: the glyph alone.
        let rows = draw(|c| c.marker_dot((56.0, 8.0), "◆", st, 1, Some(("a long name".into(), st))));
        assert!(rows[2].trim_end().ends_with('◆'), "{rows:?}");
    }

    #[test]
    fn same_text_keeps_its_distance() {
        let st = TStyle::new();
        let rows = draw(|c| {
            c.label_dot((10.0, 2.0), "Main", st, 1);
            c.label_dot((14.0, 18.0), "Main", st, 1); // 16 dots away: too close
            c.label_dot((54.0, 2.0), "Main", st, 1); // 44 dots away: far enough
        });
        assert_eq!(rows.join("\n").matches("Main").count(), 2, "{rows:?}");
        let rows = draw(|c| {
            c.label_spaced(vec![(10.0, 2.0)], "Road", st, 1, 60.0);
            c.label_spaced(vec![(54.0, 2.0)], "Road", st, 1, 60.0);
        });
        assert_eq!(rows.join("\n").matches("Road").count(), 1, "a wider repeat distance: {rows:?}");
        // Candidates are tried in order.
        let rows = draw(|c| {
            c.label_dot((20.0, 8.0), "Taken", st, 9);
            c.label_dots(vec![(20.0, 8.0), (20.0, 16.0)], "Moved", st, 1);
        });
        assert!(rows[4].contains("Moved"), "{rows:?}");
    }
}
