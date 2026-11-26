//! A timeline for the map's [clock](crate::clock), after Cesium's Timeline widget: a bar
//! across the bottom with time ticks that adapt to the span shown, the clock's time as a
//! needle, and highlighted stretches where there's data (radar scans, a forecast, an alert's
//! period). Clicking or dragging on it gives a time to move the clock to
//! ([`Timeline::time_at`]); the wheel zooms the span about the pointer ([`Timeline::zoom`])
//! and shift+wheel pans it ([`Timeline::pan`]). It keeps no time of its own: the clock does.
//!
//! Two rows: the tick labels, then the bar. Times are UTC.
//!
//! ```no_run
//! # use mapterm::{clock::Clock, timeline::Timeline};
//! # use ratatui::style::Color;
//! # fn demo(buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect, clock: &Clock, now: f64) {
//! let mut tl = Timeline::new(now - 7200.0, now);
//! tl.highlight(now - 7200.0, now, Color::Rgb(0x26, 0x3a, 0x55));
//! tl.render(buf, area, clock, now);
//! # }
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// A stretch of time drawn in a color on the bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    /// Where it starts.
    pub start: f64,
    /// Where it ends.
    pub stop: f64,
    /// Its color (the bar's background there).
    pub color: Color,
}

/// The timeline: the span it shows and what's highlighted.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    /// The earliest time shown, Unix seconds.
    pub start: f64,
    /// The latest.
    pub stop: f64,
    highlights: Vec<Highlight>,
}

/// Tick steps, seconds: the first that leaves room for its labels is used.
const STEPS: [f64; 15] = [60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 10800.0, 21600.0, 43200.0, 86400.0, 172_800.0, 604_800.0, 2_592_000.0];
/// Columns a tick label and the gap after it need.
const LABEL_COLS: f64 = 9.0;
/// The shortest span it shows (zooming in stops here).
const MIN_SPAN: f64 = 300.0;

impl Timeline {
    /// A timeline showing `start..stop`.
    pub fn new(start: f64, stop: f64) -> Timeline {
        let mut t = Timeline { start, stop, highlights: Vec::new() };
        t.set_span(start, stop);
        t
    }

    /// Show `start..stop` (at least five minutes).
    pub fn set_span(&mut self, start: f64, stop: f64) {
        let mid = (start + stop) / 2.0;
        let half = ((stop - start).abs() / 2.0).max(MIN_SPAN / 2.0);
        (self.start, self.stop) = (mid - half, mid + half);
    }

    /// Mark `start..stop` in `color` (drawn in the order added).
    pub fn highlight(&mut self, start: f64, stop: f64, color: Color) {
        self.highlights.push(Highlight { start, stop, color });
    }

    /// Remove the highlights.
    pub fn clear_highlights(&mut self) {
        self.highlights.clear();
    }

    /// Zoom by `factor` (under 1 shows less time) keeping time `about` where it is.
    pub fn zoom(&mut self, factor: f64, about: f64) {
        let f = factor.clamp(0.1, 10.0);
        let (a, b) = (about + (self.start - about) * f, about + (self.stop - about) * f);
        if b - a >= MIN_SPAN {
            (self.start, self.stop) = (a, b);
        }
    }

    /// Move the span by `seconds`.
    pub fn pan(&mut self, seconds: f64) {
        self.start += seconds;
        self.stop += seconds;
    }

    /// The time under column `col` of the bar drawn in `area`, if it's on the bar.
    pub fn time_at(&self, area: Rect, col: u16) -> Option<f64> {
        if area.width == 0 || col < area.x || col >= area.right() {
            return None;
        }
        Some(self.start + (col - area.x) as f64 / (area.width.max(2) - 1) as f64 * (self.stop - self.start))
    }

    /// The column of time `t` on the bar in `area`, if it's in the span.
    fn col_of(&self, area: Rect, t: f64) -> Option<u16> {
        if !(self.start..=self.stop).contains(&t) || area.width == 0 {
            return None;
        }
        Some(area.x + ((t - self.start) / (self.stop - self.start) * (area.width.max(2) - 1) as f64).round() as u16)
    }

    /// The tick step for a bar `width` columns wide: the finest whose labels fit.
    pub fn step(&self, width: u16) -> f64 {
        let per_col = (self.stop - self.start) / width.max(1) as f64;
        STEPS.into_iter().find(|s| s / per_col >= LABEL_COLS).unwrap_or(STEPS[STEPS.len() - 1])
    }

    /// Draw it in `area` (two rows: labels, then the bar) with `clock`'s time as the needle,
    /// and "now" (real time) marked.
    pub fn render(&self, buf: &mut Buffer, area: Rect, clock: &crate::clock::Clock, now: f64) {
        if area.height < 2 || area.width < 10 {
            return;
        }
        let labels = Rect { height: 1, ..area };
        let bar = Rect { y: area.y + 1, height: 1, ..area };
        let dim = Style::new().fg(Color::DarkGray);
        let rail = Style::new().fg(Color::Rgb(0x5c, 0x63, 0x6e));
        for x in bar.x..bar.right() {
            buf[(x, bar.y)].set_symbol("─").set_style(rail);
            buf[(x, labels.y)].set_symbol(" ").set_style(Style::new());
        }
        // Highlights: the bar's background where there's data.
        for h in &self.highlights {
            let (a, b) = (h.start.max(self.start), h.stop.min(self.stop));
            if a >= b {
                continue;
            }
            let (ca, cb) = (self.col_of(bar, a).unwrap_or(bar.x), self.col_of(bar, b).unwrap_or(bar.right() - 1));
            for x in ca..=cb.min(bar.right() - 1) {
                buf[(x, bar.y)].set_bg(h.color);
            }
        }
        // Ticks and their labels, on round times.
        let step = self.step(bar.width);
        let mut t = (self.start / step).ceil() * step;
        while t <= self.stop {
            if let Some(x) = self.col_of(bar, t) {
                buf[(x, bar.y)].set_symbol("┼");
                let text = label(t, step);
                let lx = x.saturating_sub(text.chars().count() as u16 / 2).max(labels.x);
                if lx + text.chars().count() as u16 <= labels.right().saturating_sub(4) {
                    buf.set_string(lx, labels.y, &text, dim);
                }
            }
            t += step;
        }
        buf.set_string(labels.right() - 3, labels.y, "UTC", dim);
        // Now: where real time is.
        if let Some(x) = self.col_of(bar, now) {
            buf[(x, bar.y)].set_symbol("╂").set_fg(Color::Rgb(0x8a, 0x91, 0x9c));
        }
        // The needle: the clock's time, bright; live, it sits on now.
        if let Some(x) = self.col_of(bar, clock.time()) {
            let color = if clock.is_live() { Color::Rgb(0xd0, 0xd4, 0xda) } else { Color::Rgb(0x7f, 0xb2, 0xff) };
            buf[(x, bar.y)].set_symbol("●").set_style(Style::new().fg(color).add_modifier(Modifier::BOLD));
        } else {
            // Off the span: an arrow at the edge it's past.
            let (x, s) = if clock.time() < self.start { (bar.x, "◀") } else { (bar.right() - 1, "▶") };
            buf[(x, bar.y)].set_symbol(s).set_fg(Color::Rgb(0x7f, 0xb2, 0xff));
        }
    }
}

/// A tick label: `21:30` within a day, `Thu 26` for day steps, `Nov 26` beyond a week.
fn label(t: f64, step: f64) -> String {
    let days = (t / 86400.0).floor() as i64;
    let secs = t.rem_euclid(86400.0) as i64;
    if step < 86400.0 {
        return format!("{:02}:{:02}", secs / 3600, secs / 60 % 60);
    }
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if step < 604_800.0 {
        let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
        format!("{wd} {d}")
    } else {
        let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][m as usize - 1];
        format!("{mon} {d}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;

    // 2026-11-26 04:20 UTC, a Thursday.
    const NOW: f64 = 1_795_666_800.0;

    #[test]
    fn ticks_fit_the_span() {
        let t = Timeline::new(NOW - 7200.0, NOW);
        // Two hours on 80 columns: 90 s a column, so ticks every 15 minutes (10 columns).
        assert_eq!(t.step(80), 900.0);
        assert_eq!(t.step(200), 600.0, "wider: finer");
        let week = Timeline::new(NOW - 7.0 * 86400.0, NOW);
        assert_eq!(week.step(80), 86400.0);
        assert_eq!(label(NOW, 900.0), "04:20");
        assert_eq!(label(NOW, 86400.0), "Thu 26");
        assert_eq!(label(NOW, 604_800.0), "Nov 26");
    }

    #[test]
    fn a_click_is_a_time() {
        let t = Timeline::new(NOW - 7200.0, NOW);
        let area = Rect::new(10, 30, 81, 2);
        assert_eq!(t.time_at(area, 10), Some(NOW - 7200.0));
        assert_eq!(t.time_at(area, 90), Some(NOW));
        assert_eq!(t.time_at(area, 50), Some(NOW - 3600.0));
        assert_eq!(t.time_at(area, 9), None, "off the bar");
    }

    #[test]
    fn zooms_about_a_time_and_pans() {
        let mut t = Timeline::new(NOW - 7200.0, NOW);
        t.zoom(0.5, NOW);
        assert_eq!((t.start, t.stop), (NOW - 3600.0, NOW), "the time under the pointer stays put");
        t.pan(600.0);
        assert_eq!((t.start, t.stop), (NOW - 3000.0, NOW + 600.0));
        for _ in 0..20 {
            t.zoom(0.5, NOW);
        }
        assert!(t.stop - t.start >= MIN_SPAN, "not past five minutes");
    }

    #[test]
    fn draws_ticks_highlights_and_the_needle() {
        let mut t = Timeline::new(NOW - 7200.0, NOW + 600.0);
        t.highlight(NOW - 7200.0, NOW, Color::Rgb(1, 2, 3));
        let area = Rect::new(0, 0, 79, 2);
        let mut buf = Buffer::empty(area);
        let mut clock = Clock::live(NOW);
        clock.seek(NOW - 3600.0);
        t.render(&mut buf, area, &clock, NOW);
        let bar: String = (0..79).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        let labels: String = (0..79).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(bar.contains('●') && bar.contains('╂') && bar.contains('┼'), "{bar}");
        assert!(labels.contains("03:00") && labels.ends_with("UTC"), "{labels}");
        let needle = bar.chars().position(|c| c == '●').unwrap() as u16;
        assert_eq!(t.time_at(area, needle).map(|x| (x - (NOW - 3600.0)).abs() < 100.0), Some(true));
        assert_eq!(buf[(5, 1)].bg, Color::Rgb(1, 2, 3), "data highlighted");
        assert_ne!(buf[(78, 1)].bg, Color::Rgb(1, 2, 3), "after now: not");
    }
}
