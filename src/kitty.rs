//! The kitty terminal graphics protocol, for drawing raster tiles as real images underneath
//! the text layer (Ghostty, kitty, WezTerm, Rex; herdr relays it). Every command sets `q=2`, so the
//! terminal sends no replies that would show up as input (except the startup [`probe_shm`]).
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
    ["ghostty", "kitty", "wezterm", "herdr", "rex"].iter().any(|t| term_program.contains(t) || term.contains(t))
}

/// Whether to send image tiles through POSIX shared memory (`t=s`) instead of inline base64:
/// the terminal reads the pixels straight from memory, which skips encoding and parsing a
/// third more bytes per tile (and, through herdr, relaying them). A terminal that can't read
/// the object shows nothing, so this asks the terminal itself (see [`probe_shm`]); one that
/// doesn't answer graphics queries gets the old guess (a local Rex or kitty). Never over SSH,
/// where the terminal is on another machine. `MAPTERM_KITTY_MEDIUM=shm` or `direct`
/// overrides it.
///
/// Call it in raw mode, before anything else reads the terminal's input.
pub fn shm_supported() -> bool {
    if !cfg!(unix) {
        return false;
    }
    match std::env::var("MAPTERM_KITTY_MEDIUM").as_deref() {
        Ok("shm") => return true,
        Ok("direct") => return false,
        _ => {}
    }
    if ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"].iter().any(|v| std::env::var_os(v).is_some()) {
        return false;
    }
    if let Some(answer) = probe_shm(std::time::Duration::from_millis(500)) {
        return answer;
    }
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
    let term = std::env::var("TERM").unwrap_or_default().to_ascii_lowercase();
    std::env::var_os("KITTY_WINDOW_ID").is_some() || ["rex", "kitty"].iter().any(|t| term_program.contains(t) || term.contains(t))
}

/// Ask the terminal whether it reads images from shared memory: a 1x1 image in a fresh
/// object, sent as a query (`a=q`, nothing is stored or shown), then a Primary Device
/// Attributes request, which every terminal answers, so the wait ends even when the graphics
/// query gets no reply. `Some(true)` when the terminal read it, `Some(false)` when it
/// answered with an error, `None` when it said nothing about graphics (or stayed silent).
/// Through herdr, herdr's own terminal answers, which is the one that reads the tiles.
#[cfg(unix)]
pub fn probe_shm(timeout: std::time::Duration) -> Option<bool> {
    use std::io::Write as _;
    let name = shm_write(&[0, 0, 0])?;
    let query = format!("\x1b_Gi={PROBE_ID},s=1,v=1,a=q,t=s,f=24;{}\x1b\\\x1b[c", base64(name.as_bytes()));
    let mut out = std::io::stdout();
    let answer = if out.write_all(query.as_bytes()).and_then(|()| out.flush()).is_ok() { read_probe_reply(timeout) } else { None };
    // The terminal removes an object it read; one it didn't is still ours.
    if let Ok(cname) = std::ffi::CString::new(name) {
        // SAFETY: unlinking a name this process created; harmless if already gone.
        unsafe { libc::shm_unlink(cname.as_ptr()) };
    }
    answer
}

#[cfg(not(unix))]
pub fn probe_shm(_timeout: std::time::Duration) -> Option<bool> {
    None
}

/// The image id of the shared-memory query (any id: a query stores nothing).
const PROBE_ID: u32 = 4_000_000_001;

/// Read the terminal's answers to [`probe_shm`] from stdin until the Device Attributes reply
/// arrives or `timeout` passes. Bytes read here (keys typed during startup) are dropped.
#[cfg(unix)]
fn read_probe_reply(timeout: std::time::Duration) -> Option<bool> {
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        let mut pfd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd; read into a stack buffer of the length given.
        let n = unsafe {
            match libc::poll(&mut pfd, 1, left.as_millis().clamp(1, i32::MAX as u128) as i32) {
                r if r > 0 => {
                    let mut chunk = [0u8; 256];
                    let n = libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), chunk.len());
                    if n > 0 {
                        buf.extend_from_slice(&chunk[..n as usize]);
                    }
                    n
                }
                0 => return None,
                _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => continue,
                _ => -1,
            }
        };
        if n <= 0 {
            return None;
        }
        if let Some(done) = parse_probe_reply(&buf) {
            return done;
        }
    }
}

/// What the replies read so far say: `None` until the Device Attributes reply
/// (`ESC [ ? ... c`) has arrived; then whether the graphics reply was `OK`, if there was one.
fn parse_probe_reply(buf: &[u8]) -> Option<Option<bool>> {
    let da = buf.windows(3).position(|w| w == b"\x1b[?")?;
    buf[da + 3..].iter().position(|&b| b == b'c')?;
    let head = format!("\x1b_Gi={PROBE_ID};");
    let graphics = buf[..da].windows(head.len()).position(|w| w == head.as_bytes());
    Some(graphics.map(|at| buf[at + head.len()..da].starts_with(b"OK")))
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

/// Send a PNG as image `id` through a new POSIX shared memory object, which the terminal
/// reads and unlinks (see [`shm_supported`]). `None` if the object couldn't be made (or not
/// on Unix); send it with [`transmit`] then.
pub fn transmit_shm(id: u32, png: &[u8]) -> Option<String> {
    #[cfg(unix)]
    {
        if png.is_empty() {
            return None;
        }
        let name = shm_write(png)?;
        Some(format!("\x1b_Ga=t,f=100,t=s,S={},i={id},q=2;{}\x1b\\", png.len(), base64(name.as_bytes())))
    }
    #[cfg(not(unix))]
    {
        let _ = (id, png);
        None
    }
}

/// Show source rectangle `src` (x, y, w, h pixels) of image `id`, scaled to `cols` x `rows`
/// cells with its top-left at absolute cell (`col`, `row`), underneath the text. `z` is the
/// stacking order among images (negative: under the text).
#[allow(clippy::too_many_arguments)]
pub fn place(id: u32, placement: u32, col: u16, row: u16, src: (u32, u32, u32, u32), cols: u16, rows: u16, z: i32) -> String {
    let (x, y, w, h) = src;
    format!("\x1b[{};{}H\x1b_Ga=p,i={id},p={placement},x={x},y={y},w={w},h={h},c={cols},r={rows},z={z},C=1,q=2\x1b\\", row as u32 + 1, col as u32 + 1)
}

/// Remove placement `placement` of image `id` (the image stays loaded).
pub fn unplace_one(id: u32, placement: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={id},p={placement},q=2\x1b\\")
}

/// Remove every placement on the screen (the images stay loaded).
pub const CLEAR_PLACEMENTS: &str = "\x1b_Ga=d,d=a,q=2\x1b\\";

/// Free image `id` from the terminal's memory.
pub fn free(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// Free every image this program sent.
pub const FREE_ALL: &str = "\x1b_Ga=d,d=A,q=2\x1b\\";

/// Show RGBA pixels (`w` x `h`, 4 bytes each) as image `id`, placed at cell (`col`, `row`)
/// and stretched over `cols` x `rows` cells at layer `z` (negative: under the text). With
/// `shm` (the terminal is on this machine) the pixels go through POSIX shared memory, which
/// the terminal reads and unlinks, so nothing big crosses the terminal stream; otherwise
/// they go inline, zlib-compressed. Replaces an earlier image with the same id.
#[allow(clippy::too_many_arguments)]
pub fn show_rgba(id: u32, w: u32, h: u32, rgba: &[u8], col: u16, row: u16, cols: u16, rows: u16, z: i32, shm: bool) -> String {
    let mut out = format!("\x1b[{};{}H", row + 1, col + 1);
    let keys = format!("a=T,f=32,s={w},v={h},i={id},p=1,c={cols},r={rows},C=1,z={z},q=2");
    #[cfg(unix)]
    if shm && let Some(name) = shm_write(rgba) {
        let _ = write!(out, "\x1b_G{keys},t=s,S={};{}\x1b\\", rgba.len(), base64(name.as_bytes()));
        return out;
    }
    let _ = shm;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = std::io::Write::write_all(&mut z, rgba);
    let data = base64(&z.finish().unwrap_or_default());
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
    for (i, c) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        let c = std::str::from_utf8(c).unwrap_or("");
        if i == 0 {
            let _ = write!(out, "\x1b_G{keys},o=z,m={more};{c}\x1b\\");
        } else {
            let _ = write!(out, "\x1b_Gm={more};{c}\x1b\\");
        }
    }
    out
}

/// Put bytes in a new POSIX shared memory object; its name, or None if that failed.
#[cfg(unix)]
fn shm_write(data: &[u8]) -> Option<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let name = format!("/mapterm-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
    let cname = std::ffi::CString::new(name.clone()).ok()?;
    // SAFETY: plain POSIX calls on a fresh object this process created; the mapping is
    // exactly `data.len()` bytes and unmapped before returning.
    unsafe {
        let fd = libc::shm_open(cname.as_ptr(), libc::O_CREAT | libc::O_RDWR | libc::O_EXCL, 0o600);
        if fd < 0 {
            return None;
        }
        let ok = libc::ftruncate(fd, data.len() as libc::off_t) == 0 && {
            let p = libc::mmap(std::ptr::null_mut(), data.len(), libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            if p == libc::MAP_FAILED {
                false
            } else {
                std::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
                libc::munmap(p, data.len());
                true
            }
        };
        libc::close(fd);
        if !ok {
            libc::shm_unlink(cname.as_ptr());
            return None;
        }
    }
    Some(name)
}

/// Send RGBA pixels as image `id` without showing it (see [`show_rgba`] for `shm`).
pub fn transmit_rgba(id: u32, w: u32, h: u32, rgba: &[u8], shm: bool) -> String {
    let mut out = String::new();
    let keys = format!("a=t,f=32,s={w},v={h},i={id},q=2");
    #[cfg(unix)]
    if shm && let Some(name) = shm_write(rgba) {
        let _ = write!(out, "\x1b_G{keys},t=s,S={};{}\x1b\\", rgba.len(), base64(name.as_bytes()));
        return out;
    }
    let _ = shm;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = std::io::Write::write_all(&mut z, rgba);
    let data = base64(&z.finish().unwrap_or_default());
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
    for (i, c) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        let c = std::str::from_utf8(c).unwrap_or("");
        if i == 0 {
            let _ = write!(out, "\x1b_G{keys},o=z,m={more};{c}\x1b\\");
        } else {
            let _ = write!(out, "\x1b_Gm={more};{c}\x1b\\");
        }
    }
    out
}

/// Place image `id` (its placement 1, replacing an earlier one) at cell (`col`, `row`)
/// plus a pixel offset inside it, showing `src` (x, y, w, h) one image pixel per screen
/// pixel, at layer `z`.
#[allow(clippy::too_many_arguments)]
pub fn place_px(id: u32, col: u16, row: u16, x_off: u32, y_off: u32, src: (u32, u32, u32, u32), z: i32) -> String {
    let (x, y, w, h) = src;
    format!("\x1b[{};{}H\x1b_Ga=p,i={id},p=1,x={x},y={y},w={w},h={h},X={x_off},Y={y_off},C=1,z={z},q=2\x1b\\", row + 1, col + 1)
}

/// Remove image `id`'s placements, keeping its data for later.
pub fn unplace(id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={id},q=2\x1b\\")
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe_reply_waits_for_device_attributes() {
        assert_eq!(super::parse_probe_reply(b"\x1b_Gi=4000000001;OK\x1b\\"), None);
        assert_eq!(super::parse_probe_reply(b"\x1b_Gi=4000000001;OK\x1b\\\x1b[?62;22"), None);
    }

    #[test]
    fn probe_reply_reads_the_graphics_answer() {
        assert_eq!(super::parse_probe_reply(b"\x1b_Gi=4000000001;OK\x1b\\\x1b[?62;22c"), Some(Some(true)));
        assert_eq!(super::parse_probe_reply(b"\x1b_Gi=4000000001;EBADF:no shm\x1b\\\x1b[?62;22c"), Some(Some(false)));
        // No graphics reply before the Device Attributes reply: the terminal didn't say.
        assert_eq!(super::parse_probe_reply(b"\x1b[?62;22c"), Some(None));
        // A typed key before the replies doesn't confuse it.
        assert_eq!(super::parse_probe_reply(b"j\x1b_Gi=4000000001;OK\x1b\\\x1b[?1;2c"), Some(Some(true)));
    }

    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[cfg(unix)]
    #[test]
    fn transmit_shm_names_a_readable_object() {
        let png = b"\x89PNG fake tile bytes".to_vec();
        let cmd = transmit_shm(9, &png).expect("shm_open works here");
        let prefix = format!("\x1b_Ga=t,f=100,t=s,S={},i=9,q=2;", png.len());
        assert!(cmd.starts_with(&prefix) && cmd.ends_with("\x1b\\"));
        let b64 = &cmd[prefix.len()..cmd.len() - 2];
        const B64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bits: Vec<u8> = b64.bytes().filter(|&c| c != b'=').map(|c| B64.find(c as char).unwrap() as u8).collect();
        let name: Vec<u8> = bits
            .chunks(4)
            .flat_map(|q| {
                let n = q.iter().enumerate().fold(0u32, |n, (i, &v)| n | (v as u32) << (18 - 6 * i));
                [(n >> 16) as u8, (n >> 8) as u8, n as u8].into_iter().take(q.len() - 1)
            })
            .collect();
        let name = String::from_utf8(name).unwrap();
        assert!(name.starts_with("/mapterm-") && name.len() <= 31, "{name}");
        // Read it back as the terminal would, then unlink it as the terminal does.
        let c = std::ffi::CString::new(name).unwrap();
        unsafe {
            let fd = libc::shm_open(c.as_ptr(), libc::O_RDONLY, 0);
            assert!(fd >= 0);
            let p = libc::mmap(std::ptr::null_mut(), png.len(), libc::PROT_READ, libc::MAP_SHARED, fd, 0);
            assert_ne!(p, libc::MAP_FAILED);
            assert_eq!(std::slice::from_raw_parts(p.cast::<u8>(), png.len()), &png[..]);
            libc::munmap(p, png.len());
            libc::close(fd);
            assert_eq!(libc::shm_unlink(c.as_ptr()), 0);
        }
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
