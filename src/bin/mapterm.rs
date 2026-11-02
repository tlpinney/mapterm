//! mapterm: the terminal map viewer built on the mapterm library.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style as TStyle},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget},
};
use std::io::Write;

use mapterm::{
    Camera, Controller, KittyState, Map, MapSource, Markers, Overlay, RasterMode, Source, StyleLayer, TileId, Viewport,
    map::Loaded,
    smooth,
    source::OPENFREEMAP,
    spec::Kind,
    stylejson::{self, Schema},
};
use tokio::sync::{Semaphore, mpsc};

const USAGE: &str = "mapterm -- a map in your terminal

usage: mapterm [SOURCE | --style STYLE] [--geojson FILE|URL]... [--raster NAME|URL] [--at LAT,LON] [-z ZOOM] [--light] [--no-labels] [--snapshot COLSxROWS [--format F | --pick COL,ROW]]

  SOURCE        .pmtiles file or URL, XYZ template with {z}/{x}/{y}, or TileJSON URL, drawn
                with mapterm's terminal style (default: OpenFreeMap planet)
  --style       a MapLibre style instead: liberty, positron, bright, dark or fiord
                (OpenFreeMap), a style URL, a style.json file, or a Mapbox style such
                as mapbox://styles/mapbox/streets-v12 (needs a Mapbox token)
  --mapbox-token-cmd CMD
                print the Mapbox access token with a command (else MAPBOX_ACCESS_TOKEN);
                used for mapbox:// styles and --raster mapbox
  --geojson     a data file or URL drawn as a layer over the map: GeoJSON, CSV with
                lat/lon (or WKT) columns, GPX, KML, FlatGeobuf, a Shapefile (.shp, with
                its .dbf, .prj and .cpg) or GeoParquet (repeatable; each gets its own color;
                names from a `name` property; inspect shows properties)
                A GeoParquet URL, or a STAC collection of GeoParquet files (its
                collection.json, e.g. Overture's at stac.overturemaps.org), loads as the map
                moves: only the chunks under the view, from zoom 14. `microsoft-buildings`
                (or a dataset-links.csv URL) is Microsoft's current building footprints, each
                ~78 km tile downloaded once and kept split in ~/.cache/mapterm
  --buildings-cache SIZE  disk for those tiles, least recently used out first (default
                2GB; e.g. 500MB, 10GB; 0 keeps none between uses; also
                MAPTERM_BUILDINGS_CACHE)
  --bbox W,S,E,N  read a GeoParquet layer once, for this area only (lon/lat), instead of
                loading it as the map moves
  --at          start position (default: fit the GeoJSON layers, else the source's center)
  -z            start zoom
  --raster      an image tile layer under the vector map: a provider or stack name
                (--raster-list; `planetary` is Sentinel-2 with NAIP on top when zoomed in),
                a URL with {z}/{x}/{y} ({-y} or --tms for TMS rows, {key} for an API key),
                or a .pmtiles file or URL of PNG/JPEG/WebP tiles;
                real images with kitty graphics (Ghostty, kitty, WezTerm, herdr), cell colors
                elsewhere (MAPTERM_GRAPHICS=kitty|cells overrides)
  --raster-list           show the provider names and the keys they need
  --raster-key-cmd CMD    print the API key with a command, e.g. your password manager's
                          CLI: 'security find-generic-password -s vantor -w' (macOS)
                          (otherwise the provider's variable, or MAPTERM_RASTER_KEY for URLs)
  --raster-header 'N: V'  extra request header; write the key as {key}, e.g.
                          'Authorization: Bearer {key}', so it stays out of ps and history
  --raster-param N=V      fill another {N} in the URL, e.g. mosaic=NAME for Planet
  --date YYYY-MM-DD       the day to show for dated layers (gibs; default yesterday, UTC)
  --raster-list-mosaics   list the Planet mosaics your key can use, then exit
  --raster-cache / --raster-no-cache
                          keep tiles on disk or not (default: on, off for keyed providers;
                          Google's on, for as long as each tile's headers allow)
  --raster-limit N|off    at most N imagery tile requests a minute (default 300 for keyed,
                          billed providers, none for free ones; also MAPTERM_RASTER_LIMIT)
  --raster-attribution T  credit for the layer (known providers are filled in)
  --tms                   count raster rows from the bottom (TMS) even with a plain {y}
  --light       light theme (the built-in style; t switches)
  --no-labels   start with labels and symbols hidden (n shows them); with --snapshot, a
                frame of map lines and fills only
  --snapshot   render one frame to stdout as text and exit
  --format      with --snapshot: text (default), ansi (24-bit color) or html (a <pre> fragment)
  --pick        with --snapshot: print the features under that cell as JSON lines instead

keys (MapLibre conventions): wheel zooms at cursor · drag pans (with inertia) ·
      double-click zooms in, shift+double-click out · shift+drag box zoom · arrows/hjkl pan ·
      +/- zoom · sideways swipe pans · / search · i inspect · m next style · t theme ·
      c copy coordinates · v vector overlay · n labels · q quit

search (/): type a place and press enter; one match flies there, several open a list
      (up/down or tab, enter or click to fly); esc clears the marker. Uses Photon
      (photon.komoot.io); MAPTERM_GEOCODER=nominatim or a server URL switches.

inspect mode (i): hover shows what's under the pointer; click lists every feature there
      (drawn or not) with its properties, size and location; click an entry or tab/shift+tab
      to switch; y copies it as JSON; esc closes, i leaves inspect mode

experimental: MAPTERM_SMOOTH=shift (pixel-smooth dragging) or MAPTERM_SMOOTH=1 (smooth
      up/down panning) on the Ghostty Smooth terminal fork; `s` then toggles it.
      See https://github.com/tlpinney/ghostty (branch smooth).";

struct Args {
    source: Option<String>,
    style: Option<String>,
    at: Option<(f64, f64)>,
    zoom: Option<f64>,
    /// --bbox: the area of GeoParquet layers to read.
    bbox: Option<[f64; 4]>,
    light: bool,
    /// --no-labels: start with symbol layers hidden, as if `n` had been pressed.
    no_labels: bool,
    snapshot: Option<(u16, u16)>,
    /// --snapshot output: text, ansi or html.
    format: String,
    pick: Option<(u16, u16)>,
    raster: Option<String>,
    tms: bool,
    geojson: Vec<String>,
    raster_attribution: Option<String>,
    raster_key_cmd: Option<String>,
    mapbox_token_cmd: Option<String>,
    raster_headers: Vec<String>,
    raster_cache: Option<bool>,
    /// --raster-limit: Some(None) is "off", Some(Some(n)) n a minute, None the default.
    raster_limit: Option<Option<u32>>,
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
    let mut a = Args {
        source: None,
        style: None,
        at: None,
        zoom: None,
        bbox: None,
        light: false,
        no_labels: false,
        snapshot: None,
        format: "text".into(),
        pick: None,
        raster: None,
        tms: false,
        geojson: Vec::new(),
        raster_attribution: None,
        raster_key_cmd: None,
        mapbox_token_cmd: None,
        raster_headers: Vec::new(),
        raster_cache: None,
        raster_limit: None,
        raster_params: Vec::new(),
        list_mosaics: false,
        smooth,
        shift,
    };
    let parse_limit = |v: &str| -> Result<Option<u32>> {
        match v.trim() {
            "off" | "none" | "0" => Ok(None),
            n => Ok(Some(n.parse::<u32>().with_context(|| format!("--raster-limit {n}: a number of tiles a minute, or off"))?)),
        }
    };
    // Before the arguments, so --buildings-cache wins over it.
    if let Ok(v) = std::env::var("MAPTERM_BUILDINGS_CACHE") {
        mapterm::msbuildings::set_cache_limit(mapterm::msbuildings::parse_size(&v).context("MAPTERM_BUILDINGS_CACHE")?);
    }
    if let Ok(v) = std::env::var("MAPTERM_RASTER_LIMIT") {
        a.raster_limit = Some(parse_limit(&v)?);
    }
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--at" => {
                let v = it.next().context("--at LAT,LON")?;
                let (la, lo) = v.split_once(',').context("--at LAT,LON")?;
                a.at = Some((lo.trim().parse()?, la.trim().parse()?));
            }
            "--style" => a.style = Some(it.next().context("--style NAME|URL|FILE")?),
            "-z" | "--zoom" => a.zoom = Some(it.next().context("-z ZOOM")?.parse()?),
            "--light" => a.light = true,
            "--no-labels" => a.no_labels = true,
            "--raster" => a.raster = Some(it.next().context("--raster URL")?),
            "--tms" => a.tms = true,
            "--geojson" => a.geojson.push(it.next().context("--geojson FILE|URL")?),
            "--buildings-cache" => {
                let v = it.next().context("--buildings-cache SIZE")?;
                mapterm::msbuildings::set_cache_limit(mapterm::msbuildings::parse_size(&v).context("--buildings-cache")?);
            }
            "--bbox" => {
                let v = it.next().context("--bbox W,S,E,N")?;
                let n: Vec<f64> = v.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>().context("--bbox W,S,E,N")?;
                let [w, s, e, nn] = n[..] else { bail!("--bbox W,S,E,N: four numbers") };
                if w >= e || s >= nn {
                    bail!("--bbox W,S,E,N: west must be less than east and south less than north");
                }
                a.bbox = Some([w, s, e, nn]);
            }
            "--raster-key-cmd" => a.raster_key_cmd = Some(it.next().context("--raster-key-cmd CMD")?),
            "--mapbox-token-cmd" => a.mapbox_token_cmd = Some(it.next().context("--mapbox-token-cmd CMD")?),
            "--raster-header" => a.raster_headers.push(it.next().context("--raster-header 'Name: value'")?),
            "--raster-param" => {
                let v = it.next().context("--raster-param NAME=VALUE")?;
                let (n, val) = v.split_once('=').context("--raster-param NAME=VALUE")?;
                a.raster_params.push((n.trim().to_string(), val.trim().to_string()));
            }
            "--raster-list-mosaics" => a.list_mosaics = true,
            "--date" => a.raster_params.push(("date".into(), it.next().context("--date YYYY-MM-DD")?)),
            "--raster-cache" => a.raster_cache = Some(true),
            "--raster-limit" => a.raster_limit = Some(parse_limit(&it.next().context("--raster-limit N|off")?)?),
            "--raster-no-cache" => a.raster_cache = Some(false),
            "--raster-list" => {
                println!("raster providers (--raster NAME):\n");
                let w = mapterm::raster::PRESETS.iter().map(|p| p.name.len()).max().unwrap_or(10);
                for p in mapterm::raster::PRESETS {
                    let key = if p.key_env.is_empty() { "no key".to_string() } else { format!("key: {}", p.key_env.join(" or ")) };
                    println!("  {:<w$} {}\n  {:<w$} {key}", p.name, p.about, "");
                }
                println!("\nstacks (several providers by zoom):\n");
                for st in mapterm::raster::STACKS {
                    // From the later of the layer's zoom and the provider's own lowest.
                    let from = |l: &mapterm::raster::StackLayer| {
                        let own = mapterm::raster::preset(l.preset)
                            .and_then(|p| mapterm::raster::RasterSource::from_preset(p, Some(String::new())).ok())
                            .map_or(0, |r| r.min_zoom);
                        l.min_zoom.max(own)
                    };
                    let parts: Vec<String> = st.layers.iter().map(|l| format!("{} from zoom {}", l.preset, from(l))).collect();
                    println!("  {:<w$} {}\n  {:<w$} {}", st.name, st.about, "", parts.join(", then "));
                }
                println!("\nKeys can also come from --raster-key-cmd, a command that prints the key (e.g. a password manager's CLI).");
                std::process::exit(0);
            }
            "--raster-attribution" => a.raster_attribution = Some(it.next().context("--raster-attribution TEXT")?),
            "--snapshot" => {
                let v = it.next().context("--snapshot COLSxROWS")?;
                let (c, r) = v.split_once('x').context("--snapshot COLSxROWS")?;
                a.snapshot = Some((c.parse()?, r.parse()?));
            }
            "--format" => {
                let v = it.next().context("--format text|ansi|html")?;
                if !matches!(v.as_str(), "text" | "ansi" | "html") {
                    bail!("--format {v}: use text, ansi or html");
                }
                a.format = v;
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
            s => a.source = Some(s.into()),
        }
    }
    Ok(a)
}

type Done = (String, TileId, Result<Loaded>);

/// The built-in terminal style's entry in the `m` style cycle.
const BUILTIN: &str = "built-in";

type StyleDone = (String, Result<(Map, Option<(Schema, bool)>, String)>);

/// Load a basemap for the `m` style cycle: the built-in style over `source` (schema detected
/// from the tile under `cam`), or a MapLibre style. Returns the map, the built-in style's
/// schema and theme (for `t`), and a name for the status bar.
async fn load_basemap(spec: &str, source: &str, dark: bool, cam: Camera) -> Result<(Map, Option<(Schema, bool)>, String)> {
    if spec == BUILTIN {
        let src = Source::open(source).await?;
        let name = src.name.clone();
        let schema = detect_schema(&src, &cam).await;
        Ok((Map::terminal(src, schema, dark)?, Some((schema, dark)), name))
    } else {
        Ok((Map::from_style(spec).await?, None, spec.to_string()))
    }
}

/// GeoJSON layers with more single points than this cluster them.
const CLUSTER_ABOVE: usize = 1000;

/// Ids of the layers the viewer adds.
const RASTER: &str = "mapterm-raster";

/// A GeoParquet dataset layer loading as the map moves.
struct Loading {
    /// Its source and layers in the map.
    id: String,
    /// What --geojson named (the key of its saved index).
    spec: String,
    ds: mapterm::geoparquet::Dataset,
    inflight: HashSet<mapterm::geoparquet::Job>,
    /// The STAC index has been saved.
    saved: bool,
}

/// A count with thousands separators: 12,345.
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

/// Dataset features kept loaded; past this the chunks farthest from the view are dropped.
const DATASET_BUDGET: usize = 600_000;
/// Dataset jobs at once: STAC items are tiny, footers and chunks are megabytes.
const ITEM_SLOTS: usize = 32;
const CHUNK_SLOTS: usize = 4;

/// Vector tile downloads at once.
const VECTOR_SLOTS: usize = 6;
/// Imagery downloads at once: tile servers like OSM's ask for at most two connections.
const RASTER_SLOTS: usize = 2;

/// Whether a layer is the imagery under the map (`RASTER`, or a lower layer of a stack).
fn is_imagery(id: &str) -> bool {
    id == RASTER || id.strip_prefix(RASTER).is_some_and(|r| r.starts_with('-'))
}
const OVERLAYS: &str = "mapterm-overlays";

/// Debug log for troubleshooting (MAPTERM_LOG=path); a no-op unless set.
fn dlog(msg: impl FnOnce() -> String) {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    if let Some(path) = PATH.get_or_init(|| std::env::var("MAPTERM_LOG").ok()).as_deref()
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "{:8.3} {}", start.elapsed().as_secs_f64(), msg());
    }
}

/// Print the Planet basemap mosaics the key can use (name, dates), following pagination.
/// The key goes in a Basic auth header (Planet's API accepts the key as the user name).
async fn list_planet_mosaics(key_cmd: Option<&str>) -> Result<()> {
    let key = mapterm::raster::find_key(key_cmd, &["PL_API_KEY", "PLANET_API_KEY"])?.context("Planet needs a key: set PL_API_KEY or pass --raster-key-cmd")?;
    let auth = vec![("Authorization".to_string(), format!("Basic {}", mapterm::kitty::base64(format!("{key}:").as_bytes())))];
    let http = mapterm::source::keyed_client();
    const API: &str = "https://api.planet.com/";
    let mut url = format!("{API}basemaps/v1/mosaics");
    let mut count = 0;
    for _ in 0..20 {
        let bytes = mapterm::source::get_limited(http, &url, &auth, 16 << 20).await.context("Planet mosaics")?;
        let v: serde_json::Value = serde_json::from_slice(&bytes.context("Planet mosaics: not found")?)?;
        for m in v["mosaics"].as_array().into_iter().flatten() {
            let s = |k: &str| m[k].as_str().unwrap_or("").get(..10).unwrap_or("").to_string();
            println!("{:<48} {} .. {}", m["name"].as_str().unwrap_or("?"), s("first_acquired"), s("last_acquired"));
            count += 1;
        }
        match v["_links"]["_next"].as_str() {
            // The key goes with every request: only follow links back to Planet's API over https.
            Some(next) if next.starts_with(API) => url = next.to_string(),
            Some(next) if !next.is_empty() => bail!("Planet mosaics: refusing to send the key to the next-page link {next:?}"),
            _ => break,
        }
    }
    eprintln!("{count} mosaics; use one with --raster planet --raster-param mosaic=NAME");
    Ok(())
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
    d.push(Line::from(vec![key("drawn by"), Span::raw(p.style_layer.as_deref().unwrap_or("(not drawn)").to_string())]));
    d.push(Line::from(vec![key("source layer"), Span::raw(p.layer.to_string())]));
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
    match p.tile {
        Some(t) => d.push(Line::from(vec![key("tile"), Span::raw(format!("{}/{}/{}", t.0, t.1, t.2))])),
        None => d.push(Line::from(vec![key("source"), Span::raw(format!("{} (GeoJSON)", p.source.as_deref().unwrap_or("?")))])),
    }
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
    (ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0).then(|| (ws.width as f64 / ws.columns as f64, ws.height as f64 / ws.rows as f64))
}

/// Turn SGR-pixels mouse reporting (DEC 1016) on or off. While on, mouse positions arrive in
/// pixels in the column/row fields.
fn set_pixel_mouse(on: bool) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(if on { b"\x1b[?1016h" } else { b"\x1b[?1016l" })?;
    out.flush()
}

/// Search state: typing a query, waiting for the geocoder, or choosing among results.
enum Search {
    Off,
    Typing(String),
    Waiting(String),
    Results { places: Vec<mapterm::geocode::Place>, sel: usize },
}

/// The search results panel. Returns the screen rows of each entry, so clicks can choose.
fn draw_results(f: &mut ratatui::Frame, area: Rect, places: &[mapterm::geocode::Place], sel: usize) -> Vec<(u16, usize)> {
    use ratatui::style::Modifier;
    use ratatui::widgets::Borders;
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(format!(" places · {} ", places.len()));
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let w = inner.width.saturating_sub(3) as usize;
    let cut = |s: &str| if s.chars().count() > w { format!("{}…", s.chars().take(w.saturating_sub(1)).collect::<String>()) } else { s.to_string() };
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for (i, p) in places.iter().enumerate() {
        let y = inner.y + lines.len() as u16;
        if y + 1 >= inner.bottom() {
            break;
        }
        let mut st = TStyle::new().add_modifier(Modifier::BOLD);
        if i == sel {
            st = st.add_modifier(Modifier::REVERSED);
        }
        let kind = if p.kind.is_empty() { String::new() } else { format!("  {}", p.kind) };
        lines.push(Line::from(vec![Span::styled(format!(" ◆ {}", cut(&p.name)), st), Span::styled(kind, dim)]));
        lines.push(Line::from(Span::styled(format!("   {}", cut(&p.context)), dim)));
        rows.push((y, i));
        rows.push((y + 1, i));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(" ↑↓ choose · enter fly · esc close", dim)));
    f.render_widget(Paragraph::new(lines), inner);
    rows
}

/// Guess the vector tile schema from the tile under the camera (OpenMapTiles if unsure).
async fn detect_schema(src: &Source, cam: &Camera) -> Schema {
    let z = cam.tile_zoom(src.min_zoom, src.max_zoom);
    let n = (1u64 << z) as f64;
    let (wx, wy) = mapterm::render::to_world(cam.lon, cam.lat);
    let (x, y) = ((wx * n).floor().clamp(0.0, n - 1.0) as u32, (wy * n).floor().clamp(0.0, n - 1.0) as u32);
    match src.tile(z, x, y).await {
        Ok(Some(b)) => match mapterm::store::decode_features(&b) {
            Ok(f) if !f.is_empty() => Schema::detect(f.iter().map(|f| &*f.layer)),
            _ => Schema::OpenMapTiles,
        },
        _ => Schema::OpenMapTiles,
    }
}

/// What the viewer shows, from its toggles: `labels` (n), `vector` (v: 0 everything, 1
/// labels only, 2 no vector map) and whether a --raster layer is under the map (basemap and
/// GeoJSON fills are hidden over it so the imagery shows). A layer the style hides stays hidden.
fn apply_toggles(map: &mut Map, base: &std::collections::HashMap<String, bool>, labels: bool, vector: u8, raster: bool) {
    let ids: Vec<String> = map.layer_ids().into_iter().map(str::to_string).collect();
    for id in ids {
        let Some(l) = map.layer(&id) else { continue };
        let src = l.source.as_deref().and_then(|s| map.source(s));
        let basemap = matches!(src, Some(MapSource::Vector(_)));
        let geojson = matches!(src, Some(MapSource::GeoJson(_)));
        let mut on = base.get(&id).copied().unwrap_or(true);
        if l.kind == Kind::Symbol && !(labels && vector < 2) {
            on = false;
        }
        if basemap && l.kind != Kind::Symbol && vector >= 1 {
            on = false;
        }
        if raster && l.kind == Kind::Fill && (basemap || geojson) {
            on = false;
        }
        let _ = map.set_visibility(&id, on);
    }
}

/// Visibility the style itself gives each layer, before the viewer's toggles.
fn base_visibility(map: &Map) -> std::collections::HashMap<String, bool> {
    map.layer_ids().into_iter().filter_map(|id| map.layer(id).map(|l| (id.to_string(), l.visible))).collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = args()?;
    if a.list_mosaics {
        return list_planet_mosaics(a.raster_key_cmd.as_deref()).await;
    }
    if a.style.is_some() && a.source.is_some() {
        bail!("give either a SOURCE (drawn with the built-in style) or --style, not both");
    }
    // Mapbox's token, for mapbox:// styles and the mapbox imagery.
    mapterm::mapbox::set_access_token(mapterm::raster::find_key(a.mapbox_token_cmd.as_deref(), &["MAPBOX_ACCESS_TOKEN"])?);
    // GeoParquet layers read only this area: --bbox, else three screens around the start view.
    let bbox = a.bbox.or_else(|| {
        let ((lon, lat), z) = (a.at?, a.zoom?);
        let (w, h) = a.snapshot.or_else(|| crossterm::terminal::size().ok()).unwrap_or((120, 40));
        let vp = Viewport::new(&Camera::new(lon, lat, z), Rect::new(0, 0, w, h));
        let ((x0, y0), (x1, y1)) = (vp.cell_to_lonlat(0, 0), vp.cell_to_lonlat(w, h));
        let (dx, dy) = ((x1 - x0).abs(), (y0 - y1).abs());
        Some([x0.min(x1) - dx, (y0.min(y1) - dy).max(-85.0), x0.max(x1) + dx, (y0.max(y1) + dy).min(85.0)])
    });
    let mut geo = Vec::new();
    // Layers loaded a chunk at a time as the map moves: a STAC collection of GeoParquet files,
    // or a GeoParquet URL without --bbox. They start empty. (index in `geo`, spec, dataset)
    let mut opened: Vec<(usize, String, mapterm::geoparquet::Dataset)> = Vec::new();
    for (i, g) in a.geojson.iter().enumerate() {
        let color = mapterm::geojson::PALETTE[i % mapterm::geojson::PALETTE.len()];
        let path = g.split(['?', '#']).next().unwrap_or(g).to_ascii_lowercase();
        let remote = g.starts_with("https://") || g.starts_with("http://");
        let parquet = path.ends_with(".parquet") || path.ends_with(".geoparquet");
        if mapterm::msbuildings::is_spec(g) || (remote && (path.ends_with("collection.json") || (parquet && a.bbox.is_none()))) {
            let ds = mapterm::geoparquet::Dataset::open(g).await.with_context(|| format!("--geojson {g}"))?;
            let empty = mapterm::geojson::parse(br#"{"type": "FeatureCollection", "features": []}"#, ds.name.clone(), color)?;
            opened.push((geo.len(), g.clone(), ds));
            geo.push(empty);
            continue;
        }
        geo.push(mapterm::geojson::load_in(g, color, bbox).await.with_context(|| format!("--geojson {g}"))?);
    }
    // The basemap: a MapLibre style, or SOURCE with the built-in terminal style.
    let src = match &a.style {
        Some(_) => None,
        None => Some(Source::open(a.source.as_deref().unwrap_or(OPENFREEMAP)).await?),
    };
    let mut map = match &a.style {
        Some(st) => Map::from_style(st).await.with_context(|| format!("--style {st}"))?,
        None => Map::new(),
    };
    let mut basemap_name = match (&a.style, &src) {
        (Some(st), _) => st.clone(),
        (None, Some(s)) => s.name.clone(),
        _ => String::new(),
    };
    // With GeoJSON and no --at, start with every layer in view.
    let fit = (a.at.is_none() && !geo.is_empty())
        .then(|| {
            let b = geo.iter().filter_map(|l| l.bounds_lonlat()).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))?;
            let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
            let (snap_cols, snap_rows) = a.snapshot.unwrap_or((cols, rows.saturating_sub(1)));
            Some(mapterm::geojson::fit(b, snap_cols, snap_rows, 18.0))
        })
        .flatten();
    let center = src.as_ref().and_then(|s| s.center).or(map.center);
    let (lon, lat, z) = match (a.at, center) {
        _ if fit.is_some() => {
            let (lo, la, z) = fit.unwrap();
            (lo, la, a.zoom.unwrap_or(z))
        }
        (Some((lo, la)), _) => (lo, la, a.zoom.unwrap_or(12.0)),
        (None, Some((lo, la, z))) => (lo, la, a.zoom.unwrap_or(z)),
        (None, None) => (0.0, 20.0, a.zoom.unwrap_or(2.0)),
    };
    let cam = Camera::new(lon, lat, z);
    // The built-in style's schema and theme, for `t`.
    let mut theme: Option<(Schema, bool)> = None;
    if let Some(src) = src {
        let schema = detect_schema(&src, &cam).await;
        map = Map::terminal(src, schema, !a.light)?;
        theme = Some((schema, !a.light));
    }
    for w in map.warnings() {
        dlog(|| format!("style: {w}"));
    }
    let style_warnings = map.warnings().len();
    let mut basemap_ids: Vec<String> = map.layer_ids().into_iter().map(str::to_string).collect();
    let mut basemap_sources: Vec<String> = map.sources().map(|(id, _)| id.to_string()).collect();
    // The `m` cycle: the --style given, the built-in style, then the OpenFreeMap styles.
    let builtin_source = a.source.clone().unwrap_or_else(|| OPENFREEMAP.to_string());
    let mut styles: Vec<String> = a.style.iter().cloned().collect();
    for s in std::iter::once(BUILTIN).chain(stylejson::OPENFREEMAP_STYLES) {
        if !styles.iter().any(|x| x == s) {
            styles.push(s.to_string());
        }
    }
    let mut style_at = 0usize;

    // Optional raster layer under the vector map.
    let raster = match &a.raster {
        // Several providers by zoom (`planetary`): each its own source and layer, bottom
        // first; the top one is "the" imagery for the status bar.
        Some(spec) if let Some(st) = mapterm::raster::stack(spec) => {
            let bottom = map.layer_ids().first().map(|s| s.to_string());
            let mut layers = Vec::new();
            let mut top = None;
            for (i, sl) in st.layers.iter().enumerate() {
                let p = mapterm::raster::preset(sl.preset).context("stack names an unknown provider")?;
                let key = mapterm::raster::find_key(a.raster_key_cmd.as_deref(), p.key_env)?;
                let mut r = mapterm::raster::RasterSource::from_preset(p, key)?;
                if let Some(z) = sl.max_zoom {
                    r.max_zoom = r.max_zoom.min(z);
                }
                r.start_session().await?;
                if let Some(c) = a.raster_cache {
                    r.cache = c;
                }
                let id = if i + 1 == st.layers.len() { RASTER.to_string() } else { format!("{RASTER}-{i}") };
                let r = Arc::new(r);
                map.add_source(&id, MapSource::raster(r.clone()))?;
                layers.push(StyleLayer::raster(&id, &id).min_zoom(sl.min_zoom as f64));
                top = Some(r);
            }
            map.add_layers(layers, bottom.as_deref())?;
            top
        }
        Some(spec) => {
            let mut r = match mapterm::raster::preset(spec) {
                None if spec.ends_with(".pmtiles") => mapterm::raster::RasterSource::from_pmtiles(spec).await?,
                Some(p) => {
                    let mut key = mapterm::raster::find_key(a.raster_key_cmd.as_deref(), p.key_env)?;
                    if key.is_none() && p.name == "mapbox" {
                        key = mapterm::mapbox::access_token();
                    }
                    mapterm::raster::RasterSource::from_preset(p, key)?
                }
                None => {
                    let mut r = mapterm::raster::RasterSource::new(spec, a.tms, 19)
                        .with_context(|| format!("--raster {spec}: not a provider name (see --raster-list) or a tile URL"))?;
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
            r.start_session().await?;
            let missing = r.missing_params();
            if !missing.is_empty() {
                let hint = if missing.iter().any(|m| m == "mosaic") {
                    " (Planet: --raster-param mosaic=NAME or PLANET_MOSAIC; list yours with --raster-list-mosaics)"
                } else {
                    ""
                };
                bail!(
                    "the raster URL needs {}: use --raster-param NAME=VALUE{hint}",
                    missing.iter().map(|m| format!("{{{m}}}")).collect::<Vec<_>>().join(", ")
                );
            }
            if let Some(c) = a.raster_cache {
                r.cache = c;
            }
            // Google's credits depend on the view: these are for the start view (about the
            // terminal's width of map around the center); the viewer updates them as you move.
            // "Google Maps" alone if that fails.
            if r.is_google() {
                let span = 360.0 / 2f64.powf(cam.zoom) * 2.0;
                let bounds = ((cam.lat + span * 0.4).min(85.0), (cam.lat - span * 0.4).max(-85.0), cam.lon + span, cam.lon - span);
                if let Ok(c) = r.google_credits(cam.zoom.round().clamp(0.0, 22.0) as u8, bounds).await {
                    r.attribution = c;
                }
            }
            if let Some(t) = &a.raster_attribution {
                r.attribution = t.clone();
            }
            let r = Arc::new(r);
            map.add_source(RASTER, MapSource::raster(r.clone()))?;
            let bottom = map.layer_ids().first().map(|s| s.to_string());
            map.add_layers(vec![StyleLayer::raster(RASTER, RASTER)], bottom.as_deref())?;
            Some(r)
        }
        None => None,
    };
    // GeoJSON layers over the basemap, each in its own color.
    let mut legend: Vec<(String, Color)> = Vec::new();
    let mut datasets: Vec<Loading> = Vec::new();
    let mut opened = opened.into_iter().peekable();
    for (i, g) in geo.into_iter().enumerate() {
        let color = g.color;
        let mut id = g.name.clone();
        if map.source(&id).is_some() {
            id = format!("{id}-{}", i + 1);
        }
        // Many points cluster (as MapLibre's `cluster: true`) so they don't bury the map.
        let points = g.features.iter().filter(|f| f.geom == mapterm::mvt::GeomType::Point && f.parts.len() == 1).count();
        let mut src = MapSource::from(g);
        if points > CLUSTER_ABOVE {
            src = src.with_cluster(mapterm::cluster::ClusterOptions::default());
        }
        map.add_source(&id, src)?;
        map.add_layers(mapterm::geojson::overlay_layers(&id, color, true), None)?;
        if opened.peek().is_some_and(|(at, ..)| *at == i) {
            let (_, spec, ds) = opened.next().expect("peeked");
            datasets.push(Loading { id: id.clone(), spec, ds, inflight: HashSet::new(), saved: false });
        }
        legend.push((id, color));
    }
    // Highlights, the crosshair and the clicked spot, on top of everything.
    map.add_custom_layer(OVERLAYS, Markers::default(), None)?;
    let mut base = base_visibility(&map);
    let mut labels = !a.no_labels;
    // Vector overlay: 0 = everything (no fills over a raster), 1 = labels only, 2 = off.
    let mut vector: u8 = 0;
    apply_toggles(&mut map, &base, labels, vector, raster.is_some());

    let sem = Arc::new(Semaphore::new(VECTOR_SLOTS));
    let rsem = Arc::new(Semaphore::new(RASTER_SLOTS));
    let (tx, mut rx) = mpsc::unbounded_channel::<Done>();
    let (dtx, mut drx) = mpsc::unbounded_channel::<(usize, mapterm::geoparquet::Done)>();
    let spawn_load = |req: mapterm::TileRequest, tx: mpsc::UnboundedSender<Done>| {
        let sem = if req.is_raster() { rsem.clone() } else { sem.clone() };
        tokio::spawn(async move {
            let _permit = sem.acquire_owned().await;
            let r = req.load().await;
            let _ = tx.send((req.source, req.id, r));
        });
    };

    if let Some((w, h)) = a.snapshot {
        let area = Rect::new(0, 0, w, h);
        for req in map.tile_requests(&cam, area) {
            let r = req.load().await;
            if let Err(e) = map.insert_loaded(&req.source, req.id, r) {
                eprintln!("{e:#}");
            }
        }
        // Dataset layers: every job the view needs, until none are left.
        let vp = Viewport::new(&cam, area);
        let ((x0, y0), (x1, y1)) = (vp.cell_to_lonlat(0, 0), vp.cell_to_lonlat(w, h));
        let view = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
        for d in &mut datasets {
            use futures::StreamExt;
            loop {
                let jobs = d.ds.wanted(view, cam.zoom);
                if jobs.is_empty() {
                    break;
                }
                let width = if matches!(jobs[0], mapterm::geoparquet::Job::Item(_)) { ITEM_SLOTS } else { CHUNK_SLOTS };
                let futs: Vec<_> = jobs.into_iter().map(|j| d.ds.run(j)).collect();
                let done: Vec<_> = futures::stream::iter(futs).buffer_unordered(width).collect().await;
                for r in done {
                    match d.ds.apply(r) {
                        Ok(Some((key, features))) => map.add_part(&d.id, key, features)?,
                        Ok(None) => {}
                        Err(e) => eprintln!("{}: {e:#}", d.id),
                    }
                }
            }
            d.ds.save_index(&d.spec);
        }
        if let Some((c, r)) = a.pick {
            let dot = ((c as f64 - area.x as f64) * 2.0 + 1.0, (r as f64 - area.y as f64) * 4.0 + 2.0);
            for p in map.query_features(&cam, area, dot) {
                println!("{}", p.to_json());
            }
            return Ok(());
        }
        // MAPTERM_PIXELS_PNG=FILE: also save pixel mode's image (fills and lines; labels are
        // text) for a Retina-sized terminal, to check the drawing without one.
        let png = std::env::var("MAPTERM_PIXELS_PNG").ok();
        if png.is_some() {
            map.pixel_mode = Some((16.0, 34.0));
        }
        let mut buf = Buffer::empty(area);
        let t = Instant::now();
        map.view(&cam).render(area, &mut buf);
        // Pixel mode leaves tiles blank past its per-frame drawing time: draw frames until
        // all are in, keeping every image sent so far.
        let mut sent: Vec<mapterm::pixels::PixImage> = Vec::new();
        let mut last = map.take_pixels();
        while let Some(f) = last.as_mut().filter(|f| f.pending > 0) {
            sent.append(&mut f.new);
            map.view(&cam).render(area, &mut Buffer::empty(area));
            last = map.take_pixels();
        }
        if let Some(f) = last.as_mut() {
            sent.append(&mut f.new);
            f.new = std::mem::take(&mut sent);
        }
        if let (Some(path), Some(f)) = (png, last) {
            // The frame put together from its tiles as the terminal would place them.
            let (cw, ch) = (16u32, 34u32);
            let (w, h) = (area.width as u32 * cw, area.height as u32 * ch);
            let mut img = vec![0u8; (w * h * 4) as usize];
            let images: std::collections::HashMap<u32, &mapterm::pixels::PixImage> = f.new.iter().map(|i| (i.id, i)).collect();
            for p in &f.placed {
                let Some(src) = images.get(&p.id) else { continue };
                let (sx, sy, sw, sh) = p.src;
                let (dx, dy) = ((p.col - area.x) as u32 * cw + p.x_off, (p.row - area.y) as u32 * ch + p.y_off);
                for y in 0..sh {
                    for x in 0..sw {
                        let (tx, ty) = (dx + x, dy + y);
                        if tx >= w || ty >= h || sx + x >= src.width || sy + y >= src.height {
                            continue;
                        }
                        let s = ((sy + y) * src.width + sx + x) as usize * 4;
                        let o = (ty * w + tx) as usize * 4;
                        img[o..o + 4].copy_from_slice(&src.rgba[s..s + 4]);
                    }
                }
            }
            image::save_buffer(&path, &img, w, h, image::ExtendedColorType::Rgba8)?;
            eprintln!("pixels: {} tiles drawn, {} placed, {:.1} ms -> {path}", f.new.len(), f.placed.len(), t.elapsed().as_secs_f64() * 1000.0);
        }
        let out = match a.format.as_str() {
            "ansi" => mapterm::export::to_ansi(&buf),
            "html" => mapterm::export::to_html(&buf),
            _ => mapterm::export::to_text(&buf),
        };
        print!("{out}");
        return Ok(());
    }

    // Raster drawing: real images with kitty graphics, else cell background colors.
    let has_raster = map.sources().any(|(_, s)| matches!(s, MapSource::Raster(_)));
    let kitty = has_raster && mapterm::kitty::supported();
    // Experimental pixel mode: fills and lines as an image, labels as text (MAPTERM_PIXELS=1).
    let pixels_on = std::env::var("MAPTERM_PIXELS").is_ok_and(|v| !v.is_empty() && v != "0") && mapterm::kitty::supported();
    // Through shared memory when the terminal runs here; inline (compressed) over SSH.
    let pixels_shm = std::env::var_os("SSH_CONNECTION").is_none();
    // Tile images placed last frame, to take down the ones that left the view.
    let mut pixels_placed: std::collections::HashSet<u32> = std::collections::HashSet::new();
    // Tiles still blank (drawing or loading): keep drawing frames until they're in.
    let mut pixels_pending = 0usize;
    // The backdrop shown under the tiles while some aren't ready.
    let mut backdrop_shown: Option<u32> = None;
    // MAPTERM_DEMO="+@2,+@3.5,-@5,right@6,q@9": press keys at those seconds after start
    // (+ - left right up down, q quits), to replay a scenario for recordings and tests.
    let mut demo: Vec<(f64, String)> = std::env::var("MAPTERM_DEMO")
        .unwrap_or_default()
        .split(',')
        .filter_map(|e| {
            let (k, t) = e.trim().split_once('@')?;
            Some((t.trim().parse().ok()?, k.trim().to_string()))
        })
        .collect();
    demo.sort_by(|a, b| b.0.total_cmp(&a.0));
    let demo_start = Instant::now();
    let mut kstate = KittyState::default();
    // Terminal image memory to stay within (herdr keeps 64 MiB per pane).
    if let Some(mb) = std::env::var("MAPTERM_KITTY_BUDGET_MB").ok().and_then(|v| v.parse::<usize>().ok()) {
        kstate.budget = mb.max(1) << 20;
    }
    let mut cpx = cell_px().unwrap_or((9.0, 20.0));
    if kitty {
        map.raster_mode = RasterMode::Kitty { cell_px: cpx };
    }
    if pixels_on {
        map.pixel_mode = Some(cpx);
        // Fading sends each new tile a few more times: cheap through shared memory, not over SSH.
        map.pixel_fade = pixels_shm;
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

    // Tiles being downloaded, and whether each is imagery (for the per-kind slot limits).
    let mut pending: HashMap<(String, TileId), bool> = HashMap::new();
    // Wanted tiles not started for lack of a free slot this frame (for the status bar).
    let mut waiting = 0usize;
    let mut errors = 0usize;
    let mut smooth_on = a.smooth;
    // `s` only toggles smooth scrolling when MAPTERM_SMOOTH opted in to the experiment.
    let smooth_available = a.smooth;
    let shift_mode = a.shift;
    let (zmin, zmax) = map.zoom_range();
    let mut ctl = Controller::new(cam, zmin as f64, zmax as f64 + 3.0);
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
    let mut note: Option<(Instant, String)> =
        (style_warnings > 0).then(|| (Instant::now(), format!("{style_warnings} style properties not drawn (MAPTERM_LOG lists them)")));
    let mut last_cam = cam;
    let mut map_area = Rect::default();
    let mut frame_ms = 0.0f64;
    // Place search: the state, the chosen place (marked on the map), and the geocoder.
    let geocoder = mapterm::geocode::Geocoder::from_env()?;
    let mut search = Search::Off;
    let mut found: Option<mapterm::geocode::Place> = None;
    let (gtx, mut grx) = mpsc::unbounded_channel::<(String, Result<Vec<mapterm::geocode::Place>>)>();
    // Style switching (m) loads on a task; the pointer (dots in the map) is for c.
    let (stx, mut srx) = mpsc::unbounded_channel::<StyleDone>();
    let mut style_loading: Option<String> = None;
    let mut pointer: Option<(f64, f64)> = None;
    // Google's credits follow the view: once it has stayed on a new spot for a moment, ask
    // for the credits there (a quarter of the view's width counts as a new spot).
    let google = raster.as_ref().filter(|r| r.is_google()).cloned();
    let billed_raster = raster.as_ref().is_some_and(|r| r.is_google() || r.wants_key());
    // Imagery requests a minute: a bucket that holds a minute's worth and refills steadily.
    let mut limiter = match a.raster_limit {
        Some(n) => n,
        None if billed_raster => Some(300),
        None => None,
    }
    .map(|n| Bucket::new(n.max(1)));
    let mut held_back = false;
    let mut throttled_at: Option<Instant> = None;
    let (ctx, mut crx) = mpsc::unbounded_channel::<Result<String>>();
    let mut credit_view: Option<(i32, i64, i64)> = None;
    let mut credit_seen: Option<((i32, i64, i64), Instant)> = None;
    let mut credit_pending = false;
    let result: Result<()> = async {
        loop {
            while demo.last().is_some_and(|(t, _)| demo_start.elapsed().as_secs_f64() >= *t) {
                let (_, k) = demo.pop().unwrap_or_default();
                let code = match k.as_str() {
                    "q" => return Ok(()),
                    "left" => KeyCode::Left,
                    "right" => KeyCode::Right,
                    "up" => KeyCode::Up,
                    "down" => KeyCode::Down,
                    other => KeyCode::Char(other.chars().next().unwrap_or(' ')),
                };
                dlog(|| format!("demo: {k} at {:.2} s", demo_start.elapsed().as_secs_f64()));
                ctl.key(crossterm::event::KeyEvent::from(code));
            }
            ctl.tick(Instant::now());
            // One synchronized update per frame when escape sequences follow the cells.
            let sync = smooth_on || kitty || pixels_on;
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
            // Search results as small diamonds, the selected or chosen one with its name.
            let mark = TStyle::new().fg(Color::Rgb(255, 120, 200));
            if let Search::Results { places, sel } = &search {
                for (i, p) in places.iter().enumerate() {
                    let label = (i == *sel).then(|| (p.name.clone(), mark.add_modifier(ratatui::style::Modifier::BOLD)));
                    overlays.push(Overlay::Marker {
                        lon: p.lon,
                        lat: p.lat,
                        glyph: if i == *sel { "◆" } else { "◇" }.into(),
                        style: mark,
                        label,
                        priority: 97 - i as i32,
                    });
                }
            } else if let Some(p) = &found {
                overlays.push(Overlay::Marker {
                    lon: p.lon,
                    lat: p.lat,
                    glyph: "◆".into(),
                    style: mark,
                    label: Some((p.name.clone(), mark.add_modifier(ratatui::style::Modifier::BOLD))),
                    priority: 98,
                });
            }
            if let (true, Some((lon, lat))) = (inspect && !picks.is_empty(), click_at) {
                overlays.push(Overlay::Marker { lon, lat, glyph: "+".into(), style: TStyle::new().fg(Color::Rgb(255, 215, 0)), label: None, priority: 99 });
            }
            if let Some(m) = map.custom_layer_mut::<Markers>(OVERLAYS) {
                m.0 = overlays;
            }
            term.draw(|f| {
                let [main, s] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
                let results = matches!(search, Search::Results { .. });
                let panel_w = if results || (inspect && !picks.is_empty()) { 52.min(main.width / 2) } else { 0 };
                let [m, panel] = Layout::horizontal([Constraint::Min(1), Constraint::Length(panel_w)]).areas(main);
                map_area = m;
                panel_area = panel;
                f.render_widget(map.view(&draw_cam), m);
                list_rows = match &search {
                    Search::Results { places, sel } if panel_w > 0 => draw_results(f, panel, places, *sel),
                    _ if panel_w > 0 => draw_inspector(f, panel, &picks, sel),
                    _ => Vec::new(),
                };
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
                let ds_busy: usize = datasets.iter().map(|d| d.inflight.len()).sum();
                if !pending.is_empty() || waiting > 0 || ds_busy > 0 {
                    spans.push(Span::styled(format!("loading {} ", pending.len() + waiting + ds_busy), TStyle::new().fg(Color::Yellow)));
                }
                if errors > 0 {
                    spans.push(Span::styled(format!("{errors} failed "), TStyle::new().fg(Color::Red)));
                }
                if let Some(t) = throttled_at.filter(|t| t.elapsed() < Duration::from_secs(60)) {
                    let wait = 60 - t.elapsed().as_secs();
                    spans.push(Span::styled(
                        format!("imagery: provider says too many requests (or daily quota used up), trying again in {wait} s "),
                        TStyle::new().fg(Color::Yellow),
                    ));
                } else if held_back {
                    let n = limiter.as_ref().map_or(0, |b| b.per_min);
                    spans.push(Span::styled(format!("imagery limit {n}/min (--raster-limit) "), TStyle::new().fg(Color::Yellow)));
                }
                // Imagery that only renders zoomed in (Planetary Computer mosaics): say so
                // rather than show nothing.
                for d in datasets.iter().filter(|d| draw_cam.zoom < d.ds.min_zoom) {
                    spans.push(Span::styled(format!("{}: zoom in to {} ", d.ds.name, d.ds.min_zoom), TStyle::new().fg(Color::Yellow)));
                }
                for (name, z) in map.rasters_below_min_zoom(&draw_cam) {
                    spans.push(Span::styled(format!("{name}: zoom in to {z} for imagery "), TStyle::new().fg(Color::Yellow)));
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
                    // Its credit is with the others at the end; this says how it draws.
                    let mode = ["", "· labels ", "· raster only "][vector as usize];
                    let how = if kitty { "" } else { "(cells) " };
                    let name = if r.attribution.is_empty() { format!("{} ", r.name) } else { String::new() };
                    spans.push(Span::styled(format!("imagery {name}{how}{mode}"), TStyle::new().fg(Color::LightBlue)));
                }
                // Each data layer with how many of its features are in view (to compare layers).
                for (name, color) in &legend {
                    spans.push(Span::styled("● ", TStyle::new().fg(*color)));
                    match map.features_in_view(name, &draw_cam, map_area) {
                        Some(n) => spans.push(Span::styled(format!("{name} {} ", thousands(n)), dim)),
                        None => spans.push(Span::styled(format!("{name} "), dim)),
                    }
                }
                spans.push(Span::styled(format!("{frame_ms:.0} ms "), dim));
                // Credits of the sources being drawn, as the style or tiles give them.
                let credits = map.attributions(cam.zoom);
                let credits = if credits.is_empty() { basemap_name.clone() } else { credits.join(" · ") };
                spans.push(Span::styled(format!("· {credits} · q quit"), dim));
                // While typing a search, the status line is the search box.
                if let Search::Typing(q) = &search {
                    spans = vec![
                        Span::styled(" / ", TStyle::new().fg(Color::Black).bg(Color::Rgb(255, 120, 200))),
                        Span::raw(format!(" {q}")),
                        Span::styled("█", TStyle::new().fg(Color::Rgb(255, 120, 200))),
                        Span::styled("  enter search · esc cancel", dim),
                    ];
                } else if let Search::Waiting(q) = &search {
                    spans.insert(1, Span::styled(format!("searching {q}… "), TStyle::new().fg(Color::Yellow)));
                }
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
                if kitty {
                    // Re-place the raster images under the text: images are sent once, then
                    // each frame only moves placements.
                    let cmds = map.kitty_commands(&draw_cam, map_area, &mut kstate);
                    dlog(|| format!("frame: kitty cmds {} bytes, {} KiB of images held, cell_px={cpx:?} map={map_area:?}", cmds.len(), kstate.bytes() >> 10));
                    out.write_all(cmds.as_bytes())?;
                }
                if pixels_on && let Some(f) = map.take_pixels() {
                    // New tile images once; then every tile is placed, above the imagery
                    // (-1000 and up) and under the text, one image pixel per screen pixel.
                    let t = Instant::now();
                    let mut cmds = String::new();
                    for img in &f.new {
                        cmds.push_str(&mapterm::kitty::transmit_rgba(img.id, img.width, img.height, &img.rgba, pixels_shm));
                    }
                    let now: std::collections::HashSet<u32> = f.placed.iter().map(|p| p.id).collect();
                    for id in pixels_placed.difference(&now) {
                        cmds.push_str(&mapterm::kitty::unplace(*id));
                    }
                    for p in &f.placed {
                        cmds.push_str(&mapterm::kitty::place_px(p.id, p.col, p.row, p.x_off, p.y_off, p.src, -500));
                    }
                    for id in &f.freed {
                        cmds.push_str(&mapterm::kitty::free(*id));
                    }
                    // Under the tiles, stretched over the map area's cells.
                    if let Some(b) = &f.backdrop {
                        cmds.push_str(&mapterm::kitty::transmit_rgba(b.id, b.width, b.height, &b.rgba, pixels_shm));
                        cmds.push_str(&mapterm::kitty::place(
                            b.id,
                            1,
                            map_area.x,
                            map_area.y,
                            (0, 0, b.width, b.height),
                            map_area.width,
                            map_area.height,
                            -600,
                        ));
                    }
                    if let Some(old) = std::mem::replace(&mut backdrop_shown, f.backdrop.as_ref().map(|b| b.id)) {
                        cmds.push_str(&mapterm::kitty::free(old));
                    }
                    pixels_placed = now;
                    pixels_pending = f.pending;
                    out.write_all(cmds.as_bytes())?;
                    dlog(|| {
                        format!(
                            "pixels: {} new tiles, {} placed, {} freed, {} bytes, send {:.1} ms, frame {frame_ms:.1} ms",
                            f.new.len(),
                            f.placed.len(),
                            f.freed.len(),
                            cmds.len(),
                            t.elapsed().as_secs_f64() * 1000.0
                        )
                    });
                }
                crossterm::execute!(out, crossterm::terminal::EndSynchronizedUpdate)?;
            }

            // Fetch what the view needs that we don't have yet.
            // While the camera moves, paid imagery (keyed providers, billed per tile) loads for
            // where it will stop (a jump's destination, the end of a zoom or glide), not for
            // every view on the way.
            let target = ctl.target().filter(|_| billed_raster);
            let mut reqs: Vec<_> = map.tile_requests(&draw_cam, map_area).into_iter().filter(|r| target.is_none() || r.source != RASTER).collect();
            if let Some(t) = target {
                reqs.extend(map.tile_requests(&t, map_area).into_iter().filter(|r| r.source == RASTER));
            }
            // Dataset layers: the chunks under the view, into their own slots; far ones dropped.
            {
                let vp = Viewport::new(&draw_cam, map_area);
                let ((x0, y0), (x1, y1)) = (vp.cell_to_lonlat(0, 0), vp.cell_to_lonlat(map_area.width, map_area.height));
                let view = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
                for (di, d) in datasets.iter_mut().enumerate() {
                    for k in d.ds.evict(view, DATASET_BUDGET) {
                        map.remove_part(&d.id, k)?;
                    }
                    let mut busy = (
                        d.inflight.iter().filter(|j| matches!(j, mapterm::geoparquet::Job::Item(_))).count(),
                        d.inflight.iter().filter(|j| !matches!(j, mapterm::geoparquet::Job::Item(_))).count(),
                    );
                    for job in d.ds.wanted(view, draw_cam.zoom) {
                        if d.inflight.contains(&job) {
                            continue;
                        }
                        let (used, slots) =
                            if matches!(job, mapterm::geoparquet::Job::Item(_)) { (&mut busy.0, ITEM_SLOTS) } else { (&mut busy.1, CHUNK_SLOTS) };
                        if *used >= slots {
                            continue;
                        }
                        *used += 1;
                        d.inflight.insert(job);
                        dlog(|| format!("start {} {job:?}", d.id));
                        let (fut, tx) = (d.ds.run(job), dtx.clone());
                        tokio::spawn(async move {
                            let _ = tx.send((di, fut.await));
                        });
                    }
                }
            }
            held_back = false;
            // Downloads start only into free slots, nearest the view's center first (the order
            // tile_requests gives); the rest wait for a later frame, which asks again for the
            // view it then shows. So zooming through levels never queues tiles for views
            // already left, and the view you stop on loads first.
            waiting = 0;
            let mut busy = (pending.values().filter(|r| **r).count(), pending.values().filter(|r| !**r).count());
            for req in reqs {
                if pending.contains_key(&(req.source.clone(), req.id)) {
                    continue;
                }
                let raster = req.is_raster();
                let (used, slots) = if raster { (&mut busy.0, RASTER_SLOTS) } else { (&mut busy.1, VECTOR_SLOTS) };
                if *used >= slots {
                    waiting += 1;
                    continue;
                }
                // The provider said "too many requests" (or its daily quota is used up): ask it
                // nothing for a minute, rather than every waiting tile asking on its own.
                if req.source == RASTER && throttled_at.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
                    continue;
                }
                if req.source == RASTER
                    && let Some(b) = limiter.as_mut()
                    && !b.take()
                {
                    // Over --raster-limit: asked for again on a later frame.
                    held_back = true;
                    continue;
                }
                dlog(|| format!("start {} {:?}", req.source, req.id));
                *used += 1;
                pending.insert((req.source.clone(), req.id), raster);
                spawn_load(req, tx.clone());
            }

            if let Some(g) = google.as_ref().filter(|_| !credit_pending) {
                let vp = Viewport::new(&cam, map_area);
                let (west, north) = vp.cell_to_lonlat(0, 0);
                let (east, south) = vp.cell_to_lonlat(map_area.width, map_area.height);
                let step = ((east - west).abs() / 4.0).max(1e-9);
                let key = (cam.zoom.round() as i32, (cam.lon / step).round() as i64, (cam.lat / step).round() as i64);
                match credit_seen {
                    Some((k, since)) if k == key => {
                        if credit_view != Some(key) && since.elapsed() >= Duration::from_millis(600) && !ctl.animating() {
                            credit_view = Some(key);
                            credit_pending = true;
                            let (g, ctx) = (g.clone(), ctx.clone());
                            let zoom = cam.zoom.round().clamp(0.0, 22.0) as u8;
                            tokio::spawn(async move {
                                let _ = ctx.send(g.google_credits(zoom, (north.min(85.0), south.max(-85.0), east, west)).await);
                            });
                        }
                    }
                    _ => credit_seen = Some((key, Instant::now())),
                }
            }

            // ~60 fps while animating, otherwise wake only for input, tiles, or a slow tick.
            let idle = if ctl.animating() || pixels_pending > 0 || !demo.is_empty() { Duration::from_millis(16) } else { Duration::from_millis(500) };
            tokio::select! {
                Some((di, done)) = drx.recv() => {
                    let d = &mut datasets[di];
                    let job = done.job();
                    d.inflight.remove(&job);
                    dlog(|| format!("done {} {job:?}", d.id));
                    match d.ds.apply(done) {
                        Ok(Some((key, features))) => map.add_part(&d.id, key, features)?,
                        Ok(None) => {
                            if !d.saved && d.ds.items_unread() == 0 {
                                d.ds.save_index(&d.spec);
                                d.saved = true;
                            }
                        }
                        Err(e) => {
                            errors += 1;
                            dlog(|| format!("{}: {e:#}", d.id));
                        }
                    }
                }
                Some(first) = rx.recv() => {
                    let mut batch = vec![first];
                    while let Ok(d) = rx.try_recv() {
                        batch.push(d);
                    }
                    for (source, id, r) in batch {
                        pending.remove(&(source.clone(), id));
                        dlog(|| format!("done {source} {id:?}"));
                        if let Err(e) = map.insert_loaded(&source, id, r) {
                            // The map asks for it again after a growing wait.
                            dlog(|| format!("{e:#}"));
                            if mapterm::map::is_rate_limited(&e) {
                                throttled_at = Some(Instant::now());
                            } else {
                                errors += 1;
                            }
                        }
                    }
                }
                Some(c) = crx.recv() => {
                    credit_pending = false;
                    // On failure keep the credits shown; the next new view asks again.
                    if let Ok(text) = c {
                        let _ = map.set_attribution(RASTER, &text);
                    }
                }
                Some((spec, r)) = srx.recv() => {
                    style_loading = None;
                    match r {
                        Ok((new, th, name)) => {
                            // A source id the new style shares with your own layers would clash.
                            let clash = new.sources().map(|(id, _)| id.to_string()).find(|id| map.source(id).is_some() && !basemap_sources.contains(id));
                            if let Some(id) = clash {
                                note = Some((Instant::now(), format!("style {spec}: source \"{id}\" clashes with a layer of yours")));
                            } else {
                                // Swap the basemap's layers and sources; imagery stays under it,
                                // your layers and markers over it.
                                let ids: Vec<&str> = basemap_ids.iter().map(String::as_str).collect();
                                map.remove_layers(&ids);
                                for id in &basemap_sources {
                                    map.remove_source(id);
                                }
                                basemap_sources = new.sources().map(|(id, _)| id.to_string()).collect();
                                let warnings = new.warnings().len();
                                let before = map.layer_ids().into_iter().find(|id| !is_imagery(id)).map(str::to_string);
                                basemap_ids = map.add_map(new, before.as_deref())?;
                                base = base_visibility(&map);
                                apply_toggles(&mut map, &base, labels, vector, raster.is_some());
                                theme = th;
                                basemap_name = name;
                                let more = if warnings > 0 { format!(" ({warnings} properties not drawn)") } else { String::new() };
                                note = Some((Instant::now(), format!("style: {spec}{more}")));
                            }
                        }
                        Err(e) => note = Some((Instant::now(), format!("style {spec} failed: {e:#}"))),
                    }
                }
                Some((query, r)) = grx.recv() => {
                    // Only the answer to the search still waiting counts.
                    if matches!(&search, Search::Waiting(q) if *q == query) {
                        search = Search::Off;
                        match r {
                            Ok(places) if places.is_empty() => note = Some((Instant::now(), format!("no places found for {query}"))),
                            Ok(mut places) if places.len() == 1 => {
                                let p = places.remove(0);
                                let (lon, lat, z) = p.view(map_area.width, map_area.height);
                                ctl.fly_to(Camera::new(lon, lat, z));
                                found = Some(p);
                            }
                            Ok(places) => search = Search::Results { places, sel: 0 },
                            Err(e) => note = Some((Instant::now(), format!("search failed: {e:#}"))),
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
                                // Typing a search: every key goes to the search box.
                                if let Search::Typing(q) = &mut search {
                                    match k.code {
                                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                                        KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => q.clear(),
                                        KeyCode::Char(c) => q.push(c),
                                        KeyCode::Backspace => {
                                            q.pop();
                                        }
                                        KeyCode::Esc => search = Search::Off,
                                        KeyCode::Enter if !q.trim().is_empty() => {
                                            let query = q.trim().to_string();
                                            let (geo, gtx) = (geocoder.clone(), gtx.clone());
                                            let near = (ctl.cam.lon, ctl.cam.lat, ctl.cam.zoom);
                                            let qq = query.clone();
                                            tokio::spawn(async move {
                                                let r = geo.search(&qq, Some(near), 8).await;
                                                let _ = gtx.send((qq, r));
                                            });
                                            search = Search::Waiting(query);
                                        }
                                        _ => {}
                                    }
                                    continue;
                                }
                                // Choosing among results.
                                if let Search::Results { places, sel: rsel } = &mut search {
                                    let n = places.len();
                                    match k.code {
                                        KeyCode::Down | KeyCode::Tab => {
                                            *rsel = (*rsel + 1) % n;
                                            continue;
                                        }
                                        KeyCode::Up | KeyCode::BackTab => {
                                            *rsel = (*rsel + n - 1) % n;
                                            continue;
                                        }
                                        KeyCode::Enter => {
                                            let p = places.swap_remove(*rsel);
                                            let (lon, lat, z) = p.view(map_area.width, map_area.height);
                                            ctl.fly_to(Camera::new(lon, lat, z));
                                            found = Some(p);
                                            search = Search::Off;
                                            continue;
                                        }
                                        KeyCode::Esc => {
                                            search = Search::Off;
                                            continue;
                                        }
                                        _ => {}
                                    }
                                }
                                if ctl.key(k) {
                                    continue;
                                }
                                match k.code {
                                    KeyCode::Char('/') => {
                                        search = Search::Typing(String::new());
                                    }
                                    KeyCode::Esc if found.is_some() && picks.is_empty() => found = None,
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
                                    KeyCode::Char('n') => {
                                        labels = !labels;
                                        apply_toggles(&mut map, &base, labels, vector, raster.is_some());
                                    }
                                    KeyCode::Char('v') => {
                                        vector = (vector + 1) % 3;
                                        apply_toggles(&mut map, &base, labels, vector, raster.is_some());
                                    }
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
                                    KeyCode::Char('m') if style_loading.is_none() => {
                                        style_at = (style_at + 1) % styles.len();
                                        let spec = styles[style_at].clone();
                                        let (source, dark, cam, stx) = (builtin_source.clone(), theme.map_or(!a.light, |t| t.1), ctl.cam, stx.clone());
                                        let sp = spec.clone();
                                        tokio::spawn(async move {
                                            let r = load_basemap(&sp, &source, dark, cam).await;
                                            let _ = stx.send((sp, r));
                                        });
                                        note = Some((Instant::now(), format!("loading style {spec}…")));
                                        style_loading = Some(spec);
                                    }
                                    KeyCode::Char('c') if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                                        // The pointer's position on the map, else the center.
                                        let (what, (lon, lat)) = match pointer {
                                            Some(d) => ("pointer", Viewport::new(&last_cam, map_area).dot_to_lonlat(d.0, d.1)),
                                            None => ("center", (ctl.cam.lon, ctl.cam.lat)),
                                        };
                                        let text = format!("{lat:.6}, {lon:.6}");
                                        osc52_copy(&text)?;
                                        note = Some((Instant::now(), format!("copied {text} ({what})")));
                                    }
                                    KeyCode::Char('t') => match theme {
                                        Some((schema, dark)) => {
                                            // Swap the basemap layers for the other theme's, in place.
                                            let (layers, _) = stylejson::style_layers(&stylejson::terminal_style(schema, !dark))?;
                                            let ids: Vec<&str> = basemap_ids.iter().map(String::as_str).collect();
                                            map.remove_layers(&ids);
                                            let before = map.layer_ids().into_iter().find(|id| !is_imagery(id)).map(str::to_string);
                                            basemap_ids = layers.iter().map(|l| l.id.clone()).collect();
                                            map.add_layers(layers, before.as_deref())?;
                                            base = base_visibility(&map);
                                            apply_toggles(&mut map, &base, labels, vector, raster.is_some());
                                            theme = Some((schema, !dark));
                                        }
                                        None => note = Some((Instant::now(), "t switches the built-in style's theme; this is --style".into())),
                                    },
                                    _ => {}
                                }
                            }
                            Event::Resize(..) => {
                                cpx = cell_px().unwrap_or(cpx);
                                if kitty {
                                    map.raster_mode = RasterMode::Kitty { cell_px: cpx };
                                }
                                if pixels_on {
                                    map.pixel_mode = Some(cpx);
                                }
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
                                // Where the pointer is on the map (exact with pixel reports), for c.
                                pointer = map_area.contains(pos).then(|| {
                                    let (ax, ay) = (map_area.x as f64, map_area.y as f64);
                                    match ctl.pixel_mouse {
                                        Some((cw, ch)) => ((m.column as f64 / cw - ax) * 2.0, (m.row as f64 / ch - ay) * 4.0),
                                        None => ((cell.0 as f64 - ax) * 2.0 + 1.0, (cell.1 as f64 - ay) * 4.0 + 2.0),
                                    }
                                });
                                // Clicks in the inspector select a feature; the map never sees them.
                                if panel_area.width > 0 && panel_area.contains(pos) {
                                    if m.kind == event::MouseEventKind::Down(event::MouseButton::Left)
                                        && let Some(&(_, i)) = list_rows.iter().find(|(r, _)| *r == cell.1)
                                    {
                                        if let Search::Results { places, .. } = &mut search {
                                            let p = places.swap_remove(i);
                                            let (lon, lat, z) = p.view(map_area.width, map_area.height);
                                            ctl.fly_to(Camera::new(lon, lat, z));
                                            found = Some(p);
                                            search = Search::Off;
                                        } else {
                                            sel = i;
                                        }
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
                                        let under = map.query_features(&last_cam, map_area, dot);
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
                                    let dot = Viewport::new(&last_cam, map_area).dot_ll(k.lon, k.lat);
                                    picks = map.query_features(&last_cam, map_area, dot);
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
    if kitty || pixels_on {
        let _ = std::io::stdout().write_all(mapterm::kitty::FREE_ALL.as_bytes());
    }
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}

/// A token bucket for --raster-limit: holds a minute's worth of requests, refilled steadily.
struct Bucket {
    per_min: u32,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(per_min: u32) -> Self {
        Self { per_min, tokens: per_min as f64, last: Instant::now() }
    }

    /// Take one request if there's one left.
    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * self.per_min as f64 / 60.0).min(self.per_min as f64);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}
