//! Pixel mode: fills and lines drawn into an image at the terminal's real resolution
//! (anti-aliased, with the style's widths and dashes) instead of braille dots, for terminals
//! that show kitty graphics. Labels and markers stay terminal text on top.

use ratatui::style::Color;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

/// A tile image to send to the terminal once: straight (not premultiplied) RGBA rows.
#[derive(Clone, Debug)]
pub struct PixImage {
    /// The kitty graphics image id.
    pub id: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixels, 4 bytes each (red, green, blue, alpha), row by row from the top.
    pub rgba: std::sync::Arc<Vec<u8>>,
}

/// Where a tile image goes this frame: at cell (`col`, `row`) plus a pixel offset inside that
/// cell, showing the `src` rectangle (x, y, width, height in the image's pixels) at its own
/// size (one image pixel per screen pixel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixPlacement {
    /// The image.
    pub id: u32,
    /// Top-left cell, in the terminal.
    pub col: u16,
    /// Top-left cell, in the terminal.
    pub row: u16,
    /// Pixels right of the cell's left edge (less than a cell).
    pub x_off: u32,
    /// Pixels below the cell's top edge (less than a cell).
    pub y_off: u32,
    /// The part of the image shown: x, y, width, height.
    pub src: (u32, u32, u32, u32),
}

/// A frame in pixel mode: images new to the terminal, every tile's placement, and images it
/// can forget. See [`crate::Map::take_pixels`].
#[derive(Clone, Debug, Default)]
pub struct PixFrame {
    /// Send these first (each once; later frames only place them).
    pub new: Vec<PixImage>,
    /// Place these, bottom to top.
    pub placed: Vec<PixPlacement>,
    /// Images to delete from the terminal (evicted from the cache).
    pub freed: Vec<u32>,
    /// Tiles left blank this frame: their data is still loading, or there wasn't time to
    /// draw them. Draw again soon while this is above zero.
    pub pending: usize,
}

/// Tile images drawn so far, by (grid tile, zoom, style signature), least recently used out
/// first past a byte budget.
#[derive(Default)]
pub(crate) struct PixCache {
    pub(crate) tiles: std::collections::HashMap<PixKey, CachedTile>,
    pub(crate) bytes: usize,
    pub(crate) frame: u64,
}

/// A grid tile (z, x, y), the zoom in 1/64ths, and a signature of the style and visibility.
pub(crate) type PixKey = ((u8, u32, u32), i64, u64);

pub(crate) struct CachedTile {
    pub(crate) image: PixImage,
    pub(crate) used: u64,
}

/// Time to spend drawing new tiles per frame; the rest stay blank until later frames.
pub(crate) const DRAW_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

/// Bytes of tile images to keep (the terminal holds them too).
pub(crate) const PIX_BUDGET: usize = 192 << 20;

pub(crate) fn next_image_id() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0x7e00_0000);
    N.fetch_add(1, Ordering::Relaxed)
}

/// The image a frame's fills and lines go into. Coordinates arrive in braille dots (2 per
/// column, 4 per row) and are scaled to pixels.
pub(crate) struct Pix {
    map: Pixmap,
    /// Pixels per dot, across and down.
    sx: f32,
    sy: f32,
    /// Device pixels per CSS pixel, for style widths (about 2 on a Retina display).
    dpr: f32,
}

impl Pix {
    /// A transparent image of `w` x `h` pixels, for cells of `cell_px` pixels.
    pub(crate) fn sized(w: u32, h: u32, cell_px: (f64, f64)) -> Option<Pix> {
        let (cw, ch) = (cell_px.0.max(1.0) as f32, cell_px.1.max(1.0) as f32);
        // A terminal cell is about 8 CSS pixels wide.
        Some(Pix { map: Pixmap::new(w.max(1), h.max(1))?, sx: cw / 2.0, sy: ch / 4.0, dpr: (cw / 8.0).clamp(1.0, 4.0) })
    }

    fn paint(c: Color) -> Paint<'static> {
        let (r, g, b) = match c {
            Color::Rgb(r, g, b) => (r, g, b),
            // Named colors only come from apps' own overlays; a mid grey keeps them visible.
            _ => (128, 128, 128),
        };
        let mut p = Paint::default();
        p.set_color_rgba8(r, g, b, 255);
        p.anti_alias = true;
        p
    }

    fn path(&self, parts: &[Vec<(f64, f64)>], close: bool) -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        for part in parts {
            let mut it = part.iter();
            let Some(&(x, y)) = it.next() else { continue };
            pb.move_to(x as f32 * self.sx, y as f32 * self.sy);
            for &(x, y) in it {
                pb.line_to(x as f32 * self.sx, y as f32 * self.sy);
            }
            if close {
                pb.close();
            }
        }
        pb.finish()
    }

    /// Fill polygon rings (even-odd, so holes cut out), in dots.
    pub(crate) fn fill(&mut self, rings: &[Vec<(f64, f64)>], c: Color) {
        if let Some(p) = self.path(rings, true) {
            self.map.fill_path(&p, &Self::paint(c), FillRule::EvenOdd, Transform::identity(), None);
        }
    }

    /// A polyline in dots, `width_css` CSS pixels wide; `dash` is on/off lengths in line
    /// widths, as `line-dasharray` gives them.
    pub(crate) fn stroke(&mut self, pts: &[(f64, f64)], c: Color, width_css: f64, dash: Option<&[f64]>) {
        let Some(p) = self.path(std::slice::from_ref(&pts.to_vec()), false) else { return };
        let width = (width_css as f32 * self.dpr).max(0.75);
        let dash = dash.and_then(|d| {
            let v: Vec<f32> = d.iter().map(|&x| (x as f32 * width).max(0.5)).collect();
            StrokeDash::new(if v.len().is_multiple_of(2) { v } else { [v.clone(), v].concat() }, 0.0)
        });
        let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, dash, ..Stroke::default() };
        self.map.stroke_path(&p, &Self::paint(c), &stroke, Transform::identity(), None);
    }

    /// The finished image, un-premultiplied for the terminal.
    pub(crate) fn finish(self, id: u32) -> PixImage {
        let (width, height) = (self.map.width(), self.map.height());
        let mut rgba = Vec::with_capacity(self.map.data().len());
        for px in self.map.pixels() {
            let c = px.demultiply();
            rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        PixImage { id, width, height, rgba: std::sync::Arc::new(rgba) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_and_strokes_land_in_pixels() {
        // 10x5 cells of 10x20 px: 100x100 pixels, 5 px per dot.
        let mut p = Pix::sized(100, 100, (10.0, 20.0)).unwrap();
        p.fill(&[vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]], Color::Rgb(0, 0, 255));
        p.stroke(&[(0.0, 15.0), (20.0, 15.0)], Color::Rgb(255, 0, 0), 4.0, None);
        let f = p.finish(1);
        assert_eq!((f.width, f.height), (100, 100));
        let at = |x: usize, y: usize| &f.rgba[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4];
        assert_eq!(at(25, 25), &[0, 0, 255, 255], "inside the fill");
        assert_eq!(at(75, 25)[3], 0, "outside: transparent");
        assert_eq!(at(50, 75)[0], 255, "on the line");
    }
}
