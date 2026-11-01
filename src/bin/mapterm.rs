//! mapterm: the terminal map viewer built on the mapterm library.

use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style as TStyle},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget},
};
use std::io::Write;

use mapterm::{
    Controller,
    Camera, MapWidget, Overlay, Source, Style, Theme, TileId, TileStore, Viewport, prepare, smooth, source::OPENFREEMAP,
    store::Prepared,
};
use tokio::sync::{Semaphore, mpsc};

const USAGE: &str = "mapterm -- vector map tiles in the terminal

usage: mapterm [SOURCE] [--raster NAME|URL] [--at LAT,LON] [-z ZOOM] [--light] [--snapshot COLSxROWS [--pick COL,ROW]]

  SOURCE        .pmtiles file or URL, XYZ template with {z}/{x}/{y}, or TileJSON URL
                (default: OpenFreeMap planet)
  --at          start position (default: the source's center)
  -z            start zoom
  --raster      an image tile layer under the vector map: a provider name (--raster-list),
                a URL with {z}/{x}/{y} ({-y} or --tms for TMS rows, {key} for an API key),
                or a .pmtiles file or URL of PNG/JPEG/WebP tiles;
                real images with kitty graphics (Ghostty, kitty, WezTerm, herdr), cell colors
                elsewhere (MAPTERM_GRAPHICS=kitty|cells overrides)
  --raster-list           show the provider names and the keys they need
  --raster-key-cmd CMD    print the API key with a command, e.g. 'pass show vantor/api-key'
                          (otherwise the provider's variable, or MAPTERM_RASTER_KEY for URLs)
  --raster-header 'N: V'  extra request header; {key} in the value is the API key
  --raster-param N=V      fill another {N} in the URL, e.g. mosaic=NAME for Planet
  --date YYYY-MM-DD       the day to show for dated layers (gibs; default yesterday, UTC)
  --raster-list-mosaics   list the Planet mosaics your key can use, then exit
  --raster-cache / --raster-no-cache
                          keep tiles on disk or not (default: on, off for keyed providers)
  --raster-attribution T  credit for the layer (known providers are filled in)
  --tms                   count raster rows from the bottom (TMS) even with a plain {y}
  --light       light theme
  --snapshot    render one frame to stdout as text and exit
  --pick        with --snapshot: print the features under that cell as JSON lines instead

keys (MapLibre conventions): wheel zooms at cursor · drag pans (with inertia) ·
      double-click zooms in, shift+double-click out · shift+drag box zoom · arrows/hjkl pan ·
      +/- zoom · sideways swipe pans · i inspect · v vector overlay · n labels · t theme · q quit

inspect mode (i): hover shows what's under the pointer; click lists every feature there
      (drawn or not) with its properties, size and location; click an entry or tab/shift+tab
      to switch; y copies it as JSON; esc closes, i leaves inspect mode

experimental: MAPTERM_SMOOTH=shift (pixel-smooth dragging) or MAPTERM_SMOOTH=1 (smooth
      up/down panning) on the Ghostty Smooth terminal fork; `s` then toggles it.
      See https://github.com/tlpinney/ghostty (branch smooth).";

struct Args {
    source: String,
    at: Option<(f64, f64)>,
    zoom: Option<f64>,
    light: bool,
    snapshot: Option<(u16, u16)>,
    pick: Option<(u16, u16)>,
    raster: Option<String>,
    tms: bool,
    raster_attribution: Option<String>,
    raster_key_cmd: Option<String>,
    raster_headers: Vec<String>,
    raster_cache: Option<bool>,
    raster_params: Vec<(String, String)>,
    list_mosaics: bool,
    /// Experimental smooth scrolling, from MAPTERM_SMOOTH only (no flag): `1` pans the wheel
    /// smoothly, `shift` makes dragging pixel-exact (smooth protocol v3).
    smooth: bool,
    shift: bool,
}

fn args() -> Result<Args> {
    let (smooth, shift) = match std::env::var("MAPTERM_SMOOTH").unwrap_or_default().to_ascii_lowercase().as_str() {
        "" | "0" => (false, false),
        "1" => (true, false),
        "shift" => (true, true),
        other => bail!("MAPTERM_SMOOTH={other}: use `shift` (pixel-smooth dragging) or `1` (smooth up/down panning)"),
    };
    let mut a = Args { source: OPENFREEMAP.into(), at: None, zoom: None, light: false, snapshot: None, pick: None, raster: None, tms: false, raster_attribution: None, raster_key_cmd: None, raster_headers: Vec::new(), raster_cache: None, raster_params: Vec::new(), list_mosaics: false, smooth, shift };
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
            "--raster" => a.raster = Some(it.next().context("--raster URL")?),
            "--tms" => a.tms = true,
            "--raster-key-cmd" => a.raster_key_cmd = Some(it.next().context("--raster-key-cmd CMD")?),
            "--raster-header" => a.raster_headers.push(it.next().context("--raster-header 'Name: value'")?),
            "--raster-param" => {
                let v = it.next().context("--raster-param NAME=VALUE")?;
                let (n, val) = v.split_once('=').context("--raster-param NAME=VALUE")?;
                a.raster_params.push((n.trim().to_string(), val.trim().to_string()));
            }
            "--raster-list-mosaics" => a.list_mosaics = true,
            "--date" => a.raster_params.push(("date".into(), it.next().context("--date YYYY-MM-DD")?)),
            "--raster-cache" => a.raster_cache = Some(true),
            "--raster-no-cache" => a.raster_cache = Some(false),
            "--raster-list" => {
                println!("raster providers (--raster NAME):\n");
                for p in mapterm::raster::PRESETS {
                    let key = if p.key_env.is_empty() { "no key".to_string() } else { format!("key: {}", p.key_env.join(" or ")) };
                    println!("  {:<11} {}\n              {key}", p.name, p.about);
                }
                println!("\nKeys can also come from --raster-key-cmd, e.g. 'pass show vantor/api-key'.");
                std::process::exit(0);
            }
            "--raster-attribution" => a.raster_attribution = Some(it.next().context("--raster-attribution TEXT")?),
            "--snapshot" => {
                let v = it.next().context("--snapshot COLSxROWS")?;
                let (c, r) = v.split_once('x').context("--snapshot COLSxROWS")?;
                a.snapshot = Some((c.parse()?, r.parse()?));
            }
            "--pick" => {
                let v = it.next().context("--pick COL,ROW")?;
                let (c, r) = v.split_once(',').context("--pick COL,ROW")?;
                a.pick = Some((c.trim().parse()?, r.trim().parse()?));
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

/// Debug log for troubleshooting (MAPTERM_LOG=path); a no-op unless set.
fn dlog(msg: impl FnOnce() -> String) {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    if let Some(path) = PATH.get_or_init(|| std::env::var("MAPTERM_LOG").ok()).as_deref()
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "{}", msg());
    }
}

/// Print the Planet basemap mosaics the key can use (name, dates), following pagination.
/// The key goes in a Basic auth header (Planet's API accepts the key as the user name).
async fn list_planet_mosaics(key_cmd: Option<&str>) -> Result<()> {
    let key = mapterm::raster::find_key(key_cmd, &["PL_API_KEY", "PLANET_API_KEY"])?
        .context("Planet needs a key: set PL_API_KEY or pass --raster-key-cmd")?;
    let auth = format!("Basic {}", mapterm::kitty::base64(format!("{key}:").as_bytes()));
    let http = mapterm::source::http_client();
    let mut url = "https://api.planet.com/basemaps/v1/mosaics".to_string();
    let mut count = 0;
    for _ in 0..20 {
        let resp = http.get(&url).header("Authorization", &auth).send().await?;
        if !resp.status().is_success() {
            bail!("Planet mosaics: HTTP {}", resp.status());
        }
        let v: serde_json::Value = serde_json::from_slice(&resp.bytes().await?)?;
        for m in v["mosaics"].as_array().into_iter().flatten() {
            let s = |k: &str| m[k].as_str().unwrap_or("").get(..10).unwrap_or("").to_string();
            println!("{:<48} {} .. {}", m["name"].as_str().unwrap_or("?"), s("first_acquired"), s("last_acquired"));
            count += 1;
        }
        match v["_links"]["_next"].as_str() {
            Some(next) if !next.is_empty() => url = next.to_string(),
            _ => break,
        }
    }
    eprintln!("{count} mosaics; use one with --raster planet --raster-param mosaic=NAME");
    Ok(())
}
type RasterDone = (TileId, Result<Option<mapterm::raster::RasterTile>>);

fn fetch_raster(src: &Arc<mapterm::raster::RasterSource>, sem: &Arc<Semaphore>, id: TileId, tx: mpsc::UnboundedSender<RasterDone>) {
    let (src, sem) = (src.clone(), sem.clone());
    tokio::spawn(async move {
        let _permit = sem.acquire_owned().await;
        let res = match src.tile(id.0, id.1, id.2).await {
            Ok(Some(bytes)) => tokio::task::spawn_blocking(move || mapterm::raster::decode(&bytes).map(Some))
                .await
                .map_err(Into::into)
                .and_then(|r| r),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        };
        let _ = tx.send((id, res));
    });
}

/// Copy text to the system clipboard with OSC 52 (Ghostty and most modern terminals; herdr
/// relays it).
fn osc52_copy(text: &str) -> std::io::Result<()> {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut enc = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            enc.push(if i <= chunk.len() { B64[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{enc}\x07")?;
    out.flush()
}

/// Set the mouse pointer shape (OSC 22: "crosshair", "default", ...). Terminals that don't
/// support it ignore the sequence.
fn set_pointer(shape: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    write!(out, "\x1b]22;{shape}\x1b\\")?;
    out.flush()
}

/// Translated names (name:de, name_fr, ...) are collapsed in the panel; the JSON keeps them.
fn is_translation(key: &str) -> bool {
    (key.starts_with("name:") || key.starts_with("name_")) && key != "name_en" && key != "name:en"
}

fn fmt_area(km2: f64) -> String {
    if km2 < 0.01 { format!("{:.0} m²", km2 * 1e6) } else { format!("{km2:.3} km²") }
}

fn fmt_len(km: f64) -> String {
    if km < 1.0 { format!("{:.0} m", km * 1000.0) } else { format!("{km:.2} km") }
}

/// The inspector panel: the features under the clicked spot, then the selected one's details.
/// Returns the screen rows of the list entries, so clicks can select them.
fn draw_inspector(f: &mut ratatui::Frame, area: Rect, picks: &[mapterm::Pick], sel: usize) -> Vec<(u16, usize)> {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let title = format!(" inspect · {} of {} ", sel + 1, picks.len());
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(title);
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let w = inner.width.saturating_sub(1) as usize;
    let cut = |s: &str, n: usize| if s.chars().count() > n { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) } else { s.to_string() };

    // List of features under the spot.
    const MAX_LIST: usize = 8;
    let mut rows = Vec::new();
    let mut list: Vec<Line> = Vec::new();
    for (i, p) in picks.iter().enumerate().take(MAX_LIST) {
        let marker = match p.geom {
            mapterm::mvt::GeomType::Point => "•",
            mapterm::mvt::GeomType::Line => "─",
            _ => "▧",
        };
        let kind = p.kind();
        let name = cut(&p.title(), w.saturating_sub(kind.chars().count() + 4).max(8));
        let mut st = TStyle::new();
        if i == sel {
            st = st.add_modifier(Modifier::REVERSED);
        }
        list.push(Line::from(vec![Span::styled(format!(" {marker} {name}"), st), Span::styled(format!("  {kind}"), dim)]));
        rows.push((inner.y + i as u16, i));
    }
    if picks.len() > MAX_LIST {
        list.push(Line::from(Span::styled(format!("   … {} more (tab)", picks.len() - MAX_LIST), dim)));
    }
    let list_h = list.len() as u16;
    f.render_widget(Paragraph::new(list), Rect { height: list_h.min(inner.height), ..inner });

    // Details of the selected feature.
    let Some(p) = picks.get(sel) else { return rows };
    let key = |k: &str| Span::styled(format!(" {k:<17}"), dim);
    let mut d: Vec<Line> = vec![Line::default(), Line::from(Span::styled(format!(" {}", p.title()), TStyle::new().add_modifier(Modifier::BOLD)))];
    d.push(Line::from(vec![key("layer"), Span::raw(p.layer.to_string())]));
    let geom = mapterm::pick::geom_name(p.geom);
    d.push(Line::from(vec![key("geometry"), Span::raw(if p.drawn { geom.to_string() } else { format!("{geom} (not drawn)") })]));
    d.push(Line::from(vec![key("center"), Span::raw(format!("{:.5}, {:.5}", p.stats.center.1, p.stats.center.0))]));
    if let Some(l) = p.stats.length_km {
        d.push(Line::from(vec![key("length"), Span::raw(fmt_len(l))]));
    }
    if let Some(a) = p.stats.area_km2 {
        d.push(Line::from(vec![key("area"), Span::raw(fmt_area(a))]));
    }
    d.push(Line::from(vec![key("vertices"), Span::raw(p.stats.vertices.to_string())]));
    d.push(Line::from(vec![key("tile"), Span::raw(format!("{}/{}/{}", p.tile.0, p.tile.1, p.tile.2))]));
    d.push(Line::default());
    d.push(Line::from(Span::styled(" properties", TStyle::new().add_modifier(Modifier::BOLD))));
    let mut translations = 0;
    for (k, v) in &p.props {
        if is_translation(k) {
            translations += 1;
            continue;
        }
        d.push(Line::from(vec![key(&cut(k, 17)), Span::raw(v.to_string())]));
    }
    if translations > 0 {
        d.push(Line::from(Span::styled(format!(" + {translations} translated names (in the JSON)"), dim)));
    }
    if p.props.is_empty() {
        d.push(Line::from(Span::styled(" (none)", dim)));
    }
    d.push(Line::default());
    d.push(Line::from(Span::styled(" tab next · y copy JSON · esc close", dim)));
    let rest = Rect { y: inner.y + list_h, height: inner.height.saturating_sub(list_h), ..inner };
    f.render_widget(Paragraph::new(d).wrap(Wrap { trim: false }), rest);
    rows
}

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
    if a.list_mosaics {
        return list_planet_mosaics(a.raster_key_cmd.as_deref()).await;
    }
    let src = Arc::new(Source::open(&a.source).await?);
    let (lon, lat, z) = match (a.at, src.center) {
        (Some((lo, la)), _) => (lo, la, a.zoom.unwrap_or(12.0)),
        (None, Some((lo, la, z))) => (lo, la, a.zoom.unwrap_or(z)),
        (None, None) => (0.0, 20.0, a.zoom.unwrap_or(2.0)),
    };
    let cam = Camera::new(lon, lat, z);
    let mut style = Style { theme: if a.light { Theme::Light } else { Theme::Dark } };
    let sem = Arc::new(Semaphore::new(6));
    let (tx, mut rx) = mpsc::unbounded_channel::<Done>();
    let mut store = TileStore::default();
    // Optional raster layer under the vector map.
    let raster = match &a.raster {
        Some(spec) => {
            let mut r = match mapterm::raster::preset(spec) {
                None if spec.ends_with(".pmtiles") => mapterm::raster::RasterSource::from_pmtiles(spec).await?,
                Some(p) => {
                    let key = mapterm::raster::find_key(a.raster_key_cmd.as_deref(), p.key_env)?;
                    mapterm::raster::RasterSource::from_preset(p, key)?
                }
                None => {
                    let mut r = mapterm::raster::RasterSource::new(spec, a.tms, 19).with_context(|| {
                        format!("--raster {spec}: not a provider name (see --raster-list) or a tile URL")
                    })?;
                    for h in &a.raster_headers {
                        r.add_header(h)?;
                    }
                    if r.wants_key() {
                        let key = mapterm::raster::find_key(a.raster_key_cmd.as_deref(), &["MAPTERM_RASTER_KEY"])?;
                        if key.is_none() {
                            bail!("the raster URL uses {{key}}: set MAPTERM_RASTER_KEY or pass --raster-key-cmd");
                        }
                        r.set_key(key);
                    }
                    r
                }
            };
            if mapterm::raster::preset(spec).is_some() {
                for h in &a.raster_headers {
                    r.add_header(h)?;
                }
            }
            for (n, v) in &a.raster_params {
                r.set_param(n, v);
            }
            let missing = r.missing_params();
            if !missing.is_empty() {
                let hint = if missing.iter().any(|m| m == "mosaic") {
                    " (Planet: --raster-param mosaic=NAME or PLANET_MOSAIC; list yours with --raster-list-mosaics)"
                } else {
                    ""
                };
                bail!("the raster URL needs {}: use --raster-param NAME=VALUE{hint}", missing.iter().map(|m| format!("{{{m}}}")).collect::<Vec<_>>().join(", "));
            }
            if let Some(c) = a.raster_cache {
                r.cache = c;
            }
            if let Some(t) = &a.raster_attribution {
                r.attribution = t.clone();
            }
            Some(Arc::new(r))
        }
        None => None,
    };
    let mut rstore = mapterm::raster::RasterStore::default();
    let mut rpending: HashSet<TileId> = HashSet::new();
    let (rtx, mut rrx) = mpsc::unbounded_channel::<RasterDone>();
    // Tile servers like OSM's ask for at most two connections.
    let rsem = Arc::new(Semaphore::new(2));

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
        if let Some((c, r)) = a.pick {
            for p in mapterm::pick(&store, &cam, src.min_zoom, src.max_zoom, area, c, r) {
                println!("{}", p.to_json());
            }
            return Ok(());
        }
        let mut buf = Buffer::empty(area);
        MapWidget { store: &store, camera: &cam, min_zoom: src.min_zoom, max_zoom: src.max_zoom, overlays: &[], fills: true, lines: true, labels: true }
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
    // Vector overlay: 0 = everything (no fills over a raster), 1 = labels only, 2 = off.
    let mut vector: u8 = 0;
    // Raster drawing: real images with kitty graphics, else cell background colors.
    let kitty = raster.is_some() && mapterm::kitty::supported();
    let mut sent: HashSet<u32> = HashSet::new();
    let mut cpx = cell_px().unwrap_or((9.0, 20.0));
    let mut smooth_on = a.smooth;
    // `s` only toggles smooth scrolling when MAPTERM_SMOOTH opted in to the experiment.
    let smooth_available = a.smooth;
    let shift_mode = a.shift;
    let mut ctl = Controller::new(cam, src.min_zoom as f64, src.max_zoom as f64 + 3.0);
    // Smooth mode: pixel mouse when the terminal reports its pixel size; in shift mode drags
    // move by exact fractions (the terminal draws the sub-cell rest), otherwise the wheel
    // pans rows for the v2 region.
    let apply_smooth = |ctl: &mut Controller, on: bool| -> std::io::Result<()> {
        ctl.pixel_mouse = if on { cell_px() } else { None };
        ctl.exact_drag = on && shift_mode;
        ctl.wheel_pans = on && !shift_mode;
        set_pixel_mouse(ctl.pixel_mouse.is_some())
    };
    if smooth_on {
        apply_smooth(&mut ctl, true)?;
    }
    // Inspect mode: features under the clicked spot, the selected one, and a hover preview.
    let mut inspect = false;
    let mut picks: Vec<mapterm::Pick> = Vec::new();
    let mut sel = 0usize;
    let mut click_at: Option<(f64, f64)> = None;
    let mut hover: Option<((i32, i32), String)> = None;
    // The pointer position while inspecting, in braille dots relative to the map (2 per column,
    // 4 per row), for the crosshair. Exact when the terminal reports pixels.
    let mut hover_dot: Option<(f64, f64)> = None;
    let mut list_rows: Vec<(u16, usize)> = Vec::new();
    let mut panel_area = Rect::default();
    let mut note: Option<(Instant, String)> = None;
    let mut last_cam = cam;
    let mut map_area = Rect::default();
    let mut frame_ms = 0.0f64;
    let result: Result<()> = async {
        loop {
            ctl.tick(Instant::now());
            // One synchronized update per frame when escape sequences follow the cells.
            let sync = smooth_on || raster.is_some();
            if sync {
                crossterm::execute!(std::io::stdout(), crossterm::terminal::BeginSynchronizedUpdate)?;
            }
            let t0 = Instant::now();
            let cam = ctl.cam;
            // With shift, render on the cell grid and let the terminal draw the sub-cell rest.
            let (draw_cam, ox, oy) = if smooth_on && shift_mode { smooth::snap(&cam) } else { (cam, 0, 0) };
            let box_rect = ctl.box_rect();
            last_cam = draw_cam;
            // Highlight the selected feature and the clicked spot.
            let mut overlays = Vec::new();
            if let Some(p) = picks.get(sel) {
                let hl = Color::Rgb(255, 215, 0);
                if p.geom == mapterm::mvt::GeomType::Point {
                    for &(lon, lat) in p.lonlat.iter().flatten() {
                        overlays.push(Overlay::Marker { lon, lat, glyph: "◎".into(), style: TStyle::new().fg(hl), label: None, priority: 100 });
                    }
                } else {
                    for part in &p.lonlat {
                        overlays.push(Overlay::Line { pts: part.clone(), color: hl });
                    }
                }
            }
            if let (true, Some((x, y))) = (inspect, hover_dot) {
                // A small crosshair drawn with braille dots, centered on the pointer.
                let vp = Viewport::new(&draw_cam, map_area);
                let (x, y) = (x.floor() + 0.5, y.floor() + 0.5);
                let cross = Color::Rgb(0, 235, 255);
                for ((ax, ay), (bx, by)) in [((x - 3.0, y), (x + 3.0, y)), ((x, y - 3.0), (x, y + 3.0))] {
                    overlays.push(Overlay::Line { pts: vec![vp.dot_to_lonlat(ax, ay), vp.dot_to_lonlat(bx, by)], color: cross });
                }
            }
            if let (true, Some((lon, lat))) = (inspect && !picks.is_empty(), click_at) {
                overlays.push(Overlay::Marker { lon, lat, glyph: "+".into(), style: TStyle::new().fg(Color::Rgb(255, 215, 0)), label: None, priority: 99 });
            }
            // Raster zoom: about 2 screen pixels per source pixel as images (how browsers show
            // 256 px tiles on Retina screens, and half the tiles of 1:1), one cell per
            // source pixel as cell colors.
            let rz = raster.as_ref().map(|r| {
                let dots_per_px = if kitty { 2.0 / (cpx.0 / 2.0) } else { 2.0 };
                mapterm::raster::raster_zoom(&draw_cam, r.tile_px, dots_per_px, r.max_zoom)
            });
            term.draw(|f| {
                let [main, s] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
                let panel_w = if inspect && !picks.is_empty() { 52.min(main.width / 2) } else { 0 };
                let [m, panel] = Layout::horizontal([Constraint::Min(1), Constraint::Length(panel_w)]).areas(main);
                map_area = m;
                panel_area = panel;
                if let (false, Some(rz)) = (kitty, rz) {
                    let blocks = mapterm::raster::layout(&rstore, &draw_cam, m, rz);
                    f.render_widget(mapterm::raster::RasterCells { blocks: &blocks }, m);
                }
                f.render_widget(
                    MapWidget {
                        store: &store,
                        camera: &draw_cam,
                        min_zoom: src.min_zoom,
                        max_zoom: src.max_zoom,
                        overlays: &overlays,
                        fills: raster.is_none() && vector == 0,
                        lines: vector == 0,
                        labels: labels && vector < 2,
                    },
                    m,
                );
                list_rows = if panel_w > 0 { draw_inspector(f, panel, &picks, sel) } else { Vec::new() };
                if let Some(r) = box_rect {
                    f.render_widget(Block::bordered().border_style(TStyle::new().fg(Color::Yellow)), r);
                }
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
                if inspect {
                    spans.push(Span::styled("inspect ", TStyle::new().fg(Color::Black).bg(Color::Rgb(255, 215, 0))));
                    spans.push(Span::raw(" "));
                    match &hover {
                        Some((_, h)) => spans.push(Span::styled(format!("{h} "), TStyle::new().fg(Color::Rgb(255, 215, 0)))),
                        None => spans.push(Span::styled("click a spot to inspect · i to leave ", dim)),
                    }
                }
                if let Some((t, n)) = &note
                    && t.elapsed() < Duration::from_secs(3)
                {
                    spans.push(Span::styled(format!("{n} "), TStyle::new().fg(Color::Green)));
                }
                if !pending.is_empty() || !rpending.is_empty() {
                    spans.push(Span::styled(format!("loading {} ", pending.len() + rpending.len()), TStyle::new().fg(Color::Yellow)));
                }
                if errors > 0 {
                    spans.push(Span::styled(format!("{errors} failed "), TStyle::new().fg(Color::Red)));
                }
                if smooth_on {
                    let tag = match (shift_mode, ctl.pixel_mouse.is_some()) {
                        (true, true) => "smooth·shift·px ",
                        (true, false) => "smooth·shift ",
                        (false, true) => "smooth·px ",
                        (false, false) => "smooth ",
                    };
                    spans.push(Span::styled(tag, TStyle::new().fg(Color::Cyan)));
                }
                if let Some(r) = &raster {
                    let mode = ["", "· labels ", "· raster only "][vector as usize];
                    let how = if kitty { "" } else { "(cells) " };
                    let credit = if r.attribution.is_empty() { r.name.clone() } else { format!("imagery {}", r.attribution) };
                    spans.push(Span::styled(format!("{credit} {how}{mode}"), TStyle::new().fg(Color::LightBlue)));
                }
                spans.push(Span::styled(format!("{frame_ms:.0} ms "), dim));
                spans.push(Span::styled(format!("· {} · © OpenStreetMap · q quit", src.name), dim));
                f.render_widget(Paragraph::new(Line::from(spans)), s);
            })?;
            ctl.set_area(map_area);
            frame_ms = t0.elapsed().as_secs_f64() * 1000.0;
            if sync {
                let mut out = std::io::stdout();
                if smooth_on {
                    // Declare after drawing, in the same synchronized update as the cells.
                    let decl = if shift_mode { smooth::declare_shift(0, map_area, ox, oy) } else { smooth::declare(0, map_area, &cam) };
                    out.write_all(decl.as_bytes())?;
                }
                dlog(|| format!("frame: kitty={kitty} rz={rz:?} cell_px={cpx:?} map={map_area:?} stored_pending={}", rpending.len()));
                if let (true, Some(rz)) = (kitty, rz) {
                    // Re-place the raster blocks under the text: images are sent once, then
                    // each frame only moves placements.
                    let mut cmds = String::from(mapterm::kitty::CLEAR_PLACEMENTS);
                    for id in rstore.take_evicted() {
                        if sent.remove(&id) {
                            cmds.push_str(&mapterm::kitty::free(id));
                        }
                    }
                    // Pieces no bigger than 32x16 cells, which herdr relays (it drops
                    // placements stretched over very large areas).
                    let blocks = mapterm::raster::layout_max(&rstore, &draw_cam, map_area, rz, (32, 16));
                    dlog(|| format!("  placing {} blocks", blocks.len()));
                    for (i, b) in blocks.iter().enumerate() {
                        if sent.insert(b.tile.id) {
                            cmds.push_str(&mapterm::kitty::transmit(b.tile.id, &b.tile.png));
                        }
                        cmds.push_str(&mapterm::kitty::place(b.tile.id, i as u32 + 1, b.cells.x, b.cells.y, b.src, b.cells.width, b.cells.height));
                    }
                    out.write_all(cmds.as_bytes())?;
                }
                crossterm::execute!(out, crossterm::terminal::EndSynchronizedUpdate)?;
            }

            // Fetch what the view needs that we don't have yet.
            let vp = Viewport::new(&cam, map_area);
            for id in vp.tiles(cam.tile_zoom(src.min_zoom, src.max_zoom)) {
                if !store.contains(&id) && pending.insert(id) {
                    fetch(&src, &sem, style, id, tx.clone());
                }
            }

            if let (Some(r), Some(rz)) = (&raster, rz) {
                for id in mapterm::raster::tiles_for(&draw_cam, map_area, rz) {
                    if !rstore.contains(&id) && rpending.insert(id) {
                        fetch_raster(r, &rsem, id, rtx.clone());
                    }
                }
            }

            // ~60 fps while animating, otherwise wake only for input, tiles, or a slow tick.
            let idle = if ctl.animating() { Duration::from_millis(16) } else { Duration::from_millis(500) };
            tokio::select! {
                Some(first) = rrx.recv() => {
                    let mut batch = vec![first];
                    while let Ok(d) = rrx.try_recv() {
                        batch.push(d);
                    }
                    for (id, r) in batch {
                        rpending.remove(&id);
                        dlog(|| format!("raster tile {id:?}: {}", match &r { Ok(Some(t)) => format!("{}x{} image", t.size().0, t.size().1), Ok(None) => "none".into(), Err(e) => format!("error {e:#}") }));
                        match r {
                            Ok(t) => rstore.insert(id, t),
                            Err(_) => {
                                // Remember the failure so the tile isn't fetched again and again.
                                errors += 1;
                                rstore.insert(id, None);
                            }
                        }
                    }
                }
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
                    // Handle everything already queued before drawing again, so a fast drag or
                    // flick catches up in one frame instead of trailing the pointer.
                    let mut queued = vec![first];
                    while let Ok(ev) = krx.try_recv() {
                        queued.push(ev);
                    }
                    for ev in queued {
                        match ev {
                            Event::Key(k) if k.kind != KeyEventKind::Release => {
                                if ctl.key(k) {
                                    continue;
                                }
                                match k.code {
                                    KeyCode::Esc if !picks.is_empty() => {
                                        picks.clear();
                                        click_at = None;
                                    }
                                    KeyCode::Esc | KeyCode::Char('i') if inspect => {
                                        inspect = false;
                                        picks.clear();
                                        click_at = None;
                                        hover = None;
                                        hover_dot = None;
                                        set_pointer("default")?;
                                        // Pixel mouse reports stay on only if smooth mode needs them.
                                        if !smooth_on && ctl.pixel_mouse.is_some() {
                                            ctl.pixel_mouse = None;
                                            set_pixel_mouse(false)?;
                                        }
                                    }
                                    KeyCode::Char('i') => {
                                        inspect = true;
                                        set_pointer("crosshair")?;
                                        // Exact pointer positions, for the crosshair and picking.
                                        if ctl.pixel_mouse.is_none()
                                            && let Some(px) = cell_px()
                                        {
                                            ctl.pixel_mouse = Some(px);
                                            set_pixel_mouse(true)?;
                                        }
                                    }
                                    KeyCode::Tab if !picks.is_empty() => sel = (sel + 1) % picks.len(),
                                    KeyCode::BackTab if !picks.is_empty() => sel = (sel + picks.len() - 1) % picks.len(),
                                    KeyCode::Char('y') if !picks.is_empty() => {
                                        if let Some(p) = picks.get(sel) {
                                            let json = serde_json::to_string_pretty(&p.to_json()).unwrap_or_default();
                                            osc52_copy(&json)?;
                                            note = Some((Instant::now(), format!("copied {} as JSON", p.title())));
                                        }
                                    }
                                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                                    KeyCode::Char('n') => labels = !labels,
                                    KeyCode::Char('v') => vector = (vector + 1) % 3,
                                    KeyCode::Char('s') if smooth_available => {
                                        smooth_on = !smooth_on;
                                        apply_smooth(&mut ctl, smooth_on)?;
                                        if inspect && ctl.pixel_mouse.is_none() {
                                            ctl.pixel_mouse = cell_px();
                                            set_pixel_mouse(ctl.pixel_mouse.is_some())?;
                                        }
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
                                }
                            }
                            Event::Resize(..) => {
                                cpx = cell_px().unwrap_or(cpx);
                                if ctl.pixel_mouse.is_some() {
                                    ctl.pixel_mouse = cell_px();
                                }
                            }
                            Event::Mouse(m) => {
                                // The cell under the pointer (reports are in pixels in pixel-mouse mode).
                                let cell = match ctl.pixel_mouse {
                                    Some((cw, ch)) => ((m.column as f64 / cw) as u16, (m.row as f64 / ch) as u16),
                                    None => (m.column, m.row),
                                };
                                let pos = ratatui::layout::Position::new(cell.0, cell.1);
                                // Clicks in the inspector select a feature; the map never sees them.
                                if panel_area.width > 0 && panel_area.contains(pos) {
                                    if m.kind == event::MouseEventKind::Down(event::MouseButton::Left)
                                        && let Some((_, i)) = list_rows.iter().find(|(r, _)| *r == cell.1)
                                    {
                                        sel = *i;
                                    }
                                    continue;
                                }
                                if inspect && !map_area.contains(pos) {
                                    hover_dot = None;
                                }
                                if inspect && m.kind == event::MouseEventKind::Moved && map_area.contains(pos) {
                                    // The pointer in dots relative to the map: exact from pixel
                                    // reports, else the middle of the cell.
                                    let (ax, ay) = (map_area.x as f64, map_area.y as f64);
                                    let dot = match ctl.pixel_mouse {
                                        Some((cw, ch)) => ((m.column as f64 / cw - ax) * 2.0, (m.row as f64 / ch - ay) * 4.0),
                                        None => ((cell.0 as f64 - ax) * 2.0 + 1.0, (cell.1 as f64 - ay) * 4.0 + 2.0),
                                    };
                                    hover_dot = Some(dot);
                                    let key = (dot.0.floor() as i32, dot.1.floor() as i32);
                                    if hover.as_ref().is_none_or(|(k, _)| *k != key) {
                                        let under = mapterm::pick_dot(&store, &last_cam, src.min_zoom, src.max_zoom, map_area, dot);
                                        hover = Some((key, match under.first() {
                                            Some(p) => {
                                                let more = if under.len() > 1 { format!(" +{} more", under.len() - 1) } else { String::new() };
                                                format!("{} ({}){more}", p.title(), p.kind())
                                            }
                                            None => "nothing here".into(),
                                        }));
                                    }
                                    continue;
                                }
                                ctl.mouse(m);
                                if let Some(k) = ctl.take_click()
                                    && inspect
                                {
                                    picks = mapterm::pick_lonlat(&store, &last_cam, src.min_zoom, src.max_zoom, map_area, k.lon, k.lat);
                                    sel = 0;
                                    click_at = Some((k.lon, k.lat));
                                    if picks.is_empty() {
                                        note = Some((Instant::now(), "nothing here".into()));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ = tokio::time::sleep(idle) => {}
            }
        }
    }
    .await;
    if smooth_on {
        let _ = std::io::stdout().write_all(smooth::SMOOTH_OFF.as_bytes());
    }
    let _ = set_pixel_mouse(false);
    let _ = set_pointer("default");
    if kitty {
        let _ = std::io::stdout().write_all(mapterm::kitty::FREE_ALL.as_bytes());
    }
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}
