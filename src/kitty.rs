//! The kitty terminal graphics protocol, for drawing raster tiles as real images underneath
//! the text layer (Ghostty, kitty, WezTerm; herdr relays it). Every command sets `q=2`, so the
//! terminal sends no replies that would show up as input.
//! Spec: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use std::fmt::Write as _;

/// Whether the terminal likely supports kitty graphics. `MAPTERM_GRAPHICS=kitty` or `cells`
/// overrides the guess.
pub fn supported() -> bool {
    match std::env::var("MAPTERM_GRAPHICS").as_deref() {
        Ok("kitty") => return true,
        Ok("cells") => return false,
        _ => {}
    }
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return true;
    }
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
    let term = std::env::var("TERM").unwrap_or_default().to_ascii_lowercase();
    ["ghostty", "kitty", "wezterm", "herdr"].iter().any(|t| term_program.contains(t) || term.contains(t))
}

/// Standard base64 (RFC 4648), for image data and OSC 52.
pub fn base64(bytes: &[u8]) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            out.push(if i <= chunk.len() { B64[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Send a PNG to the terminal as image `id`, without displaying it.
pub fn transmit(id: u32, png: &[u8]) -> String {
    let data = base64(png);
    let mut out = String::with_capacity(data.len() + 64);
    let chunks: Vec<&str> = data.as_bytes().chunks(4096).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        if i == 0 {
            let _ = write!(out, "\x1b_Ga=t,f=100,t=d,i={id},q=2,m={more};{chunk}\x1b\\");
        } else {
            let _ = write!(out, "\x1b_Gm={more};{chunk}\x1b\\");
        }
    }
    out
}

/// Show source rectangle `src` (x, y, w, h pixels) of image `id`, scaled to `cols` x `rows`
/// cells with its top-left at absolute cell (`col`, `row`), underneath the text. `z` is the
/// stacking order among images (negative: under the text).
#[allow(clippy::too_many_arguments)]
pub fn place(id: u32, placement: u32, col: u16, row: u16, src: (u32, u32, u32, u32), cols: u16, rows: u16, z: i32) -> String {
    let (x, y, w, h) = src;
    format!("\x1b[{};{}H\x1b_Ga=p,i={id},p={placement},x={x},y={y},w={w},h={h},c={cols},r={rows},z={z},C=1,q=2\x1b\\", row as u32 + 1, col as u32 + 1)
}

/// Remove every placement on the screen (the images stay loaded).
pub const CLEAR_PLACEMENTS: &str = "\x1b_Ga=d,d=a,q=2\x1b\\";

/// Free image `id` from the terminal's memory.
pub fn free(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// Free every image this program sent.
pub const FREE_ALL: &str = "\x1b_Ga=d,d=A,q=2\x1b\\";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn transmit_chunks_and_place() {
        let t = transmit(7, &vec![0u8; 6000]);
        assert!(t.starts_with("\x1b_Ga=t,f=100,t=d,i=7,q=2,m=1;"));
        assert!(t.ends_with("\x1b\\") && t.contains("\x1b_Gm=0;"));
        assert_eq!(t.matches("\x1b_G").count(), 2, "8000 base64 chars -> two chunks");
        let p = place(7, 3, 4, 2, (10, 20, 30, 40), 5, 6, -1);
        assert_eq!(p, "\x1b[3;5H\x1b_Ga=p,i=7,p=3,x=10,y=20,w=30,h=40,c=5,r=6,z=-1,C=1,q=2\x1b\\");
    }
}
