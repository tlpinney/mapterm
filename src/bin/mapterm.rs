//! mapterm: a terminal map viewer on top of termtiles.

use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style as TStyle},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use std::io::Write;

use termtiles::{
    Camera, MapWidget, Source, Style, Theme, TileId, TileStore, Viewport, prepare, smooth, source::OPENFREEMAP,
    store::Prepared,
};
use tokio::sync::{Semaphore, mpsc};

const USAGE: &str = "mapterm -- vector map tiles in the terminal

usage: mapterm [SOURCE] [--at LAT,LON] [-z ZOOM] [--light] [--smooth | --smooth-shift] [--snapshot COLSxROWS]

  SOURCE        .pmtiles file or URL, XYZ template with {z}/{x}/{y}, or TileJSON URL
                (default: OpenFreeMap planet)
  --at          start position (default: the source's center)
  -z            start zoom
  --light       light theme
  --smooth      pixel smooth trackpad panning on terminals with the smooth-scroll region
                protocol (tlpinney/ghostty); also MAPTERM_SMOOTH=1. The trackpad then pans
                up/down instead of zooming; zoom with +/-, double-click, right-click
  --smooth-shift  --smooth plus pixel-exact dragging: the terminal shifts the map by the
                sub-cell remainder (needs smooth protocol v3); also MAPTERM_SMOOTH=shift
  --snapshot    render one frame to stdout as text and exit

keys: arrows/hjkl pan · +/- zoom · wheel zooms at cursor · swipe sideways/drag/shift+wheel pan ·
      double-click zoom in · right-click zoom out · s smooth on/off · n labels · t theme · q quit";

struct Args {
    source: String,
    at: Option<(f64, f64)>,
    zoom: Option<f64>,
    light: bool,
    snapshot: Option<(u16, u16)>,
    smooth: bool,
    /// Also send the sub-cell remainder for the terminal to shift by (protocol v3).
    shift: bool,
}

fn args() -> Result<Args> {
    let mut a = Args { source: OPENFREEMAP.into(), at: None, zoom: None, light: false, snapshot: None, smooth: smooth::env_enabled("MAPTERM_SMOOTH"), shift: std::env::var("MAPTERM_SMOOTH").is_ok_and(|v| v == "shift") };
    a.smooth |= a.shift;
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--at" => {
                let v = it.next().context("--at LAT,LON")?;
                let (la, lo) = v.split_once(',').context("--at LAT,LON")?;
                a.at = Some((lo.trim().parse()?, la.trim().parse()?));
            }
            "-z" | "--zoom" => a.zoom = Some(it.next().context("-z ZOOM")?.parse()?),
            "--light" => a.light = true,
            "--smooth" => a.smooth = true,
            "--smooth-shift" => {
                a.smooth = true;
                a.shift = true;
            }
            "--snapshot" => {
                let v = it.next().context("--snapshot COLSxROWS")?;
                let (c, r) = v.split_once('x').context("--snapshot COLSxROWS")?;
                a.snapshot = Some((c.parse()?, r.parse()?));
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            s if s.starts_with('-') => bail!("unknown option {s}\n\n{USAGE}"),
            s => a.source = s.into(),
        }
    }
    Ok(a)
}

type Done = (TileId, Result<Prepared>);

/// Cell size in pixels, when the terminal reports its pixel size (needed for SGR-pixels mouse).
fn cell_px() -> Option<(f64, f64)> {
    let ws = crossterm::terminal::window_size().ok()?;
    (ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0)
        .then(|| (ws.width as f64 / ws.columns as f64, ws.height as f64 / ws.rows as f64))
}

/// Turn SGR-pixels mouse reporting (DEC 1016) on or off. While on, mouse positions arrive in
/// pixels in the column/row fields.
fn set_pixel_mouse(on: bool) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(if on { b"\x1b[?1016h" } else { b"\x1b[?1016l" })?;
    out.flush()
}

fn fetch(src: &Arc<Source>, sem: &Arc<Semaphore>, style: Style, id: TileId, tx: mpsc::UnboundedSender<Done>) {
    let (src, sem) = (src.clone(), sem.clone());
    tokio::spawn(async move {
        let _permit = sem.acquire_owned().await;
        let res = match src.tile(id.0, id.1, id.2).await {
            Ok(Some(bytes)) => tokio::task::spawn_blocking(move || prepare(&bytes, &style, id.0)).await.map_err(Into::into).and_then(|r| r),
            Ok(None) => Ok(Prepared::default()),
            Err(e) => Err(e),
        };
        let _ = tx.send((id, res));
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = args()?;
    let src = Arc::new(Source::open(&a.source).await?);
    let (lon, lat, z) = match (a.at, src.center) {
        (Some((lo, la)), _) => (lo, la, a.zoom.unwrap_or(12.0)),
        (None, Some((lo, la, z))) => (lo, la, a.zoom.unwrap_or(z)),
        (None, None) => (0.0, 20.0, a.zoom.unwrap_or(2.0)),
    };
    let mut cam = Camera::new(lon, lat, z);
    let mut style = Style { theme: if a.light { Theme::Light } else { Theme::Dark } };
    let sem = Arc::new(Semaphore::new(6));
    let (tx, mut rx) = mpsc::unbounded_channel::<Done>();
    let mut store = TileStore::default();

    if let Some((w, h)) = a.snapshot {
        let area = Rect::new(0, 0, w, h);
        let vp = Viewport::new(&cam, area);
        let ids = vp.tiles(cam.tile_zoom(src.min_zoom, src.max_zoom));
        for id in &ids {
            fetch(&src, &sem, style, *id, tx.clone());
        }
        for _ in 0..ids.len() {
            let (id, r) = rx.recv().await.context("fetch channel closed")?;
            match r {
                Ok(t) => store.insert(id, t),
                Err(e) => eprintln!("tile {id:?}: {e:#}"),
            }
        }
        let mut buf = Buffer::empty(area);
        MapWidget { store: &store, camera: &cam, min_zoom: src.min_zoom, max_zoom: src.max_zoom, overlays: &[], labels: true }
            .render(area, &mut buf);
        for y in 0..h {
            let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
            println!("{}", line.trim_end());
        }
        return Ok(());
    }

    let mut term = ratatui::init();
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    let (ktx, mut krx) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if ktx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut pending: HashSet<TileId> = HashSet::new();
    let mut errors = 0usize;
    let mut labels = true;
    let mut smooth_on = a.smooth;
    let shift = a.shift;
    // Last drag position (cells, or pixels in pixel-mouse mode) and the sub-dot remainder.
    let mut drag: Option<(u16, u16)> = None;
    let mut drag_rem = (0.0f64, 0.0f64);
    // Pixel mouse is used in smooth mode when the terminal reports its pixel size.
    let mut px: Option<(f64, f64)> = None;
    if smooth_on {
        px = cell_px();
        if px.is_some() {
            set_pixel_mouse(true)?;
        }
    }
    let mut last_click: Option<(Instant, u16, u16)> = None;
    let mut map_area = Rect::default();
    let mut frame_ms = 0.0f64;
    let result: Result<()> = async {
        loop {
            if smooth_on {
                crossterm::execute!(std::io::stdout(), crossterm::terminal::BeginSynchronizedUpdate)?;
            }
            let t0 = Instant::now();
            // With shift, render on the cell grid and let the terminal draw the sub-cell rest.
            let (draw_cam, ox, oy) = if smooth_on && shift { smooth::snap(&cam) } else { (cam, 0, 0) };
            term.draw(|f| {
                let [m, s] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
                map_area = m;
                f.render_widget(
                    MapWidget { store: &store, camera: &draw_cam, min_zoom: src.min_zoom, max_zoom: src.max_zoom, overlays: &[], labels },
                    m,
                );
                let vp = Viewport::new(&cam, m);
                let km = vp.km_per_col(cam.lat) * 10.0;
                let dim = TStyle::new().fg(Color::DarkGray);
                let mut spans = vec![Span::raw(format!(
                    " z{:.1}  {:.4},{:.4}  10 cols ≈ {} ",
                    cam.zoom,
                    cam.lat,
                    cam.lon,
                    if km < 1.0 { format!("{:.0} m", km * 1000.0) } else { format!("{km:.1} km") }
                ))];
                if !pending.is_empty() {
                    spans.push(Span::styled(format!("loading {} ", pending.len()), TStyle::new().fg(Color::Yellow)));
                }
                if errors > 0 {
                    spans.push(Span::styled(format!("{errors} failed "), TStyle::new().fg(Color::Red)));
                }
                if smooth_on {
                    let tag = match (shift, px.is_some()) {
                        (true, true) => "smooth·shift·px ",
                        (true, false) => "smooth·shift ",
                        (false, true) => "smooth·px ",
                        (false, false) => "smooth ",
                    };
                    spans.push(Span::styled(tag, TStyle::new().fg(Color::Cyan)));
                }
                spans.push(Span::styled(format!("{frame_ms:.0} ms "), dim));
                spans.push(Span::styled(format!("· {} · © OpenStreetMap · ? keys in --help · q quit", src.name), dim));
                f.render_widget(Paragraph::new(Line::from(spans)), s);
            })?;
            frame_ms = t0.elapsed().as_secs_f64() * 1000.0;
            if smooth_on {
                // Declare after drawing, in the same synchronized update as the cells.
                let mut out = std::io::stdout();
                let decl = if shift {
                    smooth::declare_shift(0, map_area, &draw_cam, ox, oy)
                } else {
                    smooth::declare(0, map_area, &cam)
                };
                out.write_all(decl.as_bytes())?;
                crossterm::execute!(out, crossterm::terminal::EndSynchronizedUpdate)?;
            }

            // Fetch what the view needs that we don't have yet.
            let vp = Viewport::new(&cam, map_area);
            for id in vp.tiles(cam.tile_zoom(src.min_zoom, src.max_zoom)) {
                if !store.contains(&id) && pending.insert(id) {
                    fetch(&src, &sem, style, id, tx.clone());
                }
            }

            tokio::select! {
                Some(done) = rx.recv() => {
                    let mut batch = vec![done];
                    while let Ok(d) = rx.try_recv() {
                        batch.push(d);
                    }
                    for (id, r) in batch {
                        pending.remove(&id);
                        match r {
                            Ok(t) => store.insert(id, t),
                            Err(_) => errors += 1,
                        }
                    }
                }
                Some(first) = krx.recv() => {
                    let step = 6.0;
                    let max = src.max_zoom as f64 + 3.0;
                    // Handle everything already queued before drawing again, so a fast drag or
                    // flick catches up in one frame instead of trailing the pointer.
                    let mut queued = vec![first];
                    while let Ok(ev) = krx.try_recv() {
                        queued.push(ev);
                    }
                    for ev in queued {
                    match ev {
                        Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                            KeyCode::Left | KeyCode::Char('h') => cam.pan_cells(-step * 2.0, 0.0),
                            KeyCode::Right | KeyCode::Char('l') => cam.pan_cells(step * 2.0, 0.0),
                            KeyCode::Up | KeyCode::Char('k') => cam.pan_cells(0.0, -step),
                            KeyCode::Down | KeyCode::Char('j') => cam.pan_cells(0.0, step),
                            KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('a') => cam.zoom_by(0.5, max),
                            KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Char('z') => cam.zoom_by(-0.5, max),
                            KeyCode::Char('n') => labels = !labels,
                            KeyCode::Char('s') => {
                                smooth_on = !smooth_on;
                                px = if smooth_on { cell_px() } else { None };
                                set_pixel_mouse(px.is_some())?;
                                if !smooth_on {
                                    let mut out = std::io::stdout();
                                    out.write_all(smooth::SMOOTH_OFF.as_bytes())?;
                                    out.flush()?;
                                }
                            }
                            KeyCode::Char('t') => {
                                style.theme = if style.theme == Theme::Dark { Theme::Light } else { Theme::Dark };
                                store.clear();
                                pending.clear();
                            }
                            _ => {}
                        },
                        Event::Resize(..) if px.is_some() => px = cell_px(),
                        Event::Mouse(raw) => {
                            // In pixel-mouse mode positions are pixels; keep them for dragging and
                            // use the cell for everything else.
                            let pix = (raw.column, raw.row);
                            let mut m = raw;
                            if let Some((cw, ch)) = px {
                                m.column = (raw.column as f64 / cw) as u16;
                                m.row = (raw.row as f64 / ch) as u16;
                            }
                            let inside = map_area.contains(ratatui::layout::Position::new(m.column, m.row));
                            let shift = m.modifiers.contains(KeyModifiers::SHIFT);
                            match m.kind {
                                // Smooth mode: the terminal sends one wheel event per row of trackpad
                                // travel and shifts the map by the remainder, so pan exactly one row.
                                MouseEventKind::ScrollUp if smooth_on && inside => cam.pan_rows(-1),
                                MouseEventKind::ScrollDown if smooth_on && inside => cam.pan_rows(1),
                                // Web-map style: the wheel zooms toward the cursor, shift+wheel pans.
                                MouseEventKind::ScrollUp if shift => cam.pan_cells(0.0, -2.0),
                                MouseEventKind::ScrollDown if shift => cam.pan_cells(0.0, 2.0),
                                MouseEventKind::ScrollUp if inside => cam.zoom_at(0.25, max, map_area, m.column, m.row),
                                MouseEventKind::ScrollDown if inside => cam.zoom_at(-0.25, max, map_area, m.column, m.row),
                                MouseEventKind::ScrollLeft => cam.pan_cells(-4.0, 0.0),
                                MouseEventKind::ScrollRight => cam.pan_cells(4.0, 0.0),
                                MouseEventKind::Down(MouseButton::Left) if inside => {
                                    let now = Instant::now();
                                    let double = last_click.is_some_and(|(t, c, r)| {
                                        now.duration_since(t) < Duration::from_millis(400)
                                            && c.abs_diff(m.column) <= 1
                                            && r.abs_diff(m.row) <= 1
                                    });
                                    if double {
                                        cam.zoom_at(1.0, max, map_area, m.column, m.row);
                                        last_click = None;
                                    } else {
                                        last_click = Some((now, m.column, m.row));
                                    }
                                    drag = Some(if px.is_some() { pix } else { (m.column, m.row) });
                                    drag_rem = (0.0, 0.0);
                                }
                                MouseEventKind::Down(MouseButton::Right) if inside => {
                                    cam.zoom_at(-1.0, max, map_area, m.column, m.row)
                                }
                                MouseEventKind::Drag(MouseButton::Left) => {
                                    if let Some((c, r)) = drag {
                                        let cur = if px.is_some() { pix } else { (m.column, m.row) };
                                        // Pointer travel in braille dots (2 per column, 4 per row).
                                        let (dpx, dpy) = (c as f64 - cur.0 as f64, r as f64 - cur.1 as f64);
                                        let (dx, dy) = match px {
                                            Some((cw, ch)) => (dpx * 2.0 / cw, dpy * 4.0 / ch),
                                            None => (dpx * 2.0, dpy * 4.0),
                                        };
                                        // Move in whole dots so every feature shifts by the same
                                        // amount; carry the remainder to the next event.
                                        drag_rem = (drag_rem.0 + dx, drag_rem.1 + dy);
                                        let whole = (drag_rem.0.trunc(), drag_rem.1.trunc());
                                        drag_rem = (drag_rem.0 - whole.0, drag_rem.1 - whole.1);
                                        if smooth_on && shift {
                                            // The terminal shifts by the sub-cell rest, so follow
                                            // the pointer exactly.
                                            cam.pan_dots(dx, dy);
                                            drag_rem = (0.0, 0.0);
                                        } else {
                                            cam.pan_dots(whole.0, whole.1);
                                        }
                                        drag = Some(cur);
                                        last_click = None;
                                    }
                                }
                                MouseEventKind::Up(_) => drag = None,
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }
    .await;
    if smooth_on {
        let _ = std::io::stdout().write_all(smooth::SMOOTH_OFF.as_bytes());
    }
    let _ = set_pixel_mouse(false);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}
