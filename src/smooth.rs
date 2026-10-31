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

use crate::render::{Camera, to_world};

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
    fn declaration_format() {
        let cam = Camera::new(0.0, 0.0, 0.0);
        let s = declare(3, Rect::new(0, 1, 80, 20), &cam);
        assert_eq!(s, "\x1b_smooth;3;1;20;0;79;32;64\x1b\\");
    }
}
