//! Text drawn into images, for labels that move with what they name: a kitty image can place
//! them to the pixel where terminal text hops a cell at a time.
//!
//! The font is [JetBrains Mono](https://www.jetbrains.com/lp/mono/) (Ghostty's built-in
//! font), bundled under the SIL Open Font License 1.1 (`fonts/OFL.txt`), or any TrueType or
//! OpenType file in `MAPTERM_FONT`.

use std::collections::HashMap;
use std::sync::OnceLock;

/// JetBrains Mono Regular 2.304, SIL OFL 1.1 (see `fonts/OFL.txt`).
const JETBRAINS_MONO: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");

/// The font labels are drawn in: `MAPTERM_FONT` if it names a font that loads, else the
/// bundled JetBrains Mono.
pub fn font() -> &'static fontdue::Font {
    static FONT: OnceLock<fontdue::Font> = OnceLock::new();
    FONT.get_or_init(|| {
        let custom = std::env::var_os("MAPTERM_FONT")
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| fontdue::Font::from_bytes(b, fontdue::FontSettings::default()).ok());
        custom.unwrap_or_else(|| fontdue::Font::from_bytes(JETBRAINS_MONO, fontdue::FontSettings::default()).expect("the bundled font"))
    })
}

/// Glyphs drawn at one size, kept for the next frame.
pub struct Glyphs {
    px: f32,
    cache: HashMap<char, (fontdue::Metrics, Vec<u8>)>,
}

/// A label's box in pixels: x, y, width, height.
pub type Rect = (f32, f32, f32, f32);

impl Glyphs {
    /// Glyphs `px` pixels tall (the font's em).
    pub fn new(px: f32) -> Glyphs {
        Glyphs { px, cache: HashMap::new() }
    }

    fn glyph(&mut self, c: char) -> &(fontdue::Metrics, Vec<u8>) {
        let px = self.px;
        self.cache.entry(c).or_insert_with(|| font().rasterize(c, px))
    }

    /// How wide `text` is, pixels.
    pub fn width(&mut self, text: &str) -> f32 {
        text.chars().map(|c| self.glyph(c).0.advance_width).sum()
    }

    /// The box `text` covers with its left edge at `x` and its middle at `y`.
    pub fn bounds(&mut self, text: &str, x: f32, y: f32) -> Rect {
        let h = self.px * 1.2;
        (x - 1.0, y - h / 2.0, self.width(text) + 2.0, h)
    }

    /// Draw `text` into `pm` with its left edge at `x` and its middle at `y`, in `color`, over
    /// a dark one-pixel outline so it reads on any map.
    pub fn draw(&mut self, pm: &mut tiny_skia::Pixmap, text: &str, x: f32, y: f32, color: (u8, u8, u8)) {
        let baseline = y + self.px * 0.35;
        for (dx, dy, c, a) in
            [(-1, 0, (13, 17, 23), 0.85), (1, 0, (13, 17, 23), 0.85), (0, -1, (13, 17, 23), 0.85), (0, 1, (13, 17, 23), 0.85), (0, 0, color, 1.0)]
        {
            let mut pen = x;
            for ch in text.chars() {
                let (m, bitmap) = self.glyph(ch).clone();
                let gx = (pen + m.xmin as f32).round() as i32 + dx;
                let gy = (baseline - m.ymin as f32 - m.height as f32).round() as i32 + dy;
                blend(pm, gx, gy, m.width, m.height, &bitmap, c, a);
                pen += m.advance_width;
            }
        }
    }
}

/// Paint a coverage bitmap (`w` x `h`, 0 to 255) at (`x`, `y`) in `color` at `alpha`.
#[allow(clippy::too_many_arguments)]
fn blend(pm: &mut tiny_skia::Pixmap, x: i32, y: i32, w: usize, h: usize, coverage: &[u8], (r, g, b): (u8, u8, u8), alpha: f32) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    for row in 0..h as i32 {
        let py = y + row;
        if py < 0 || py >= ph {
            continue;
        }
        for col in 0..w as i32 {
            let px = x + col;
            if px < 0 || px >= pw {
                continue;
            }
            let a = coverage[(row * w as i32 + col) as usize] as f32 / 255.0 * alpha;
            if a <= 0.0 {
                continue;
            }
            // Premultiplied RGBA: source over destination.
            let i = ((py * pw + px) * 4) as usize;
            for (k, c) in [r, g, b].into_iter().enumerate() {
                data[i + k] = (c as f32 * a + data[i + k] as f32 * (1.0 - a)).round() as u8;
            }
            data[i + 3] = (255.0 * a + data[i + 3] as f32 * (1.0 - a)).round() as u8;
        }
    }
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
        let mut pm = tiny_skia::Pixmap::new(200, 40).unwrap();
        g.draw(&mut pm, "CREW DRAGON 13", 4.0, 20.0, (201, 209, 217));
        let lit = pm.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(lit > 200, "{lit} pixels drawn");
        // Light text over its dark outline: some pixels are bright.
        assert!(pm.pixels().iter().any(|p| p.demultiply().red() > 180));
    }

    #[test]
    fn boxes() {
        assert!(overlaps((0.0, 0.0, 10.0, 10.0), (5.0, 5.0, 10.0, 10.0)));
        assert!(!overlaps((0.0, 0.0, 10.0, 10.0), (10.0, 0.0, 10.0, 10.0)));
    }
}
