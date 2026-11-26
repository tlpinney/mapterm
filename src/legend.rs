//! A map legend: a small box in a corner of the map saying what its colors and symbols mean.
//!
//! ```no_run
//! # use mapterm::legend::{Corner, Legend};
//! # use ratatui::style::Color;
//! # fn demo(f: &mut ratatui::Frame, map_area: ratatui::layout::Rect) {
//! let legend = Legend::new("satellites by country")
//!     .entry("●", Color::Rgb(0x79, 0xc0, 0xff), "United States", Some(13_168))
//!     .entry("●", Color::Rgb(0xff, 0x7b, 0x72), "China", Some(1_605))
//!     .entry("●", Color::DarkGray, "other", Some(2_131));
//! legend.render_in(f.buffer_mut(), map_area, Corner::BottomLeft);
//! # }
//! ```

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

/// Which corner of the map a legend sits in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Corner {
    /// Top left.
    TopLeft,
    /// Top right.
    TopRight,
    /// Bottom left (the default).
    #[default]
    BottomLeft,
    /// Bottom right.
    BottomRight,
}

/// One line of a legend: a symbol in a color, what it means, and optionally how many.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The symbol (`●`, `▲`, a glyph the layer draws).
    pub glyph: String,
    /// Its color.
    pub color: Color,
    /// What it stands for.
    pub label: String,
    /// How many there are, if that's worth saying.
    pub count: Option<usize>,
}

/// A legend: a title and its entries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Legend {
    /// The title, in the box's top border.
    pub title: String,
    /// The entries, top to bottom.
    pub entries: Vec<Entry>,
}

/// `12345` as `12,345`.
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl Legend {
    /// A legend titled `title`, with no entries yet.
    pub fn new(title: impl Into<String>) -> Legend {
        Legend { title: title.into(), entries: Vec::new() }
    }

    /// Add an entry.
    pub fn entry(mut self, glyph: impl Into<String>, color: Color, label: impl Into<String>, count: Option<usize>) -> Legend {
        self.entries.push(Entry { glyph: glyph.into(), color, label: label.into(), count });
        self
    }

    /// The lines inside the box.
    fn lines(&self) -> Vec<Line<'static>> {
        let label_w = self.entries.iter().map(|e| e.label.chars().count()).max().unwrap_or(0);
        self.entries
            .iter()
            .map(|e| {
                let mut spans = vec![Span::styled(format!(" {} ", e.glyph), Style::new().fg(e.color)), Span::raw(format!("{:<label_w$}", e.label))];
                if let Some(n) = e.count {
                    spans.push(Span::styled(format!(" {:>7}", thousands(n)), Style::new().fg(Color::DarkGray)));
                }
                spans.push(Span::raw(" "));
                Line::from(spans)
            })
            .collect()
    }

    /// The size the box needs: (width, height) in cells, borders included.
    pub fn size(&self) -> (u16, u16) {
        let inner = self.lines().iter().map(Line::width).max().unwrap_or(0).max(self.title.chars().count() + 2);
        (inner as u16 + 2, self.entries.len() as u16 + 2)
    }

    /// Where the box goes in `area`'s `corner` (clipped to fit; empty if `area` is too small
    /// for any of it).
    pub fn rect(&self, area: Rect, corner: Corner) -> Rect {
        let (w, h) = self.size();
        let (w, h) = (w.min(area.width), h.min(area.height));
        let x = match corner {
            Corner::TopLeft | Corner::BottomLeft => area.x,
            Corner::TopRight | Corner::BottomRight => area.right() - w,
        };
        let y = match corner {
            Corner::TopLeft | Corner::TopRight => area.y,
            Corner::BottomLeft | Corner::BottomRight => area.bottom() - h,
        };
        Rect::new(x, y, w, h)
    }

    /// Draw it in `area`'s `corner`, over whatever is there.
    pub fn render_in(&self, buf: &mut Buffer, area: Rect, corner: Corner) {
        if self.entries.is_empty() {
            return;
        }
        let r = self.rect(area, corner);
        if r.width < 4 || r.height < 3 {
            return;
        }
        Clear.render(r, buf);
        let block = Block::new().borders(Borders::ALL).border_style(Style::new().fg(Color::DarkGray)).title(format!(" {} ", self.title));
        let inner = block.inner(r);
        block.render(r, buf);
        Paragraph::new(self.lines()).render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legend() -> Legend {
        Legend::new("by country").entry("●", Color::Blue, "United States", Some(13_168)).entry("●", Color::Red, "China", Some(1_605))
    }

    #[test]
    fn sizes_to_its_entries() {
        let (w, h) = legend().size();
        assert_eq!(h, 4, "two entries and the borders");
        assert!(w as usize >= " ● United States  13,168 ".len());
    }

    #[test]
    fn sits_in_its_corner() {
        let area = Rect::new(0, 0, 80, 24);
        let l = legend();
        let r = l.rect(area, Corner::BottomLeft);
        assert_eq!((r.x, r.bottom()), (0, 24));
        let r = l.rect(area, Corner::TopRight);
        assert_eq!((r.right(), r.y), (80, 0));
        // Too small: clipped, never past the area.
        assert!(l.rect(Rect::new(0, 0, 10, 3), Corner::BottomRight).width <= 10);
    }

    #[test]
    fn draws_title_entries_and_counts() {
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        legend().render_in(&mut buf, area, Corner::BottomLeft);
        let text = crate::export::to_text(&buf);
        assert!(text.contains("by country") && text.contains("United States") && text.contains("13,168") && text.contains("China"), "{text}");
        assert_eq!(buf[(1, 7)].fg, Color::Blue, "the symbol in its color");
    }
}
