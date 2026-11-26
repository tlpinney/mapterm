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

usage: mapterm [SOURCE | --style STYLE] [--geojson FILE|URL]... [--raster NAME|URL] [--at LAT,LON|here] [-z ZOOM] [--adsb [FEED]] [--ais] [--sats [GROUPS]] [--light] [--no-labels] [--snapshot COLSxROWS [--format F | --pick COL,ROW]]

  SOURCE        .pmtiles file or URL, .mbtiles file, XYZ template with {z}/{x}/{y}, or
                TileJSON URL, drawn with mapterm's terminal style (default: OpenFreeMap planet)
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
  --cache       show mapterm's disk caches (tiles, imagery, geoparquet, buildings, stac,
                and old files from earlier versions): how big, and their limits; then exit
  --cache-clear NAME|all  empty one cache, or all of them, then exit
  --tile-cache SIZE, --imagery-cache SIZE  disk for map tiles and for imagery (default
                2GB each, least recently used out first; also MAPTERM_TILE_CACHE,
                MAPTERM_IMAGERY_CACHE). Each source's cache is an MBTiles file
  --geoparquet-cache SIZE  disk for parts of remote GeoParquet files already read
                (footers and chunks; least recently used out first; default 2GB, 0 keeps
                none; also MAPTERM_GEOPARQUET_CACHE)
  --all-columns  read every column of GeoParquet layers (by default heavy ones, over 5% of
                a chunk such as Overture's ids and sources, are skipped once a read passes
                1 MB; they only show in inspect)
  --buildings-cache SIZE  disk for those tiles, least recently used out first (default
                2GB; e.g. 500MB, 10GB; 0 keeps none between uses; also
                MAPTERM_BUILDINGS_CACHE)
  --bbox W,S,E,N  read a GeoParquet layer once, for this area only (lon/lat), instead of
                loading it as the map moves
  --at          start position (default: fit the GeoJSON layers, else the source's center);
                `here` is where you are (see --location-cmd), zoomed to its accuracy
  --adsb [FEED]  live aircraft over the map, from a free ADS-B feed: lol (adsb.lol, the
                default), fi (adsb.fi), or a URL with {lat}, {lon} and {nm}. Polled every
                10 s for the area in view; between polls planes move along their tracks.
                Arrows point along the track, colored by altitude; i inspects one
  --sats [GROUPS]
                satellites, placed from CelesTrak's orbital elements (no key): ◇ (◆ the
                ISS) colored by orbit height, with names; click or Tab for altitude, speed,
                period, its ground track and, with --at here, its next pass over you.
                GROUPS is CelesTrak's, comma-separated (default active: every active
                satellite, ~17,000 with Starlink's ~11,000 unlabelled; or a smaller group:
                visual, stations, starlink, resource, amateur, weather, gnss, ...).
                p finds one by name or operator. The panel lists its radio transmitters
                (frequency, mode) from the SatNOGS DB, kept a day; p radio, fm, lora...
                filters to the satellites with one active.
                Elements are cached for 2 hours, as CelesTrak asks
  --sats-color orbit|country|mission
                color satellites by orbit class (default: LEO, MEO, GEO, GSO, HEO), by
                owner (the six with the most in a color each, Starlink on its own), or by
                mission (communications, Earth observation, radar/SAR, weather,
                navigation, science, ...); o cycles through them while it runs
  --sats-country CODE[,CODE]
                only the satellites of these owners: SATCAT codes (US, PRC, CIS, JPN, UK,
                ESA, ...) or names (china, russia, japan); also from the p finder
  --ais          live ships from aisstream.io's free AIS stream (a free key from
                aisstream.io: AISSTREAM_API_KEY or --ais-key-cmd). Arrows along the
                course colored by kind (cargo, tanker, passenger, fishing, pleasure, tug);
                streamed for the area in view from zoom 7; click or Tab for details
  --radar       weather radar over the map: rain and snow worldwide from RainViewer (no
                key), the last two hours in 10-minute scans. space plays the two hours
                in a loop (again to pause), , and . step a scan (. past the last goes
                back to now), R hides or shows it. Detail stops at zoom 7
  --ais-key-cmd CMD
                print the aisstream.io API key with a command (else AISSTREAM_API_KEY)
  --adsb-key-cmd CMD
                print an adsb.lol API key with a command (else ADSB_LOL_KEY). Without
                one adsb.lol allows 500 requests a day per address (--adsb asks every
                30 s); a free feeder key allows 7,200 (every 12 s)
  --adsb-radius NM
                the largest circle --adsb asks the feed for (default 60 nm); below zoom 7
                it asks for nothing, and after errors it waits 30 s to 5 min
  --proxy URL   send everything mapterm fetches through a proxy: socks5h://HOST:PORT
                (SOCKS5, host names looked up by the proxy), socks5://, or http://;
                user:pass@ in the URL when it asks. Also MAPTERM_PROXY. Local addresses
                go direct
  --location-cmd CMD
                a command that prints your location as LAT LON [ACCURACY_M], for
                `--at here` and `g` (else MAPTERM_LOCATION_CMD; on macOS, CoreLocationCLI
                when installed: brew install --cask corelocationcli)
  -z            start zoom
  --raster      an image tile layer under the vector map: a provider or stack name
                (--raster-list; `planetary` is Sentinel-2 with NAIP on top when zoomed in),
                a URL with {z}/{x}/{y} ({-y} or --tms for TMS rows, {key} for an API key),
                a .pmtiles file or URL, or a .mbtiles file, of PNG/JPEG/WebP tiles;
                real images with kitty graphics (Ghostty, kitty, WezTerm, Rex, herdr), cell colors
                elsewhere (MAPTERM_GRAPHICS=kitty|cells overrides; MAPTERM_KITTY_MEDIUM=shm|direct
                picks shared memory or inline image data)
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
  --format      with --snapshot: text (default), ansi (24-bit color), html (a <pre> fragment),
                describe (the view in words: labels, areas and roads with positions) or
                png (the view as an image, 16 x 34 pixels per cell, over any --raster
                imagery, no labels; to stdout)
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
    /// --adsb: the live aircraft feed (lol, fi or a URL template).
    adsb: Option<String>,
    /// --sats-color: color satellites by orbit, owner or mission.
    sats_color: SatColor,
    /// --sats-country: owner codes or names to show satellites of.
    sats_country: Vec<String>,
    /// --sats: CelesTrak groups to show.
    sats: Option<Vec<String>>,
    /// --ais: live ships from aisstream.io.
    ais: bool,
    /// --radar: weather radar from RainViewer.
    radar: bool,
    /// --ais-key-cmd: prints the aisstream.io API key.
    ais_key_cmd: Option<String>,
    /// --adsb-key-cmd: prints the adsb.lol API key.
    adsb_key_cmd: Option<String>,
    /// --adsb-radius: the largest circle asked for, nautical miles.
    adsb_radius: f64,
    /// --at here: start where you are.
    at_here: bool,
    /// --location-cmd: prints LAT LON [ACCURACY_M].
    location_cmd: Option<String>,
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
        adsb: None,
        adsb_radius: ADSB_RADIUS,
        adsb_key_cmd: None,
        ais: false,
        radar: false,
        sats: None,
        sats_country: Vec::new(),
        sats_color: SatColor::Orbit,
        ais_key_cmd: None,
        at_here: false,
        location_cmd: None,
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
    if let Ok(v) = std::env::var("MAPTERM_TILE_CACHE") {
        mapterm::tilecache::set_tile_limit(mapterm::msbuildings::parse_size(&v).context("MAPTERM_TILE_CACHE")?);
    }
    if let Ok(v) = std::env::var("MAPTERM_IMAGERY_CACHE") {
        mapterm::tilecache::set_imagery_limit(mapterm::msbuildings::parse_size(&v).context("MAPTERM_IMAGERY_CACHE")?);
    }
    if let Ok(v) = std::env::var("MAPTERM_GEOPARQUET_CACHE") {
        mapterm::geoparquet::set_cache_limit(mapterm::msbuildings::parse_size(&v).context("MAPTERM_GEOPARQUET_CACHE")?);
    }
    if let Ok(v) = std::env::var("MAPTERM_BUILDINGS_CACHE") {
        mapterm::msbuildings::set_cache_limit(mapterm::msbuildings::parse_size(&v).context("MAPTERM_BUILDINGS_CACHE")?);
    }
    if let Ok(v) = std::env::var("MAPTERM_RASTER_LIMIT") {
        a.raster_limit = Some(parse_limit(&v)?);
    }
    let mut it = std::env::args().skip(1).peekable();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--at" => {
                let v = it.next().context("--at LAT,LON")?;
                if v.trim() == "here" {
                    a.at_here = true;
                    continue;
                }
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
            "--all-columns" => mapterm::geoparquet::set_all_columns(true),
            "--cache" => {
                let caches = mapterm::cache::caches();
                let total: u64 = caches.iter().map(|c| c.bytes).sum();
                println!("mapterm's caches in {} ({} in all):\n", mapterm::cache::caches_root().display(), mapterm::cache::size(total));
                for c in &caches {
                    println!("  {:<11} {:>8}  {:>6} files  {}", c.name, mapterm::cache::size(c.bytes), c.files, c.about);
                    println!("  {:<11} {:>8}  limit: {}", "", "", c.limit);
                }
                println!("\nEmpty one with --cache-clear NAME, or all with --cache-clear all.");
                std::process::exit(0);
            }
            "--cache-clear" => {
                let name = it.next().context("--cache-clear NAME|all")?;
                let freed = mapterm::cache::clear(&name)?;
                println!("cleared {name}: {} freed", mapterm::cache::size(freed));
                std::process::exit(0);
            }
            "--tile-cache" => {
                let v = it.next().context("--tile-cache SIZE")?;
                mapterm::tilecache::set_tile_limit(mapterm::msbuildings::parse_size(&v).context("--tile-cache")?);
            }
            "--imagery-cache" => {
                let v = it.next().context("--imagery-cache SIZE")?;
                mapterm::tilecache::set_imagery_limit(mapterm::msbuildings::parse_size(&v).context("--imagery-cache")?);
            }
            "--geoparquet-cache" => {
                let v = it.next().context("--geoparquet-cache SIZE")?;
                mapterm::geoparquet::set_cache_limit(mapterm::msbuildings::parse_size(&v).context("--geoparquet-cache")?);
            }
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
            "--adsb" => {
                // The feed is optional: take the next argument only if it names one.
                let feed = it.next_if(|n| ["lol", "fi", "adsb.lol", "adsb.fi"].contains(&n.as_str()) || n.contains("{lat}"));
                a.adsb = Some(feed.unwrap_or_default());
            }
            "--ais" => a.ais = true,
            "--radar" => a.radar = true,
            "--sats" | "--satellites" => {
                // The groups are optional: take the next argument only if it's a list of
                // group names (not a file, URL or option).
                let groups = it.next_if(|n| {
                    n.starts_with(|c: char| c.is_ascii_alphanumeric())
                        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == ',')
                });
                let groups: Vec<String> = groups.as_deref().unwrap_or("active").split(',').filter(|g| !g.is_empty()).map(str::to_string).collect();
                // CelesTrak asks: the Active list already holds Starlink.
                if groups.iter().any(|g| g == "active") && groups.iter().any(|g| g == "starlink") {
                    bail!("--sats: active already includes starlink (CelesTrak asks not to download both)");
                }
                a.sats = Some(groups);
            }
            "--sats-color" => {
                a.sats_color = match it.next().context("--sats-color orbit|country|mission")?.as_str() {
                    "orbit" => SatColor::Orbit,
                    "country" | "owner" => SatColor::Country,
                    "mission" | "purpose" => SatColor::Mission,
                    other => bail!("--sats-color is orbit, country or mission, not {other:?}"),
                };
            }
            "--sats-country" => {
                let v = it.next().context("--sats-country CODE[,CODE] (PRC, US, JPN, or a name: china)")?;
                for w in v.split(',').filter(|w| !w.trim().is_empty()) {
                    let code = mapterm::satellites::owner_code_for(w).with_context(|| format!("--sats-country: no owner called {w:?}"))?;
                    a.sats_country.push(code.to_string());
                }
                a.sats.get_or_insert_with(|| vec!["active".to_string()]);
            }
            "--proxy" => mapterm::proxy::set(&it.next().context("--proxy URL (socks5h://127.0.0.1:1080)")?)?,
            "--ais-key-cmd" => a.ais_key_cmd = Some(it.next().context("--ais-key-cmd CMD")?),
            "--adsb-key-cmd" => a.adsb_key_cmd = Some(it.next().context("--adsb-key-cmd CMD")?),
            "--adsb-radius" => {
                let nm: f64 = it.next().context("--adsb-radius NM")?.parse().context("--adsb-radius NM")?;
                if !(1.0..=250.0).contains(&nm) {
                    bail!("--adsb-radius is 1 to 250 nautical miles");
                }
                a.adsb_radius = nm;
            }
            "--location-cmd" => a.location_cmd = Some(it.next().context("--location-cmd CMD")?),
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
                let v = it.next().context("--format text|ansi|html|describe|png")?;
                if !matches!(v.as_str(), "text" | "ansi" | "html" | "describe" | "png") {
                    bail!("--format {v}: use text, ansi, html, describe or png");
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
/// The aviation style's entry in the `m` style cycle (and its --style name).
const AVIATION: &str = "mapterm-aviation";

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

/// Pixels per cell for PNG snapshots (a Retina-sized terminal cell): 100 columns x 30 rows
/// make a 1600 x 1020 image.
const PNG_CELL: (u32, u32) = (16, 34);

/// A pixel-mode frame put together from its tiles as the terminal would place them, over
/// `under` (imagery from [`mapterm::Map::raster_rgba`]) or white: RGBA rows and the size.
fn compose_png(f: &mapterm::pixels::PixFrame, area: Rect, under: Option<Vec<u8>>) -> (Vec<u8>, u32, u32) {
    let (cw, ch) = PNG_CELL;
    let (w, h) = (area.width as u32 * cw, area.height as u32 * ch);
    let mut img = under.filter(|u| u.len() == (w * h * 4) as usize).unwrap_or_else(|| vec![255u8; (w * h * 4) as usize]);
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
                // Over what's there (white at first), by the tile's alpha.
                let a = src.rgba[s + 3] as u32;
                for c in 0..3 {
                    img[o + c] = ((src.rgba[s + c] as u32 * a + img[o + c] as u32 * (255 - a)) / 255) as u8;
                }
                img[o + 3] = 255;
            }
        }
    }
    (img, w, h)
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
/// The --adsb source and layer.
const AIRCRAFT: &str = "aircraft";
/// The largest circle --adsb asks for by default, nautical miles (the feeds take 250).
const ADSB_RADIUS: f64 = 60.0;
/// Below this zoom --adsb asks for nothing: a region's worth of aircraft every 5 s is more
/// than a free feed should carry for one viewer.
const ADSB_MIN_ZOOM: f64 = 7.0;
/// Seconds of trail behind each aircraft.
const ADSB_TRAIL: f64 = 90.0;
/// The --adsb trails' source and layer.
const TRAILS: &str = "aircraft-trails";
/// One trail image to draw: whether its layer is on, its paths and colors, the time and
/// seconds of trail, its kitty image id and z, and whether it's up.
type TrailSet<'a> = (bool, Vec<(&'a mapterm::timeseries::SampledPosition, (u8, u8, u8))>, f64, f64, u32, i32, &'a mut bool);

/// The kitty image id of the --adsb trail image (far from the tile ids).
const TRAIL_IMAGE: u32 = 4_000_000_002;
/// The --ais source and layer, and its trail image.
const SHIPS: &str = "ships";
const SHIP_TRAIL_IMAGE: u32 = 4_000_000_003;
/// The kitty image of paths: the selected satellite's ground track (over the trails).
const PATH_IMAGE: u32 = 4_000_000_005;
/// The kitty image of satellite dots and their names.
const SAT_IMAGE: u32 = 4_000_000_006;
/// --ais draws ships this many seconds in the past (ships under way report every 2 to 10 s;
/// small boats every 30 s, which past the last fix move along their course).
const AIS_DELAY: f64 = 10.0;
/// Seconds of trail behind each ship.
const AIS_TRAIL: f64 = 600.0;
/// Below this zoom --ais streams nothing.
const AIS_MIN_ZOOM: f64 = 7.0;
/// Something live the finder can go to.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    /// A satellite owner (SATCAT code), or None: all satellites again.
    Country(Option<String>),
    /// Satellite missions (one, or imaging: Earth observation and radar together).
    Missions(Vec<mapterm::satellites::Mission>),
    /// A satellite operator (Planet, Vantor...).
    Operator(&'static str),
    /// Satellites with an active downlink: any (""), or in this mode.
    Radio(String),
    /// Satellites you can transmit to: an active uplink (repeaters, transponders).
    Uplink,
    /// Satellites with a voice or a data channel.
    Usage(mapterm::satnogs::Use),
    Sat(u64),
    Ship(u32),
    Plane(String),
}

/// One finder result: what it is, its name, and a word or two about it.
struct Hit {
    target: Target,
    name: String,
    detail: String,
}

/// The finder: what's typed, the chosen row, and the matches, best first.
struct Finder {
    query: String,
    sel: usize,
    hits: Vec<Hit>,
}

/// Everything live matching `query` fzf-style, best first (the first 200).
fn find(
    query: &str,
    sats: &[mapterm::satellites::Sat],
    hidden: &[mapterm::satellites::Sat],
    filter: &SatFilter,
    aircraft: &mapterm::adsb::Tracks,
    fleet: &mapterm::ais::Fleet,
) -> Vec<Hit> {
    let mut v: Vec<(i32, Hit)> = Vec::new();
    // Countries first: the owners among the satellites, with how many each has.
    let mut owners: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for s in sats.iter().chain(hidden) {
        if let Some(c) = &s.catalog {
            *owners.entry(c.owner.as_str()).or_insert(0) += 1;
        }
    }
    if filter.on() && mapterm::fuzzy::score(query, "show all satellites every country mission operator").is_some() {
        v.push((
            10_000,
            Hit { target: Target::Country(None), name: "Show all satellites".into(), detail: format!("{} satellites", sats.len() + hidden.len()) },
        ));
    }
    // Missions too, with how many each has: "sar", "weather", "comms".
    if !query.trim().is_empty() {
        let mut by: std::collections::HashMap<mapterm::satellites::Mission, usize> = std::collections::HashMap::new();
        for s in sats.iter().chain(hidden) {
            *by.entry(s.mission()).or_insert(0) += 1;
        }
        for (m, n) in &by {
            if let Some(sc) = mapterm::fuzzy::score(query, &format!("{} {}", m.label(), mission_words(*m))) {
                v.push((sc + 40, Hit { target: Target::Missions(vec![*m]), name: m.label().to_string(), detail: format!("mission · {n} satellites") }));
            }
        }
        // Imaging: optical and radar together.
        use mapterm::satellites::Mission::{EarthObservation, Radar};
        if let Some(sc) = mapterm::fuzzy::score(query, "imaging imagery images earth observation eo sar") {
            let n = by.get(&EarthObservation).unwrap_or(&0) + by.get(&Radar).unwrap_or(&0);
            v.push((
                sc + 45,
                Hit {
                    target: Target::Missions(vec![EarthObservation, Radar]),
                    name: "imaging (Earth observation + SAR)".into(),
                    detail: format!("mission · {n} satellites"),
                },
            ));
        }
        // Operators, with how many each has: "planet", "vantor", "digitalglobe".
        let mut ops: std::collections::HashMap<&'static str, usize> = std::collections::HashMap::new();
        for s in sats.iter().chain(hidden) {
            if let Some(op) = s.operator() {
                *ops.entry(op).or_insert(0) += 1;
            }
        }
        for (op, n) in &ops {
            if let Some(sc) = mapterm::fuzzy::score(query, op) {
                let name = op.split(" (").next().unwrap_or(op);
                v.push((sc + 42, Hit { target: Target::Operator(op), name: name.to_string(), detail: format!("operator · {n} satellites") }));
            }
        }
        // Radio, from the SatNOGS DB: any active downlink, or one mode ("fm", "lora").
        let mut modes: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let (mut any, mut up, mut voice, mut data) = (0, 0, 0, 0);
        for s in sats.iter().chain(hidden) {
            let mut seen: Vec<&str> = Vec::new();
            for t in s.radio.iter().filter(|t| t.active && t.downlink_low.is_some()) {
                if let Some(m) = t.mode.as_deref()
                    && !seen.contains(&m)
                {
                    seen.push(m);
                    *modes.entry(m).or_insert(0) += 1;
                }
            }
            any += usize::from(s.radio.iter().any(|t| t.active && t.downlink_low.is_some()));
            up += usize::from(s.radio.iter().any(two_way));
            let live = |u| s.radio.iter().any(|t| t.active && t.downlink_low.is_some() && t.usage() == Some(u));
            voice += usize::from(live(mapterm::satnogs::Use::Voice));
            data += usize::from(live(mapterm::satnogs::Use::Data));
        }
        for (u, n, words, name) in [
            (mapterm::satnogs::Use::Voice, voice, "voice fm ssb phone repeater", "voice (FM, SSB)"),
            (mapterm::satnogs::Use::Data, data, "data packet aprs telemetry digital", "data (packet, telemetry)"),
        ] {
            if n > 0
                && let Some(sc) = mapterm::fuzzy::score(query, words)
            {
                v.push((sc + 44, Hit { target: Target::Usage(u), name: name.into(), detail: format!("SatNOGS · {n} satellites") }));
            }
        }
        if up > 0
            && let Some(sc) = mapterm::fuzzy::score(query, "uplink repeater transponder transceiver two-way")
        {
            v.push((
                sc + 44,
                Hit { target: Target::Uplink, name: "with an uplink (repeaters, transponders)".into(), detail: format!("SatNOGS · {up} satellites") },
            ));
        }
        if any > 0
            && let Some(sc) = mapterm::fuzzy::score(query, "radio transmitters frequencies downlink satnogs")
        {
            v.push((
                sc + 38,
                Hit { target: Target::Radio(String::new()), name: "with radio (an active downlink)".into(), detail: format!("SatNOGS · {any} satellites") },
            ));
        }
        let q = query.trim().to_lowercase();
        let q = q.strip_prefix("radio").unwrap_or(&q).trim();
        if q.len() >= 2 {
            for (m, n) in &modes {
                if m.to_lowercase().starts_with(q) {
                    let exact = if m.eq_ignore_ascii_case(q) { 20 } else { 0 };
                    v.push((
                        150 + exact + (*n as f64).log10() as i32,
                        Hit { target: Target::Radio(m.to_string()), name: format!("radio: {m}"), detail: format!("SatNOGS · {n} satellites") },
                    ));
                }
            }
        }
    }
    if !query.trim().is_empty() {
        let q = query.trim().to_lowercase();
        for (code, n) in &owners {
            let name = mapterm::satellites::owner_name(code).unwrap_or(code);
            let text = format!("{name} {code} {}", mapterm::satellites::owner_aliases(code));
            if let Some(sc) = mapterm::fuzzy::score(query, &text) {
                // The country the word is (china: PRC) beats one whose name merely starts
                // with it (China/Brazil); then the one with more satellites.
                let exact = if code.eq_ignore_ascii_case(&q) || mapterm::satellites::owner_says(code, name, &q) { 60 } else { 0 };
                let size = (*n as f64).log10().round() as i32;
                v.push((
                    sc + 40 + exact + size,
                    Hit { target: Target::Country(Some(code.to_string())), name: name.to_string(), detail: format!("country · {n} satellites") },
                ));
            }
        }
    }
    for s in sats.iter().chain(hidden) {
        // The operator matches too (so "digitalglobe" finds WorldView and Legion), scored a
        // little below a name match.
        let text = format!("{} {} {}", s.name, s.norad, s.intl.as_deref().unwrap_or(""));
        let by_name = mapterm::fuzzy::score(query, &text);
        let by_operator = s.operator().and_then(|op| mapterm::fuzzy::score(query, op)).map(|sc| sc - 8);
        let by_country = s
            .catalog
            .as_ref()
            .and_then(|c| {
                mapterm::fuzzy::score(
                    query,
                    &format!("{} {}", mapterm::satellites::owner_name(&c.owner).unwrap_or(""), mapterm::satellites::owner_aliases(&c.owner)),
                )
            })
            .map(|sc| sc - 12);
        let by_mission = mapterm::fuzzy::score(query, s.mission().label()).map(|sc| sc - 12);
        if let Some(sc) = by_name.max(by_operator).max(by_country).max(by_mission) {
            let detail = match s.operator() {
                Some(op) => format!("{} · {}", op.split(" (").next().unwrap_or(op), s.norad),
                None => format!("satellite · {}", s.norad),
            };
            v.push((sc, Hit { target: Target::Sat(s.norad), name: s.name.clone(), detail }));
        }
    }
    for a in aircraft.all() {
        let name = a.flight.clone().or_else(|| a.registration.clone()).unwrap_or_else(|| a.hex.to_uppercase());
        let text = format!("{name} {} {} {}", a.registration.as_deref().unwrap_or(""), a.hex, a.type_code.as_deref().unwrap_or(""));
        if let Some(sc) = mapterm::fuzzy::score(query, &text) {
            v.push((sc, Hit { target: Target::Plane(a.hex.clone()), name, detail: format!("aircraft · {}", a.type_code.as_deref().unwrap_or("?")) }));
        }
    }
    for s in fleet.all() {
        let text = format!("{} {}", s.label(), s.details.mmsi);
        if let Some(sc) = mapterm::fuzzy::score(query, &text) {
            v.push((
                sc,
                Hit { target: Target::Ship(s.details.mmsi), name: s.label(), detail: format!("ship · {}", mapterm::ais::category(s.details.ship_type)) },
            ));
        }
    }
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    v.into_iter().take(200).map(|(_, h)| h).collect()
}

/// The credits as lines `width` wide: each line starts with a space, credits are joined
/// with " · " and broken between credits, and only a credit wider than a whole line is
/// broken inside (at a space when it can be).
fn credit_lines(items: &[String], width: usize) -> Vec<String> {
    let room = width.max(8) - 1;
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let len = |s: &str| s.chars().count();
    for item in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
        let need = if cur.is_empty() { len(item) } else { len(&cur) + 3 + len(item) };
        if need <= room {
            if !cur.is_empty() {
                cur += " · ";
            }
            cur += item;
            continue;
        }
        if !cur.is_empty() {
            lines.push(format!(" {cur}"));
            cur.clear();
        }
        // Too long for a line by itself: break it by words, or by characters.
        let mut rest: Vec<char> = item.chars().collect();
        while rest.len() > room {
            let cut = rest[..=room].iter().rposition(|c| *c == ' ').filter(|&i| i > 0).unwrap_or(room);
            lines.push(format!(" {}", rest[..cut].iter().collect::<String>().trim_end()));
            rest = rest[cut..].iter().copied().skip_while(|c| *c == ' ').collect();
        }
        cur = rest.into_iter().collect();
    }
    if !cur.is_empty() {
        lines.push(format!(" {cur}"));
    }
    lines
}

/// How satellites are colored (o cycles through them).
#[derive(Clone, Copy, Debug, PartialEq)]
enum SatColor {
    Orbit,
    Country,
    Mission,
}

/// Other words for a mission, for the finder.
fn mission_words(m: mapterm::satellites::Mission) -> &'static str {
    use mapterm::satellites::Mission::*;
    match m {
        Communications => "comms telecom internet broadband",
        EarthObservation => "eo imaging optical earth observation",
        Radar => "sar synthetic aperture radar",
        Weather => "meteorology met",
        Navigation => "gps gnss positioning",
        Science => "research geodesy",
        Technology => "engineering demo",
        Amateur => "ham radio education cubesat",
        Military => "defense defence",
        SpaceStation => "iss tiangong css",
        CrewCargo => "crew cargo capsule spacecraft soyuz progress dragon shenzhou tianzhou cygnus",
        Other => "unknown",
    }
}

/// The satellites' coloring (by orbit, owner or mission) and its legend.
fn recolor(sats: &[mapterm::satellites::Sat], how: SatColor) -> (mapterm::satellites::Coloring, Option<mapterm::legend::Legend>) {
    let c = match how {
        SatColor::Orbit => mapterm::satellites::Coloring::Orbit,
        SatColor::Country => mapterm::satellites::Coloring::by_country(sats),
        SatColor::Mission => mapterm::satellites::Coloring::Mission,
    };
    let l = (!sats.is_empty()).then(|| c.legend(sats));
    (c, l)
}

/// Clear an image (`w` pixels wide, drawn over `area` in cells of `cell_px` pixels) where
/// `holes` (cell rectangles, such as the legends) are, since images under the text would
/// otherwise show through the boxes drawn there.
fn punch(rgba: &mut [u8], w: u32, area: Rect, holes: &[Rect], cell_px: (f64, f64)) {
    let h = (rgba.len() / 4) as u32 / w.max(1);
    for r in holes {
        let x0 = (((r.x.saturating_sub(area.x)) as f64 * cell_px.0).floor() as u32).min(w);
        let y0 = (((r.y.saturating_sub(area.y)) as f64 * cell_px.1).floor() as u32).min(h);
        let x1 = (((r.right().saturating_sub(area.x)) as f64 * cell_px.0).ceil() as u32).min(w);
        let y1 = (((r.bottom().saturating_sub(area.y)) as f64 * cell_px.1).ceil() as u32).min(h);
        for y in y0..y1 {
            let row = (y * w) as usize * 4;
            rgba[row + x0 as usize * 4..row + x1 as usize * 4].fill(0);
        }
    }
}

/// Which satellites to show: owners, missions and an operator (each empty for any), from
/// the finder or --sats-country.
#[derive(Clone, Debug, Default, PartialEq)]
struct SatFilter {
    countries: Vec<String>,
    missions: Vec<mapterm::satellites::Mission>,
    operator: Option<&'static str>,
    /// An active downlink: any (""), or in this mode.
    radio: Option<String>,
    /// An active uplink as well: something to transmit to.
    uplink: bool,
    /// Voice or data.
    usage: Option<mapterm::satnogs::Use>,
}

/// A transmitter you can talk through: active, with an uplink and a downlink.
fn two_way(t: &mapterm::satnogs::Transmitter) -> bool {
    t.active && t.uplink_low.is_some() && t.downlink_low.is_some()
}

impl SatFilter {
    /// Whether it leaves any out.
    fn on(&self) -> bool {
        !self.countries.is_empty() || !self.missions.is_empty() || self.operator.is_some() || self.radio.is_some() || self.uplink || self.usage.is_some()
    }

    /// Whether any of the radio conditions is on.
    fn radio_on(&self) -> bool {
        self.radio.is_some() || self.uplink || self.usage.is_some()
    }

    /// Whether one transmitter meets every radio condition (so "uplink" with "voice" is a
    /// voice channel you can transmit on, not an uplink and a voice channel apart).
    fn radio_keeps(&self, t: &mapterm::satnogs::Transmitter) -> bool {
        t.active
            && t.downlink_low.is_some()
            && self.radio.as_deref().is_none_or(|m| m.is_empty() || t.mode.as_deref() == Some(m))
            && (!self.uplink || two_way(t))
            && self.usage.is_none_or(|u| t.usage() == Some(u))
    }

    /// Whether `s` is shown.
    fn keeps(&self, s: &mapterm::satellites::Sat) -> bool {
        (self.countries.is_empty() || s.catalog.as_ref().is_some_and(|c| self.countries.contains(&c.owner)))
            && (self.missions.is_empty() || self.missions.contains(&s.mission()))
            && self.operator.is_none_or(|op| s.operator() == Some(op))
            && (!self.radio_on() || s.radio.iter().any(|t| self.radio_keeps(t)))
    }

    /// What it shows, for the status line: " · China · imaging", or nothing.
    fn describe(&self) -> String {
        let mut out = String::new();
        match self.countries.as_slice() {
            [] => {}
            [one] => out += &format!(" · {}", country_short(one)),
            many => out += &format!(" · {} countries", many.len()),
        }
        match self.missions.as_slice() {
            [] => {}
            [one] => out += &format!(" · {}", one.label()),
            _ => out += " · imaging",
        }
        if let Some(op) = self.operator {
            out += &format!(" · {}", op.split(" (").next().unwrap_or(op));
        }
        match self.radio.as_deref() {
            None => {}
            Some("") => out += " · radio",
            Some(m) => out += &format!(" · radio {m}"),
        }
        if let Some(u) = self.usage {
            out += &format!(" · {}", u.label());
        }
        if self.uplink {
            out += " · uplink";
        }
        out
    }
}

/// Keep `sats` to those `filter` shows, the rest in `hidden`.
fn filter_sats(sats: &mut Vec<mapterm::satellites::Sat>, hidden: &mut Vec<mapterm::satellites::Sat>, filter: &SatFilter) {
    sats.append(hidden);
    if !filter.on() {
        return;
    }
    let (keep, rest): (Vec<_>, Vec<_>) = std::mem::take(sats).into_iter().partition(|s| filter.keeps(s));
    *sats = keep;
    *hidden = rest;
}

/// A short name for an owner code, for the status line (`PRC`: China).
fn country_short(code: &str) -> String {
    match mapterm::satellites::owner_aliases(code).split(' ').next().filter(|a| !a.is_empty()) {
        Some(alias) if code != "SKOR" && code != "NKOR" => alias.to_string(),
        _ => mapterm::satellites::owner_name(code).unwrap_or(code).to_string(),
    }
}

/// The finder's list, fzf-style: the chosen row reversed, the best at the top.
fn draw_finder(f: &mut ratatui::Frame, area: Rect, fd: &Finder) {
    use ratatui::style::Modifier;
    use ratatui::widgets::Borders;
    let dim = TStyle::new().fg(Color::DarkGray);
    let more = if fd.hits.len() == 200 { "200+" } else { "" };
    let count = if more.is_empty() { fd.hits.len().to_string() } else { more.to_string() };
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(format!(" find · {count} "));
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let rows = inner.height as usize;
    // Keep the chosen row in view.
    let first = fd.sel.saturating_sub(rows.saturating_sub(1));
    let w = inner.width.saturating_sub(2) as usize;
    let lines: Vec<Line> = fd
        .hits
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, h)| {
            let room = w.saturating_sub(h.detail.chars().count() + 2);
            let name: String = h.name.chars().take(room).collect();
            let pad = " ".repeat(room.saturating_sub(name.chars().count()));
            let st = if i == fd.sel { TStyle::new().add_modifier(Modifier::REVERSED) } else { TStyle::new() };
            Line::from(vec![Span::styled(format!(" {name}{pad}"), st), Span::styled(format!("  {}", h.detail), dim)])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// The --sats source and layer.
const SATS: &str = "satellites";

/// `HH:MM` (UTC) of a Unix time.
fn hhmm(t: f64) -> String {
    let s = t.rem_euclid(86400.0) as u64;
    format!("{:02}:{:02}", s / 3600, s / 60 % 60)
}

/// Seconds since the Unix epoch, the clock aircraft fixes are kept on.
fn unix_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The circle around the view: its center and the distance to a corner, in nautical miles.
fn view_circle(cam: &Camera, area: Rect) -> (f64, f64, f64) {
    let vp = Viewport::new(cam, area);
    let corner = vp.cell_to_lonlat(0, 0);
    (cam.lon, cam.lat, mapterm::adsb::distance_nm((cam.lon, cam.lat), corner).max(1.0))
}

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
/// The --ais panel for the selected ship: who it is and where it's going, live.
/// The --sats panel for the selected satellite: its orbit, and when it next passes over you.
#[allow(clippy::too_many_arguments)]
fn draw_sat(
    f: &mut ratatui::Frame,
    area: Rect,
    (s, fix): (&mapterm::satellites::Sat, mapterm::satellites::Fix),
    elements_age: f64,
    pass: Option<Option<mapterm::satellites::Pass>>,
    now: f64,
    follow: bool,
    me: Option<&mapterm::locate::Fix>,
) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" satellite ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let key = |k: &str| Span::styled(format!(" {k:<13}"), dim);
    let class = s.orbit_class();
    let (r, g, b) = class.rgb();
    let mut l = vec![Line::from(Span::styled(format!(" {} {}", s.glyph(), s.name), TStyle::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)))];
    l.push(Line::default());
    l.push(Line::from(vec![key("altitude"), Span::raw(format!("{:.0} km", fix.alt_km))]));
    l.push(Line::from(vec![key("speed"), Span::raw(format!("{:.2} km/s", fix.speed)), Span::styled(format!("  {:.0} km/h", fix.speed * 3600.0), dim)]));
    l.push(Line::from(vec![key("orbit"), Span::styled(class.short(), TStyle::new().fg(Color::Rgb(r, g, b))), Span::raw(format!(" · {}", class.long()))]));
    let (perigee, apogee) = s.perigee_apogee();
    let between = if (apogee - perigee).abs() < 20.0 { format!("{:.0} km", (perigee + apogee) / 2.0) } else { format!("{perigee:.0} to {apogee:.0} km") };
    l.push(Line::from(vec![key("height"), Span::raw(between), Span::styled("  perigee to apogee", dim)]));
    l.push(Line::from(vec![key("period"), Span::raw(format!("{:.1} min", s.period_min())), Span::styled(format!("  {:.1}° inclined", s.inclination), dim)]));
    l.push(Line::from(vec![key("position"), Span::raw(format!("{:.3}, {:.3}", fix.lat, fix.lon))]));
    l.push(Line::from(vec![key("mission"), Span::raw(s.mission().label())]));
    if let Some(op) = s.operator() {
        l.push(Line::from(vec![key("operator"), Span::raw(op)]));
    }
    if let Some(c) = &s.catalog {
        let name = mapterm::satellites::owner_name(&c.owner).unwrap_or("?");
        l.push(Line::from(vec![key("country"), Span::raw(name.to_string()), Span::styled(format!("  {}", c.owner), dim)]));
        if let Some(d) = &c.launched {
            l.push(Line::from(vec![key("launched"), Span::raw(d.clone())]));
        }
    }
    l.push(Line::from(vec![key("NORAD"), Span::raw(s.norad.to_string())]));
    if let Some(i) = &s.intl {
        l.push(Line::from(vec![key("launched as"), Span::raw(i.clone())]));
    }
    l.push(Line::from(vec![key("elements"), Span::raw(format!("{:.0} min old", elements_age / 60.0))]));
    if let Some(me) = me {
        l.push(Line::default());
        let (az, el) = mapterm::satellites::look((me.lon, me.lat, 0.0), &fix);
        if el > 0.0 {
            l.push(Line::from(vec![key("in your sky"), Span::styled(format!("{el:.0}° up, toward {az:03.0}°"), TStyle::new().fg(Color::Green))]));
        }
        match pass {
            Some(Some(p)) => {
                let mins = |t: f64| ((t - now) / 60.0).max(0.0);
                let when = if p.rise <= now { "now".to_string() } else { format!("{} UTC, in {:.0} min", hhmm(p.rise), mins(p.rise)) };
                l.push(Line::from(vec![key("next pass"), Span::raw(when)]));
                l.push(Line::from(vec![key("highest"), Span::raw(format!("{:.0}° at {} UTC", p.max_elevation, hhmm(p.peak)))]));
                l.push(Line::from(vec![key("sets"), Span::raw(format!("{} UTC", hhmm(p.set)))]));
            }
            Some(None) => l.push(Line::from(vec![key("next pass"), Span::styled("none over 10° in 24 h", dim)])),
            None => {}
        }
    }
    if !s.radio.is_empty() {
        l.push(Line::default());
        l.push(Line::from(vec![key("radio"), Span::styled("SatNOGS DB", dim)]));
        let active: Vec<&mapterm::satnogs::Transmitter> = s.radio.iter().filter(|t| t.active).collect();
        let inactive = s.radio.len() - active.len();
        // As many as fit above the footer, three at least.
        let room = (inner.height as usize).saturating_sub(l.len() + 3).max(3);
        let width = inner.width as usize;
        for t in active.iter().take(room) {
            let (arrow, freq) = match (t.downlink(), t.uplink()) {
                (Some(d), _) => ("↓", d),
                (None, Some(u)) => ("↑", u),
                (None, None) => (" ", "?".to_string()),
            };
            let mode = t.mode.clone().unwrap_or_default();
            let head = format!(" {arrow} {freq:<9} {mode:<5} ");
            let mut what = t.description.clone();
            if t.downlink().is_some()
                && let Some(up) = t.uplink()
            {
                what = format!("{what} · up {up}");
            }
            let left = width.saturating_sub(head.chars().count() + usize::from(t.frequency_violation) * 2);
            if what.chars().count() > left {
                what = what.chars().take(left.saturating_sub(1)).collect::<String>() + "…";
            }
            let mut spans =
                vec![Span::raw(format!(" {arrow} {freq:<9} ")), Span::styled(format!("{mode:<5} "), TStyle::new().fg(Color::Cyan)), Span::styled(what, dim)];
            if t.frequency_violation {
                spans.push(Span::styled(" ⚠", TStyle::new().fg(Color::Yellow)));
            }
            l.push(Line::from(spans));
        }
        let more = active.len().saturating_sub(room);
        let mut rest = Vec::new();
        if active.is_empty() {
            rest.push("none active".to_string());
        }
        if more > 0 {
            rest.push(format!("{more} more"));
        }
        if inactive > 0 {
            rest.push(format!("{inactive} inactive"));
        }
        if !rest.is_empty() {
            l.push(Line::from(Span::styled(format!("   {}", rest.join(" · ")), dim)));
        }
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(format!(" f {} · esc close", if follow { "stop following" } else { "follow" }), dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

fn draw_ship(
    f: &mut ratatui::Frame,
    area: Rect,
    (s, at): (&mapterm::ais::Ship, mapterm::timeseries::Sample),
    now: f64,
    follow: bool,
    me: Option<&mapterm::locate::Fix>,
) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" ship ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let d = &s.details;
    let key = |k: &str| Span::styled(format!(" {k:<13}"), dim);
    let cat = mapterm::ais::category(d.ship_type);
    let (r, g, b) = mapterm::ais::category_rgb(cat);
    let mut l = vec![Line::from(vec![
        Span::styled(format!(" {} {}", s.glyph(), s.label()), TStyle::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {cat}"), dim),
    ])];
    l.push(Line::default());
    if let Some(st) = s.status.map(mapterm::ais::status_text).filter(|t| !t.is_empty()) {
        let style = if st.contains("distress") || st == "aground" { TStyle::new().fg(Color::Red).add_modifier(Modifier::BOLD) } else { TStyle::new() };
        l.push(Line::from(vec![key("status"), Span::styled(st, style)]));
    }
    match s.sog {
        Some(v) => l.push(Line::from(vec![key("speed"), Span::raw(format!("{v:.1} kn")), Span::styled(format!("  {:.1} km/h", v * 1.852), dim)])),
        None => l.push(Line::from(vec![key("speed"), Span::styled("unknown", dim)])),
    }
    if let Some(c) = s.cog.filter(|_| s.sog.is_some_and(|v| v >= 0.5)) {
        l.push(Line::from(vec![key("course"), Span::raw(format!("{:03.0}°", c.rem_euclid(360.0)))]));
    }
    if let Some(dest) = &d.destination {
        l.push(Line::from(vec![key("destination"), Span::raw(dest.clone())]));
    }
    if let Some(eta) = &d.eta {
        l.push(Line::from(vec![key("ETA"), Span::raw(format!("{eta} UTC"))]));
    }
    if let Some((len, beam)) = d.size {
        l.push(Line::from(vec![key("size"), Span::raw(format!("{len:.0} × {beam:.0} m"))]));
    }
    if let Some(dr) = d.draught {
        l.push(Line::from(vec![key("draught"), Span::raw(format!("{dr:.1} m"))]));
    }
    if let Some(c) = &d.callsign {
        l.push(Line::from(vec![key("call sign"), Span::raw(c.clone())]));
    }
    if let Some(i) = d.imo {
        l.push(Line::from(vec![key("IMO"), Span::raw(i.to_string())]));
    }
    l.push(Line::from(vec![key("MMSI"), Span::raw(d.mmsi.to_string())]));
    l.push(Line::from(vec![key("position"), Span::raw(format!("{:.4}, {:.4}", at.lat, at.lon))]));
    if let Some(me) = me {
        let nm = mapterm::adsb::distance_nm((me.lon, me.lat), (at.lon, at.lat));
        l.push(Line::from(vec![key("from you"), Span::raw(format!("{nm:.1} nm")), Span::styled(format!("  {:.1} km", nm * 1.852), dim)]));
    }
    l.push(Line::from(vec![key("last report"), Span::raw(format!("{:.0} s ago", (now - s.seen).max(0.0)))]));
    if d.ship_type.is_none() && d.destination.is_none() {
        l.push(Line::from(Span::styled(" (details arrive every few minutes)", dim)));
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(format!(" f {} · esc close", if follow { "stop following" } else { "follow" }), dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// The --adsb panel for the selected aircraft: who it is and what it's doing, live.
fn draw_aircraft(
    f: &mut ratatui::Frame,
    area: Rect,
    (a, at, seen): (&mapterm::adsb::Aircraft, mapterm::timeseries::Sample, f64),
    now: f64,
    follow: bool,
    me: Option<&mapterm::locate::Fix>,
) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" aircraft ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let key = |k: &str| Span::styled(format!(" {k:<13}"), dim);
    let name = a.flight.clone().or_else(|| a.registration.clone()).unwrap_or_else(|| a.hex.to_uppercase());
    let mut d = vec![Line::from(vec![
        Span::styled(format!(" {} {name}", mapterm::adsb::arrow(a)), TStyle::new().fg(Color::Rgb(127, 178, 255)).add_modifier(Modifier::BOLD)),
        Span::styled(a.type_code.as_deref().map(|t| format!("  {t}")).unwrap_or_default(), dim),
    ])];
    for s in [a.desc.as_deref(), a.operator.as_deref()].into_iter().flatten() {
        d.push(Line::from(Span::raw(format!(" {s}"))));
    }
    d.push(Line::default());
    if let Some(e) = &a.emergency {
        d.push(Line::from(vec![key("emergency"), Span::styled(e.to_uppercase(), TStyle::new().fg(Color::Red).add_modifier(Modifier::BOLD))]));
    }
    let thousands = |n: f64| {
        let s = format!("{:.0}", n.abs());
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        if n < 0.0 { format!("-{out}") } else { out }
    };
    let altitude = match (a.on_ground, at.alt) {
        (true, _) => "on the ground".to_string(),
        (false, Some(ft)) => format!("{} ft", thousands(ft)),
        _ => "unknown".to_string(),
    };
    let climb = match a.vert_rate {
        Some(r) if !a.on_ground && r.abs() >= 200.0 => format!("  {} {} ft/min", if r > 0.0 { "↑" } else { "↓" }, thousands(r.abs())),
        _ => String::new(),
    };
    d.push(Line::from(vec![key("altitude"), Span::raw(altitude), Span::styled(climb, dim)]));
    if let Some(gs) = a.gs_kt {
        d.push(Line::from(vec![key("speed"), Span::raw(format!("{gs:.0} kt")), Span::styled(format!("  {:.0} km/h", gs * 1.852), dim)]));
    }
    if let Some(t) = a.track {
        d.push(Line::from(vec![key("track"), Span::raw(format!("{:03.0}°", t.rem_euclid(360.0)))]));
    }
    if let Some(r) = &a.registration {
        d.push(Line::from(vec![key("registration"), Span::raw(r.clone())]));
    }
    d.push(Line::from(vec![key("ICAO address"), Span::raw(a.hex.to_uppercase())]));
    if let Some(s) = &a.squawk {
        d.push(Line::from(vec![key("squawk"), Span::raw(s.clone())]));
    }
    d.push(Line::from(vec![key("position"), Span::raw(format!("{:.4}, {:.4}", at.lat, at.lon))]));
    if let Some(me) = me {
        let nm = mapterm::adsb::distance_nm((me.lon, me.lat), (at.lon, at.lat));
        d.push(Line::from(vec![key("from you"), Span::raw(format!("{nm:.1} nm")), Span::styled(format!("  {:.1} km", nm * 1.852), dim)]));
    }
    d.push(Line::from(vec![key("last report"), Span::raw(format!("{:.0} s ago", (now - seen + a.seen_pos).max(0.0)))]));
    d.push(Line::default());
    d.push(Line::from(Span::styled(format!(" f {} · esc close", if follow { "stop following" } else { "follow" }), dim)));
    f.render_widget(Paragraph::new(d).wrap(Wrap { trim: false }), inner);
}

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
        // Live aircraft stay up whatever the overlay shows: imagery with only the planes on
        // it is the clearest view of them.
        if id == AIRCRAFT || id == TRAILS || id == SHIPS || id == SATS {
            continue;
        }
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
        // Fills (and flat 3D buildings) would hide the imagery under them.
        if raster && matches!(l.kind, Kind::Fill | Kind::FillExtrusion) && (basemap || geojson) {
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
    let mut a = args()?;
    // --at here: ask the location helper before anything depends on the start view.
    let mut me: Option<mapterm::locate::Fix> = None;
    if a.at_here {
        let cmd = a.location_cmd.clone();
        let fix = tokio::task::spawn_blocking(move || mapterm::locate::locate(cmd.as_deref())).await?.context("--at here")?;
        a.at = Some((fix.lon, fix.lat));
        a.zoom.get_or_insert(fix.zoom());
        me = Some(fix);
    }
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
        // Watching aircraft over the default basemap: the aviation style, unless --light.
        if (a.adsb.is_some() || a.ais || a.sats.is_some()) && a.source.is_none() && !a.light {
            map = Map::aviation(src, schema)?;
        } else {
            map = Map::terminal(src, schema, !a.light)?;
        }
        theme = Some((schema, !a.light));
    }
    for w in map.warnings() {
        dlog(|| format!("style: {w}"));
    }
    let style_warnings = map.warnings().len();
    let mut basemap_ids: Vec<String> = map.layer_ids().into_iter().map(str::to_string).collect();
    let mut basemap_sources: Vec<String> = map.sources().map(|(id, _)| id.to_string()).collect();
    // The `m` cycle: the --style given, the built-in style, the aviation style, then the
    // OpenFreeMap styles.
    let builtin_source = a.source.clone().unwrap_or_else(|| OPENFREEMAP.to_string());
    let mut styles: Vec<String> = a.style.iter().cloned().collect();
    if (a.adsb.is_some() || a.ais || a.sats.is_some()) && a.style.is_none() && a.source.is_none() && !a.light {
        styles.push(AVIATION.to_string());
    }
    for s in [BUILTIN, AVIATION].into_iter().chain(stylejson::OPENFREEMAP_STYLES) {
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
                None if mapterm::mbtiles::is_mbtiles(spec) => mapterm::raster::RasterSource::from_mbtiles(spec)?,
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
    // Live aircraft (--adsb): a GeoJSON source the loop refills, over your layers.
    let mut adsb_feed = a.adsb.as_deref().map(mapterm::adsb::Feed::named).transpose()?;
    // adsb.lol's key, only ever sent to adsb.lol.
    if let Some(feed) = adsb_feed.as_mut().filter(|f| f.name == "adsb.lol") {
        feed.key = mapterm::raster::find_key(a.adsb_key_cmd.as_deref(), &["ADSB_LOL_KEY"])?;
    }
    // How often to ask (slowed further by the feed's quota), and how far in the past to draw:
    // a poll interval and a second, so frames fall between two real fixes.
    let adsb_every = adsb_feed.as_ref().map_or(Duration::from_secs(10), mapterm::adsb::Feed::every);
    let adsb_delay = adsb_every.as_secs_f64() + 1.0;
    if let Some(feed) = &adsb_feed {
        let empty = serde_json::json!({"type": "FeatureCollection", "features": []});
        map.add_source(AIRCRAFT, MapSource::geojson(&empty)?)?;
        map.add_source(TRAILS, MapSource::geojson(&empty)?)?;
        map.set_attribution(AIRCRAFT, &feed.attribution)?;
        map.add_layers(vec![mapterm::adsb::trail_layer(TRAILS, TRAILS), mapterm::adsb::layer(AIRCRAFT, AIRCRAFT)], None)?;
    }
    // Live ships (--ais), under the aircraft.
    let ais_key = if a.ais {
        let key = mapterm::raster::find_key(a.ais_key_cmd.as_deref(), &["AISSTREAM_API_KEY"])?;
        Some(key.context("--ais needs a free aisstream.io API key: sign up at https://aisstream.io, then set AISSTREAM_API_KEY or --ais-key-cmd")?)
    } else {
        None
    };
    if ais_key.is_some() {
        let empty = serde_json::json!({"type": "FeatureCollection", "features": []});
        map.add_source(SHIPS, MapSource::geojson(&empty)?)?;
        map.set_attribution(SHIPS, "aisstream.io")?;
        let above = map.layer_ids().into_iter().find(|id| *id == TRAILS).map(str::to_string);
        map.add_layers(vec![mapterm::ais::layer(SHIPS, SHIPS)], above.as_deref())?;
    }
    // Satellites (--sats), over everything else that moves.
    if a.sats.is_some() {
        let empty = serde_json::json!({"type": "FeatureCollection", "features": []});
        map.add_source(SATS, MapSource::geojson(&empty)?)?;
        map.set_attribution(SATS, "CelesTrak")?;
        map.add_layers(vec![mapterm::satellites::layer(SATS, SATS)], None)?;
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
        if let Some(feed) = &adsb_feed {
            let (lon, lat, nm) = view_circle(&cam, area);
            match mapterm::adsb::fetch(feed, lon, lat, nm).await {
                Ok(answer) => {
                    let mut tracks = mapterm::adsb::Tracks::default();
                    let now = unix_now();
                    tracks.update(answer.aircraft, now);
                    map.set_data(AIRCRAFT, &tracks.points(now))?;
                }
                Err(e) => eprintln!("{e:#}"),
            }
        }
        // An image export fetches imagery at image resolution, not one color per cell.
        if a.format == "png" {
            map.raster_mode = RasterMode::Kitty { cell_px: (PNG_CELL.0 as f64, PNG_CELL.1 as f64) };
        }
        // MAPTERM_PIXELS_PNG=FILE: also save pixel mode's image (fills and lines; labels are
        // text) for a Retina-sized terminal, to check the drawing without one. Set before
        // loading: pixel mode needs the tiles under its whole grid tiles.
        let png = std::env::var("MAPTERM_PIXELS_PNG").ok();
        if png.is_some() || a.format == "png" {
            map.pixel_mode = Some((PNG_CELL.0 as f64, PNG_CELL.1 as f64));
        }
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
        let mut buf = Buffer::empty(area);
        let t = Instant::now();
        map.view(&cam).render(area, &mut buf);
        // Pixel mode leaves tiles blank past its per-frame drawing time: draw frames until
        // all are in, keeping every image sent so far.
        let mut sent: Vec<mapterm::pixels::PixImage> = Vec::new();
        let mut last = map.take_pixels();
        // A frame that draws nothing new while tiles are still blank won't ever finish them
        // (data that never came): stop with what's there rather than spin.
        while let Some(f) = last.as_mut().filter(|f| f.pending > 0) {
            let progress = !f.new.is_empty();
            sent.append(&mut f.new);
            map.view(&cam).render(area, &mut Buffer::empty(area));
            last = map.take_pixels();
            if !progress && last.as_ref().is_some_and(|l| l.new.is_empty() && l.pending > 0) {
                eprintln!("warning: {} pixel tiles left blank (their map data didn't load)", last.as_ref().map_or(0, |l| l.pending));
                break;
            }
        }
        if let Some(f) = last.as_mut() {
            sent.append(&mut f.new);
            f.new = std::mem::take(&mut sent);
        }
        if let Some(f) = &last {
            // Over the imagery when there is any, else over white.
            let under = map.raster_rgba(&cam, area, area.width as u32 * PNG_CELL.0, area.height as u32 * PNG_CELL.1);
            let (img, w, h) = compose_png(f, area, under);
            if let Some(path) = &png {
                image::save_buffer(path, &img, w, h, image::ExtendedColorType::Rgba8)?;
                eprintln!("pixels: {} tiles drawn, {} placed, {:.1} ms -> {path}", f.new.len(), f.placed.len(), t.elapsed().as_secs_f64() * 1000.0);
            }
            if a.format == "png" {
                // The view as an image on stdout: fills and lines, no labels (--format describe
                // lists those).
                let mut bytes = Vec::new();
                image::write_buffer_with_format(&mut std::io::Cursor::new(&mut bytes), &img, w, h, image::ExtendedColorType::Rgba8, image::ImageFormat::Png)?;
                std::io::stdout().write_all(&bytes)?;
                return Ok(());
            }
        }
        let out = match a.format.as_str() {
            "ansi" => mapterm::export::to_ansi(&buf),
            "html" => mapterm::export::to_html(&buf),
            // The view as facts for a language model: labels and features with positions.
            "describe" => mapterm::describe::describe(&map, &cam, area),
            _ => mapterm::export::to_text(&buf),
        };
        print!("{out}");
        return Ok(());
    }

    // Raster drawing: real images with kitty graphics, else cell background colors.
    let has_raster = map.sources().any(|(_, s)| matches!(s, MapSource::Raster(_)));
    let kitty = (has_raster || a.radar) && mapterm::kitty::supported();
    // Experimental pixel mode: fills and lines as an image, labels as text (MAPTERM_PIXELS=1).
    let pixels_on = std::env::var("MAPTERM_PIXELS").is_ok_and(|v| !v.is_empty() && v != "0") && mapterm::kitty::supported();
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
    }

    let mut term = ratatui::init();
    // Images through shared memory when the terminal (or herdr) says it reads them, else
    // inline. It asks the terminal, so it runs in raw mode, before the input thread starts.
    // --adsb trails as an image under the text where the terminal shows images: braille lines
    // re-snap to their dot grid as they grow, which shimmers.
    let trails_kitty = (adsb_feed.is_some() || ais_key.is_some() || a.sats.is_some()) && mapterm::kitty::supported();
    if trails_kitty {
        let _ = map.set_visibility(TRAILS, false);
    }
    let shm = (kitty || pixels_on || trails_kitty) && mapterm::kitty::shm_supported();
    dlog(|| format!("kitty medium: {}", if shm { "shared memory" } else { "inline" }));
    kstate.shm = kitty && shm;
    let pixels_shm = shm;
    // Fading sends each new tile a few more times: cheap through shared memory, not inline.
    map.pixel_fade = pixels_on && pixels_shm;
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
    // Rows under the map for the status line, and the credits when they need their own.
    let mut status_rows: u16 = 1;
    // The credits' own lines (0: they share the status line), kept steady: they grow at once
    // but shrink only after needing fewer for a few seconds, or when the window is resized,
    // so a status line that changes length every frame doesn't move the map up and down.
    let mut credit_rows: u16 = 0;
    let mut credit_shrink: Option<Instant> = None;
    let mut credit_width: u16 = 0;
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
    // --adsb: every aircraft's fixes over time, the last request and the one in flight, and
    // when the layers were last redrawn.
    let (atx, mut arx) = mpsc::unbounded_channel::<Result<mapterm::adsb::Answer>>();
    // The feed's quota as of the last answer (adsb.lol reports it).
    let mut aircraft_quota: Option<mapterm::adsb::Quota> = None;
    let mut aircraft = mapterm::adsb::Tracks::default();
    let mut aircraft_asked: Option<(Instant, (f64, f64, f64))> = None;
    let mut aircraft_inflight = false;
    let mut aircraft_drawn = Instant::now();
    // After a failed request: failures in a row, and no request before this time.
    let mut aircraft_failures = 0u32;
    let mut aircraft_retry: Option<Instant> = None;
    // The aircraft clicked (its hex), shown in the panel and highlighted; `f` follows it.
    let mut selected: Option<String> = None;
    // --ais: the ships, the stream's task and the box it streams, and the selected ship.
    let mut fleet = mapterm::ais::Fleet::default();
    let (area_tx, area_rx) = tokio::sync::watch::channel::<Option<mapterm::ais::BBox>>(None);
    let (stx_ais, mut srx_ais) = mpsc::unbounded_channel::<mapterm::ais::Event>();
    if let Some(key) = ais_key.clone() {
        tokio::spawn(mapterm::ais::stream(key, area_rx, stx_ais));
    } else {
        drop((area_rx, stx_ais));
    }
    let mut ais_area: Option<(Instant, mapterm::ais::BBox)> = None;
    let mut ais_state = String::new();
    let mut ships_drawn = Instant::now();
    let mut selected_ship: Option<u32> = None;
    // --sats: the satellites (loaded in the background, again every 2 hours), the selected
    // one, and its next pass over you (worked out once a minute).
    let mut sats: Vec<mapterm::satellites::Sat> = Vec::new();
    let mut sats_age = 0.0f64;
    let (otx, mut orx) = mpsc::unbounded_channel::<(Vec<mapterm::satellites::Sat>, f64, Vec<String>)>();
    // What they're for, from CelesTrak's purpose groups: after the satellites are up (the
    // first time a day it's some twenty small requests).
    let (mtx, mut mrx) = mpsc::unbounded_channel::<(std::collections::HashMap<u64, mapterm::satellites::Mission>, Vec<String>)>();
    // Their radios, from the SatNOGS DB, after that.
    type Radio = std::collections::HashMap<u64, Vec<mapterm::satnogs::Transmitter>>;
    let (rtx, mut rrx) = mpsc::unbounded_channel::<std::result::Result<(Radio, f64, Option<String>), String>>();
    if let Some(groups) = a.sats.clone() {
        let (otx, mtx, rtx) = (otx.clone(), mtx.clone(), rtx.clone());
        tokio::spawn(async move {
            loop {
                let (mut all, mut oldest, mut notes) = (Vec::new(), 0.0f64, Vec::new());
                let mut seen = std::collections::HashSet::new();
                for g in &groups {
                    match mapterm::satellites::load(g).await {
                        Ok(l) => {
                            oldest = oldest.max(l.age);
                            notes.extend(l.note.map(|n| format!("{g}: {n}")));
                            all.extend(l.sats.into_iter().filter(|s| seen.insert(s.norad)));
                        }
                        Err(e) => notes.push(format!("{g}: {e:#}")),
                    }
                }
                // Who owns each (the satellite catalog), kept for a day.
                for g in &groups {
                    match mapterm::satellites::load_catalog(g).await {
                        Ok(cat) => {
                            for s in all.iter_mut().filter(|s: &&mut mapterm::satellites::Sat| s.catalog.is_none()) {
                                s.catalog = cat.get(&s.norad).cloned();
                            }
                        }
                        Err(e) => notes.push(format!("{g} catalog: {e:#}")),
                    }
                }
                if otx.send((all, oldest, notes)).is_err() {
                    return;
                }
                if mtx.send(mapterm::satellites::load_missions().await).is_err() {
                    return;
                }
                if rtx.send(mapterm::satnogs::load().await.map_err(|e| format!("{e:#}"))).is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2 * 3600 + 60)).await;
            }
        });
    }
    drop(otx);
    // --radar: the map's clock, the radar frames that follow it, and RainViewer's scan list
    // (asked for every 5 minutes; a new scan comes every 10). Its tiles stay under 60
    // requests a minute (RainViewer allows 100 per address).
    let mut clock = mapterm::clock::Clock::live(unix_now());
    let mut clock_at = Instant::now();
    let mut radar = a.radar.then(|| mapterm::timelayer::TimeRaster::new("radar", 0.8));
    let mut radar_bucket = Bucket::new(60);
    let mut radar_generated = 0i64;
    let (rvtx, mut rvrx) = mpsc::unbounded_channel::<Result<mapterm::rainviewer::Scans>>();
    if a.radar {
        tokio::spawn(async move {
            loop {
                let r = mapterm::rainviewer::frames().await;
                let wait = if r.is_ok() { 300 } else { 600 };
                if rvtx.send(r).is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
        });
    }
    let mut sats_drawn = Instant::now();
    let mut selected_sat: Option<u64> = None;
    let mut sat_pass: Option<(u64, Instant, Option<mapterm::satellites::Pass>)> = None;
    // Where kitty graphics work, satellites are dots in an image placed to the pixel (smooth),
    // with their names as text: while the image is up, and when it was last drawn.
    let mut sat_dots = false;
    // How satellites are colored (o switches), and what the colors mean; the legends' boxes
    // (L shows or hides them), which the images under the text keep clear of.
    let mut sat_color = a.sats_color;
    let mut sat_coloring = mapterm::satellites::Coloring::Orbit;
    let mut sat_legend: Option<mapterm::legend::Legend> = None;
    let mut show_legend = true;
    let mut legend_rects: Vec<Rect> = Vec::new();
    // `p`: the fzf-style finder over everything live (satellites, aircraft, ships).
    let mut finder: Option<Finder> = None;
    // Satellites of other owners, set aside while a country filter is on.
    let mut radio: std::collections::HashMap<u64, Vec<mapterm::satnogs::Transmitter>> = std::collections::HashMap::new();
    let mut sat_filter = SatFilter { countries: a.sats_country.clone(), ..SatFilter::default() };
    let mut sats_hidden: Vec<mapterm::satellites::Sat> = Vec::new();
    let mut sat_dots_drawn = Instant::now();
    // Satellite positions by SGP4 once a second, interpolated between; and how often to
    // redraw the dots: often enough that the fastest move about a pixel a frame.
    let mut ephemeris = mapterm::satellites::Ephemeris::default();
    let mut sat_every = Duration::from_millis(200);
    let mut follow = false;
    // The trail image needs drawing again (new positions, or the view moved).
    let mut trails_dirty = false;
    let mut trails_cam: Option<(f64, f64, f64, Rect)> = None;
    // When the view last moved, when the trails were last drawn, and which trail images
    // (aircraft, ships) are up.
    let mut trails_moved = Instant::now();
    let mut trails_drawn = Instant::now() - Duration::from_secs(10);
    let mut trails_shown = (false, false, false);
    // `g`: the location helper runs on a blocking task; its answer flies the map there.
    let (ltx, mut lrx) = mpsc::unbounded_channel::<Result<mapterm::locate::Fix>>();
    let mut locating = false;
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
            // Keep the followed aircraft in the middle (where it's drawn, adsb_delay back).
            let followed = selected
                .as_deref()
                .and_then(|h| aircraft.get(h, unix_now() - adsb_delay).map(|x| x.1))
                .or_else(|| selected_ship.and_then(|m| fleet.get(m, unix_now() - AIS_DELAY).map(|x| x.1)))
                .or_else(|| {
                    let f = sats.iter().find(|s| Some(s.norad) == selected_sat)?.at(unix_now())?;
                    Some(mapterm::timeseries::Sample { t: unix_now(), lon: f.lon, lat: f.lat, alt: None })
                });
            if follow
                && !ctl.animating()
                && let Some(at) = followed
            {
                ctl.cam.lon = at.lon;
                ctl.cam.lat = at.lat;
            }
            // One synchronized update per frame when escape sequences follow the cells.
            let sync = smooth_on || kitty || pixels_on || trails_kitty;
            if sync {
                crossterm::execute!(std::io::stdout(), crossterm::terminal::BeginSynchronizedUpdate)?;
            }
            let t0 = Instant::now();
            let cam = ctl.cam;
            // With shift, render on the cell grid and let the terminal draw the sub-cell rest.
            let (draw_cam, ox, oy) = if smooth_on && shift_mode { smooth::snap(&cam) } else { (cam, 0, 0) };
            let box_rect = ctl.box_rect();
            last_cam = draw_cam;
            // The clock moves on, and the radar shows its time.
            let clock_t = clock.tick(unix_now(), clock_at.elapsed().as_secs_f64());
            clock_at = Instant::now();
            if let Some(r) = radar.as_mut() {
                // Playing: every scan ahead, as the request budget allows; paused, the next
                // one (for stepping); live, none.
                let ahead = if clock.is_playing() {
                    r.frames().len()
                } else if clock.is_live() {
                    0
                } else {
                    1
                };
                r.update(&mut map, clock_t, ahead, &draw_cam, map_area);
            }
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
            // The selected satellite: its ground track for the next orbit, and the satellite.
            if let Some(s) = sats.iter().find(|s| Some(s.norad) == selected_sat)
                && let Some(f) = s.at(unix_now())
            {
                let dim = Color::Rgb(127, 178, 255);
                // As an image with the trails where the terminal shows images; braille otherwise.
                if !trails_kitty {
                    for part in s.track(unix_now(), s.period_min()) {
                        overlays.push(Overlay::Line { pts: part, color: dim });
                    }
                }
                // While the satellites are an image, the image rings it and names it (smoothly);
                // otherwise a text marker.
                if !sat_dots {
                    let st = TStyle::new().fg(Color::Black).bg(dim).add_modifier(ratatui::style::Modifier::BOLD);
                    overlays.push(Overlay::Marker {
                        lon: f.lon,
                        lat: f.lat,
                        glyph: s.glyph().into(),
                        style: st,
                        label: Some((s.name.clone(), st)),
                        priority: 101,
                    });
                }
            }
            // The selected ship, the same way.
            if let Some((s, at)) = selected_ship.and_then(|m| fleet.get(m, unix_now() - AIS_DELAY)) {
                let st = TStyle::new().fg(Color::Black).bg(Color::Rgb(127, 178, 255)).add_modifier(ratatui::style::Modifier::BOLD);
                overlays.push(Overlay::Marker { lon: at.lon, lat: at.lat, glyph: s.glyph().into(), style: st, label: Some((s.label(), st)), priority: 101 });
            }
            // The selected aircraft: its arrow reversed, with its callsign, over the others.
            if let Some((a, at, _)) = selected.as_deref().and_then(|h| aircraft.get(h, unix_now() - adsb_delay)) {
                let st = TStyle::new().fg(Color::Black).bg(Color::Rgb(127, 178, 255)).add_modifier(ratatui::style::Modifier::BOLD);
                let name = a.flight.clone().or_else(|| a.registration.clone()).unwrap_or_else(|| a.hex.clone());
                overlays.push(Overlay::Marker {
                    lon: at.lon,
                    lat: at.lat,
                    glyph: mapterm::adsb::arrow(a).into(),
                    style: st,
                    label: Some((name, st)),
                    priority: 101,
                });
            }
            // Where you are (--at here, g): a blue dot under the search markers.
            if let Some(fix) = &me {
                overlays.push(Overlay::Marker {
                    lon: fix.lon,
                    lat: fix.lat,
                    glyph: "◉".into(),
                    style: TStyle::new().fg(Color::Rgb(70, 170, 255)).add_modifier(ratatui::style::Modifier::BOLD),
                    label: None,
                    priority: 96,
                });
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
                let [main, s] = Layout::vertical([Constraint::Min(1), Constraint::Length(status_rows)]).areas(f.area());
                let results = matches!(search, Search::Results { .. }) || finder.is_some();
                let plane = selected.as_deref().and_then(|h| aircraft.get(h, unix_now() - adsb_delay));
                let ship = selected_ship.and_then(|m| fleet.get(m, unix_now() - AIS_DELAY));
                let sat = sats.iter().find(|s| Some(s.norad) == selected_sat).and_then(|s| Some((s, s.at(unix_now())?)));
                let panel_w =
                    if results || (inspect && !picks.is_empty()) || plane.is_some() || ship.is_some() || sat.is_some() { 52.min(main.width / 2) } else { 0 };
                let [m, panel] = Layout::horizontal([Constraint::Min(1), Constraint::Length(panel_w)]).areas(main);
                map_area = m;
                panel_area = panel;
                f.render_widget(map.view(&draw_cam), m);
                // Legends for what's colored, stacked up from the map's lower left.
                legend_rects.clear();
                if show_legend {
                    let mut legends: Vec<mapterm::legend::Legend> = Vec::new();
                    legends.extend(sat_legend.clone().filter(|_| !sats.is_empty()));
                    if adsb_feed.is_some() && !aircraft.is_empty() {
                        legends.push(mapterm::adsb::legend(&aircraft));
                    }
                    if ais_key.is_some() && !fleet.is_empty() {
                        legends.push(mapterm::ais::legend(&fleet));
                    }
                    let mut room = m;
                    for l in &legends {
                        let r = l.rect(room, mapterm::legend::Corner::BottomLeft);
                        if r.height < 3 || r.width < 8 {
                            break;
                        }
                        l.render_in(f.buffer_mut(), room, mapterm::legend::Corner::BottomLeft);
                        legend_rects.push(r);
                        room.height = room.height.saturating_sub(r.height);
                    }
                }
                list_rows = match &search {
                    _ if let Some(fd) = finder.as_ref() => {
                        draw_finder(f, panel, fd);
                        Vec::new()
                    }
                    Search::Results { places, sel } if panel_w > 0 => draw_results(f, panel, places, *sel),
                    _ if inspect && !picks.is_empty() => draw_inspector(f, panel, &picks, sel),
                    _ if let Some(p) = plane => {
                        draw_aircraft(f, panel, p, unix_now(), follow, me.as_ref());
                        Vec::new()
                    }
                    _ if let Some(s) = sat => {
                        let pass = sat_pass.as_ref().filter(|p| p.0 == s.0.norad).map(|p| p.2);
                        draw_sat(f, panel, s, sats_age, pass, unix_now(), follow, me.as_ref());
                        Vec::new()
                    }
                    _ if let Some(s) = ship => {
                        draw_ship(f, panel, s, unix_now(), follow, me.as_ref());
                        Vec::new()
                    }
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
                if a.sats.is_some() {
                    let st = TStyle::new().fg(Color::Rgb(127, 178, 255));
                    let of = sat_filter.describe();
                    spans.push(Span::styled(format!("◇ {} sats{of} ", sats.len()), st));
                }
                if ais_key.is_some() {
                    let st = TStyle::new().fg(Color::Rgb(127, 178, 255));
                    let text = if cam.zoom < AIS_MIN_ZOOM { "⛴ zoom in for ships ".to_string() } else { format!("⛴ {}{} ", fleet.len(), ais_state) };
                    spans.push(Span::styled(text, st));
                }
                if adsb_feed.is_some() {
                    let st = TStyle::new().fg(Color::Rgb(127, 178, 255));
                    let text = match aircraft_retry.map(|t| t.saturating_duration_since(Instant::now())) {
                        _ if cam.zoom < ADSB_MIN_ZOOM => "✈ zoom in for aircraft ".to_string(),
                        Some(w) if !w.is_zero() => format!("✈ paused {} s ", w.as_secs() + 1),
                        _ => match aircraft_quota {
                            Some(q) => format!("✈ {} · {} left ", aircraft.len(), q.remaining),
                            None => format!("✈ {} ", aircraft.len()),
                        },
                    };
                    spans.push(Span::styled(text, st));
                }
                if let Some(r) = radar.as_ref().filter(|r| !r.is_off()) {
                    match r.shown_time() {
                        Some(t) => {
                            let ago = ((unix_now() - t) / 60.0).max(0.0).round();
                            let state = if clock.is_playing() {
                                "▶ "
                            } else if clock.is_live() {
                                ""
                            } else {
                                "⏸ "
                            };
                            let st = if clock.is_live() { dim } else { TStyle::new().fg(Color::Rgb(127, 178, 255)) };
                            spans.push(Span::styled(format!("☂ {state}{} UTC · {ago:.0} min ago ", hhmm(t)), st));
                        }
                        None => spans.push(Span::styled("☂ … ", dim)),
                    }
                }
                spans.push(Span::styled(format!("{frame_ms:.0} ms "), dim));
                // Credits of the sources being drawn, as the style or tiles give them.
                let mut credits = map.attributions(cam.zoom);
                // The aircraft feed's credit (ODbL for adsb.lol) first, where it isn't cut off.
                if let Some(f) = &adsb_feed {
                    credits.retain(|c| *c != f.attribution);
                    credits.insert(0, f.attribution.clone());
                }
                // Satellites drawn as an image have their style layer hidden, but they're on
                // the map all the same: their credit stays.
                if sat_dots
                    && let Some(c) = map.source(SATS).and_then(MapSource::attribution)
                    && !credits.iter().any(|x| x == c)
                {
                    credits.push(c.to_string());
                }
                let credit_items = if credits.is_empty() { vec![basemap_name.clone()] } else { credits };
                let credits = credit_items.join(" · ");
                // While the finder is open, the status line is its prompt.
                if let Some(fd) = finder.as_ref() {
                    spans = vec![
                        Span::styled(" find ", TStyle::new().fg(Color::Black).bg(Color::Rgb(127, 178, 255))),
                        Span::raw(format!(" > {}", fd.query)),
                        Span::styled("█", TStyle::new().fg(Color::Rgb(127, 178, 255))),
                        Span::styled("  ↑↓ choose · enter go · esc close ", dim),
                    ];
                }
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
                // One line when it all fits. Otherwise the credits (which the data's licenses
                // ask for) get their own lines above the status, never cut off; the row count
                // takes effect from the next frame.
                let width = s.width.max(1) as usize;
                let status_width: usize = spans.iter().map(Span::width).sum();
                let together = status_width + 2 + Span::raw(credits.as_str()).width() + 10 <= width;
                // Otherwise as many lines as they take, broken between credits, up to half
                // the window.
                let lines = credit_lines(&credit_items, width);
                let most = (f.area().height / 2).max(1) as usize;
                let want = if together { 0 } else { lines.len().min(most) as u16 };
                if s.width != credit_width || want >= credit_rows {
                    // Resized, or they need more: at once.
                    (credit_rows, credit_shrink, credit_width) = (want, None, s.width);
                } else if credit_shrink.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(5) {
                    // They've needed fewer for 5 s straight (a frame that needs more resets it).
                    (credit_rows, credit_shrink) = (want, None);
                }
                status_rows = 1 + credit_rows;
                if credit_rows == 0 || s.height < 2 {
                    spans.push(Span::styled(format!("· {credits} · q quit"), dim));
                    f.render_widget(Paragraph::new(Line::from(spans)), Rect { y: s.bottom() - 1, height: 1, ..s });
                } else {
                    let [above, line] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(s);
                    // Bottom-aligned, so a frame where the rows haven't caught up yet loses
                    // the top line, not the end of the last.
                    let shown = lines.len().min(above.height as usize);
                    let text: Vec<Line> = lines[lines.len() - shown..].iter().map(|l| Line::from(Span::styled(l.clone(), dim))).collect();
                    f.render_widget(Paragraph::new(text), Rect { y: above.bottom() - shown as u16, height: shown as u16, ..above });
                    spans.push(Span::styled("· q quit", dim));
                    f.render_widget(Paragraph::new(Line::from(spans)), line);
                }
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
                if trails_kitty && map_area.width > 0 {
                    // The trail images cover the map, so moving it means drawing them again, a
                    // few MB each that the terminal (and herdr) must take in. Take them down
                    // while the view moves and draw them once it has settled; while it stays
                    // put, at most once a second. Nothing to draw: no image at all.
                    let view = (draw_cam.lon, draw_cam.lat, draw_cam.zoom, map_area);
                    if trails_cam != Some(view) {
                        trails_cam = Some(view);
                        trails_moved = Instant::now();
                        for (id, shown) in [(TRAIL_IMAGE, &mut trails_shown.0), (SHIP_TRAIL_IMAGE, &mut trails_shown.1), (PATH_IMAGE, &mut trails_shown.2)] {
                            if std::mem::take(shown) {
                                out.write_all(mapterm::kitty::free(id).as_bytes())?;
                            }
                        }
                    }
                    let settled = trails_moved.elapsed() >= Duration::from_millis(150) && !ctl.animating();
                    // Paths: the selected satellite's ground track.
                    let path_lines: Vec<mapterm::timeseries::Line> = sats
                        .iter()
                        .find(|s| Some(s.norad) == selected_sat)
                        .map(|s| s.track(unix_now(), s.period_min()).into_iter().map(|p| (p, (127, 178, 255))).collect())
                        .unwrap_or_default();
                    let hidden =
                        (adsb_feed.is_some() && !trails_shown.0) || (ais_key.is_some() && !trails_shown.1) || (!path_lines.is_empty() && !trails_shown.2);
                    if settled && ((trails_dirty && trails_drawn.elapsed() >= Duration::from_secs(1)) || (hidden && trails_dirty)) {
                        trails_dirty = false;
                        trails_drawn = Instant::now();
                        let t_trails = Instant::now();
                        let (w, h) = ((map_area.width as f64 * cpx.0).round() as u32, (map_area.height as f64 * cpx.1).round() as u32);
                        let vp = Viewport::new(&draw_cam, map_area);
                        let now = unix_now();
                        let sets: [TrailSet; 2] = [
                            (
                                adsb_feed.is_some(),
                                mapterm::adsb::trail_paths(&aircraft).collect(),
                                now - adsb_delay,
                                ADSB_TRAIL,
                                TRAIL_IMAGE,
                                -10,
                                &mut trails_shown.0,
                            ),
                            (ais_key.is_some(), fleet.trail_paths().collect(), now - AIS_DELAY, AIS_TRAIL, SHIP_TRAIL_IMAGE, -11, &mut trails_shown.1),
                        ];
                        for (on, paths, t, secs, id, z, shown) in sets {
                            let any = paths.iter().any(|(p, _)| p.trail_samples(t, secs).len() >= 2);
                            if !on || !any {
                                if std::mem::take(shown) {
                                    out.write_all(mapterm::kitty::free(id).as_bytes())?;
                                }
                                continue;
                            }
                            let mut rgba = mapterm::timeseries::render_trails(paths, t, secs, &vp, w, h, cpx);
                            punch(&mut rgba, w, map_area, &legend_rects, cpx);
                            out.write_all(
                                mapterm::kitty::show_rgba(id, w, h, &rgba, map_area.x, map_area.y, map_area.width, map_area.height, z, shm).as_bytes(),
                            )?;
                            *shown = true;
                        }
                        if path_lines.is_empty() {
                            if std::mem::take(&mut trails_shown.2) {
                                out.write_all(mapterm::kitty::free(PATH_IMAGE).as_bytes())?;
                            }
                        } else {
                            let mut rgba = mapterm::timeseries::render_lines(&path_lines, &vp, w, h, cpx);
                            punch(&mut rgba, w, map_area, &legend_rects, cpx);
                            out.write_all(
                                mapterm::kitty::show_rgba(PATH_IMAGE, w, h, &rgba, map_area.x, map_area.y, map_area.width, map_area.height, -9, shm).as_bytes(),
                            )?;
                            trails_shown.2 = true;
                        }
                        dlog(|| format!("trails: {w}x{h} px, {:.1} ms, shown {trails_shown:?}", t_trails.elapsed().as_secs_f64() * 1000.0));
                    }
                    // Satellites as dots with their names, five times a second once the map is
                    // still; the cell glyphs and text names stand in while it moves. Full
                    // resolution when there are names to draw (text needs it), else half,
                    // scaled up by the terminal (dots don't).
                    let want_dots = !sats.is_empty() && settled;
                    if want_dots && sat_dots_drawn.elapsed() >= sat_every {
                        sat_dots_drawn = Instant::now();
                        // Full resolution when there's text to draw: names, or the selected one's.
                        let named = sats.iter().any(|s| !s.is_starlink());
                        let scale = if named || selected_sat.is_some() { 1.0 } else { 0.5 };
                        let (w, h) = ((map_area.width as f64 * cpx.0 * scale).round() as u32, (map_area.height as f64 * cpx.1 * scale).round() as u32);
                        let vp = Viewport::new(&draw_cam, map_area);
                        // Low orbit crosses the ground at about 7 km/s: redraw so it moves ~1 px a frame,
                        // from 4 to 15 times a second (zoomed out it barely moves).
                        let px_per_km = cpx.0 / vp.km_per_col(draw_cam.lat).max(1e-6);
                        let fps = (7.0 * px_per_km).ceil().clamp(4.0, 15.0);
                        sat_every = Duration::from_secs_f64(1.0 / fps);
                        let at = ephemeris.positions(&sats, unix_now());
                        let mut rgba = mapterm::satellites::render_dots_at(
                            &sats,
                            &at,
                            &vp,
                            w,
                            h,
                            (cpx.0 * scale, cpx.1 * scale),
                            named,
                            selected_sat,
                            &sat_coloring,
                        );
                        punch(&mut rgba, w, map_area, &legend_rects, (cpx.0 * scale, cpx.1 * scale));
                        out.write_all(
                            mapterm::kitty::show_rgba(SAT_IMAGE, w, h, &rgba, map_area.x, map_area.y, map_area.width, map_area.height, -8, shm).as_bytes(),
                        )?;
                        if !sat_dots {
                            sat_dots = true;
                            let _ = map.set_visibility(SATS, false);
                        }
                    } else if !want_dots && sat_dots {
                        sat_dots = false;
                        out.write_all(mapterm::kitty::free(SAT_IMAGE).as_bytes())?;
                        let _ = map.set_visibility(SATS, true);
                        // Bring the text layer up to date right away.
                        sats_drawn = Instant::now() - Duration::from_secs(10);
                    }
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
                if radar.as_ref().is_some_and(|r| r.owns(&req.source)) && !radar_bucket.take() {
                    held_back = true;
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

            // Go easy on the free feed: nothing when zoomed out past ADSB_MIN_ZOOM, a circle of
            // at most --adsb-radius, and a growing pause after errors.
            let adsb_waiting = aircraft_retry.is_some_and(|t| Instant::now() < t);
            if let Some(feed) = adsb_feed.as_ref().filter(|_| !aircraft_inflight && !adsb_waiting && map_area.width > 0 && cam.zoom >= ADSB_MIN_ZOOM) {
                // Ask every adsb_every (longer when the day's quota runs low), or sooner (but
                // not more than three times as often) once the view has left the circle last
                // asked for, and not mid-animation. A little more than the view, so short
                // pans have planes already.
                let every = aircraft_quota.map_or(adsb_every, |q| q.pace(adsb_every));
                let (lon, lat, view_nm) = view_circle(&cam, map_area);
                let nm = (view_nm * 1.3).min(a.adsb_radius).min(feed.max_nm);
                let due = match aircraft_asked {
                    None => true,
                    Some((at, (alon, alat, anm))) => {
                        // The capped circle, so a view wider than the cap isn't always "moved".
                        let moved = mapterm::adsb::distance_nm((lon, lat), (alon, alat)) + view_nm.min(nm) > anm * 1.1;
                        at.elapsed() >= every || (moved && at.elapsed() >= (every / 3).max(Duration::from_secs(2)) && !ctl.animating())
                    }
                };
                if due {
                    aircraft_asked = Some((Instant::now(), (lon, lat, nm)));
                    aircraft_inflight = true;
                    dlog(|| format!("adsb: asking for {nm:.0} nm at z{:.1}", cam.zoom));
                    let (feed, atx) = (feed.clone(), atx.clone());
                    tokio::spawn(async move {
                        let _ = atx.send(mapterm::adsb::fetch(&feed, lon, lat, nm).await);
                    });
                }
            }
            // --ais: stream a box around the view (half a view more on each side, at most 12° a
            // side), a new one once the view leaves it; none when zoomed out.
            if ais_key.is_some() && map_area.width > 0 && !ctl.animating() {
                let want = (cam.zoom >= AIS_MIN_ZOOM).then(|| {
                    let vp = Viewport::new(&cam, map_area);
                    let (w, n) = vp.cell_to_lonlat(0, 0);
                    let (e, s) = vp.cell_to_lonlat(map_area.width, map_area.height);
                    let (dx, dy) = (((e - w) / 2.0).min(6.0 - (e - w) / 2.0).max(0.0), ((n - s) / 2.0).min(6.0 - (n - s) / 2.0).max(0.0));
                    let view: mapterm::ais::BBox = [(s, w), (n, e)];
                    (view, [((s - dy).max(-90.0), (w - dx).max(-180.0)), ((n + dy).min(90.0), (e + dx).min(180.0))])
                });
                let current = ais_area.map(|(_, b)| b);
                let change = match (want, current) {
                    (None, Some(_)) => Some(None),
                    (Some((view, wide)), cur) => {
                        let inside = cur.is_some_and(|[(cs, cw), (cn, ce)]| view[0].0 >= cs && view[0].1 >= cw && view[1].0 <= cn && view[1].1 <= ce);
                        // Reconnect at most every 3 s.
                        (!inside && ais_area.is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(3))).then_some(Some(wide))
                    }
                    _ => None,
                };
                if let Some(b) = change {
                    dlog(|| format!("ais: area {b:?}"));
                    ais_area = b.map(|b| (Instant::now(), b));
                    let _ = area_tx.send(b);
                }
            }
            // --sats: positions straight from the orbits, a few times a second; the selected
            // one's next pass over you, once a minute.
            // Big groups (Starlink's 11,000 take ~30 ms) twice a second: half a second is a few
            // km of orbit, under a cell at the zooms they're watched at.
            // While the dots image draws them, this text layer is only for when the map moves:
            // every 2 s is plenty (it catches up as soon as the image comes down).
            let every = if sat_dots {
                Duration::from_secs(2)
            } else if sats.len() > 2000 {
                Duration::from_millis(500)
            } else {
                Duration::from_millis(250)
            };
            if !sats.is_empty() && sats_drawn.elapsed() >= every {
                sats_drawn = Instant::now();
                // The selected one's ground track moves on with it (or goes, when deselected).
                if selected_sat.is_some() || trails_shown.2 {
                    trails_dirty = true;
                }
                map.set_data(SATS, &mapterm::satellites::points(&sats, unix_now(), &sat_coloring))?;
            }
            if let (Some(norad), Some(me)) = (selected_sat, me.as_ref())
                && sat_pass.as_ref().is_none_or(|p| p.0 != norad || p.1.elapsed() >= Duration::from_secs(60))
                && let Some(s) = sats.iter().find(|s| s.norad == norad)
            {
                let observer = (me.lon, me.lat, 0.0);
                sat_pass = Some((norad, Instant::now(), s.next_pass(observer, unix_now(), 24.0, 10.0)));
            }
            if ais_key.is_some() && !fleet.is_empty() && ships_drawn.elapsed() >= Duration::from_millis(250) {
                ships_drawn = Instant::now();
                fleet.prune(unix_now(), None);
                if selected_ship.is_some_and(|m| fleet.get(m, unix_now()).is_none()) {
                    selected_ship = None;
                    follow = false;
                }
                map.set_data(SHIPS, &fleet.points(unix_now() - AIS_DELAY))?;
                trails_dirty = true;
            }
            // Move the aircraft a few times a second, drawn adsb_delay in the past so they glide
            // between real fixes (and follow turns) rather than guess ahead.
            if adsb_feed.is_some() && !aircraft.is_empty() && aircraft_drawn.elapsed() >= Duration::from_millis(250) {
                aircraft_drawn = Instant::now();
                let t = unix_now() - adsb_delay;
                map.set_data(AIRCRAFT, &aircraft.points(t))?;
                if trails_kitty {
                    trails_dirty = true;
                } else {
                    map.set_data(TRAILS, &aircraft.trails(t, ADSB_TRAIL))?;
                }
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
            let idle = if ctl.animating() || pixels_pending > 0 || !demo.is_empty() || clock.is_playing() {
                Duration::from_millis(16)
            } else if sat_dots {
                // Satellite dots redraw up to 15 times a second (see sat_every).
                (sat_every / 2).max(Duration::from_millis(16))
            } else if !aircraft.is_empty() || !fleet.is_empty() || !sats.is_empty() {
                // Aircraft and ships keep moving.
                Duration::from_millis(250)
            } else {
                Duration::from_millis(500)
            };
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
                Some(r) = rvrx.recv() => {
                    match r {
                        Ok(scans) if scans.generated != radar_generated || radar_generated == 0 => {
                            radar_generated = scans.generated;
                            let frames: Vec<(f64, std::sync::Arc<mapterm::raster::RasterSource>)> = scans
                                .past
                                .iter()
                                .filter_map(|s| Some((s.time as f64, mapterm::rainviewer::frame_source(s).ok()?)))
                                .collect();
                            dlog(|| format!("radar: {} scans, the latest {}", frames.len(), scans.past.last().map_or(0, |s| s.time)));
                            if let Some(r) = radar.as_mut() {
                                r.set_frames(&mut map, frames, mapterm::rainviewer::SCAN_SECS, None)?;
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            dlog(|| format!("radar: {e:#}"));
                            note = Some((Instant::now(), format!("radar: {e:#}")));
                        }
                    }
                }
                Some(r) = rrx.recv() => {
                    match r {
                        Ok((by, age, why)) => {
                            dlog(|| format!("sats: radio for {} satellites from SatNOGS DB, {:.0} h old; {why:?}", by.len(), age / 3600.0));
                            radio = by;
                            for s in sats.iter_mut().chain(sats_hidden.iter_mut()) {
                                s.radio = radio.get(&s.norad).cloned().unwrap_or_default();
                            }
                            if sat_filter.radio_on() {
                                filter_sats(&mut sats, &mut sats_hidden, &sat_filter);
                            }
                            map.set_attribution(SATS, "CelesTrak · SatNOGS DB (CC BY-SA 4.0)")?;
                        }
                        // No radio section; the rest works as before.
                        Err(e) => dlog(|| format!("sats: no radio from SatNOGS DB: {e}")),
                    }
                }
                Some((missions, notes)) = mrx.recv() => {
                    dlog(|| format!("sats: {} missions from groups; {notes:?}", missions.len()));
                    for s in sats.iter_mut().chain(sats_hidden.iter_mut()) {
                        s.group_mission = missions.get(&s.norad).copied();
                    }
                    if !sat_filter.missions.is_empty() {
                        filter_sats(&mut sats, &mut sats_hidden, &sat_filter);
                    }
                    (sat_coloring, sat_legend) = recolor(&sats, sat_color);
                }
                Some((loaded, age, notes)) = orx.recv() => {
                    dlog(|| format!("sats: {} loaded, {:.0} min old; {notes:?}", loaded.len(), age / 60.0));
                    if !notes.is_empty() {
                        note = Some((Instant::now(), notes.join("; ")));
                    }
                    if !loaded.is_empty() {
                        let known: std::collections::HashMap<u64, mapterm::satellites::Mission> =
                            sats.iter().chain(sats_hidden.iter()).filter_map(|s| s.group_mission.map(|m| (s.norad, m))).collect();
                        sats = loaded;
                        for s in &mut sats {
                            s.group_mission = known.get(&s.norad).copied();
                            s.radio = radio.get(&s.norad).cloned().unwrap_or_default();
                        }
                        sats_hidden.clear();
                        sats_age = age;
                        filter_sats(&mut sats, &mut sats_hidden, &sat_filter);
                        (sat_coloring, sat_legend) = recolor(&sats, sat_color);
                    }
                }
                Some(ev) = srx_ais.recv() => {
                    match ev {
                        mapterm::ais::Event::Update(u) => fleet.apply(u, unix_now()),
                        mapterm::ais::Event::Connected(b) => {
                            ais_state.clear();
                            fleet.prune(unix_now(), Some(b));
                            dlog(|| format!("ais: streaming {b:?}"));
                        }
                        mapterm::ais::Event::Retrying(e, wait) => {
                            dlog(|| format!("ais: {e}; again in {} s", wait.as_secs()));
                            ais_state = format!(" · reconnecting in {} s", wait.as_secs());
                            note = Some((Instant::now(), format!("aisstream.io: {e}")));
                        }
                        mapterm::ais::Event::Stopped(e) => {
                            dlog(|| format!("ais: stopped: {e}"));
                            ais_state = " · stopped".into();
                            note = Some((Instant::now(), e));
                        }
                    }
                }
                Some(r) = arx.recv() => {
                    aircraft_inflight = false;
                    match r {
                        Ok(answer) => {
                            aircraft_quota = answer.quota.or(aircraft_quota);
                            aircraft.update(answer.aircraft, unix_now());
                            aircraft_failures = 0;
                            aircraft_retry = None;
                            if let Some(h) = selected.as_deref().filter(|h| aircraft.get(h, unix_now()).is_none()) {
                                note = Some((Instant::now(), format!("lost contact with {h}")));
                                selected = None;
                                follow = false;
                            }
                        }
                        Err(e) => {
                            // Wait before asking again: what the feed said, else 30 s doubling.
                            aircraft_failures += 1;
                            let wait = mapterm::adsb::backoff(&e, aircraft_failures);
                            aircraft_retry = Some(Instant::now() + wait);
                            dlog(|| format!("adsb: {e:#}; next try in {} s", wait.as_secs()));
                            note = Some((Instant::now(), format!("{e:#} · trying again in {} s", wait.as_secs())));
                        }
                    }
                }
                Some(r) = lrx.recv() => {
                    locating = false;
                    match r {
                        Ok(fix) => {
                            ctl.fly_to(Camera::new(fix.lon, fix.lat, fix.zoom().max(ctl.cam.zoom.min(17.0))));
                            let acc = fix.accuracy_m.map(|m| format!(" (±{m:.0} m)")).unwrap_or_default();
                            note = Some((Instant::now(), format!("you are here{acc}")));
                            me = Some(fix);
                        }
                        Err(e) => note = Some((Instant::now(), format!("location: {e:#}"))),
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
                                // The finder takes every key while it's open.
                                if let Some(fd) = finder.as_mut() {
                                    let mut refilter = false;
                                    match k.code {
                                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                                        KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                                            fd.query.clear();
                                            refilter = true;
                                        }
                                        KeyCode::Esc => finder = None,
                                        KeyCode::Down | KeyCode::Tab if !fd.hits.is_empty() => fd.sel = (fd.sel + 1) % fd.hits.len(),
                                        KeyCode::Up | KeyCode::BackTab if !fd.hits.is_empty() => fd.sel = (fd.sel + fd.hits.len() - 1) % fd.hits.len(),
                                        KeyCode::Char(c) => {
                                            fd.query.push(c);
                                            refilter = true;
                                        }
                                        KeyCode::Backspace => {
                                            fd.query.pop();
                                            refilter = true;
                                        }
                                        KeyCode::Enter => {
                                            // Select it, as a click would, and fly there.
                                            if let Some(hit) = fd.hits.get(fd.sel) {
                                                let target = hit.target.clone();
                                                let now = unix_now();
                                                // A country: filter the satellites to it (or show them all again).
                                                if let Target::Missions(ms) = &target {
                                                    sat_filter.missions = ms.clone();
                                                    filter_sats(&mut sats, &mut sats_hidden, &sat_filter);
                                                    (sat_coloring, sat_legend) = recolor(&sats, sat_color);
                                                    if selected_sat.is_some_and(|n| !sats.iter().any(|s| s.norad == n)) {
                                                        selected_sat = None;
                                                    }
                                                    note = Some((Instant::now(), format!("{} satellites:{}", sats.len(), sat_filter.describe())));
                                                    finder = None;
                                                    continue;
                                                }
                                                if matches!(target, Target::Radio(_) | Target::Uplink | Target::Usage(_)) {
                                                    match &target {
                                                        Target::Radio(m) => sat_filter.radio = Some(m.clone()),
                                                        Target::Usage(u) => sat_filter.usage = Some(*u),
                                                        _ => sat_filter.uplink = true,
                                                    }
                                                    filter_sats(&mut sats, &mut sats_hidden, &sat_filter);
                                                    (sat_coloring, sat_legend) = recolor(&sats, sat_color);
                                                    if selected_sat.is_some_and(|n| !sats.iter().any(|s| s.norad == n)) {
                                                        selected_sat = None;
                                                    }
                                                    note = Some((Instant::now(), format!("{} satellites:{}", sats.len(), sat_filter.describe())));
                                                    finder = None;
                                                    continue;
                                                }
                                                if let Target::Operator(op) = &target {
                                                    sat_filter.operator = Some(op);
                                                    filter_sats(&mut sats, &mut sats_hidden, &sat_filter);
                                                    (sat_coloring, sat_legend) = recolor(&sats, sat_color);
                                                    if selected_sat.is_some_and(|n| !sats.iter().any(|s| s.norad == n)) {
                                                        selected_sat = None;
                                                    }
                                                    note = Some((Instant::now(), format!("{} satellites:{}", sats.len(), sat_filter.describe())));
                                                    finder = None;
                                                    continue;
                                                }
                                                if let Target::Country(code) = &target {
                                                    // A country filters to it; "Show all satellites" clears both filters.
                                                    if code.is_none() {
                                                        sat_filter = SatFilter::default();
                                                    }
                                                    sat_filter.countries = code.iter().cloned().collect();
                                                    filter_sats(&mut sats, &mut sats_hidden, &sat_filter);
                                                    (sat_coloring, sat_legend) = recolor(&sats, sat_color);
                                                    if selected_sat.is_some_and(|n| !sats.iter().any(|s| s.norad == n)) {
                                                        selected_sat = None;
                                                    }
                                                    let what = code.as_deref().map_or("all".to_string(), country_short);
                                                    note = Some((Instant::now(), format!("{} satellites: {what}", sats.len())));
                                                    finder = None;
                                                    continue;
                                                }
                                                // A satellite set aside by the filter: show everything again first.
                                                if let Target::Sat(n) = &target
                                                    && sats_hidden.iter().any(|s| s.norad == *n)
                                                {
                                                    sat_filter = SatFilter::default();
                                                    filter_sats(&mut sats, &mut sats_hidden, &sat_filter);
                                                    (sat_coloring, sat_legend) = recolor(&sats, sat_color);
                                                }
                                                let at = match &target {
                                                    Target::Sat(n) => sats.iter().find(|s| s.norad == *n).and_then(|s| s.at(now)).map(|f| (f.lon, f.lat)),
                                                    Target::Ship(m) => fleet.get(*m, now - AIS_DELAY).map(|(_, p)| (p.lon, p.lat)),
                                                    Target::Plane(h) => aircraft.get(h, now - adsb_delay).map(|(_, p, _)| (p.lon, p.lat)),
                                                    Target::Country(_) | Target::Missions(_) | Target::Operator(_) | Target::Radio(_) | Target::Uplink | Target::Usage(_) => None,
                                                };
                                                (selected, selected_ship, selected_sat) = (None, None, None);
                                                match target {
                                                    Target::Country(_) | Target::Missions(_) | Target::Operator(_) | Target::Radio(_) | Target::Uplink | Target::Usage(_) => {}
                                                    Target::Sat(n) => selected_sat = Some(n),
                                                    Target::Ship(m) => selected_ship = Some(m),
                                                    Target::Plane(h) => selected = Some(h),
                                                }
                                                follow = false;
                                                if let Some((lon, lat)) = at {
                                                    ctl.fly_to(Camera::new(lon, lat, ctl.cam.zoom.max(3.0)));
                                                }
                                            }
                                            finder = None;
                                        }
                                        _ => {}
                                    }
                                    if refilter && let Some(fd) = finder.as_mut() {
                                        fd.hits = find(&fd.query, &sats, &sats_hidden, &sat_filter, &aircraft, &fleet);
                                        fd.sel = 0;
                                    }
                                    continue;
                                }
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
                                    KeyCode::Char('o') if !sats.is_empty() => {
                                        sat_color = match sat_color {
                                            SatColor::Orbit => SatColor::Country,
                                            SatColor::Country => SatColor::Mission,
                                            SatColor::Mission => SatColor::Orbit,
                                        };
                                        (sat_coloring, sat_legend) = recolor(&sats, sat_color);
                                        let by = ["orbit", "country", "mission"][sat_color as usize];
                                        note = Some((Instant::now(), format!("satellites colored by {by}")));
                                    }
                                    KeyCode::Char(' ') if radar.as_ref().is_some_and(|r| !r.frames().is_empty()) => {
                                        let span = radar.as_ref().and_then(|r| r.span());
                                        if clock.is_playing() {
                                            clock.pause();
                                            note = Some((Instant::now(), "radar paused · space plays · . to now".into()));
                                        } else if let Some((start, stop)) = span {
                                            // Two hours in about six seconds.
                                            clock.play(start, stop, mapterm::clock::Range::Loop, (stop - start) / 6.0);
                                            note = Some((Instant::now(), "radar playing the last two hours · space pauses".into()));
                                        }
                                    }
                                    KeyCode::Char(c @ (',' | '.')) if radar.as_ref().is_some_and(|r| !r.frames().is_empty()) => {
                                        let r = radar.as_ref().expect("checked");
                                        // From the clock's frame, not the one on screen, which waits
                                        // for its tiles: quick presses each count.
                                        let last = r.frames().all().last().map(|f| f.start);
                                        let at = if clock.is_live() { last.unwrap_or(clock.time()) } else { clock.time() };
                                        let at = r.frames().at_or_before(at).map_or(at, |i| r.frames().all()[i].start);
                                        if c == '.' && (clock.is_live() || Some(at) == last) {
                                            clock.go_live(unix_now());
                                            note = Some((Instant::now(), "radar: now".into()));
                                        } else if let Some(t) = r.frames().step(at, if c == '.' { 1 } else { -1 }) {
                                            clock.seek(t);
                                        }
                                    }
                                    KeyCode::Char('R') if radar.is_some() => {
                                        let r = radar.as_mut().expect("checked");
                                        let off = !r.is_off();
                                        r.set_off(&mut map, off);
                                        if off {
                                            clock.go_live(unix_now());
                                        }
                                        note = Some((Instant::now(), if off { "radar off · R shows it" } else { "radar on" }.into()));
                                    }
                                    KeyCode::Char('L') => {
                                        show_legend = !show_legend;
                                        note = Some((Instant::now(), if show_legend { "legend on" } else { "legend off" }.into()));
                                    }
                                    KeyCode::Char('p') if !sats.is_empty() || !aircraft.is_empty() || !fleet.is_empty() => {
                                        finder = Some(Finder { query: String::new(), sel: 0, hits: find("", &sats, &sats_hidden, &sat_filter, &aircraft, &fleet) });
                                    }
                                    KeyCode::Char('/') => {
                                        search = Search::Typing(String::new());
                                    }
                                    KeyCode::Char('g') if !locating => {
                                        locating = true;
                                        note = Some((Instant::now(), "locating…".into()));
                                        let (tx, cmd) = (ltx.clone(), a.location_cmd.clone());
                                        tokio::task::spawn_blocking(move || {
                                            let _ = tx.send(mapterm::locate::locate(cmd.as_deref()));
                                        });
                                    }
                                    KeyCode::Esc if (selected.is_some() || selected_ship.is_some() || selected_sat.is_some()) && !inspect => {
                                        selected = None;
                                        selected_ship = None;
                                        selected_sat = None;
                                        follow = false;
                                    }
                                    KeyCode::Char('f') if selected.is_some() || selected_ship.is_some() || selected_sat.is_some() => {
                                        follow = !follow;
                                        note = Some((Instant::now(), if follow { "following" } else { "not following" }.into()));
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
                                    // --adsb: Tab / shift+Tab step through the aircraft from the
                                    // middle of the map outward.
                                    // --sats alone: Tab steps through the satellites the same way.
                                    KeyCode::Tab | KeyCode::BackTab if !inspect && aircraft.is_empty() && fleet.is_empty() && !sats.is_empty() => {
                                        let order = mapterm::satellites::by_distance(&sats, unix_now(), ctl.cam.lon, ctl.cam.lat);
                                        let at = selected_sat.and_then(|n| order.iter().position(|x| *x == n));
                                        let next = match (at, k.code) {
                                            (None, _) => 0,
                                            (Some(i), KeyCode::BackTab) => (i + order.len() - 1) % order.len(),
                                            (Some(i), _) => (i + 1) % order.len(),
                                        };
                                        if order.get(next).copied() != selected_sat {
                                            follow = false;
                                        }
                                        selected_sat = order.get(next).copied();
                                    }
                                    // --ais alone: Tab steps through the ships the same way.
                                    KeyCode::Tab | KeyCode::BackTab if !inspect && aircraft.is_empty() && !fleet.is_empty() => {
                                        let order = fleet.by_distance(unix_now() - AIS_DELAY, ctl.cam.lon, ctl.cam.lat);
                                        let at = selected_ship.and_then(|m| order.iter().position(|x| *x == m));
                                        let next = match (at, k.code) {
                                            (None, _) => 0,
                                            (Some(i), KeyCode::BackTab) => (i + order.len() - 1) % order.len(),
                                            (Some(i), _) => (i + 1) % order.len(),
                                        };
                                        if order.get(next).copied() != selected_ship {
                                            follow = false;
                                        }
                                        selected_ship = order.get(next).copied();
                                    }
                                    KeyCode::Tab | KeyCode::BackTab if !inspect && !aircraft.is_empty() => {
                                        let order = aircraft.by_distance(unix_now() - adsb_delay, ctl.cam.lon, ctl.cam.lat);
                                        let at = selected.as_ref().and_then(|h| order.iter().position(|x| x == h));
                                        let next = match (at, k.code) {
                                            (None, _) => 0,
                                            (Some(i), KeyCode::BackTab) => (i + order.len() - 1) % order.len(),
                                            (Some(i), _) => (i + 1) % order.len(),
                                        };
                                        if order.get(next) != selected.as_ref() {
                                            follow = false;
                                        }
                                        selected = order.get(next).cloned();
                                    }
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
                                if let Some(k) = ctl.take_click() {
                                    if inspect {
                                        let dot = Viewport::new(&last_cam, map_area).dot_ll(k.lon, k.lat);
                                        picks = map.query_features(&last_cam, map_area, dot);
                                        sel = 0;
                                        click_at = Some((k.lon, k.lat));
                                        if picks.is_empty() {
                                            note = Some((Instant::now(), "nothing here".into()));
                                        }
                                    } else if adsb_feed.is_some() || ais_key.is_some() || a.sats.is_some() {
                                        // A click on (or next to) an aircraft, else a ship, selects it.
                                        let vp = Viewport::new(&last_cam, map_area);
                                        let dot = vp.dot_ll(k.lon, k.lat);
                                        if let Some(hex) = aircraft.nearest(unix_now() - adsb_delay, &vp, dot, 6.0) {
                                            if selected.as_deref() != Some(hex.as_str()) {
                                                follow = false;
                                            }
                                            selected = Some(hex);
                                            selected_ship = None;
                                            selected_sat = None;
                                        } else if let Some((_, mmsi)) = fleet.nearest(unix_now() - AIS_DELAY, &vp, dot, 6.0) {
                                            if selected_ship != Some(mmsi) {
                                                follow = false;
                                            }
                                            selected_ship = Some(mmsi);
                                            selected_sat = None;
                                            selected = None;
                                        } else if let Some(norad) = mapterm::satellites::nearest(&sats, unix_now(), &vp, dot, 6.0) {
                                            if selected_sat != Some(norad) {
                                                follow = false;
                                            }
                                            selected_sat = Some(norad);
                                            selected = None;
                                            selected_ship = None;
                                        }
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
    if kitty || pixels_on || trails_kitty {
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

#[cfg(test)]
mod tests {
    #[test]
    fn credits_break_between_credits() {
        let items: Vec<String> =
            ["© OpenStreetMap contributors", "OpenFreeMap", "CelesTrak · SatNOGS DB (CC BY-SA 4.0)", "adsb.lol (ODbL 1.0)"].map(String::from).to_vec();
        let lines = super::credit_lines(&items, 40);
        assert_eq!(lines, vec![" © OpenStreetMap contributors", " OpenFreeMap", " CelesTrak · SatNOGS DB (CC BY-SA 4.0)", " adsb.lol (ODbL 1.0)"]);
        assert!(lines.iter().all(|l| l.chars().count() <= 40));
        // Wide: as few lines as fit.
        assert_eq!(super::credit_lines(&items, 200).len(), 1);
        // A credit wider than the line breaks at a space, and nothing is lost.
        let long = vec!["Imagery © a very long provider name that does not fit".to_string()];
        let lines = super::credit_lines(&long, 20);
        assert!(lines.iter().all(|l| l.chars().count() <= 20), "{lines:?}");
        assert_eq!(lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "), long[0]);
    }
}
