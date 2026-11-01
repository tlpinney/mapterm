//! Render a drawn [`Buffer`] as plain text, ANSI-colored text or an HTML fragment, for
//! snapshots, docs and web pages (`mapterm --snapshot WxH --format html`).

use std::fmt::Write as _;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// The cells of row `y` a terminal shows: the cells hidden under a wide character (CJK
/// labels) are skipped, as the terminal draws the wide character over them.
fn row(buf: &Buffer, y: u16) -> impl Iterator<Item = &ratatui::buffer::Cell> {
    let a = buf.area;
    let mut hidden = 0usize;
    (a.left()..a.right()).filter_map(move |x| {
        if hidden > 0 {
            hidden -= 1;
            return None;
        }
        let c = &buf[(x, y)];
        hidden = c.symbol().width().saturating_sub(1);
        Some(c)
    })
}

/// The buffer as plain text, one line per row, trailing spaces trimmed.
pub fn to_text(buf: &Buffer) -> String {
    let a = buf.area;
    let mut out = String::new();
    for y in a.top()..a.bottom() {
        let line: String = row(buf, y).map(|c| c.symbol()).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The buffer as text with 24-bit ANSI colors, bold and italic; each row ends with a reset.
pub fn to_ansi(buf: &Buffer) -> String {
    let a = buf.area;
    let mut out = String::new();
    for y in a.top()..a.bottom() {
        let mut cur: Option<Look> = None;
        for c in row(buf, y) {
            let look = Look::of(c.fg, c.bg, c.modifier);
            if cur != Some(look) {
                out.push_str("\x1b[0");
                if let Some((r, g, b)) = look.fg {
                    let _ = write!(out, ";38;2;{r};{g};{b}");
                }
                if let Some((r, g, b)) = look.bg {
                    let _ = write!(out, ";48;2;{r};{g};{b}");
                }
                if look.bold {
                    out.push_str(";1");
                }
                if look.italic {
                    out.push_str(";3");
                }
                out.push('m');
                cur = Some(look);
            }
            out.push_str(c.symbol());
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// The buffer as a `<pre class="mapterm">` fragment: runs of cells with the same look become
/// one `<span>` with inline `color` / `background` (and bold/italic). Cells with no color
/// inherit from the page, so the page sets the default foreground and background.
pub fn to_html(buf: &Buffer) -> String {
    let a = buf.area;
    let mut out = format!("<pre class=\"mapterm\" data-cols=\"{}\" data-rows=\"{}\">", a.width, a.height);
    for y in a.top()..a.bottom() {
        let mut run = String::new();
        let mut cur: Option<Look> = None;
        for c in row(buf, y) {
            let look = Look::of(c.fg, c.bg, c.modifier);
            if cur != Some(look) {
                if let Some(l) = cur {
                    l.span(&mut out, &run);
                }
                run.clear();
                cur = Some(look);
            }
            escape(&mut run, c.symbol());
        }
        if let Some(l) = cur {
            l.span(&mut out, &run);
        }
        if y + 1 < a.bottom() {
            out.push('\n');
        }
    }
    out.push_str("</pre>\n");
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Look {
    fg: Option<(u8, u8, u8)>,
    bg: Option<(u8, u8, u8)>,
    bold: bool,
    italic: bool,
}

impl Look {
    fn of(fg: Color, bg: Color, m: Modifier) -> Look {
        Look { fg: rgb(fg), bg: rgb(bg), bold: m.contains(Modifier::BOLD), italic: m.contains(Modifier::ITALIC) }
    }

    fn span(&self, out: &mut String, text: &str) {
        if self.fg.is_none() && self.bg.is_none() && !self.bold && !self.italic {
            out.push_str(text);
            return;
        }
        out.push_str("<span style=\"");
        if let Some((r, g, b)) = self.fg {
            let _ = write!(out, "color:#{r:02x}{g:02x}{b:02x};");
        }
        if let Some((r, g, b)) = self.bg {
            let _ = write!(out, "background:#{r:02x}{g:02x}{b:02x};");
        }
        if self.bold {
            out.push_str("font-weight:bold;");
        }
        if self.italic {
            out.push_str("font-style:italic;");
        }
        out.push_str("\">");
        out.push_str(text);
        out.push_str("</span>");
    }
}

fn escape(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => {}
            _ => out.push(ch),
        }
    }
}

/// A color as RGB, or `None` for the terminal default. Named and indexed colors use the
/// xterm defaults.
fn rgb(c: Color) -> Option<(u8, u8, u8)> {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0), (205, 0, 0), (0, 205, 0), (205, 205, 0), (0, 0, 238), (205, 0, 205), (0, 205, 205), (229, 229, 229),
        (127, 127, 127), (255, 0, 0), (0, 255, 0), (255, 255, 0), (92, 92, 255), (255, 0, 255), (0, 255, 255), (255, 255, 255),
    ];
    let indexed = |i: u8| -> (u8, u8, u8) {
        match i {
            0..=15 => BASE[i as usize],
            16..=231 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                (level(i / 36), level(i / 6 % 6), level(i % 6))
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                (v, v, v)
            }
        }
    };
    Some(match c {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i) => indexed(i),
        Color::Black => BASE[0],
        Color::Red => BASE[1],
        Color::Green => BASE[2],
        Color::Yellow => BASE[3],
        Color::Blue => BASE[4],
        Color::Magenta => BASE[5],
        Color::Cyan => BASE[6],
        Color::Gray => BASE[7],
        Color::DarkGray => BASE[8],
        Color::LightRed => BASE[9],
        Color::LightGreen => BASE[10],
        Color::LightYellow => BASE[11],
        Color::LightBlue => BASE[12],
        Color::LightMagenta => BASE[13],
        Color::LightCyan => BASE[14],
        Color::White => BASE[15],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    fn sample() -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 2));
        buf.set_string(0, 0, "a<b", Style::default().fg(Color::Rgb(255, 0, 0)));
        buf.set_string(3, 0, "c", Style::default().bg(Color::Cyan).add_modifier(Modifier::BOLD));
        buf.set_string(0, 1, "⣿", Style::default());
        buf
    }

    #[test]
    fn text_trims_rows() {
        assert_eq!(to_text(&sample()), "a<bc\n⣿\n");
    }

    #[test]
    fn html_groups_runs_and_escapes() {
        let h = to_html(&sample());
        assert!(h.starts_with("<pre class=\"mapterm\" data-cols=\"6\" data-rows=\"2\">"), "{h}");
        assert!(h.contains("<span style=\"color:#ff0000;\">a&lt;b</span>"), "{h}");
        assert!(h.contains("<span style=\"background:#00cdcd;font-weight:bold;\">c</span>  \n⣿     </pre>"), "{h}");
    }

    #[test]
    fn wide_characters_hide_the_next_cell() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf.set_string(0, 0, "東京", Style::default());
        assert_eq!(to_text(&buf), "東京\n");
        assert_eq!(to_html(&buf), "<pre class=\"mapterm\" data-cols=\"4\" data-rows=\"1\">東京</pre>\n");
    }

    #[test]
    fn ansi_sets_truecolor() {
        let s = to_ansi(&sample());
        assert!(s.starts_with("\x1b[0;38;2;255;0;0ma<b\x1b[0;48;2;0;205;205;1mc"), "{s:?}");
        assert!(s.ends_with("⣿     \x1b[0m\n"), "{s:?}");
    }
}
