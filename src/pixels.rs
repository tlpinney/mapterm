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
    /// Stand-in images shown for one frame, freed with the next.
    pub(crate) temp: Vec<u32>,
    /// Tests: drawing time per frame instead of [`DRAW_BUDGET`].
    pub(crate) budget: Option<std::time::Duration>,
}

/// A grid tile (z, x, y), the zoom in 1/64ths, and a signature of the style and visibility.
pub(crate) type PixKey = ((u8, u32, u32), i64, u64);

pub(crate) struct CachedTile {
    /// Its id and size (the pixels go to the terminal once and aren't kept here).
    pub(crate) image: PixImage,
    /// The drawn tile, premultiplied, for stand-ins at other zooms.
    pub(crate) pixmap: std::sync::Arc<Pixmap>,
    pub(crate) used: u64,
}

/// Time to spend drawing new tiles per frame; the rest stay blank until later frames.
pub(crate) const DRAW_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

/// Time to spend building stand-ins (other zooms' tiles scaled into place) per frame; tiles
/// past it, at the edges since the center comes first, stay blank for a frame or two.
pub(crate) const STAND_IN_BUDGET: std::time::Duration = std::time::Duration::from_millis(12);

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

    /// The finished image, un-premultiplied for the terminal, and the drawn pixmap.
    pub(crate) fn finish(self, id: u32) -> (PixImage, Pixmap) {
        let (width, height) = (self.map.width(), self.map.height());
        let mut rgba = Vec::with_capacity(self.map.data().len());
        for px in self.map.pixels() {
            let c = px.demultiply();
            rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        (PixImage { id, width, height, rgba: std::sync::Arc::new(rgba) }, self.map)
    }

    /// Fill the whole image with one color (a stand-in's base: the map's background, so
    /// nothing in it is see-through).
    pub(crate) fn clear(&mut self, c: (u8, u8, u8)) {
        self.map.fill(tiny_skia::Color::from_rgba8(c.0, c.1, c.2, 255));
    }

    /// Draw `src` (another tile's pixmap) scaled by `k` with its top-left at (`x`, `y`)
    /// pixels, smoothly filtered: a stand-in from another zoom. Its edge pixels are carried
    /// out to the rectangle's hard edges (no fading border), so neighbouring tiles meet
    /// without a see-through seam.
    pub(crate) fn blit(&mut self, src: &Pixmap, x: f32, y: f32, k: f32) {
        let (w, h) = (src.width() as f32 * k, src.height() as f32 * k);
        let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w, h) else { return };
        let shader = tiny_skia::Pattern::new(
            src.as_ref(),
            tiny_skia::SpreadMode::Pad,
            tiny_skia::FilterQuality::Bilinear,
            1.0,
            Transform::from_row(k, 0.0, 0.0, k, x, y),
        );
        let paint = Paint { shader, anti_alias: false, ..Paint::default() };
        self.map.fill_rect(rect, &paint, Transform::identity(), None);
    }

    /// Whether anything was drawn (a stand-in with no overlapping tiles stays blank).
    pub(crate) fn is_empty(&self) -> bool {
        self.map.pixels().iter().all(|p| p.alpha() == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_scales_into_place() {
        let mut src = Pix::sized(10, 10, (10.0, 20.0)).unwrap();
        src.fill(&[vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]], Color::Rgb(0, 0, 255));
        let (_, pm) = src.finish(1);
        let mut dst = Pix::sized(20, 20, (10.0, 20.0)).unwrap();
        dst.blit(&pm, 0.0, 0.0, 1.414);
        // A tile's own edge pixels reach the edge of where it lands: no see-through fringe.
        let mut full = Pix::sized(10, 10, (10.0, 20.0)).unwrap();
        full.clear((200, 100, 50));
        let (_, fpm) = full.finish(3);
        let mut d2 = Pix::sized(30, 30, (10.0, 20.0)).unwrap();
        d2.blit(&fpm, 0.0, 0.0, 1.7);
        let (f2, _) = d2.finish(4);
        for i in 0..17usize {
            for j in 0..17usize {
                assert_eq!(f2.rgba[(j * 30 + i) * 4 + 3], 255, "opaque at {i},{j}");
            }
        }
        let (f, _) = dst.finish(2);
        let px = |x: usize, y: usize| f.rgba[(y * 20 + x) * 4..(y * 20 + x) * 4 + 4].to_vec();
        assert_eq!(px(0, 0), vec![0, 0, 255, 255], "corner");
        assert_eq!(px(7, 7), vec![0, 0, 255, 255], "inside the scaled square");
        assert_eq!(px(18, 18)[3], 0, "past it");
    }

    #[test]
    fn fills_and_strokes_land_in_pixels() {
        // 10x5 cells of 10x20 px: 100x100 pixels, 5 px per dot.
        let mut p = Pix::sized(100, 100, (10.0, 20.0)).unwrap();
        p.fill(&[vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]], Color::Rgb(0, 0, 255));
        p.stroke(&[(0.0, 15.0), (20.0, 15.0)], Color::Rgb(255, 0, 0), 4.0, None);
        let (f, _) = p.finish(1);
        assert_eq!((f.width, f.height), (100, 100));
        let at = |x: usize, y: usize| &f.rgba[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4];
        assert_eq!(at(25, 25), &[0, 0, 255, 255], "inside the fill");
        assert_eq!(at(75, 25)[3], 0, "outside: transparent");
        assert_eq!(at(50, 75)[0], 255, "on the line");
    }
}
