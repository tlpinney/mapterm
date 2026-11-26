//! Text drawn into images, for labels that move with what they name: a kitty image can place
//! them to the pixel where terminal text hops a cell at a time.
//!
//! The font is [JetBrains Mono](https://www.jetbrains.com/lp/mono/) (Ghostty's built-in
//! font), bundled under the SIL Open Font License 1.1 (`fonts/OFL.txt`), or any TrueType or
//! OpenType file in `MAPTERM_FONT`. Glyph outlines come from [`skrifa`] and are filled with
//! tiny-skia, anti-aliased, over a dark outline so labels read on any map.

use std::collections::HashMap;
use std::sync::OnceLock;

use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use tiny_skia::{FillRule, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// JetBrains Mono Regular 2.304, SIL OFL 1.1 (see `fonts/OFL.txt`).
const JETBRAINS_MONO: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");

/// The font labels are drawn in: `MAPTERM_FONT` if it names a font that reads, else the
/// bundled JetBrains Mono.
pub fn font() -> &'static skrifa::FontRef<'static> {
    static FONT: OnceLock<skrifa::FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| {
        let custom = std::env::var_os("MAPTERM_FONT").and_then(|p| std::fs::read(p).ok()).and_then(|b| {
            // Kept for the life of the program, as the bundled font is.
            let b: &'static [u8] = Box::leak(b.into_boxed_slice());
            skrifa::FontRef::new(b).ok().filter(|f| f.charmap().map('A').is_some())
        });
        custom.unwrap_or_else(|| skrifa::FontRef::new(JETBRAINS_MONO).expect("the bundled font"))
    })
}

/// Collects a glyph's outline as a tiny-skia path, flipping y (fonts go up, images down).
struct Pen(PathBuilder);

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, -y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, -y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(cx0, -cy0, x, -y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.cubic_to(cx0, -cy0, cx1, -cy1, x, -y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

/// Glyphs at one size and color, kept for the next frame: each one's advance and a sprite
/// of it already drawn (dark outline and fill), with where the sprite's corner sits relative
/// to the pen on the baseline. Copying a sprite is far cheaper than filling an outline.
pub struct Glyphs {
    px: f32,
    color: (u8, u8, u8),
    cache: HashMap<char, (f32, Option<Sprite>)>,
}

/// A glyph drawn into a small image, and where that image's corner sits relative to the pen
/// on the baseline.
type Sprite = (Pixmap, i32, i32);

/// A label's box in pixels: x, y, width, height.
pub type Rect = (f32, f32, f32, f32);

impl Glyphs {
    /// Glyphs `px` pixels tall (the font's em).
    pub fn new(px: f32) -> Glyphs {
        Glyphs { px, color: (0xc9, 0xd1, 0xd9), cache: HashMap::new() }
    }

    fn glyph(&mut self, c: char) -> &(f32, Option<Sprite>) {
        let (px, color) = (self.px, self.color);
        self.cache.entry(c).or_insert_with(|| {
            let f = font();
            let Some(gid) = f.charmap().map(c).or_else(|| f.charmap().map('?')) else { return (px * 0.6, None) };
            let size = Size::new(px);
            let advance = f.glyph_metrics(size, LocationRef::default()).advance_width(gid).unwrap_or(px * 0.6);
            let mut pen = Pen(PathBuilder::new());
            let drawn = f.outline_glyphs().get(gid).and_then(|g| g.draw(DrawSettings::unhinted(size, LocationRef::default()), &mut pen).ok());
            let path = drawn.and_then(|_| pen.0.finish());
            (advance, path.and_then(|p| sprite(&p, px, color)))
        })
    }

    /// How wide `text` is, pixels.
    pub fn width(&mut self, text: &str) -> f32 {
        text.chars().map(|c| self.glyph(c).0).sum()
    }

    /// The box `text` covers with its left edge at `x` and its middle at `y`.
    pub fn bounds(&mut self, text: &str, x: f32, y: f32) -> Rect {
        let h = self.px * 1.2;
        (x - 1.0, y - h / 2.0, self.width(text) + 2.0, h)
    }

    /// Draw `text` into `pm` with its left edge at `x` and its middle at `y`, in `color`, over
    /// a dark outline so it reads on any map.
    pub fn draw(&mut self, pm: &mut Pixmap, text: &str, x: f32, y: f32, color: (u8, u8, u8)) {
        if color != self.color {
            self.color = color;
            self.cache.clear();
        }
        let baseline = (y + self.px * 0.35).round() as i32;
        let mut pen_x = x;
        for c in text.chars() {
            let (advance, sprite) = self.glyph(c);
            if let Some((img, dx, dy)) = sprite {
                pm.draw_pixmap(pen_x.round() as i32 + dx, baseline + dy, img.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
            }
            pen_x += *advance;
        }
    }
}

/// A glyph outline drawn once, with its dark outline, into a small image: the image and where
/// its corner sits relative to the pen on the baseline.
fn sprite(path: &tiny_skia::Path, px: f32, color: (u8, u8, u8)) -> Option<Sprite> {
    let halo_w = (px / 8.0).max(1.5);
    let b = path.bounds();
    let pad = halo_w.ceil() + 1.0;
    let (x0, y0) = ((b.left() - pad).floor(), (b.top() - pad).floor());
    let (w, h) = (((b.right() + pad).ceil() - x0) as u32, ((b.bottom() + pad).ceil() - y0) as u32);
    let mut img = Pixmap::new(w.max(1), h.max(1))?;
    let at = Transform::from_translate(-x0, -y0);
    let mut halo = Paint::default();
    halo.set_color_rgba8(13, 17, 23, 220);
    halo.anti_alias = true;
    img.stroke_path(path, &halo, &Stroke { width: halo_w, line_join: LineJoin::Round, ..Stroke::default() }, at, None);
    let mut fill = Paint::default();
    fill.set_color_rgba8(color.0, color.1, color.2, 255);
    fill.anti_alias = true;
    img.fill_path(path, &fill, FillRule::Winding, at, None);
    Some((img, x0 as i32, y0 as i32))
}

/// Whether two boxes overlap.
pub fn overlaps(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_font_draws_text() {
        let mut g = Glyphs::new(16.0);
        assert!(g.width("ISS (ZARYA)") > 50.0);
        // Monospace: every character the same width.
        assert_eq!(g.width("iiii"), g.width("MMMM"));
        let mut pm = Pixmap::new(200, 40).unwrap();
        g.draw(&mut pm, "CREW DRAGON 13", 4.0, 20.0, (201, 209, 217));
        let lit = pm.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(lit > 200, "{lit} pixels drawn");
        // Light text over its dark outline: some pixels are bright.
        assert!(pm.pixels().iter().any(|p| p.demultiply().red() > 180));
        // Nothing drawn left of where the text starts.
        assert!((0..40).all(|y| pm.pixel(1, y).is_some_and(|p| p.alpha() == 0)));
    }

    #[test]
    fn boxes() {
        assert!(overlaps((0.0, 0.0, 10.0, 10.0), (5.0, 5.0, 10.0, 10.0)));
        assert!(!overlaps((0.0, 0.0, 10.0, 10.0), (10.0, 0.0, 10.0, 10.0)));
    }
}
