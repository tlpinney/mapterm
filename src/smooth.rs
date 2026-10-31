//! Opt-in pixel smooth vertical panning for terminals that implement the experimental
//! `ESC _ smooth;...` region protocol (tlpinney/ghostty, relayed by tlpinney/herdr).
//! See SMOOTH_SCROLL.md in that fork.
//!
//! The map area is declared as a region whose last row is the hidden overscan row. The
//! terminal accumulates trackpad pixels, shifts the region by the sub-row remainder, and sends
//! one wheel event per whole row; the app pans exactly one row per event ([`Camera::pan_rows`])
//! and re-declares the region after each draw. `pos` is the camera's vertical position in rows
//! from the top of the world at the current zoom. Terminals without the patch ignore the APC.

use ratatui::layout::Rect;

use crate::render::{Camera, to_lonlat, to_world};

/// Clears every region this app declared; send on exit or when turning smooth off.
pub const SMOOTH_OFF: &str = "\x1b_smooth;off\x1b\\";

/// Whether the environment variable `var` turns smooth scrolling on. Apps pick their own
/// variable: a terminal-wide `SMOOTH_SCROLL=1` would also flip apps whose wheel does something else.
pub fn env_enabled(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Rows per world height at this zoom (4 braille dots per row).
pub fn world_rows(zoom: f64) -> f64 {
    256.0 * 2f64.powf(zoom) / 4.0
}

/// The camera's row position: what the protocol calls `pos`.
pub fn pos(cam: &Camera) -> u64 {
    let (_, y) = to_world(cam.lon, cam.lat);
    (y * world_rows(cam.zoom)).round().max(0.0) as u64
}

/// Split `cam` into a camera on the cell grid (what to render) and the sub-cell remainder in
/// thousandths of a cell, (x, y), for the terminal to shift the region left and up by.
pub fn snap(cam: &Camera) -> (Camera, u16, u16) {
    let cols = world_rows(cam.zoom) * 2.0; // 2 dots per column vs 4 per row
    let rows = world_rows(cam.zoom);
    let (x, y) = to_world(cam.lon, cam.lat);
    let (cx, cy) = (x * cols, y * rows);
    // A hair of slack so float noise right at a cell edge doesn't flip a whole cell.
    let (fx, fy) = ((cx + 1e-6).floor(), (cy + 1e-6).floor());
    let milli = |v: f64| ((v * 1000.0).round().clamp(0.0, 999.0)) as u16;
    let (lon, lat) = to_lonlat(fx / cols, (fy / rows).clamp(0.0, 1.0));
    (Camera::new(lon, lat, cam.zoom), milli(cx - fx), milli(cy - fy))
}

/// The declaration for a map drawn in `area`, using region `id`.
pub fn declare(id: u32, area: Rect, cam: &Camera) -> String {
    if area.width == 0 || area.height < 2 {
        return format!("\x1b_smooth;off;{id}\x1b\\");
    }
    format!(
        "\x1b_smooth;{id};{};{};{};{};{};{}\x1b\\",
        area.y,
        area.bottom() - 1,
        area.x,
        area.right() - 1,
        pos(cam),
        world_rows(cam.zoom) as u64,
    )
}

/// A shift-only region (protocol v3): the region's last row and last column are overscan,
/// and the terminal draws the region `ox`/`oy` thousandths of a cell left/up. It declares
/// `pos` 0 and `max` 0, nothing to scroll, so the terminal leaves the wheel to the app (the
/// map keeps zooming on scroll) and the shift only carries what the app moves itself, like a
/// drag. Render with the camera from [`snap`].
pub fn declare_shift(id: u32, area: Rect, ox: u16, oy: u16) -> String {
    if area.width < 2 || area.height < 2 {
        return format!("\x1b_smooth;off;{id}\x1b\\");
    }
    format!(
        "\x1b_smooth;{id};{};{};{};{};0;0;{ox};{oy}\x1b\\",
        area.y,
        area.bottom() - 1,
        area.x,
        area.right() - 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_row_per_pan() {
        let mut cam = Camera::new(-77.0353, 38.8895, 12.3);
        cam.snap_rows();
        let p = pos(&cam);
        for i in 1..=50 {
            cam.pan_rows(1);
            assert_eq!(pos(&cam), p + i);
        }
        for i in 1..=50 {
            cam.pan_rows(-1);
            assert_eq!(pos(&cam), p + 50 - i);
        }
    }

    #[test]
    fn snap_splits_into_cells_and_remainder() {
        let mut cam = Camera::new(-77.0353, 38.8895, 13.0);
        cam.snap_rows();
        let (c0, _, oy0) = snap(&cam);
        assert!(oy0 < 2 || oy0 > 997, "row-aligned camera has no y remainder: {oy0}");
        // A quarter row down: same cell row, 250 thousandths of shift.
        cam.pan_cells(0.0, 0.25);
        let (c1, _, oy1) = snap(&cam);
        assert_eq!(pos(&c1), pos(&c0));
        assert!((oy1 as i32 - 250).abs() <= 2, "{oy1}");
        // A full row: next cell row, same remainder.
        cam.pan_cells(0.0, 1.0);
        let (c2, _, oy2) = snap(&cam);
        assert_eq!(pos(&c2), pos(&c0) + 1);
        assert!((oy2 as i32 - 250).abs() <= 2, "{oy2}");
        // Horizontal: 0.6 of a column.
        let (_, ox_a, _) = snap(&cam);
        cam.pan_cells(0.6, 0.0);
        let (_, ox_b, _) = snap(&cam);
        assert!(((ox_b as i32 - ox_a as i32).rem_euclid(1000) - 600).abs() <= 2, "{ox_a} {ox_b}");
    }

    #[test]
    fn shift_declaration_format() {
        let s = declare_shift(3, Rect::new(0, 1, 80, 20), 250, 750);
        assert_eq!(s, "\x1b_smooth;3;1;20;0;79;0;0;250;750\x1b\\");
    }

    #[test]
    fn declaration_format() {
        let cam = Camera::new(0.0, 0.0, 0.0);
        let s = declare(3, Rect::new(0, 1, 80, 20), &cam);
        assert_eq!(s, "\x1b_smooth;3;1;20;0;79;32;64\x1b\\");
    }
}
