//! Camera, projection and the ratatui map widget. Lines are drawn with braille dots (2x4 per
//! cell); filled areas color the cell background; labels and markers are text on top.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style as TStyle},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    store::TileId,
};

/// Dots per tile edge at integer zoom.
pub(crate) const TILE_DOTS: f64 = 256.0;

/// lon/lat -> Web Mercator world coordinates in 0..1 (y grows south).
pub fn to_world(lon: f64, lat: f64) -> (f64, f64) {
    let lat = lat.clamp(-85.0511, 85.0511).to_radians();
    ((lon + 180.0) / 360.0, (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0)
}

/// Web Mercator world coordinates (0..1) to lon/lat.
pub fn to_lonlat(x: f64, y: f64) -> (f64, f64) {
    let n = std::f64::consts::PI * (1.0 - 2.0 * y);
    (x * 360.0 - 180.0, n.sinh().atan().to_degrees())
}

#[derive(Clone, Copy, Debug)]
/// Where the map looks: a center and a zoom (MapLibre's zoom levels).
pub struct Camera {
    /// Longitude of the center.
    pub lon: f64,
    /// Latitude of the center.
    pub lat: f64,
    /// Zoom level (0 shows the world).
    pub zoom: f64,
}

impl Camera {
    /// A camera at (lon, lat), zoom `zoom`.
    pub fn new(lon: f64, lat: f64, zoom: f64) -> Self {
        Self { lon, lat, zoom }
    }

    /// Move by a number of cells (positive = east / south).
    pub fn pan_cells(&mut self, cols: f64, rows: f64) {
        let scale = TILE_DOTS * 2f64.powf(self.zoom);
        let (x, y) = to_world(self.lon, self.lat);
        let (lon, lat) = to_lonlat(x + cols * 2.0 / scale, (y + rows * 4.0 / scale).clamp(0.0, 1.0));
        self.lon = ((lon + 540.0) % 360.0) - 180.0;
        self.lat = lat;
    }

    /// Move by braille dots (2 per column, 4 per row), the map's finest drawing step.
    pub fn pan_dots(&mut self, dx: f64, dy: f64) {
        self.pan_cells(dx / 2.0, dy / 4.0);
    }

    /// Zoom by `dz` levels, at most to `max`.
    pub fn zoom_by(&mut self, dz: f64, max: f64) {
        self.zoom = (self.zoom + dz).clamp(0.0, max);
    }

    /// Pan by whole rows, staying on the row grid (used for smooth scrolling, where the
    /// terminal counts on each wheel event moving the content exactly one row).
    pub fn pan_rows(&mut self, rows: i64) {
        let world = TILE_DOTS * 2f64.powf(self.zoom) / 4.0;
        let (_, y) = to_world(self.lon, self.lat);
        let r = (y * world + rows as f64).clamp(0.0, world);
        self.lat = to_lonlat(0.0, r / world).1;
    }

    /// Align the vertical position to the row grid.
    pub fn snap_rows(&mut self) {
        let world = TILE_DOTS * 2f64.powf(self.zoom) / 4.0;
        let (_, y) = to_world(self.lon, self.lat);
        self.lat = to_lonlat(0.0, ((y * world).round() / world).clamp(0.0, 1.0)).1;
    }

    /// Zoom by `dz` keeping the point under cell (col, row) of `area` fixed on screen, the way
    /// web maps zoom toward the cursor.
    pub fn zoom_at(&mut self, dz: f64, max: f64, area: Rect, col: u16, row: u16) {
        let new_zoom = (self.zoom + dz).clamp(0.0, max);
        if new_zoom == self.zoom {
            return;
        }
        // Dot offset of the cursor from the view center, which stays constant across the zoom.
        let ox = (col.saturating_sub(area.x) as f64 * 2.0 + 1.0) - area.width as f64;
        let oy = (row.saturating_sub(area.y) as f64 * 4.0 + 2.0) - area.height as f64 * 2.0;
        let (cx, cy) = to_world(self.lon, self.lat);
        let old = TILE_DOTS * 2f64.powf(self.zoom);
        let new = TILE_DOTS * 2f64.powf(new_zoom);
        let (px, py) = (cx + ox / old, cy + oy / old);
        let (lon, lat) = to_lonlat(px - ox / new, (py - oy / new).clamp(0.0, 1.0));
        self.lon = ((lon + 540.0) % 360.0) - 180.0;
        self.lat = lat;
        self.zoom = new_zoom;
    }

    /// Integer tile zoom to fetch for this camera.
    pub fn tile_zoom(&self, min: u8, max: u8) -> u8 {
        ((self.zoom + 0.2).floor() as i64).clamp(min as i64, max as i64) as u8
    }
}

/// Camera + screen area -> dot transform.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    /// The map's area on screen.
    pub area: Rect,
    cx: f64,
    cy: f64,
    scale: f64,
    w: f64,
    h: f64,
}

impl Viewport {
    /// The viewport of `cam` drawn in `area`.
    pub fn new(cam: &Camera, area: Rect) -> Self {
        let (cx, cy) = to_world(cam.lon, cam.lat);
        Self { area, cx, cy, scale: TILE_DOTS * 2f64.powf(cam.zoom), w: area.width as f64 * 2.0, h: area.height as f64 * 4.0 }
    }

    /// World coordinates to braille dots relative to the area.
    pub fn dot(&self, wx: f64, wy: f64) -> (f64, f64) {
        ((wx - self.cx) * self.scale + self.w / 2.0, (wy - self.cy) * self.scale + self.h / 2.0)
    }

    /// Lon/lat to braille dots relative to the area.
    pub fn dot_ll(&self, lon: f64, lat: f64) -> (f64, f64) {
        let (x, y) = to_world(lon, lat);
        self.dot(x, y)
    }

    /// Braille dot position (relative to the area; 2 dots per column, 4 per row) -> lon/lat.
    pub fn dot_to_lonlat(&self, x: f64, y: f64) -> (f64, f64) {
        to_lonlat(self.cx + (x - self.w / 2.0) / self.scale, self.cy + (y - self.h / 2.0) / self.scale)
    }

    /// Screen cell (relative to the area) -> lon/lat.
    pub fn cell_to_lonlat(&self, col: u16, row: u16) -> (f64, f64) {
        let dx = (col as f64 * 2.0 + 1.0 - self.w / 2.0) / self.scale;
        let dy = (row as f64 * 4.0 + 2.0 - self.h / 2.0) / self.scale;
        to_lonlat(self.cx + dx, self.cy + dy)
    }

    /// Tiles at zoom `z` covering the view, nearest the center first.
    pub fn tiles(&self, z: u8) -> Vec<TileId> {
        let n = 1u32 << z;
        let nf = n as f64;
        let (hw, hh) = (self.w / 2.0 / self.scale, self.h / 2.0 / self.scale);
        let clamp = |v: f64| (v.floor() as i64).clamp(0, n as i64 - 1) as u32;
        let (x0, x1) = (clamp((self.cx - hw) * nf), clamp((self.cx + hw) * nf));
        let (y0, y1) = (clamp((self.cy - hh) * nf), clamp((self.cy + hh) * nf));
        let mut out: Vec<TileId> = (x0..=x1).flat_map(|x| (y0..=y1).map(move |y| (z, x, y))).collect();
        let (ccx, ccy) = (self.cx * nf - 0.5, self.cy * nf - 0.5);
        out.sort_by(|a, b| {
            let d = |t: &TileId| (t.1 as f64 - ccx).powi(2) + (t.2 as f64 - ccy).powi(2);
            d(a).total_cmp(&d(b))
        });
        out
    }

    /// km per screen column at the center, for a scale bar.
    pub fn km_per_col(&self, lat: f64) -> f64 {
        40075.0 * lat.to_radians().cos() * 2.0 / self.scale
    }
}

/// Things the host app draws on top of the map.
#[derive(Clone, Debug)]
pub enum Overlay {
    /// A polyline in (lon, lat).
    Line {
        /// The points, (lon, lat).
        pts: Vec<(f64, f64)>,
        /// Its color.
        color: Color,
    },
    /// A glyph at (lon, lat) with an optional label to its right.
    Marker {
        /// Longitude.
        lon: f64,
        /// Latitude.
        lat: f64,
        /// The character(s) drawn.
        glyph: String,
        /// The glyph's style.
        style: TStyle,
        /// Text to its right, and its style.
        label: Option<(String, TStyle)>,
        /// Higher wins when markers and labels collide.
        priority: i32,
    },
}

pub(crate) struct Canvas {
    w: usize,
    h: usize,
    dots: Vec<u8>,
    fg: Vec<Option<Color>>,
    bg: Vec<Option<Color>>,
    text: Vec<Option<(String, TStyle)>>,
    used: Vec<bool>,
    cov: Vec<u8>,
    touched: Vec<usize>,
    rows: Vec<Vec<f32>>,
}

const BRAILLE: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

impl Canvas {
    pub(crate) fn new(w: usize, h: usize) -> Self {
        let n = w * h;
        Self {
            w,
            h,
            dots: vec![0; n],
            fg: vec![None; n],
            bg: vec![None; n],
            text: vec![None; n],
            used: vec![false; n],
            cov: vec![0; n],
            touched: Vec::new(),
            rows: vec![Vec::new(); h * 4],
        }
    }

    fn dot(&mut self, x: i64, y: i64, c: Color) {
        if x < 0 || y < 0 || x >= self.w as i64 * 2 || y >= self.h as i64 * 4 {
            return;
        }
        let (cx, cy) = (x as usize / 2, y as usize / 4);
        let i = cy * self.w + cx;
        self.dots[i] |= BRAILLE[y as usize % 4][x as usize % 2];
        self.fg[i] = Some(c);
    }

    pub(crate) fn line(&mut self, a: (f64, f64), b: (f64, f64), c: Color) {
        self.line_where(a, b, c, |_| true);
    }

    /// A dashed segment: `dash` alternates on and off lengths in dots, and `phase` is how far
    /// into the pattern `a` is, so a dashed polyline continues its pattern across segments.
    pub(crate) fn line_dashed(&mut self, a: (f64, f64), b: (f64, f64), c: Color, dash: &[f64], phase: f64) {
        let period: f64 = dash.iter().sum();
        if period <= 0.0 {
            return self.line(a, b, c);
        }
        self.line_where(a, b, c, |d| {
            let mut t = (phase + d).rem_euclid(period);
            for (i, len) in dash.iter().enumerate() {
                if t < *len {
                    return i % 2 == 0;
                }
                t -= len;
            }
            false
        });
    }

    /// Draw the dots of a segment for which `on(distance from a)` holds.
    fn line_where(&mut self, a: (f64, f64), b: (f64, f64), c: Color, on: impl Fn(f64) -> bool) {
        let v = [0.0, 0.0, self.w as f64 * 2.0 - 0.001, self.h as f64 * 4.0 - 0.001];
        let Some((x0, y0, x1, y1)) = clip(a, b, v) else { return };
        let (mut x, mut y) = (x0.floor() as i64, y0.floor() as i64);
        let (xe, ye) = (x1.floor() as i64, y1.floor() as i64);
        let (dx, dy) = ((xe - x).abs(), -(ye - y).abs());
        let (sx, sy) = (if x < xe { 1 } else { -1 }, if y < ye { 1 } else { -1 });
        let mut err = dx + dy;
        for _ in 0..(dx - dy + 2) {
            if on((x as f64 + 0.5 - a.0).hypot(y as f64 + 0.5 - a.1)) {
                self.dot(x, y, c);
            }
            if x == xe && y == ye {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Even-odd scanline fill at dot resolution; a cell takes the color when half its dots are in.
    pub(crate) fn fill(&mut self, rings: &[Vec<(f64, f64)>], c: Color) {
        let hd = self.h * 4;
        let wd = (self.w * 2) as f32;
        for r in self.rows.iter_mut() {
            r.clear();
        }
        let mut any = false;
        for ring in rings {
            for s in ring.windows(2) {
                let ((x0, y0), (x1, y1)) = (s[0], s[1]);
                if y0 == y1 {
                    continue;
                }
                let (ya, yb) = (y0.min(y1), y0.max(y1));
                let r0 = (ya - 0.5).ceil().max(0.0) as usize;
                let r1 = ((yb - 0.5).ceil().min(hd as f64)) as usize;
                for row in r0..r1.min(hd) {
                    let yc = row as f64 + 0.5;
                    let x = x0 + (yc - y0) * (x1 - x0) / (y1 - y0);
                    self.rows[row].push(x as f32);
                    any = true;
                }
            }
        }
        if !any {
            return;
        }
        for row in 0..hd {
            let xs = &mut self.rows[row];
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.total_cmp(b));
            let cy = row / 4;
            for pair in xs.as_chunks::<2>().0 {
                let a = (pair[0] - 0.5).ceil().max(0.0);
                let b = (pair[1] - 0.5).ceil().min(wd);
                let (mut x, b) = (a as usize, b.max(0.0) as usize);
                while x < b {
                    let i = cy * self.w + x / 2;
                    if self.cov[i] == 0 {
                        self.touched.push(i);
                    }
                    self.cov[i] = self.cov[i].saturating_add(1);
                    x += 1;
                }
            }
        }
        for i in self.touched.drain(..) {
            if self.cov[i] >= 4 {
                self.bg[i] = Some(c);
            }
            self.cov[i] = 0;
        }
    }

    /// Set one cell's background.
    pub(crate) fn set_bg(&mut self, col: usize, row: usize, c: Color) {
        if col < self.w && row < self.h {
            self.bg[row * self.w + col] = Some(c);
        }
    }

    /// Place text starting at cell (col,row) if it fits and the cells are free (with 1-cell padding).
    /// Whether `s` would fit at cell (col,row) without overlapping placed text.
    pub(crate) fn fits(&self, col: i64, row: i64, s: &str, pad: bool) -> bool {
        let wdt = s.width() as i64;
        if row < 0 || row >= self.h as i64 || col < 0 || col + wdt > self.w as i64 || wdt == 0 {
            return false;
        }
        let (lo, hi) = if pad { ((col - 1).max(0), (col + wdt + 1).min(self.w as i64)) } else { (col, col + wdt) };
        let base = row as usize * self.w;
        !(lo..hi).any(|c| self.used[base + c as usize])
    }

    pub(crate) fn width(&self) -> usize {
        self.w
    }

    pub(crate) fn text_at(&mut self, col: i64, row: i64, s: &str, st: TStyle, pad: bool) -> bool {
        let wdt = s.width() as i64;
        if row < 0 || row >= self.h as i64 || col < 0 || col + wdt > self.w as i64 || wdt == 0 {
            return false;
        }
        let (lo, hi) = if pad { ((col - 1).max(0), (col + wdt + 1).min(self.w as i64)) } else { (col, col + wdt) };
        let base = row as usize * self.w;
        if (lo..hi).any(|c| self.used[base + c as usize]) {
            return false;
        }
        for c in lo..hi {
            self.used[base + c as usize] = true;
        }
        let mut c = col as usize;
        for ch in s.chars() {
            let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]) as &str).max(1);
            self.text[base + c] = Some((ch.to_string(), st));
            for k in 1..cw {
                self.text[base + c + k] = Some((String::new(), st));
            }
            c += cw;
        }
        true
    }

    pub(crate) fn flush(&self, area: Rect, buf: &mut Buffer) {
        for row in 0..self.h {
            for col in 0..self.w {
                let i = row * self.w + col;
                let Some(cell) = buf.cell_mut((area.x + col as u16, area.y + row as u16)) else { continue };
                if let Some(bg) = self.bg[i] {
                    cell.set_bg(bg);
                }
                if let Some((s, st)) = &self.text[i] {
                    cell.set_symbol(if s.is_empty() { " " } else { s });
                    if let Some(f) = st.fg {
                        cell.set_fg(f);
                    }
                    if let Some(b) = st.bg {
                        cell.set_bg(b);
                    }
                    cell.modifier = st.add_modifier;
                } else if self.dots[i] != 0 {
                    cell.set_char(char::from_u32(0x2800 + self.dots[i] as u32).unwrap_or(' '));
                    if let Some(f) = self.fg[i] {
                        cell.set_fg(f);
                    }
                }
            }
        }
    }
}

/// Clip a segment to [x0, y0, x1, y1] (Liang-Barsky).
pub fn clip(a: (f64, f64), b: (f64, f64), v: [f64; 4]) -> Option<(f64, f64, f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, a.0 - v[0]), (dx, v[2] - a.0), (-dy, a.1 - v[1]), (dy, v[3] - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return None;
                }
                t0 = t0.max(r);
            } else {
                if r < t0 {
                    return None;
                }
                t1 = t1.min(r);
            }
        }
    }
    Some((a.0 + t0 * dx, a.1 + t0 * dy, a.0 + t1 * dx, a.1 + t1 * dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_roundtrip() {
        for (lon, lat) in [(-77.0353, 38.8895), (0.0, 0.0), (151.2, -33.9)] {
            let (x, y) = to_world(lon, lat);
            let (lo, la) = to_lonlat(x, y);
            assert!((lo - lon).abs() < 1e-9 && (la - lat).abs() < 1e-9);
        }
    }

    #[test]
    fn viewport_tiles_center_first() {
        let cam = Camera::new(-77.0353, 38.8895, 12.0);
        let vp = Viewport::new(&cam, Rect::new(0, 0, 100, 40));
        let t = vp.tiles(12);
        let (x, y) = to_world(-77.0353, 38.8895);
        assert_eq!(t[0], (12, (x * 4096.0) as u32, (y * 4096.0) as u32));
    }

    #[test]
    fn zoom_at_keeps_cursor_point() {
        let area = Rect::new(0, 0, 100, 40);
        let mut cam = Camera::new(-77.0353, 38.8895, 12.0);
        let before = Viewport::new(&cam, area).cell_to_lonlat(80, 10);
        cam.zoom_at(1.0, 18.0, area, 80, 10);
        let after = Viewport::new(&cam, area).cell_to_lonlat(80, 10);
        assert!((before.0 - after.0).abs() < 1e-6 && (before.1 - after.1).abs() < 1e-6, "{before:?} {after:?}");
        assert_eq!(cam.zoom, 13.0);
    }

    #[test]
    fn fill_covers_square() {
        let mut cv = Canvas::new(10, 5);
        let sq = vec![vec![(0.0, 0.0), (10.0, 0.0), (10.0, 8.0), (0.0, 8.0), (0.0, 0.0)]];
        cv.fill(&sq, Color::Blue);
        assert_eq!(cv.bg[0], Some(Color::Blue));
        assert_eq!(cv.bg[4], Some(Color::Blue));
        assert_eq!(cv.bg[5], None);
        assert_eq!(cv.bg[2 * 10], None);
    }
}
