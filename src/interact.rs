//! Map interaction following the MapLibre GL / Mapbox GL JS default handlers, adapted to
//! terminal mouse reports (cells, or pixels with SGR-pixels mode):
//!
//! - wheel / two-finger scroll: zoom around the cursor (MapLibre trackpad rate, 1/100 per px)
//! - drag: pan, with inertia on release (160 ms window, linearity 0.3, max 1400 px/s,
//!   deceleration 2500 px/s², easing cubic-bezier(0, 0, 0.3, 1))
//! - double-click: zoom in one level around the point; shift+double-click: zoom out
//! - shift+drag: box zoom
//! - keyboard: arrows pan 100 px, +/- zoom to the next whole level, eased over 300 ms with
//!   the default easing cubic-bezier(0.25, 0.1, 0.25, 1)
//! - a single click is reported to the app (for feature inspection) via [`Controller::take_click`]
//!
//! Terminal extras: hjkl pan, sideways swipes pan, and a row-scroll mode for the smooth-scroll
//! region protocol (see [`crate::smooth`]).

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::render::{Camera, to_lonlat, to_world};

/// Cell size assumed when the terminal doesn't report pixels.
const DEFAULT_CELL_PX: (f64, f64) = (9.0, 20.0);
const PAN_STEP_PX: f64 = 100.0;
const EASE_DURATION: Duration = Duration::from_millis(300);
const FIT_DURATION: Duration = Duration::from_millis(500);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const CLICK_TOLERANCE_PX: f64 = 3.0;
const TRACKPAD_ZOOM_RATE: f64 = 1.0 / 100.0;
const INERTIA_WINDOW: Duration = Duration::from_millis(160);
const INERTIA_LINEARITY: f64 = 0.3;
const INERTIA_MAX_SPEED: f64 = 1400.0;
const INERTIA_DECELERATION: f64 = 2500.0;

/// CSS-style cubic bezier easing (the WebKit UnitBezier solver MapLibre uses).
#[derive(Clone, Copy, Debug)]
pub struct Bezier {
    ax: f64,
    bx: f64,
    cx: f64,
    ay: f64,
    by: f64,
    cy: f64,
}

impl Bezier {
    /// A curve through (0,0), (p1x,p1y), (p2x,p2y), (1,1), as CSS `cubic-bezier()`.
    pub const fn new(p1x: f64, p1y: f64, p2x: f64, p2y: f64) -> Self {
        let cx = 3.0 * p1x;
        let bx = 3.0 * (p2x - p1x) - cx;
        let cy = 3.0 * p1y;
        let by = 3.0 * (p2y - p1y) - cy;
        Self { ax: 1.0 - cx - bx, bx, cx, ay: 1.0 - cy - by, by, cy }
    }

    fn x(&self, t: f64) -> f64 {
        ((self.ax * t + self.bx) * t + self.cx) * t
    }

    fn y(&self, t: f64) -> f64 {
        ((self.ay * t + self.by) * t + self.cy) * t
    }

    fn dx(&self, t: f64) -> f64 {
        (3.0 * self.ax * t + 2.0 * self.bx) * t + self.cx
    }

    /// The eased value at `x` (0..1).
    pub fn ease(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        let mut t = x;
        for _ in 0..8 {
            let err = self.x(t) - x;
            if err.abs() < 1e-6 {
                return self.y(t);
            }
            let d = self.dx(t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= err / d;
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        t = x;
        for _ in 0..32 {
            let v = self.x(t);
            if (v - x).abs() < 1e-6 {
                break;
            }
            if x > v { lo = t } else { hi = t }
            t = (lo + hi) / 2.0;
        }
        self.y(t)
    }
}

/// MapLibre's default easing for camera moves.
pub const DEFAULT_EASING: Bezier = Bezier::new(0.25, 0.1, 0.25, 1.0);
/// MapLibre's easing for drag inertia.
pub const INERTIA_EASING: Bezier = Bezier::new(0.0, 0.0, 0.3, 1.0);

#[derive(Clone, Copy, Debug)]
enum Motion {
    /// Zoom to `to` keeping the absolute cell `at` fixed.
    ZoomAround { to: f64, at: (u16, u16) },
    /// Pan by this many braille dots.
    Pan { dots: (f64, f64) },
    /// Move to a camera (center and zoom).
    To { to: Camera },
    /// Fly to a camera along MapLibre's flyTo curve: zoom out, travel, zoom in.
    Fly { to: Camera, fly: FlyPath },
}

/// The van Wijk and Nuij path MapLibre's flyTo follows (rho 1.42), in braille dots at the
/// starting zoom.
#[derive(Clone, Copy, Debug)]
struct FlyPath {
    r0: f64,
    /// Path length.
    s: f64,
    w0: f64,
    u1: f64,
}

const FLY_RHO: f64 = 1.42;

impl FlyPath {
    /// None when the centers coincide (a plain eased zoom does then).
    fn new(from: &Camera, to: &Camera, area: Rect) -> Option<FlyPath> {
        let scale = 256.0 * 2f64.powf(from.zoom);
        let (fx, fy) = to_world(from.lon, from.lat);
        let (tx, ty) = to_world(to.lon, to.lat);
        let u1 = (tx - fx).hypot(ty - fy) * scale;
        let w0 = (area.width as f64 * 2.0).max(area.height as f64 * 4.0).max(1.0);
        let w1 = w0 / 2f64.powf(to.zoom - from.zoom);
        let rho2 = FLY_RHO * FLY_RHO;
        if u1 < 1.0 {
            return None;
        }
        let r = |last: bool| {
            let (w, sign) = if last { (w1, -1.0) } else { (w0, 1.0) };
            let b = (w1 * w1 - w0 * w0 + sign * rho2 * rho2 * u1 * u1) / (2.0 * w * rho2 * u1);
            ((b * b + 1.0).sqrt() - b).ln()
        };
        let (r0, r1) = (r(false), r(true));
        let s = (r1 - r0) / FLY_RHO;
        (r0.is_finite() && s.is_finite() && s > 0.0).then_some(FlyPath { r0, s, w0, u1 })
    }

    /// Zoom change and fraction of the distance traveled at progress `k` (0..1).
    fn at(&self, k: f64) -> (f64, f64) {
        let s = k * self.s;
        let w = self.r0.cosh() / (self.r0 + FLY_RHO * s).cosh();
        let u = self.w0 * ((self.r0.cosh() * (self.r0 + FLY_RHO * s).tanh() - self.r0.sinh()) / (FLY_RHO * FLY_RHO)) / self.u1;
        ((1.0 / w).log2(), u)
    }

    /// MapLibre's duration: path length over speed 1.2, kept between 0.4 and 3 s here.
    fn duration(&self) -> Duration {
        Duration::from_secs_f64((self.s / 1.2).clamp(0.4, 3.0))
    }
}

#[derive(Clone, Copy, Debug)]
struct Anim {
    from: Camera,
    motion: Motion,
    start: Instant,
    dur: Duration,
    easing: Bezier,
}

#[derive(Debug)]
struct Drag {
    start: (f64, f64),
    last: (f64, f64),
    /// Cumulative pointer offset over time, for inertia.
    samples: VecDeque<(Instant, (f64, f64))>,
    offset: (f64, f64),
    /// Sub-dot remainder when panning in whole dots.
    rem: (f64, f64),
    moved: bool,
    boxing: bool,
}

/// A single click (not a drag or double-click) the app may want to act on, such as picking
/// the feature under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Click {
    /// Longitude of the click.
    pub lon: f64,
    /// Latitude of the click.
    pub lat: f64,
    /// Absolute terminal cell.
    pub col: u16,
    /// Absolute terminal row.
    pub row: u16,
}

/// Mouse and keyboard handling for a map, MapLibre style. Feed it events, call [`Controller::tick`] each frame, and draw with `cam`. See the [module docs](self).
pub struct Controller {
    /// The camera it moves.
    pub cam: Camera,
    /// Lowest zoom it allows.
    pub min_zoom: f64,
    /// Highest zoom it allows.
    pub max_zoom: f64,
    /// Cell size in pixels when mouse reports are in pixels (SGR-pixels mode).
    pub pixel_mouse: Option<(f64, f64)>,
    /// Pan by fractional dots while dragging (the terminal draws the sub-cell rest).
    pub exact_drag: bool,
    /// Smooth-scroll protocol v2: the terminal turns trackpad travel into one wheel event per
    /// row, so the wheel pans a row instead of zooming.
    pub wheel_pans: bool,
    area: Rect,
    drag: Option<Drag>,
    anim: Option<Anim>,
    last_click: Option<(Instant, (f64, f64))>,
    click: Option<Click>,
    /// Where the pointer last was over the map, in braille dots relative to the map area.
    pointer: Option<(f64, f64)>,
}

impl Controller {
    /// A controller for `cam`, limited to `min_zoom..=max_zoom`.
    pub fn new(cam: Camera, min_zoom: f64, max_zoom: f64) -> Self {
        Self {
            cam,
            min_zoom,
            max_zoom,
            pixel_mouse: None,
            exact_drag: false,
            wheel_pans: false,
            area: Rect::default(),
            drag: None,
            anim: None,
            last_click: None,
            click: None,
            pointer: None,
        }
    }

    /// The map's screen area, set every frame before handling input.
    pub fn set_area(&mut self, area: Rect) {
        self.area = area;
    }

    /// Whether a move is in flight (draw again soon, at about 60 fps).
    pub fn animating(&self) -> bool {
        self.anim.is_some()
    }

    /// Stop any running animation (new input interrupts it, as in MapLibre).
    pub fn stop(&mut self) {
        self.anim = None;
    }

    /// Where the pointer is over the map, in braille dots relative to the map area (2 per
    /// column, 4 per row), or `None` when it's elsewhere. For hover effects with
    /// [`crate::events::Events`].
    pub fn pointer(&self) -> Option<(f64, f64)> {
        self.pointer
    }

    /// The last single click, once.
    pub fn take_click(&mut self) -> Option<Click> {
        self.click.take()
    }

    /// The box-zoom rectangle being drawn, in absolute cells.
    pub fn box_rect(&self) -> Option<Rect> {
        let d = self.drag.as_ref().filter(|d| d.boxing && d.moved)?;
        let (a, b) = (self.cell_of(d.start), self.cell_of(d.last));
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        Some(Rect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1).intersection(self.area))
    }

    fn cell_px(&self) -> (f64, f64) {
        self.pixel_mouse.unwrap_or(DEFAULT_CELL_PX)
    }

    /// Pointer position in pixels.
    fn px_of(&self, m: &MouseEvent) -> (f64, f64) {
        match self.pixel_mouse {
            Some(_) => (m.column as f64, m.row as f64),
            None => {
                let (cw, ch) = DEFAULT_CELL_PX;
                ((m.column as f64 + 0.5) * cw, (m.row as f64 + 0.5) * ch)
            }
        }
    }

    fn cell_of(&self, px: (f64, f64)) -> (u16, u16) {
        let (cw, ch) = self.cell_px();
        ((px.0 / cw).max(0.0) as u16, (px.1 / ch).max(0.0) as u16)
    }

    fn px_to_dots(&self, px: (f64, f64)) -> (f64, f64) {
        let (cw, ch) = self.cell_px();
        (px.0 * 2.0 / cw, px.1 * 4.0 / ch)
    }

    /// World (Web Mercator 0..1) coordinates under an absolute pixel position.
    fn world_at(&self, px: (f64, f64)) -> (f64, f64) {
        let (cw, ch) = self.cell_px();
        let scale = 256.0 * 2f64.powf(self.cam.zoom);
        let (cx, cy) = to_world(self.cam.lon, self.cam.lat);
        let dx = (px.0 / cw - self.area.x as f64 - self.area.width as f64 / 2.0) * 2.0;
        let dy = (px.1 / ch - self.area.y as f64 - self.area.height as f64 / 2.0) * 4.0;
        (cx + dx / scale, cy + dy / scale)
    }

    fn start(&mut self, motion: Motion, dur: Duration, easing: Bezier) {
        self.anim = Some(Anim { from: self.cam, motion, start: Instant::now(), dur, easing });
    }

    /// Animated zoom by `dz` around absolute cell `at`, stacking on a zoom already in flight.
    pub fn ease_zoom(&mut self, dz: f64, at: (u16, u16)) {
        let base = match self.anim {
            Some(Anim { motion: Motion::ZoomAround { to, .. }, .. }) => to,
            _ => self.cam.zoom,
        };
        let to = (base + dz).clamp(self.min_zoom, self.max_zoom);
        self.start(Motion::ZoomAround { to, at }, EASE_DURATION, DEFAULT_EASING);
    }

    /// Animated move to a camera.
    pub fn ease_to(&mut self, to: Camera) {
        self.start(Motion::To { to }, FIT_DURATION, DEFAULT_EASING);
    }

    /// Fly to a camera the way MapLibre's `flyTo` does: zoom out, travel, zoom back in, taking
    /// longer for longer trips. Short or zoom-only moves ease directly.
    pub fn fly_to(&mut self, to: Camera) {
        let to = Camera::new(to.lon, to.lat, to.zoom.clamp(self.min_zoom, self.max_zoom));
        match FlyPath::new(&self.cam, &to, self.area) {
            Some(fly) => self.start(Motion::Fly { to, fly }, fly.duration(), DEFAULT_EASING),
            None => self.ease_to(to),
        }
    }

    /// Fly so that lon/lat `bounds` (west, south, east, north) fill the map, zooming in no
    /// further than `max_zoom`.
    pub fn fit_bounds(&mut self, bounds: (f64, f64, f64, f64), max_zoom: f64) {
        let (lon, lat, zoom) = crate::geojson::fit(bounds, self.area.width.max(1), self.area.height.max(1), max_zoom.min(self.max_zoom));
        self.fly_to(Camera::new(lon, lat, zoom));
    }

    fn center_cell(&self) -> (u16, u16) {
        (self.area.x + self.area.width / 2, self.area.y + self.area.height / 2)
    }

    /// Advance animations; true when the camera moved.
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some(a) = self.anim else { return false };
        let t = now.saturating_duration_since(a.start).as_secs_f64() / a.dur.as_secs_f64().max(1e-6);
        let e = a.easing.ease(t);
        let mut cam = a.from;
        match a.motion {
            Motion::ZoomAround { to, at } => cam.zoom_at((to - a.from.zoom) * e, self.max_zoom, self.area, at.0, at.1),
            Motion::Pan { dots } => cam.pan_dots(dots.0 * e, dots.1 * e),
            Motion::Fly { to, fly } => {
                let (dz, u) = if t >= 1.0 { (to.zoom - a.from.zoom, 1.0) } else { fly.at(e) };
                let (fx, fy) = to_world(a.from.lon, a.from.lat);
                let (tx, ty) = to_world(to.lon, to.lat);
                let (lon, lat) = to_lonlat(fx + (tx - fx) * u, fy + (ty - fy) * u);
                cam = Camera::new(lon, lat, (a.from.zoom + dz).clamp(self.min_zoom, self.max_zoom));
            }
            Motion::To { to } => {
                let (fx, fy) = to_world(a.from.lon, a.from.lat);
                let (tx, ty) = to_world(to.lon, to.lat);
                let (lon, lat) = to_lonlat(fx + (tx - fx) * e, fy + (ty - fy) * e);
                cam = Camera::new(lon, lat, a.from.zoom + (to.zoom - a.from.zoom) * e);
            }
        }
        self.cam = cam;
        if t >= 1.0 {
            self.anim = None;
        }
        true
    }

    /// Handle a key; false when it isn't a map key.
    pub fn key(&mut self, k: KeyEvent) -> bool {
        if k.kind == KeyEventKind::Release {
            return false;
        }
        let pan = |c: &mut Self, x: f64, y: f64| {
            c.stop();
            let dots = c.px_to_dots((x, y));
            c.start(Motion::Pan { dots }, EASE_DURATION, DEFAULT_EASING);
        };
        match k.code {
            KeyCode::Left | KeyCode::Char('h') => pan(self, -PAN_STEP_PX, 0.0),
            KeyCode::Right | KeyCode::Char('l') => pan(self, PAN_STEP_PX, 0.0),
            KeyCode::Up | KeyCode::Char('k') => pan(self, 0.0, -PAN_STEP_PX),
            KeyCode::Down | KeyCode::Char('j') => pan(self, 0.0, PAN_STEP_PX),
            KeyCode::Char('+' | '=') => {
                // MapLibre zooms to the next whole level from the rounded zoom.
                let at = self.center_cell();
                let dz = self.cam.zoom.round() + 1.0 - self.cam.zoom;
                self.ease_zoom(dz, at);
            }
            KeyCode::Char('-' | '_') => {
                let at = self.center_cell();
                let dz = self.cam.zoom.round() - 1.0 - self.cam.zoom;
                self.ease_zoom(dz, at);
            }
            _ => return false,
        }
        true
    }

    /// Handle a mouse event; true when the view or the box changed.
    pub fn mouse(&mut self, m: MouseEvent) -> bool {
        let px = self.px_of(&m);
        let cell = self.cell_of(px);
        let inside = self.area.contains(Position::new(cell.0, cell.1));
        // Remember the pointer for hover (exact with pixel reports, else the cell's middle).
        self.pointer = inside.then(|| {
            let d = self.px_to_dots(px);
            (d.0 - self.area.x as f64 * 2.0, d.1 - self.area.y as f64 * 4.0)
        });
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        let now = Instant::now();
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside => {
                self.stop();
                let up = m.kind == MouseEventKind::ScrollUp;
                if self.wheel_pans {
                    self.cam.pan_rows(if up { -1 } else { 1 });
                } else {
                    // MapLibre: scale = 2 / (1 + e^(-|delta| * rate)); one event is a row of travel.
                    let delta = self.cell_px().1;
                    let dz = (2.0 / (1.0 + (-delta * TRACKPAD_ZOOM_RATE).exp())).log2();
                    let dz = if up { dz } else { -dz };
                    let to = (self.cam.zoom + dz).clamp(self.min_zoom, self.max_zoom);
                    self.cam.zoom_at(to - self.cam.zoom, self.max_zoom, self.area, cell.0, cell.1);
                }
                true
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight if inside => {
                self.stop();
                let w = self.cell_px().0 * 2.0;
                let dots = self.px_to_dots((if m.kind == MouseEventKind::ScrollLeft { -w } else { w }, 0.0));
                self.cam.pan_dots(dots.0, dots.1);
                true
            }
            MouseEventKind::Down(MouseButton::Left) if inside => {
                self.stop();
                let double = self.last_click.is_some_and(|(t, p)| {
                    now.duration_since(t) < DOUBLE_CLICK && (p.0 - px.0).hypot(p.1 - px.1) < self.cell_px().0 * 1.5
                });
                if double {
                    self.last_click = None;
                    self.click = None;
                    self.ease_zoom(if shift { -1.0 } else { 1.0 }, cell);
                    return true;
                }
                self.drag = Some(Drag {
                    start: px,
                    last: px,
                    samples: VecDeque::from([(now, (0.0, 0.0))]),
                    offset: (0.0, 0.0),
                    rem: (0.0, 0.0),
                    moved: false,
                    boxing: shift,
                });
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let exact = self.exact_drag;
                let to_dots = |c: &Self, p: (f64, f64)| c.px_to_dots(p);
                let Some(mut d) = self.drag.take() else { return false };
                let delta = (px.0 - d.last.0, px.1 - d.last.1);
                d.last = px;
                if (px.0 - d.start.0).hypot(px.1 - d.start.1) > CLICK_TOLERANCE_PX {
                    d.moved = true;
                }
                if !d.boxing {
                    // The map follows the pointer: the camera moves the other way.
                    let dots = to_dots(self, (-delta.0, -delta.1));
                    if exact {
                        self.cam.pan_dots(dots.0, dots.1);
                    } else {
                        // Whole dots, so every feature shifts by the same amount.
                        d.rem = (d.rem.0 + dots.0, d.rem.1 + dots.1);
                        let whole = (d.rem.0.trunc(), d.rem.1.trunc());
                        d.rem = (d.rem.0 - whole.0, d.rem.1 - whole.1);
                        self.cam.pan_dots(whole.0, whole.1);
                    }
                    d.offset = (d.offset.0 + delta.0, d.offset.1 + delta.1);
                    d.samples.push_back((now, d.offset));
                    while d.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > INERTIA_WINDOW) {
                        d.samples.pop_front();
                    }
                }
                self.drag = Some(d);
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(mut d) = self.drag.take() else { return false };
                if d.boxing && d.moved {
                    self.fit_box(d.start, d.last);
                    return true;
                }
                if !d.moved {
                    // A click: remember it for double-click, and report it.
                    self.last_click = Some((now, px));
                    if !d.boxing {
                        let (wx, wy) = self.world_at(px);
                        let (lon, lat) = to_lonlat(wx, wy);
                        self.click = Some(Click { lon, lat, col: cell.0, row: cell.1 });
                    }
                    return self.click.is_some();
                }
                while d.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > INERTIA_WINDOW) {
                    d.samples.pop_front();
                }
                self.inertia(&d);
                true
            }
            _ => false,
        }
    }

    fn inertia(&mut self, d: &Drag) {
        let (Some(first), Some(last)) = (d.samples.front(), d.samples.back()) else { return };
        let secs = last.0.duration_since(first.0).as_secs_f64();
        if secs <= 0.0 {
            return;
        }
        let mut v = ((last.1.0 - first.1.0) * INERTIA_LINEARITY / secs, (last.1.1 - first.1.1) * INERTIA_LINEARITY / secs);
        let mut speed = v.0.hypot(v.1);
        if speed > INERTIA_MAX_SPEED {
            v = (v.0 / speed * INERTIA_MAX_SPEED, v.1 / speed * INERTIA_MAX_SPEED);
            speed = INERTIA_MAX_SPEED;
        }
        let dur = speed / (INERTIA_DECELERATION * INERTIA_LINEARITY);
        if dur < 0.02 {
            return;
        }
        // The map keeps moving with the fling; the camera goes the other way.
        let dots = self.px_to_dots((-v.0 * dur / 2.0, -v.1 * dur / 2.0));
        self.start(Motion::Pan { dots }, Duration::from_secs_f64(dur), INERTIA_EASING);
    }

    fn fit_box(&mut self, a: (f64, f64), b: (f64, f64)) {
        let (wa, wb) = (self.world_at(a), self.world_at(b));
        let (bw, bh) = ((wa.0 - wb.0).abs(), (wa.1 - wb.1).abs());
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let (w_dots, h_dots) = (self.area.width as f64 * 2.0, self.area.height as f64 * 4.0);
        let scale = (w_dots / bw).min(h_dots / bh);
        let zoom = (scale / 256.0).log2().clamp(self.min_zoom, self.max_zoom);
        let (lon, lat) = to_lonlat((wa.0 + wb.0) / 2.0, (wa.1 + wb.1) / 2.0);
        self.ease_to(Camera::new(lon, lat, zoom));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: MouseEventKind, col: u16, row: u16, mods: KeyModifiers) -> MouseEvent {
        MouseEvent { kind, column: col, row, modifiers: mods }
    }

    fn ctl() -> Controller {
        let mut c = Controller::new(Camera::new(-77.0353, 38.8895, 12.0), 0.0, 18.0);
        c.set_area(Rect::new(0, 0, 100, 40));
        c
    }

    fn finish(c: &mut Controller) {
        c.tick(Instant::now() + Duration::from_secs(10));
        assert!(!c.animating());
    }

    #[test]
    fn easing_curves() {
        for b in [DEFAULT_EASING, INERTIA_EASING] {
            assert!(b.ease(0.0).abs() < 1e-6);
            assert!((b.ease(1.0) - 1.0).abs() < 1e-6);
            assert!(b.ease(0.5) > 0.5, "ease-out curves are past halfway at t=0.5");
        }
    }

    #[test]
    fn keys_ease_to_whole_zoom_levels() {
        let mut c = ctl();
        c.cam.zoom = 12.3;
        assert!(c.key(KeyEvent::from(KeyCode::Char('+'))));
        assert!(c.animating());
        finish(&mut c);
        assert!((c.cam.zoom - 13.0).abs() < 1e-9);
        c.key(KeyEvent::from(KeyCode::Char('-')));
        c.key(KeyEvent::from(KeyCode::Char('-')));
        finish(&mut c);
        assert!((c.cam.zoom - 11.0).abs() < 1e-9, "presses stack: {}", c.cam.zoom);
    }

    #[test]
    fn double_click_zooms_and_shift_double_click_zooms_out() {
        let mut c = ctl();
        let none = KeyModifiers::NONE;
        for _ in 0..2 {
            c.mouse(ev(MouseEventKind::Down(MouseButton::Left), 30, 10, none));
            c.mouse(ev(MouseEventKind::Up(MouseButton::Left), 30, 10, none));
        }
        finish(&mut c);
        assert!((c.cam.zoom - 13.0).abs() < 1e-9);
        let sh = KeyModifiers::SHIFT;
        for _ in 0..2 {
            c.mouse(ev(MouseEventKind::Down(MouseButton::Left), 30, 10, sh));
            c.mouse(ev(MouseEventKind::Up(MouseButton::Left), 30, 10, sh));
        }
        finish(&mut c);
        assert!((c.cam.zoom - 12.0).abs() < 1e-9);
    }

    #[test]
    fn single_click_is_reported_with_its_location() {
        let mut c = ctl();
        c.mouse(ev(MouseEventKind::Down(MouseButton::Left), 50, 20, KeyModifiers::NONE));
        c.mouse(ev(MouseEventKind::Up(MouseButton::Left), 50, 20, KeyModifiers::NONE));
        let k = c.take_click().expect("click");
        assert_eq!((k.col, k.row), (50, 20));
        assert!((k.lon - -77.0353).abs() < 0.01 && (k.lat - 38.8895).abs() < 0.01);
        assert!(c.take_click().is_none());
    }

    #[test]
    fn fling_keeps_moving_but_a_held_stop_does_not() {
        let mut c = ctl();
        let none = KeyModifiers::NONE;
        c.mouse(ev(MouseEventKind::Down(MouseButton::Left), 50, 20, none));
        for i in 1..=5 {
            std::thread::sleep(Duration::from_millis(10));
            c.mouse(ev(MouseEventKind::Drag(MouseButton::Left), 50 - i * 2, 20, none));
        }
        c.mouse(ev(MouseEventKind::Up(MouseButton::Left), 40, 20, none));
        assert!(c.animating(), "a fast drag flings");
        let before = c.cam.lon;
        finish(&mut c);
        assert!(c.cam.lon > before, "dragging left moves the camera east and keeps going");

        let mut c = ctl();
        c.mouse(ev(MouseEventKind::Down(MouseButton::Left), 50, 20, none));
        c.mouse(ev(MouseEventKind::Drag(MouseButton::Left), 40, 20, none));
        std::thread::sleep(Duration::from_millis(200));
        c.mouse(ev(MouseEventKind::Up(MouseButton::Left), 40, 20, none));
        assert!(!c.animating(), "pausing before release cancels the fling");
    }

    #[test]
    fn box_zoom_fits_the_rectangle() {
        let mut c = ctl();
        let sh = KeyModifiers::SHIFT;
        c.mouse(ev(MouseEventKind::Down(MouseButton::Left), 25, 10, sh));
        c.mouse(ev(MouseEventKind::Drag(MouseButton::Left), 75, 30, sh));
        assert_eq!(c.box_rect(), Some(Rect::new(25, 10, 51, 21)));
        c.mouse(ev(MouseEventKind::Up(MouseButton::Left), 75, 30, sh));
        finish(&mut c);
        // Half the width and half the height of the view: one level in.
        assert!((c.cam.zoom - 13.0).abs() < 0.05, "{}", c.cam.zoom);
    }

    #[test]
    fn wheel_zooms_around_the_cursor() {
        let mut c = ctl();
        c.mouse(ev(MouseEventKind::ScrollUp, 80, 10, KeyModifiers::NONE));
        assert!(c.cam.zoom > 12.0 && c.cam.zoom < 12.5);
    }

    #[test]
    fn fly_to_zooms_out_on_the_way_and_lands_exactly() {
        let mut c = Controller::new(Camera::new(-77.0353, 38.8895, 15.0), 0.0, 18.0);
        c.set_area(Rect::new(0, 0, 120, 40));
        let to = Camera::new(-122.4194, 37.7749, 14.0);
        c.fly_to(to);
        assert!(c.animating());
        let Some(Anim { start, dur, .. }) = c.anim else { panic!() };
        assert!(dur > Duration::from_millis(1500), "a cross-country flight takes a while: {dur:?}");
        c.tick(start + dur / 2);
        assert!(c.cam.zoom < 11.0, "zoomed out mid-flight: {}", c.cam.zoom);
        c.tick(start + dur);
        assert!(!c.animating());
        assert!((c.cam.lon - to.lon).abs() < 1e-9 && (c.cam.lat - to.lat).abs() < 1e-9 && (c.cam.zoom - 14.0).abs() < 1e-9);
        // In place: just eases the zoom.
        c.fly_to(Camera::new(to.lon, to.lat, 16.0));
        assert!(matches!(c.anim, Some(Anim { motion: Motion::To { .. }, .. })));
    }

    #[test]
    fn fit_bounds_flies_to_fit() {
        let mut c = Controller::new(Camera::new(0.0, 0.0, 2.0), 0.0, 18.0);
        c.set_area(Rect::new(0, 0, 100, 40));
        c.fit_bounds((-77.05, 38.88, -77.0, 38.9), 20.0);
        let Some(Anim { start, dur, .. }) = c.anim else { panic!() };
        c.tick(start + dur);
        assert!((c.cam.lon - -77.025).abs() < 1e-6 && c.cam.zoom > 12.0 && c.cam.zoom < 16.0, "{:?}", c.cam);
    }
}
