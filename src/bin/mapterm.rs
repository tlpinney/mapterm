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

usage: mapterm [SOURCE | --style STYLE] [--geojson FILE|URL]... [--raster NAME|URL] [--at LAT,LON|here] [-z ZOOM] [--adsb [FEED]] [--ais] [--sats [GROUPS]] [--sondes] [--quakes [PERIOD]] [--fires] [--floods] [--tfrs] [--closures] [--traffic] [--route FROM TO] [--radar] [--alerts] [--outages] [--proxy URL] [--light] [--no-labels] [--snapshot COLSxROWS [--format F | --pick COL,ROW]]

  SOURCE        .pmtiles file or URL, .mbtiles file, XYZ template with {z}/{x}/{y}, or
                TileJSON URL, drawn with mapterm's terminal style (default: OpenFreeMap planet)
  --style       a MapLibre style instead: liberty, positron, bright, dark or fiord
                (OpenFreeMap); mapterm's own mapterm-dark, mapterm-light,
                mapterm-aviation, mapterm-weather or mapterm-roads; a style URL, a
                style.json file, or a Mapbox style such as
                mapbox://styles/mapbox/streets-v12 (needs a Mapbox token)
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
                ~78 km tile downloaded once and kept split in ~/.cache/mapterm.
                `overture-addresses`, `overture-buildings` and `overture-places` are
                Overture Maps' latest release of each (addresses show house numbers; / finds
                addresses and streets among those loaded)
  --cache       show mapterm's disk caches (tiles, imagery, geoparquet, docs, buildings,
                stac, orbits, radio, zones, radar, outages, and old files from earlier
                versions): how big, and their limits; then exit
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
                `here` is where you are (see --location-cmd), zoomed to its accuracy; or a
                place saved with B, by name (at its zoom)
  --session NAME  open a session saved with S: its layers, view, base map, overlays and
                layers turned off (options given after it win over its own)
  --continue, -c  open the last run again, as it was when it ended
  --resume [NAME]  pick a saved session or a recent run from a list (typing filters), or
                open NAME
  --sessions    list the saved sessions and recent runs, then exit (kept in
                ~/.config/mapterm/sessions, or $XDG_CONFIG_HOME/mapterm/sessions)
  --adsb [FEED]  live aircraft over the map, from a free ADS-B feed: lol (adsb.lol, the
                default), fi (adsb.fi), or a URL with {lat}, {lon} and {nm}. Polled for the
                area in view (adsb.lol every 30 s, 12 s with a key; others every 10 s);
                between polls planes move along their tracks.
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
                key), the last two hours in 10-minute scans, with a timeline under the
                map: click or drag it to go to a time, wheel to zoom it. space plays the
                two hours in a loop (again to pause), < and > play backward or forward,
                [ and ] slower or faster, , and . step a scan (. past the last goes back
                to now), R hides or shows it. Detail stops at zoom 7
  --alerts      US weather alerts from the National Weather Service (no key): warnings,
                watches and advisories as areas colored by severity; click one for its
                text. Areas that name forecast zones fill in as their outlines arrive
                (one a second, kept 90 days). With --radar, the alerts in force at the
                radar's time
  --outages     power outages by county across the US from ODIN (the Outage Data
                Initiative Nationwide, ORNL / U.S. DOE; participating utilities only),
                and from the states that publish a feed: Pennsylvania (PEMA / PennDOT)
                and Maryland (MDEM), with municipalities or ZIP codes from zoom 9.
                Shaded by the share of customers out, or by how many where that isn't
                given; click one for its numbers, and ODIN's restoration estimates.
                Asked for every 5 minutes; no key
  --quakes [PERIOD]
                earthquakes worldwide from USGS (no key): PERIOD is hour, day (the
                default) or all magnitudes, week (M2.5+) or month (M4.5+). The symbol
                grows with the magnitude; red the past hour, orange the past day, yellow
                the past week. Click one for its magnitude, depth, time, felt reports,
                PAGER alert and USGS page. Asked for once a minute
  --fires       US wildfires from NIFC's WFIGS service (no key): current incidents, ✹
                colored by containment (◇ prescribed burns), and the perimeters of those
                still burning; click one for its size, containment, cause and dates.
                Asked for every 5 minutes
  --floods      river and creek levels from NOAA's National Water Prediction Service (no
                key): every gauge in view from zoom 8, ◆ colored by NWS flood category
                (green none, yellow action, orange minor, red moderate, purple major), ↑
                when the forecast is worse. Click one for its stage, flood stages, record
                crests and the stage over the last days with the forecast. Every 10 minutes
  --tfrs        flight restrictions from the FAA (no key): temporary flight restrictions
                (VIP, security, hazards, space launches, air shows and sports) colored by
                kind, the DC SFRA and FRZ, prohibited areas, and stadiums' 3 NM event
                circles; click one for its areas, floors and ceilings, times and NOTAM
  --closures    road closures and work zones (no key): Maryland's county and city closures
                (construction, accidents, parades, special events; planned ones a week
                ahead), Montgomery County, Prince William County, Fairfax County, and
                MD, DE and NJ work zones; red closed, orange restricted, amber lanes,
                purple planned. Click one for its road, reason, times and source
  --traffic     traffic speeds: Maryland's CHART speed sensors (no key, each minute),
                a dot per sensor in its slowest direction's color, speeds from zoom 10.
                With a route, its time with traffic beside the free-flow time
  --route FROM TO
                directions, each end `here`, LAT,LON or a place to search for: the
                route drawn on the map, its distance and time (no traffic) on the status
                line, and the turn-by-turn steps in a panel (esc hides it, x clears the
                route). Up to three routes, each with its time: the others dim, 1-3
                picks one. With --closures, it avoids the closed roads. From Valhalla over
                OpenStreetMap: FOSSGIS's public server, asked only when you ask, or
                yours with --router URL (also MAPTERM_ROUTER). Or press d after a /
                search: directions there from your location (else the map's center)
  --route-mode car|bike|walk
                how you're going (default car)
  --utc         show times (radar, timeline, satellite passes, alerts) in UTC instead of
                this computer's time zone
  --sondes      live weather balloons (radiosondes) from SondeHub, tracked by amateur
                radio receivers (no key): ▲ climbing, ▼ falling, colored by altitude, with
                their flight as a trail; click or Tab for readings, frequency and the
                predicted landing
  --towers      antenna structures registered with the FCC (US; no key): † with their height,
                from zoom 8; inspect (i) for the structure type, owner and registration.
                The FCC's weekly file (~38 MB) is kept for a week
  --nasa-balloons [all]
                NASA's scientific balloons from its balloon facility (CSBF, no key): ◓ with
                the payload's name and its track so far, for flights heard in the last 3
                days; `all` adds past flights where they came down, dimmed, with their tracks
  --vdot-key-cmd CMD
                print your VDOT SmarterRoads token with a command (else
                VDOT_SMARTERROADS_TOKEN): --closures adds Virginia's work zones. Free
                registration at smarterroads.vdot.virginia.gov; subscribe to the WZDx
                dataset, and its Details page shows your token. Your own: VDOT's
                agreement doesn't allow sharing it
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
  --version     print mapterm's version and exit
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
      +/- zoom · sideways swipe pans · / search · i inspect · b base map · L layers · K legends ·
      D data sources · S sessions · B places · m/M next or previous base map · ' the one before · t theme · c copy
      coordinates · v vector overlay · n labels · q quit

pickers, in the middle of the map, fzf-style (typing filters, matched letters lit, best
      first; up/down or ctrl-n/p choose; esc closes):
  b   the base map, one of the map styles or imagery (satellite and aerial photos; keyed
      providers once their key is set): space picks it, enter picks it and closes
  L   what's over it, each on or off (enter or space): the live layers (○ not started yet:
      enter starts it, as its option would at launch: aircraft from adsb.lol, balloons,
      alerts, quakes, fires, river levels, TFRs, closures, traffic), imagery
      overlays (hillshade, Esri roads and places, NASA's daily picture), data layers,
      labels, the vector map over imagery, legends
  D   the data sources (live feeds and data files): how many features, when each last
      updated and how often, errors, how it relates to time (events, tracks drawn a little
      behind, frames...), where it's from and its credit
  B   places you saved: enter flies there; the first row saves the spot (the pointer's,
      else the center) under what's typed; ctrl-d twice forgets one. Saved places come
      first in / search too
  S   sessions saved and recent runs, with when each was made and opened: enter opens one;
      the first row saves this view under what's typed
      Imagery goes under the style's roads and names (v: all, names only, none).

search (/): type a place and press enter; one match flies there, several open a list
      (up/down or tab, enter or click to fly); esc clears the marker. Uses Photon
      (photon.komoot.io); MAPTERM_GEOCODER=nominatim or a server URL switches.

inspect mode (i): hover shows what's under the pointer; click lists every feature there
      (drawn or not) with its properties, size and location; click an entry or tab/shift+tab
      to switch; y copies it as JSON; esc closes, i leaves inspect mode

experimental: MAPTERM_SMOOTH=shift (pixel-smooth dragging) or MAPTERM_SMOOTH=1 (smooth
      up/down panning) on the Ghostty Smooth terminal fork; `s` then toggles it.
      See https://github.com/tlpinney/ghostty (branch smooth).

disclaimer: mapterm shows other people's data as they publish it: live layers (aircraft,
      ships, weather, alerts, outages, quakes, fires, river levels, flight restrictions,
      road closures) may be late, incomplete or wrong. Not for navigation, flight planning
      or emergencies: follow official sources and local authorities.";

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
    /// --sondes: live weather balloons from SondeHub.
    sondes: bool,
    /// --nasa-balloons: NASA's scientific balloons; `all`: past flights too.
    nasa: Option<bool>,
    /// --towers: the FCC's registered antenna structures.
    towers: bool,
    /// --alerts: US weather alerts from the NWS.
    alerts: bool,
    /// --floods: river and creek gauges from NOAA NWPS.
    floods: bool,
    /// --tfrs: FAA flight restrictions.
    tfrs: bool,
    /// --closures: road closures and work zones.
    closures: bool,
    /// --traffic: traffic speeds.
    traffic: bool,
    /// --route FROM TO: directions.
    route: Option<(String, String)>,
    /// --route-mode: car, bike or walk.
    route_mode: mapterm::route::Mode,
    /// --router: a Valhalla server of your own.
    router: Option<String>,
    /// --fires: US wildfires from NIFC.
    fires: bool,
    /// --quakes: earthquakes from USGS, for this period.
    quakes: Option<mapterm::quakes::Period>,
    /// --outages: power outages (Pennsylvania, Maryland).
    outages: bool,
    /// --utc: show times in UTC rather than the computer's time zone.
    utc: bool,
    /// --ais-key-cmd: prints the aisstream.io API key.
    ais_key_cmd: Option<String>,
    /// --vdot-key-cmd: prints the VDOT SmarterRoads token.
    vdot_key_cmd: Option<String>,
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
    /// The session opened (--session, --resume NAME, --continue): what to put back once the
    /// map is up.
    session: Option<mapterm::sessions::Session>,
    /// --resume without a name: open on the list of sessions.
    resume: bool,
    /// The options to keep in a saved session (mapterm::sessions::keep_args).
    kept: Vec<String>,
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
        sondes: false,
        nasa: None,
        towers: false,
        alerts: false,
        outages: false,
        quakes: None,
        fires: false,
        floods: false,
        tfrs: false,
        closures: false,
        traffic: false,
        route: None,
        route_mode: mapterm::route::Mode::Car,
        router: None,
        utc: false,
        sats: None,
        sats_country: Vec::new(),
        sats_color: SatColor::Orbit,
        ais_key_cmd: None,
        vdot_key_cmd: None,
        at_here: false,
        location_cmd: None,
        raster_headers: Vec::new(),
        raster_cache: None,
        session: None,
        resume: false,
        kept: Vec::new(),
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
    // Sessions first: --session NAME (or --resume NAME) and --continue put the session's
    // options before the ones given, which win over them; --resume alone opens on the list.
    let given: Vec<String> = std::env::args().skip(1).collect();
    let mut argv: Vec<String> = Vec::new();
    let mut session: Option<mapterm::sessions::Session> = None;
    let mut gi = given.into_iter().peekable();
    while let Some(x) = gi.next() {
        match x.as_str() {
            "--sessions" => {
                let now = unix_now();
                let all = mapterm::sessions::all();
                if all.is_empty() {
                    println!("no sessions yet: S in the viewer saves one, and each run is kept when it ends");
                }
                for (s, past) in all {
                    let name = if past { "(recent)".to_string() } else { s.name.clone() };
                    println!("{name:<24} {:<14} {:.4},{:.4} z{:.1}  {}", mapterm::sessions::ago(s.saved_at, now), s.lat, s.lon, s.zoom, s.summary());
                }
                std::process::exit(0);
            }
            "--continue" | "-c" => {
                session = Some(mapterm::sessions::latest().context("--continue: no earlier run kept yet")?);
            }
            "--session" | "--resume" => match gi.peek() {
                Some(n) if !n.starts_with('-') => {
                    let n = gi.next().expect("peeked");
                    session = Some(mapterm::sessions::open(&n, unix_now())?);
                }
                _ if x == "--resume" => a.resume = true,
                _ => bail!("--session NAME (mapterm --sessions lists them)"),
            },
            _ => argv.push(x),
        }
    }
    if let Some(s) = &session {
        argv = s.command_line().into_iter().chain(argv).collect();
    }
    a.kept = mapterm::sessions::keep_args(&argv);
    a.session = session;
    let mut it = argv.into_iter().peekable();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--at" => {
                let v = it.next().context("--at LAT,LON")?;
                if v.trim() == "here" {
                    a.at_here = true;
                    continue;
                }
                // LAT,LON, else a saved place's name.
                match v.split_once(',').map(|(la, lo)| (la.trim().parse::<f64>(), lo.trim().parse::<f64>())) {
                    Some((Ok(la), Ok(lo))) => a.at = Some((lo, la)),
                    _ => {
                        let p = mapterm::places::find(&v).with_context(|| format!("--at {v}: not LAT,LON, here, or a saved place (B saves one)"))?;
                        a.at = Some((p.lon, p.lat));
                        a.zoom = a.zoom.or(Some(p.zoom));
                    }
                }
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
            "--version" | "-V" => {
                println!("mapterm {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--ais" => a.ais = true,
            "--sondes" | "--balloons" => a.sondes = true,
            "--nasa-balloons" => a.nasa = Some(it.next_if(|n| n == "all").is_some()),
            "--towers" => a.towers = true,
            "--radar" => a.radar = true,
            "--alerts" => a.alerts = true,
            "--outages" => a.outages = true,
            "--fires" | "--wildfires" => a.fires = true,
            "--floods" | "--rivers" | "--gauges" => a.floods = true,
            "--tfrs" | "--tfr" | "--restrictions" => a.tfrs = true,
            "--closures" | "--roads" | "--workzones" => a.closures = true,
            "--traffic" | "--speeds" => a.traffic = true,
            "--route" | "--directions" => {
                let from = it.next().context("--route FROM TO")?;
                let to = it.next().context("--route FROM TO")?;
                a.route = Some((from, to));
            }
            "--route-mode" => {
                let m = it.next().context("--route-mode car|bike|walk")?;
                a.route_mode = mapterm::route::Mode::parse(&m).with_context(|| format!("--route-mode {m}: car, bike or walk"))?;
            }
            "--router" => a.router = Some(it.next().context("--router URL")?),
            "--quakes" | "--earthquakes" => {
                // The period is optional: take the next argument only if it's one.
                let p = it.next_if(|n| mapterm::quakes::Period::parse(n).is_some());
                a.quakes = Some(p.as_deref().and_then(mapterm::quakes::Period::parse).unwrap_or_default());
            }
            "--utc" => a.utc = true,
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
            "--vdot-key-cmd" => a.vdot_key_cmd = Some(it.next().context("--vdot-key-cmd CMD")?),
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
/// The weather style's entry in the `m` style cycle (and its --style name).
const WEATHER: &str = "mapterm-weather";
/// The roads style's entry in the `m` style cycle (and its --style name).
const ROADS: &str = "mapterm-roads";

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

/// The map sources of a live layer (mapterm::basemaps::LIVE's keys): its style layers are
/// the ones drawing from them.
fn live_sources(key: &str) -> &'static [&'static str] {
    match key {
        "aircraft" => &[AIRCRAFT, TRAILS],
        "ships" => &[SHIPS],
        "satellites" => &[SATS],
        "balloons" => &[BALLOONS, STATIONS, STATION_LINKS, STATION_AREAS, STATION_LINK_TILES, LAUNCH_SITES],
        "nasa" => &[NASA, NASA_TRACKS],
        "towers" => &[TOWERS],
        "alerts" => &[ALERTS, ALERT_TILES],
        "quakes" => &[QUAKES],
        "fires" => &[FIRES, FIRE_AREAS, FIRE_TILES],
        "gauges" => &[GAUGES],
        "outages" => &[OUTAGES, OUTAGE_TILES],
        "tfrs" => &[TFRS, TFR_TILES],
        "closures" => &[CLOSURES, CLOSURE_TILES],
        "traffic" => &[TRAFFIC],
        _ => &[],
    }
}

/// The images a live layer draws outside the map's layers (trails, satellite dots).
fn live_images(key: &str) -> &'static [u32] {
    match key {
        "aircraft" => &[TRAIL_IMAGE],
        "ships" => &[SHIP_TRAIL_IMAGE],
        "satellites" => &[SAT_IMAGE, PATH_IMAGE],
        "balloons" => &[BALLOON_TRAIL_IMAGE],
        _ => &[],
    }
}

/// Take a live layer's style layers off the map, to put back with [`show_live`]: them, and
/// the layer they were under.
fn hide_live(map: &mut Map, key: &str) -> (Vec<StyleLayer>, Option<String>) {
    hide_sources(map, live_sources(key))
}

/// Take the style layers drawing from `sources` off the map, to put back with [`show_live`]:
/// them, and the layer they were under.
fn hide_sources(map: &mut Map, sources: &[&str]) -> (Vec<StyleLayer>, Option<String>) {
    let ids: Vec<String> = map.layer_ids().into_iter().map(str::to_string).collect();
    let mine = |id: &str| map.layer(id).and_then(|l| l.source.as_deref()).is_some_and(|s| sources.contains(&s));
    let taken: Vec<StyleLayer> = ids.iter().filter(|id| mine(id)).filter_map(|id| map.layer(id).cloned()).collect();
    // The first layer above the last of them that stays: they go back under it.
    let last = ids.iter().rposition(|id| mine(id));
    let before = last.and_then(|at| ids[at + 1..].iter().find(|id| !mine(id)).cloned());
    let refs: Vec<&str> = taken.iter().map(|l| l.id.as_str()).collect();
    map.remove_layers(&refs);
    (taken, before)
}

/// Put back a live layer's style layers that [`hide_live`] took.
fn show_live(map: &mut Map, (layers, before): (Vec<StyleLayer>, Option<String>)) -> Result<()> {
    let before = before.filter(|b| map.layer_ids().contains(&b.as_str()));
    map.add_layers(layers, before.as_deref())
}

/// Ids of the imagery overlays' sources and layers start with this.
const OVERLAY_PREFIX: &str = "mapterm-imagery-overlay-";

/// The source and layer id of imagery overlay `i` (of mapterm::basemaps::OVERLAYS).
fn overlay_id(i: usize) -> String {
    format!("{OVERLAY_PREFIX}{}", mapterm::basemaps::OVERLAYS[i].preset)
}

/// Imagery layers: source and layer id, the provider, and the zoom it shows from (bottom first;
/// the last is "the" imagery for credits and billing).
type ImageryLayers = Vec<(String, Arc<mapterm::raster::RasterSource>, f64)>;
/// A base map switch's imagery, loaded: its spec, and its layers (none: no imagery).
type ImageryDone = (String, Result<ImageryLayers>);

/// Imagery requests a minute: --raster-limit's, else 300 for keyed (billed) providers.
fn imagery_limiter(limit: Option<Option<u32>>, billed: bool) -> Option<Bucket> {
    match limit {
        Some(n) => n,
        None if billed => Some(300),
        None => None,
    }
    .map(|n| Bucket::new(n.max(1)))
}

/// A provider for the base map switcher, keyed from the environment (or Mapbox's token).
async fn open_preset(name: &str, max_zoom: Option<u8>, cache: Option<bool>) -> Result<mapterm::raster::RasterSource> {
    let p = mapterm::raster::preset(name).with_context(|| format!("no imagery called {name}"))?;
    let mut key = mapterm::raster::find_key(None, p.key_env)?;
    if key.is_none() && p.name == "mapbox" {
        key = mapterm::mapbox::access_token();
    }
    let mut r = mapterm::raster::RasterSource::from_preset(p, key)?;
    if let Some(z) = max_zoom {
        r.max_zoom = r.max_zoom.min(z);
    }
    r.start_session().await?;
    let missing = r.missing_params();
    if !missing.is_empty() {
        bail!("{name} needs {}", missing.iter().map(|m| format!("{{{m}}}")).collect::<Vec<_>>().join(", "));
    }
    if let Some(c) = cache {
        r.cache = c;
    }
    Ok(r)
}

/// Imagery for the base map switcher: a provider, or a stack of them (`planetary`), each
/// layer under its own source id so tiles still loading for other imagery don't land in it.
async fn open_imagery(spec: String, cache: Option<bool>, cam: Camera) -> Result<ImageryLayers> {
    if let Some(st) = mapterm::raster::stack(&spec) {
        let mut v = Vec::new();
        for (i, sl) in st.layers.iter().enumerate() {
            let r = open_preset(sl.preset, sl.max_zoom, cache).await?;
            v.push((format!("{RASTER}-{spec}-{i}"), Arc::new(r), sl.min_zoom as f64));
        }
        return Ok(v);
    }
    let mut r = open_preset(&spec, None, cache).await?;
    if r.is_google() {
        let span = 360.0 / 2f64.powf(cam.zoom) * 2.0;
        let bounds = ((cam.lat + span * 0.4).min(85.0), (cam.lat - span * 0.4).max(-85.0), cam.lon + span, cam.lon - span);
        if let Ok(c) = r.google_credits(cam.zoom.round().clamp(0.0, 22.0) as u8, bounds).await {
            r.attribution = c;
        }
    }
    Ok(vec![(format!("{RASTER}-{spec}"), Arc::new(r), 0.0)])
}

/// Swap the imagery under the map for `layers` (none: take it away).
fn set_imagery(map: &mut Map, layers: &ImageryLayers) -> Result<()> {
    let old: Vec<String> = map.layer_ids().into_iter().filter(|id| is_imagery(id)).map(str::to_string).collect();
    let ids: Vec<&str> = old.iter().map(String::as_str).collect();
    map.remove_layers(&ids);
    let sources: Vec<String> = map.sources().map(|(id, _)| id.to_string()).filter(|id| is_imagery(id)).collect();
    for id in &sources {
        map.remove_source(id);
    }
    if layers.is_empty() {
        return Ok(());
    }
    let bottom = map.layer_ids().first().map(|s| s.to_string());
    for (id, r, _) in layers {
        map.add_source(id, MapSource::raster(r.clone()))?;
    }
    map.add_layers(layers.iter().map(|(id, _, z)| StyleLayer::raster(id, id).min_zoom(*z)).collect(), bottom.as_deref())?;
    Ok(())
}

/// A data layer's source (`--geojson`), for the data sources picker.
struct DataInfo {
    /// What was given: a path or URL.
    spec: String,
    /// Features read (None for a dataset loaded as the map moves).
    features: Option<usize>,
    /// Its format and size: `GeoJSON · 2.1 MB`, `GeoParquet, loaded as the map moves`.
    what: String,
    /// When it was read (Unix seconds).
    loaded: f64,
}

impl DataInfo {
    fn new(spec: &str, features: Option<usize>, streamed: bool) -> Self {
        let path = spec.split(['?', '#']).next().unwrap_or(spec);
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let format = match ext.as_str() {
            "geojson" | "json" => "GeoJSON",
            "csv" | "tsv" => "CSV",
            "gpx" => "GPX",
            "kml" => "KML",
            "fgb" => "FlatGeobuf",
            "shp" | "zip" => "shapefile",
            "parquet" | "geoparquet" => "GeoParquet",
            _ if mapterm::msbuildings::is_spec(spec) => "Microsoft building footprints",
            _ if mapterm::overture::is_spec(spec) => "Overture Maps",
            _ => "data",
        };
        let remote = spec.starts_with("https://") || spec.starts_with("http://");
        let size = (!remote).then(|| std::fs::metadata(path).ok()).flatten().map(|m| format!(" · {}", human_bytes(m.len())));
        let how = if streamed { ", loaded as the map moves" } else { "" };
        DataInfo { spec: spec.to_string(), features, what: format!("{format}{how}{}", size.unwrap_or_default()), loaded: unix_now() }
    }
}

/// A path or URL in at most `max` characters: the home folder as `~`, and when that's still
/// too long, its end (where the file name is) after `…`.
fn short_path(spec: &str, max: usize) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let s = match spec.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => spec.to_string(),
    };
    let n = s.chars().count();
    if n <= max { s } else { format!("…{}", s.chars().skip(n - max + 1).collect::<String>()) }
}

/// A size in bytes, readably: `512 B`, `2.1 MB`.
fn human_bytes(n: u64) -> String {
    let n = n as f64;
    match n {
        n if n < 1024.0 => format!("{n:.0} B"),
        n if n < 1024.0 * 1024.0 => format!("{:.0} KB", n / 1024.0),
        n if n < 1024.0 * 1024.0 * 1024.0 => format!("{:.1} MB", n / 1024.0 / 1024.0),
        n => format!("{:.1} GB", n / 1024.0 / 1024.0 / 1024.0),
    }
}

/// The camera that shows `target`'s center in the middle of the map left of `panel` (which
/// floats over the map's right side), rather than under the panel.
fn beside_panel(target: Camera, map: Rect, panel: Rect) -> Camera {
    if panel.width == 0 || map.width <= panel.width {
        return target;
    }
    let vp = Viewport::new(&target, map);
    let (cx, cy) = (map.x + map.width / 2, map.y + map.height / 2);
    // The center moves right by half the panel, so the target sits mid-way across the rest.
    let (lon0, _) = vp.cell_to_lonlat(cx, cy);
    let (lon1, _) = vp.cell_to_lonlat(cx + panel.width / 2, cy);
    Camera::new(target.lon + (lon1 - lon0), target.lat, target.zoom)
}

/// The launch site balloon `b` went up from: the nearest to where it was first heard, if that
/// was low and near one.
fn launch_site<'a>(sites: &'a [mapterm::upperair::Site], b: &mapterm::sondehub::Balloon) -> Option<&'a mapterm::upperair::Site> {
    let first = b.path.samples().next()?;
    mapterm::upperair::launched_from(sites, first.lon, first.lat, first.alt.unwrap_or(f64::MAX))
}

/// `STERLING/WASHINGTON DULLES INT` as `Sterling/Washington Dulles Int`.
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = !c.is_alphanumeric();
    }
    out
}

/// A weather balloon launch site's panel: what it is, when it launches, and the balloons
/// followed that went up from it (`launched`: serial, altitude now).
fn draw_site(f: &mut ratatui::Frame, area: Rect, s: &mapterm::upperair::Site, launched: &[(String, f64)], now: f64) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" balloon launch site ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let key = |k: &str| Span::styled(format!(" {k:<13}"), dim);
    let state = s.state.as_deref().map(|st| format!(", {st}")).unwrap_or_default();
    let mut l = vec![
        Line::from(Span::styled(format!(" ⊙ {}{state}", title_case(&s.name)), TStyle::new().fg(Color::Rgb(0x7e, 0xe7, 0x87)).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from(vec![key("station"), Span::raw(format!("WMO {}", s.wmo)), Span::styled(format!("  IGRA {}", s.id), dim)]),
        Line::from(vec![key("elevation"), Span::raw(format!("{:.0} m", s.elevation))]),
        Line::from(vec![key("position"), Span::raw(format!("{:.4}, {:.4}", s.lat, s.lon))]),
        Line::from(vec![
            key("launching"),
            Span::raw(format!("since {}", s.years.0)),
            Span::styled(format!("  {} soundings on record", thousands(s.soundings as usize)), dim),
        ]),
        Line::default(),
    ];
    // Most sites launch for 00 and 12 UTC, about an hour before.
    let next = mapterm::upperair::next_launch(now);
    l.push(Line::from(vec![
        key("launches"),
        Span::raw(format!("usually for {} and {}", hhmm(next), hhmm(next + 43_200.0))),
        Span::styled("  (00 and 12 UTC)", dim),
    ]));
    let to_go = next - 3600.0 - now;
    let when = if to_go > 0.0 { format!("in about {}", mapterm::sources::span(to_go)) } else { "about now".into() };
    l.push(Line::from(vec![key("next"), Span::raw(when), Span::styled("  (balloons go up about an hour early)", dim)]));
    l.push(Line::default());
    if launched.is_empty() {
        l.push(Line::from(Span::styled(" no balloon from here is being followed", dim)));
    } else {
        l.push(Line::from(Span::styled(" its balloons being followed", dim)));
        for (serial, alt) in launched {
            l.push(Line::from(vec![key(""), Span::raw(format!("▲ {serial}")), Span::styled(format!("  {:.1} km up", alt / 1000.0), dim)]));
        }
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(" NOAA NCEI IGRA · esc close", dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// A receiving station's line in the balloon panel: its screen row, where it is, its callsign.
struct StationRow {
    row: u16,
    lon: f64,
    lat: f64,
    callsign: String,
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
/// The --sondes source and layer, and its trail image.
const BALLOONS: &str = "balloons";
/// The stations receiving the balloons, and the lines from the selected balloon to them.
const STATIONS: &str = "balloon-stations";
/// NASA's scientific balloons (--nasa-balloons) and their tracks.
const NASA: &str = "nasa-balloons";
/// The FCC's registered antenna structures (--towers).
const TOWERS: &str = "fcc-towers";
/// Towers are drawn from this zoom (142,000 of them).
const TOWERS_MIN_ZOOM: f64 = 8.0;
const NASA_TRACKS: &str = "nasa-tracks";
const STATION_LINKS: &str = "balloon-links";
/// The stations' areas (where each is, somewhere inside).
const STATION_AREAS: &str = "balloon-station-areas";
/// The lines to the stations as image tiles, where the terminal shows images.
const STATION_LINK_TILES: &str = "balloon-link-tiles";
/// Where weather balloons are launched (NOAA's upper-air stations).
const LAUNCH_SITES: &str = "balloon-launch-sites";
/// Their color: violet, apart from the stations' green and the predicted path's blue.
const STATION_LINK_RGB: (u8, u8, u8) = (0xc0, 0x84, 0xfc);
const QUAKES: &str = "quakes";
const FIRES: &str = "fires";
const GAUGES: &str = "gauges";
const CLOSURES: &str = "closures";
const ROUTE: &str = "route";
const TRAFFIC: &str = "traffic";
/// The route as an image tile layer (where the terminal shows images).
const ROUTE_TILES: &str = "route-tiles";
/// The closures' lines as image tiles drawn here (where the terminal shows images).
const CLOSURE_TILES: &str = "closure-tiles";
const FIRE_AREAS: &str = "fire-areas";
const TFRS: &str = "tfrs";
/// The flight restrictions' style layers (braille, where the terminal doesn't show images).
const TFR_LAYERS: [&str; 2] = ["tfrs-line", "tfrs-selected"];
/// The flight restrictions as image tiles drawn here (where it does).
const TFR_TILES: &str = "tfr-tiles";
/// The fire perimeters' style layers (braille, where the terminal doesn't show images).
const FIRE_LAYERS: [&str; 3] = ["fire-areas-fill", "fire-areas-line", "fire-areas-selected"];
/// The fire perimeters as image tiles drawn here (where it does).
const FIRE_TILES: &str = "fire-tiles";
const BALLOON_TRAIL_IMAGE: u32 = 4_000_000_004;
/// --sondes draws balloons this many seconds in the past (they report about once a second).
const SONDE_DELAY: f64 = 5.0;
/// The weather balloons' credit.
const SONDE_CREDIT: &str = "SondeHub (CC BY-SA 2.0)";
/// Seconds of trail behind each balloon: the flight so far.
const SONDE_TRAIL: f64 = 4.0 * 3600.0;
const SHIP_TRAIL_IMAGE: u32 = 4_000_000_003;
/// The kitty image of paths: the selected satellite's ground track (over the trails).
const PATH_IMAGE: u32 = 4_000_000_005;
/// The kitty image of satellite dots and their names.
const SAT_IMAGE: u32 = 4_000_000_006;
/// The alert areas' style layers (braille, where the terminal doesn't show images).
const ALERT_LAYERS: [&str; 3] = ["alerts-fill", "alerts-line", "alerts-selected"];
/// The alert areas as image tiles drawn here (where it does).
const ALERT_TILES: &str = "alert-tiles";
/// The outage areas' style layers (braille, where the terminal doesn't show images).
const OUTAGE_LAYERS: [&str; 3] = ["outages-fill", "outages-line", "outages-selected"];
/// The outage areas as image tiles drawn here (where it does).
const OUTAGE_TILES: &str = "outage-tiles";
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

/// What's running, for which parts of the key list to show.
#[derive(Clone, Copy)]
struct HelpOn {
    radar: bool,
    alerts: bool,
    outages: bool,
    live: bool,
    sats: bool,
}

/// The key list: sections of (keys, what they do), those that apply.
fn help_sections(on: HelpOn) -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    let mut s = vec![
        (
            "map",
            vec![
                ("drag · arrows · hjkl", "pan"),
                ("wheel · + -", "zoom (at the pointer)"),
                ("double-click", "zoom in (shift: out)"),
                ("shift+drag", "zoom to a box"),
                ("g", "go to your location"),
                ("c", "copy the coordinates"),
                ("S", "sessions: open one, or save this view"),
                ("B", "places: fly to one, or save this spot"),
            ],
        ),
        (
            "look",
            vec![
                ("b", "base map: one style or imagery"),
                ("L", "layers: live, imagery overlays, data, legends"),
                ("K", "legends on or off"),
                ("D", "data sources: updated when, errors, time"),
                ("m M", "next or previous base map"),
                ("'", "back to the base map before"),
                ("t", "light or dark"),
                ("n", "labels on or off"),
                ("v", "vector overlay: all, labels, none"),
            ],
        ),
        (
            "find",
            vec![
                ("/", "search for a place"),
                ("d", "directions to it (from you, else the center)"),
                ("1-3", "pick one of the routes"),
                ("x", "clear the route"),
                ("i", "inspect: what's under the pointer"),
                ("y", "copy the inspected feature (JSON)"),
            ],
        ),
    ];
    if on.live || on.alerts || on.outages {
        let areas = on.alerts || on.outages;
        let click = match (on.live, areas) {
            (true, true) => ("click · Tab", "select (aircraft, ship, satellite; or click an area)"),
            (true, false) => ("click · Tab", "select (aircraft, ship, satellite)"),
            _ => ("click", "an area's details"),
        };
        let mut v = vec![click, ("Esc", "close the panel")];
        if on.live {
            v.insert(1, ("f", "follow the selected one"));
            v.insert(2, ("p", "finder: names, countries, missions..."));
        }
        if on.sats {
            v.push(("o", "satellites by orbit, country or mission"));
        }
        s.push(("live", v));
    }
    if on.radar {
        s.push((
            "radar and time",
            vec![
                ("space", "play the last two hours, or pause"),
                ("< >", "play backward or forward"),
                ("[ ]", "slower or faster"),
                (", .", "a scan back or on (. past the last: now)"),
                ("R", "radar on or off"),
                ("timeline", "click or drag: go to a time; wheel: zoom"),
            ],
        ));
    } else if on.alerts {
        s.push(("time", vec![("timeline", "click or drag: go to a time; wheel: zoom")]));
    }
    s.push(("", vec![("?", "this list"), ("q", "quit")]));
    s
}

/// The key list over the map, in two columns where it's wide enough.
fn draw_help(f: &mut ratatui::Frame, area: Rect, on: HelpOn) {
    use ratatui::style::Modifier;
    let dim = TStyle::new().fg(Color::DarkGray);
    let key = TStyle::new().fg(Color::Rgb(0xe1, 0xa0, 0x46));
    let head = TStyle::new().fg(Color::Rgb(0x50, 0x82, 0xbe)).add_modifier(Modifier::BOLD);
    let section_lines = |sec: &(&str, Vec<(&str, &str)>)| -> Vec<Line<'static>> {
        let mut l = Vec::new();
        if !sec.0.is_empty() {
            l.push(Line::from(Span::styled(format!(" {}", sec.0), head)));
        }
        for (k, what) in &sec.1 {
            l.push(Line::from(vec![Span::styled(format!("  {k:<22}"), key), Span::raw(what.to_string())]));
        }
        l.push(Line::default());
        l
    };
    let sections = help_sections(on);
    let mut all: Vec<Vec<Line>> = sections.iter().map(section_lines).collect();
    // The disclaimer, wrapped to the column, last.
    let mut d = vec![Line::from(Span::styled(" disclaimer", TStyle::new().fg(DISCLAIMER_COLOR).add_modifier(Modifier::BOLD)))];
    let mut cur = String::new();
    for word in DISCLAIMER_FULL.split_whitespace() {
        if cur.chars().count() + word.chars().count() + 1 > 54 {
            d.push(Line::from(Span::styled(format!("  {cur}"), TStyle::new().fg(Color::Rgb(0xe6, 0xe6, 0xe6)))));
            cur.clear();
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur += word;
    }
    d.push(Line::from(Span::styled(format!("  {cur}"), TStyle::new().fg(Color::Rgb(0xe6, 0xe6, 0xe6)))));
    all.push(d);
    let total: usize = all.iter().map(Vec::len).sum();
    let two = area.width >= 110 && total + 2 > area.height as usize;
    // Split into two columns at the section nearest half way.
    let mut cols: Vec<Vec<Line>> = vec![Vec::new()];
    let mut run = 0;
    for sec in all {
        if two && cols.len() == 1 && run >= total / 2 {
            cols.push(Vec::new());
        }
        run += sec.len();
        cols.last_mut().expect("a column").extend(sec);
    }
    let col_w: u16 = 70;
    let w = (col_w * cols.len() as u16 + 2).min(area.width);
    let h = (cols.iter().map(Vec::len).max().unwrap_or(0) as u16 + 2).min(area.height);
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(ratatui::widgets::Clear, r);
    let block = Block::bordered()
        .border_style(dim)
        .title(Line::from(vec![Span::raw(" "), Span::styled("⠍", key), Span::styled("⠞", TStyle::new().fg(Color::Rgb(0x50, 0x82, 0xbe))), Span::raw(" keys ")]))
        .title_bottom(Line::from(Span::styled(" ? or esc closes ", dim)).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    for (i, c) in cols.into_iter().enumerate() {
        let cr = Rect { x: inner.x + i as u16 * col_w, width: col_w.min(inner.width.saturating_sub(i as u16 * col_w)), ..inner };
        f.render_widget(Paragraph::new(c), cr);
    }
}

/// Whether the timeline shows: with radar that has scans (and isn't hidden), or alerts.
fn timeline_on(radar: &Option<mapterm::timelayer::TimeRaster>, alerts: bool) -> bool {
    alerts || radar.as_ref().is_some_and(|r| !r.is_off() && !r.frames().is_empty())
}

/// mapterm's mark, as on mapterm.com: "mt" in braille (⠍⠞) and the name, "map" amber and
/// "term" blue (the road and water colors of mapterm's dark style), on the icon's dark tile.
fn logo_spans() -> Vec<Span<'static>> {
    let tile = Color::Rgb(0x10, 0x14, 0x18);
    let (amber, blue) = (TStyle::new().fg(Color::Rgb(0xe1, 0xa0, 0x46)).bg(tile), TStyle::new().fg(Color::Rgb(0x50, 0x82, 0xbe)).bg(tile));
    vec![
        Span::styled(" ", TStyle::new().bg(tile)),
        Span::styled("⠍", amber),
        Span::styled("⠞", blue),
        Span::styled(" ", TStyle::new().bg(tile)),
        Span::styled("map", amber),
        Span::styled("term", blue),
        Span::styled(" ", TStyle::new().bg(tile)),
    ]
}

/// The mark's width in cells.
const LOGO_W: u16 = 12;

/// Shown after the credits while a live or hazard layer is on.
const DISCLAIMER: &str = "Not for navigation or emergencies (? more)";

/// The disclaimer's color: amber, bold, so it stands out from the dim credits.
const DISCLAIMER_COLOR: Color = Color::Rgb(0xff, 0xc8, 0x3c);

/// A line of credits in `dim`, with the disclaimer in it (where it is) bold and amber.
fn credit_spans(line: &str, dim: TStyle) -> Vec<Span<'static>> {
    match line.find(DISCLAIMER) {
        Some(i) => vec![
            Span::styled(line[..i].to_string(), dim),
            Span::styled(DISCLAIMER, TStyle::new().fg(DISCLAIMER_COLOR).add_modifier(ratatui::style::Modifier::BOLD)),
            Span::styled(line[i + DISCLAIMER.len()..].to_string(), dim),
        ],
        None => vec![Span::styled(line.to_string(), dim)],
    }
}

/// The full disclaimer, at the end of the ? key list.
const DISCLAIMER_FULL: &str = "mapterm shows other people's data as they publish it. Live layers (aircraft, ships, weather, alerts, outages, quakes, fires, river levels, flight restrictions, road closures, traffic) can be late, incomplete or wrong, and a service can change or stop. Not for navigation, flight planning or emergencies: follow official sources and local authorities.";

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

/// The base map switcher's background.
const SWITCHER_BG: Color = Color::Rgb(17, 24, 39);

/// The base map switcher (b): a box in the middle of the map with the styles, the imagery and
/// what's drawn over it. Returns the box and each row's screen line, for clicks.
#[allow(clippy::too_many_arguments)]
fn draw_switcher(
    f: &mut ratatui::Frame,
    area: Rect,
    sw: &mapterm::basemaps::Switcher,
    list: &[mapterm::basemaps::Entry],
    current: usize,
    loading: bool,
    labels: bool,
    vector: u8,
    overlays: &[(bool, bool)],
    live_state: &dyn Fn(usize) -> (&'static str, Option<&'static str>),
    data_on: &dyn Fn(usize) -> bool,
    legends: bool,
    source: &dyn Fn(mapterm::basemaps::Row) -> Option<(String, String)>,
) -> (Rect, Vec<(u16, mapterm::basemaps::Row)>) {
    use mapterm::basemaps::{Picker, Row};
    use ratatui::style::Modifier;
    let dim = TStyle::new().fg(Color::Rgb(148, 163, 184));
    let accent = Color::Rgb(127, 178, 255);
    let head = TStyle::new().fg(accent).add_modifier(Modifier::BOLD);
    // Lines: section headings (while nothing is typed) and rows.
    let mut lines: Vec<(Option<usize>, Line)> = Vec::new();
    // Sessions say more (what, when made, when opened): wider.
    let width = if matches!(sw.kind, Picker::Sessions | Picker::Sources | Picker::Places) { 84u16 } else { 58 }.min(area.width.saturating_sub(4));
    let w = width.saturating_sub(4) as usize;
    let mut section = "";
    for (n, row) in sw.rows.iter().enumerate() {
        let name = match row {
            Row::Base(i) if list[*i].base.is_imagery() => "imagery",
            Row::Base(_) => "maps",
            Row::Live(_) => "live layers",
            Row::Data(_) => "data",
            Row::Labels | Row::Vector | Row::Legends => "map",
            Row::Session(_) | Row::SaveAs | Row::Place(_) => "",
            Row::Overlay(_) => "imagery overlays",
        };
        if sw.query.is_empty() && name != section {
            if !section.is_empty() {
                lines.push((None, Line::raw("")));
            }
            lines.push((None, Line::styled(format!(" {name}"), head)));
            section = name;
        }
        let (mark, label, detail) = match row {
            Row::Base(i) => {
                let e = &list[*i];
                let mark = if *i == current && loading {
                    "◌"
                } else if *i == current {
                    "●"
                } else {
                    "○"
                };
                let detail = if e.keyed { format!("{} · key", e.detail) } else { e.detail.clone() };
                (mark, e.label.clone(), detail)
            }
            Row::Labels => (if labels { "☑" } else { "☐" }, "labels".to_string(), "place and road names · n".to_string()),
            Row::Vector => {
                let state = ["roads and names", "names only", "nothing"][vector.min(2) as usize];
                ("▤", "over imagery".to_string(), format!("{state} · v"))
            }
            Row::Overlay(i) => {
                let o = &mapterm::basemaps::OVERLAYS[*i];
                let (on, loading) = overlays.get(*i).copied().unwrap_or_default();
                (
                    if loading {
                        "◌"
                    } else if on {
                        "☑"
                    } else {
                        "☐"
                    },
                    o.label.to_string(),
                    o.detail.to_string(),
                )
            }
            Row::Session(i) => {
                let (name, detail) = &sw.sessions[*i];
                ("↺", name.clone(), detail.clone())
            }
            Row::SaveAs => ("＋", sw.text(Row::SaveAs, list).0, if sw.kind == Picker::Places { "B" } else { "S" }.to_string()),
            Row::Place(i) => ("⚑", sw.sessions[*i].0.clone(), sw.sessions[*i].1.clone()),
            Row::Data(i) => (if data_on(*i) { "☑" } else { "☐" }, sw.data[*i].clone(), "data layer".to_string()),
            Row::Legends => (if legends { "☑" } else { "☐" }, "legends".to_string(), "keys to the colors".to_string()),
            Row::Live(i) => {
                let l = &mapterm::basemaps::LIVE[*i];
                let (mark, note) = live_state(*i);
                (mark, l.label.to_string(), note.unwrap_or(l.detail).to_string())
            }
        };
        // A data source: its status on the line, and where it's from under it.
        let (detail, origin) = match (sw.kind, source(*row)) {
            (Picker::Sources, Some((status, origin))) => (status, Some(origin)),
            _ => (detail, None),
        };
        let chosen = n == sw.sel;
        let st = if chosen { TStyle::new().add_modifier(Modifier::REVERSED) } else { TStyle::new() };
        let dst = if chosen { st } else { dim };
        // The name with the letters the query matched picked out, as fzf does.
        let hit = st.fg(accent).add_modifier(Modifier::BOLD);
        let marks = sw.marks.get(n).map_or(&[][..], Vec::as_slice);
        let mut spans = vec![Span::styled(format!(" {mark} "), st)];
        for (k, c) in label.chars().enumerate() {
            spans.push(Span::styled(c.to_string(), if marks.contains(&k) { hit } else { st }));
        }
        let left = 3 + mark.chars().count() + label.chars().count();
        let room = w.saturating_sub(left + 2);
        let detail: String =
            if detail.chars().count() > room { format!("{}…", detail.chars().take(room.saturating_sub(1)).collect::<String>()) } else { detail };
        let pad = " ".repeat(w.saturating_sub(left + detail.chars().count()));
        spans.push(Span::styled(format!("{pad}{detail} "), dst));
        lines.push((Some(n), Line::from(spans)));
        if let Some(o) = origin {
            let room = w.saturating_sub(5);
            let o: String = if o.chars().count() > room { format!("{}…", o.chars().take(room.saturating_sub(1)).collect::<String>()) } else { o };
            lines.push((None, Line::styled(format!("     {o}"), dim)));
        }
    }
    if sw.rows.is_empty() {
        lines.push((None, Line::styled(" nothing matches", dim)));
    }
    // Rows, then a rule and the prompt line.
    let height = (lines.len() as u16 + 4).min(area.height.saturating_sub(2)).max(5);
    let rect = Rect { x: area.x + (area.width.saturating_sub(width)) / 2, y: area.y + (area.height.saturating_sub(height)) / 2, width, height };
    // Opaque: its own colors (whatever the terminal's), over the map and its images (which
    // kitty puts under cells with a background; see mapterm::kitty::under_cells).
    let block = Block::bordered()
        .style(TStyle::new().bg(SWITCHER_BG).fg(Color::Rgb(226, 232, 240)))
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(TStyle::new().fg(accent))
        .title(Span::styled(format!(" {} ", sw.kind.title()), head))
        .title_bottom(
            Line::styled(
                match sw.kind {
                    Picker::Base => " ↑↓ choose · space pick · enter pick and close · esc close ",
                    Picker::Layers => " ↑↓ choose · enter or space on/off · esc close ",
                    Picker::Sessions => " ↑↓ choose · enter open or save · esc close ",
                    Picker::Sources => " ↑↓ choose · enter or space on/off · esc close ",
                    Picker::Places => " ↑↓ choose · enter fly or save · ctrl-d twice forgets · esc close ",
                },
                dim,
            )
            .right_aligned(),
        );
    let outer = block.inner(rect);
    f.render_widget(ratatui::widgets::Clear, rect);
    f.render_widget(block, rect);
    // fzf's prompt: what's typed, a cursor, and how many rows match of all of them.
    let count = format!("{}/{} ", sw.rows.len(), sw.total);
    let typed = format!(" > {}", sw.query);
    let gap = (outer.width as usize).saturating_sub(typed.chars().count() + 1 + count.chars().count());
    let prompt = Line::from(vec![
        Span::styled(" > ", TStyle::new().fg(accent).add_modifier(Modifier::BOLD)),
        Span::raw(sw.query.clone()),
        Span::styled("█", TStyle::new().fg(accent)),
        Span::styled(format!("{}{count}", " ".repeat(gap)), dim),
    ]);
    let prompt_at = Rect { y: outer.bottom().saturating_sub(1), height: 1, ..outer };
    f.render_widget(Paragraph::new(prompt), prompt_at);
    let rule = Rect { y: outer.bottom().saturating_sub(2), height: 1, ..outer };
    f.render_widget(Paragraph::new(Line::styled("─".repeat(outer.width as usize), dim)), rule);
    let inner = Rect { height: outer.height.saturating_sub(2), ..outer };
    // Keep the chosen row in view.
    let rows = inner.height as usize;
    let at = lines.iter().position(|(n, _)| *n == Some(sw.sel)).unwrap_or(0);
    let first = (at + 1).saturating_sub(rows);
    let mut hits = Vec::new();
    let mut shown = Vec::new();
    for (k, (n, line)) in lines.into_iter().skip(first).take(rows).enumerate() {
        if let Some(n) = n {
            hits.push((inner.y + k as u16, sw.rows[n]));
        }
        shown.push(line);
    }
    f.render_widget(Paragraph::new(shown), inner);
    (rect, hits)
}

/// The --sats source and layer.
const SATS: &str = "satellites";
const ALERTS: &str = "alerts";
const OUTAGES: &str = "outages";

/// `HH:MM` (UTC) of a Unix time.
/// The time zone times are shown in: this computer's, or UTC with --utc.
static ZONE: std::sync::OnceLock<mapterm::clock::Zone> = std::sync::OnceLock::new();

fn zone() -> mapterm::clock::Zone {
    ZONE.get().copied().unwrap_or_default()
}

/// `t` as `HH:MM EST` in the zone times are shown in.
fn hhmm(t: f64) -> String {
    zone().hhmm_zone(t)
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
                let when = if p.rise <= now { "now".to_string() } else { format!("{}, in {:.0} min", hhmm(p.rise), mins(p.rise)) };
                l.push(Line::from(vec![key("next pass"), Span::raw(when)]));
                l.push(Line::from(vec![key("highest"), Span::raw(format!("{:.0}° at {}", p.max_elevation, hhmm(p.peak)))]));
                l.push(Line::from(vec![key("sets"), Span::raw(hhmm(p.set))]));
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

fn draw_alert(f: &mut ratatui::Frame, area: Rect, al: &mapterm::nws::Alert) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" weather alert ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let (r, g, b) = al.severity.rgb();
    let key = |k: &str| Span::styled(format!(" {k:<9}"), dim);
    let mut l = vec![Line::from(Span::styled(format!(" {}", al.event), TStyle::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)))];
    l.push(Line::default());
    l.push(Line::from(vec![key("severity"), Span::styled(al.severity.label(), TStyle::new().fg(Color::Rgb(r, g, b)))]));
    if let Some(to) = al.to {
        // In the zone times are shown in, with the day when it's not today.
        let (off, _) = zone().at(to);
        let day = |t: f64| ((t + zone().at(t).0 as f64) / 86400.0).floor();
        let when = if day(to) == day(unix_now()) {
            hhmm(to)
        } else {
            let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(((to + off as f64) / 86400.0).floor() as i64).rem_euclid(7) as usize];
            format!("{wd} {}", hhmm(to))
        };
        l.push(Line::from(vec![key("until"), Span::raw(when)]));
    } else if let Some(u) = &al.until {
        l.push(Line::from(vec![key("until"), Span::raw(u.clone())]));
    }
    if !al.sender.is_empty() {
        l.push(Line::from(vec![key("from"), Span::raw(al.sender.clone())]));
    }
    l.push(Line::from(vec![key("area"), Span::raw(al.area.clone())]));
    let para = |l: &mut Vec<Line>, text: &str, st: TStyle| {
        l.push(Line::default());
        // NWS text is hard-wrapped at 69 columns: rejoin lines within a paragraph.
        for p in text.split("\n\n") {
            l.push(Line::from(Span::styled(format!(" {}", p.split_whitespace().collect::<Vec<_>>().join(" ")), st)));
        }
    };
    if let Some(h) = &al.headline {
        para(&mut l, h, TStyle::new().add_modifier(Modifier::BOLD));
    }
    if !al.description.is_empty() {
        para(&mut l, &al.description, TStyle::new());
    }
    if let Some(i) = &al.instruction {
        para(&mut l, i, TStyle::new().fg(Color::Yellow));
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(" esc close", dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// What the --floods requests send.
enum GaugeMsg {
    /// The gauges around the view.
    Gauges(std::result::Result<Vec<mapterm::floods::Gauge>, String>),
    /// A gauge's details and stage series.
    Detail(String, std::result::Result<mapterm::floods::Detail, String>),
}

/// The --floods panel for the selected gauge: its stage, flood stages, the stage over the
/// last three days with the forecast, and record crests.
fn draw_gauge(f: &mut ratatui::Frame, area: Rect, g: &mapterm::floods::Gauge, d: Option<&mapterm::floods::Detail>) {
    use mapterm::floods::Category;
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" river gauge ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let rgb = |c: Category| {
        let (r, gr, b) = c.rgb();
        Color::Rgb(r, gr, b)
    };
    let key = |k: &str| Span::styled(format!(" {k:<9}"), dim);
    let units = if g.units.is_empty() { "ft" } else { g.units.as_str() };
    let mut l = vec![Line::from(Span::styled(format!(" {}", g.name), TStyle::new().fg(rgb(g.worst())).add_modifier(Modifier::BOLD)))];
    l.push(Line::from(Span::styled(format!(" {} · NWS", g.lid), dim)));
    l.push(Line::default());
    match g.stage {
        Some(st) => {
            let when = g.time.map(|t| format!(" · {}", day_hhmm(t))).unwrap_or_default();
            l.push(Line::from(vec![key("stage"), Span::raw(format!("{st:.2} {units}{when}"))]));
        }
        None => l.push(Line::from(vec![key("stage"), Span::styled("no current reading", dim)])),
    }
    if let Some(q) = g.flow {
        l.push(Line::from(vec![key("flow"), Span::raw(format!("{} cfs", thousands(q.round() as usize)))]));
    }
    l.push(Line::from(vec![key("now"), Span::styled(g.category.label(), TStyle::new().fg(rgb(g.category)))]));
    if let Some(fc) = g.forecast {
        let at = g.forecast_stage.map(|s| format!(" ({s:.1} {units})")).unwrap_or_default();
        l.push(Line::from(vec![key("forecast"), Span::styled(format!("{}{at}", fc.label()), TStyle::new().fg(rgb(fc)))]));
    }
    match d {
        None => {
            l.push(Line::default());
            l.push(Line::from(Span::styled(" loading flood stages…", dim)));
        }
        Some(d) => {
            if !d.stages.is_empty() {
                l.push(Line::default());
                for (c, s) in d.stages.iter().rev() {
                    let here = g.stage.is_some_and(|st| st >= *s);
                    l.push(Line::from(vec![
                        key(match c {
                            Category::Major => "major",
                            Category::Moderate => "moderate",
                            Category::Minor => "minor",
                            _ => "action",
                        }),
                        Span::styled(format!("{s} {units}{}", if here { "  ◀ at or above" } else { "" }), TStyle::new().fg(rgb(*c))),
                    ]));
                }
            }
            let width = (inner.width as usize).saturating_sub(2).min(60);
            let (line, start, (lo, hi)) = mapterm::floods::sparkline(&d.observed, &d.forecast, unix_now() - 3.0 * 86400.0, width);
            if !line.is_empty() {
                l.push(Line::default());
                let chars: Vec<char> = line.chars().collect();
                let cut = start.unwrap_or(chars.len()).min(chars.len());
                l.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(chars[..cut].iter().collect::<String>(), TStyle::new().fg(Color::Rgb(0x4f, 0xa3, 0xff))),
                    Span::styled(chars[cut..].iter().collect::<String>(), TStyle::new().fg(Color::Rgb(0xc8, 0x4d, 0xe8))),
                ]));
                let fc = if start.is_some() { " · forecast in purple" } else { "" };
                l.push(Line::from(Span::styled(format!(" last 3 days, {lo:.1}–{hi:.1} {units}{fc}"), dim)));
            }
            if !d.crests.is_empty() {
                l.push(Line::default());
                l.push(Line::from(Span::styled(" record crests", dim)));
                for (s, t) in &d.crests {
                    let year = 1970 + (t / 31_556_952.0).floor() as i64;
                    l.push(Line::from(Span::raw(format!("   {s:.2} {units} · {year}"))));
                }
            }
            if !d.attribution.is_empty() {
                l.push(Line::default());
                l.push(Line::from(Span::styled(format!(" Data: {}; NOAA NWPS.", d.attribution.join(", ")), dim)));
            }
        }
    }
    l.push(Line::from(Span::styled(" Floods: follow your local NWS office. esc close", dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// The --closures panel for the selected closure.
fn draw_closure(f: &mut ratatui::Frame, area: Rect, c: &mapterm::closures::Closure) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let now = unix_now();
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(if c.planned(now) { " planned road closure " } else { " road closure " });
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let (r, g, b) = mapterm::closures::rgb(c, now);
    let key = |k: &str| Span::styled(format!(" {k:<9}"), dim);
    let title = if c.road.is_empty() { c.event.clone() } else { c.road.clone() };
    let mut l = vec![Line::from(Span::styled(format!(" {title}"), TStyle::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)))];
    if !c.at.is_empty() {
        l.push(Line::from(Span::raw(format!(" {}", c.at))));
    }
    l.push(Line::default());
    l.push(Line::from(vec![key("closure"), Span::styled(c.impact.label(), TStyle::new().fg(Color::Rgb(r, g, b)))]));
    if !c.event.is_empty() {
        l.push(Line::from(vec![key("why"), Span::raw(c.event.clone())]));
    }
    if !c.direction.is_empty() {
        l.push(Line::from(vec![key("direction"), Span::raw(c.direction.clone())]));
    }
    let when = match (c.start, c.end) {
        (Some(s), Some(e)) => format!("{} to {}", day_hhmm(s), day_hhmm(e)),
        (Some(s), None) => format!("since {}", day_hhmm(s)),
        (None, Some(e)) => format!("until {}", day_hhmm(e)),
        (None, None) => String::new(),
    };
    if !when.is_empty() {
        l.push(Line::from(vec![key("when"), Span::raw(when)]));
    }
    if !c.jurisdiction.is_empty() {
        l.push(Line::from(vec![key("where"), Span::raw(c.jurisdiction.clone())]));
    }
    if !c.text.is_empty() {
        l.push(Line::default());
        l.push(Line::from(Span::raw(format!(" {}", c.text))));
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(format!(" From {}. esc close", c.source), dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// A route end: `here` (your location), `LAT,LON`, or a place to search for.
async fn route_end(spec: &str, geo: &mapterm::geocode::Geocoder, location_cmd: Option<String>) -> std::result::Result<(f64, f64), String> {
    if spec.eq_ignore_ascii_case("here") {
        let fix = tokio::task::spawn_blocking(move || mapterm::locate::locate(location_cmd.as_deref()))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("here: {e:#}"))?;
        return Ok((fix.lon, fix.lat));
    }
    if let Some((a, b)) = spec.split_once(',')
        && let (Ok(lat), Ok(lon)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>())
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
    {
        return Ok((lon, lat));
    }
    let places = geo.search(spec, None, 1).await.map_err(|e| format!("{spec}: {e:#}"))?;
    places.first().map(|p| (p.lon, p.lat)).ok_or_else(|| format!("no place found for {spec}"))
}

/// The directions panel: the trip, then each step with its distance.
fn draw_route(f: &mut ratatui::Frame, area: Rect, routes: &[mapterm::route::Route], pick: usize, traffic: &[Option<mapterm::traffic::Estimate>]) {
    let Some(r) = routes.get(pick) else { return };
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let cyan = TStyle::new().fg(Color::Rgb(0x22, 0xd3, 0xee));
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" directions ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let mut l = vec![Line::from(Span::styled(
        format!(" {} {} · {}", r.mode.verb(), mapterm::route::distance(r.miles), mapterm::route::duration(r.seconds)),
        cyan.add_modifier(Modifier::BOLD),
    ))];
    let with = |i: usize| traffic.get(i).cloned().flatten();
    match with(pick) {
        Some(e) => {
            let late = (e.seconds - r.seconds) / 60.0;
            let st = TStyle::new().fg(if late >= 5.0 {
                Color::Rgb(0xf9, 0x73, 0x16)
            } else if late >= 1.0 {
                Color::Rgb(0xea, 0xb3, 0x08)
            } else {
                Color::Rgb(0x22, 0xc5, 0x5e)
            });
            l.push(Line::from(vec![
                Span::styled(format!(" {} with traffic", mapterm::route::duration(e.seconds)), st.add_modifier(Modifier::BOLD)),
                Span::styled(format!(" · {} sensor{} on the way", e.sensors, if e.sensors == 1 { "" } else { "s" }), dim),
            ]));
        }
        None => l.push(Line::from(Span::styled(" estimated, without traffic", dim))),
    }
    // The choices: each one's time, miles and main road; the picked one bright.
    if routes.len() > 1 {
        l.push(Line::default());
        for (i, o) in routes.iter().enumerate() {
            let via = o.via().map(|v| format!("  via {v}")).unwrap_or_default();
            let t = with(i).map(|e| format!(" → {}", mapterm::route::duration(e.seconds))).unwrap_or_default();
            let text = format!(" {} {:>9}{t} · {}{via}", i + 1, mapterm::route::duration(o.seconds), mapterm::route::distance(o.miles));
            let st = if i == pick { cyan.add_modifier(Modifier::BOLD | Modifier::REVERSED) } else { TStyle::new().fg(Color::Gray) };
            l.push(Line::from(Span::styled(text, st)));
        }
        l.push(Line::from(Span::styled(format!(" 1-{} picks a route", routes.len()), dim)));
    }
    if r.avoided > 0 {
        l.push(Line::from(Span::styled(
            format!(" avoiding {} closed road point{}", r.avoided, if r.avoided == 1 { "" } else { "s" }),
            TStyle::new().fg(Color::Rgb(0xff, 0x7a, 0x32)),
        )));
    }
    l.push(Line::default());
    for (i, s) in r.steps.iter().enumerate() {
        let far = if s.miles > 0.0 { format!("  {}", mapterm::route::distance(s.miles)) } else { String::new() };
        l.push(Line::from(vec![Span::styled(format!(" {:>2}. ", i + 1), dim), Span::raw(s.instruction.clone()), Span::styled(far, dim)]));
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(" Valhalla, OpenStreetMap. Not for navigation. x clears · esc hides", dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// What the --tfrs tasks send.
enum TfrMsg {
    /// The current TFRs.
    Live(Vec<mapterm::tfrs::Restriction>),
    /// The standing airspace.
    Standing(Vec<mapterm::tfrs::Restriction>),
    /// A TFR's NOTAM.
    Notam(String, std::result::Result<mapterm::tfrs::Notam, String>),
    /// Why a request failed.
    Error(String),
}

/// The --tfrs panel for the selected restriction.
fn draw_tfr(f: &mut ratatui::Frame, area: Rect, r: &mapterm::tfrs::Restriction, n: Option<&mapterm::tfrs::Notam>) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block =
        Block::new().borders(Borders::LEFT).border_style(dim).title(if r.kind.is_tfr() { " flight restriction (TFR) " } else { " restricted airspace " });
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let (red, g, b) = r.kind.rgb();
    let key = |k: &str| Span::styled(format!(" {k:<9}"), dim);
    let mut l = vec![Line::from(Span::styled(format!(" {}", r.title), TStyle::new().fg(Color::Rgb(red, g, b)).add_modifier(Modifier::BOLD)))];
    l.push(Line::from(vec![key("kind"), Span::styled(r.kind.label(), TStyle::new().fg(Color::Rgb(red, g, b)))]));
    if let Some(id) = &r.notam {
        l.push(Line::from(vec![key("NOTAM"), Span::raw(format!("FDC {id}"))]));
    }
    if let Some(v) = &r.vertical {
        l.push(Line::from(vec![key("altitude"), Span::raw(v.clone())]));
    }
    if r.kind.is_tfr() {
        match n {
            None => l.push(Line::from(Span::styled(" loading the NOTAM…", dim))),
            Some(n) => {
                if !n.name.is_empty() {
                    l.push(Line::from(vec![key("name"), Span::raw(n.name.clone())]));
                }
                if !n.place.is_empty() {
                    l.push(Line::from(vec![key("where"), Span::raw(n.place.clone())]));
                }
                for a in &n.areas {
                    l.push(Line::default());
                    let rule = if a.rule.is_empty() { String::new() } else { format!(" · 14 CFR {}", a.rule) };
                    l.push(Line::from(Span::styled(
                        format!(" {}{rule}", if a.name.is_empty() { "area" } else { &a.name }),
                        TStyle::new().add_modifier(Modifier::BOLD),
                    )));
                    l.push(Line::from(vec![key("altitude"), Span::raw(a.vertical.clone())]));
                    let now = unix_now();
                    let when = match (a.from, a.to) {
                        (Some(s), Some(e)) => {
                            let state = if now < s {
                                "coming"
                            } else if now <= e {
                                "in force"
                            } else {
                                "ended"
                            };
                            format!("{} to {} · {state}", day_hhmm(s), day_hhmm(e))
                        }
                        (Some(s), None) => format!("from {}", day_hhmm(s)),
                        _ => "until further notice".into(),
                    };
                    l.push(Line::from(vec![key("when"), Span::raw(when)]));
                }
                if !n.text.is_empty() {
                    l.push(Line::default());
                    l.push(Line::from(Span::styled(format!(" {}", n.text), dim)));
                }
            }
        }
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(" Not for navigation: check NOTAMs before flying. esc close", dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// What the --fires task sends.
enum FireMsg {
    /// The current incidents.
    Incidents(Vec<mapterm::fires::Fire>),
    /// The current perimeters, by incident.
    Perimeters(std::collections::HashMap<String, serde_json::Value>),
    /// Why a request failed.
    Error(String),
}

/// The --fires panel for the selected incident.
fn draw_fire(f: &mut ratatui::Frame, area: Rect, fi: &mapterm::fires::Fire, has_perimeter: bool) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(format!(" {} ", fi.kind_name()));
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let (r, g, b) = mapterm::fires::rgb(fi);
    let key = |k: &str| Span::styled(format!(" {k:<10}"), dim);
    let mut l = vec![Line::from(Span::styled(format!(" {}", fi.title()), TStyle::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)))];
    let place = [fi.county.as_str(), fi.state.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(", ");
    if !place.is_empty() {
        l.push(Line::from(Span::styled(format!(" {place}"), dim)));
    }
    l.push(Line::default());
    if let Some(a) = fi.acres {
        l.push(Line::from(vec![key("size"), Span::raw(format!("{} acres ({:.1} km²)", thousands(a.round() as usize), a * 0.004_046_86))]));
    }
    if let Some(c) = fi.contained {
        l.push(Line::from(vec![key("contained"), Span::styled(format!("{c:.0}%"), TStyle::new().fg(Color::Rgb(r, g, b)))]));
    }
    if !fi.behavior.is_empty() {
        l.push(Line::from(vec![key("behavior"), Span::raw(fi.behavior.clone())]));
    }
    if !fi.cause.is_empty() {
        l.push(Line::from(vec![key("cause"), Span::raw(fi.cause.clone())]));
    }
    if let Some(t) = fi.discovered {
        l.push(Line::from(vec![key("discovered"), Span::raw(day_hhmm(t))]));
    }
    if let Some(n) = fi.personnel.filter(|n| *n > 0) {
        l.push(Line::from(vec![key("personnel"), Span::raw(thousands(n as usize))]));
    }
    if let Some(t) = fi.modified {
        l.push(Line::from(vec![key("updated"), Span::raw(day_hhmm(t))]));
    }
    l.push(Line::from(vec![key("perimeter"), Span::styled(if has_perimeter { "mapped" } else { "not mapped" }, dim)]));
    if !fi.unique.is_empty() {
        l.push(Line::from(vec![key("id"), Span::styled(fi.unique.clone(), dim)]));
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(" From NIFC (WFIGS). For evacuations, follow local officials.", dim)));
    l.push(Line::from(Span::styled(" esc close", dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// The --quakes panel for the selected earthquake.
fn draw_quake(f: &mut ratatui::Frame, area: Rect, q: &mapterm::quakes::Quake) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let title = if q.kind.is_empty() || q.kind == "earthquake" { " earthquake ".to_string() } else { format!(" {} ", q.kind) };
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(title);
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let (r, g, b) = mapterm::quakes::AGES[mapterm::quakes::age_band(q, unix_now())].2;
    let key = |k: &str| Span::styled(format!(" {k:<9}"), dim);
    let head = format!(" {} {}", mapterm::quakes::mag_text(q), if q.mag_type.is_empty() { String::new() } else { format!("({})", q.mag_type) });
    let mut l = vec![Line::from(Span::styled(head, TStyle::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)))];
    if !q.place.is_empty() {
        l.push(Line::from(Span::raw(format!(" {}", q.place))));
    }
    l.push(Line::default());
    let ago = (unix_now() - q.time).max(0.0);
    let ago = if ago < 3600.0 {
        format!("{:.0} min ago", ago / 60.0)
    } else if ago < 86400.0 {
        format!("{:.1} h ago", ago / 3600.0)
    } else {
        format!("{:.1} days ago", ago / 86400.0)
    };
    l.push(Line::from(vec![key("when"), Span::raw(format!("{} · {ago}", day_hhmm(q.time)))]));
    l.push(Line::from(vec![key("depth"), Span::raw(format!("{:.1} km", q.depth))]));
    l.push(Line::from(vec![key("where"), Span::raw(format!("{:.3}, {:.3}", q.lat, q.lon))]));
    if let Some(n) = q.felt.filter(|n| *n > 0) {
        l.push(Line::from(vec![key("felt"), Span::raw(format!("{} reports", thousands(n as usize)))]));
    }
    if let Some(a) = &q.alert {
        let c = match a.as_str() {
            "red" => Color::Red,
            "orange" => Color::Rgb(0xff, 0x9f, 0x0a),
            "yellow" => Color::Yellow,
            _ => Color::Green,
        };
        l.push(Line::from(vec![key("PAGER"), Span::styled(format!("{a} alert"), TStyle::new().fg(c))]));
    }
    if q.tsunami {
        l.push(Line::from(vec![key("tsunami"), Span::raw("flagged by USGS (large, in an oceanic region; not a warning)")]));
    }
    if !q.status.is_empty() {
        l.push(Line::from(vec![key("status"), Span::styled(q.status.clone(), dim)]));
    }
    if !q.url.is_empty() {
        l.push(Line::default());
        l.push(Line::from(Span::styled(format!(" {}", q.url), dim)));
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(" From USGS; tsunami warnings come from the Tsunami Warning Centers.", dim)));
    l.push(Line::from(Span::styled(" esc close", dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// What the --outages task sends.
enum OutageMsg {
    /// A feed's level's outlines, by key.
    Outlines(usize, mapterm::outages::Level, std::collections::HashMap<String, serde_json::Value>),
    /// A feed's level's figures (finer areas: those with customers out).
    Counts(usize, mapterm::outages::Level, Vec<mapterm::outages::Area>),
    /// ODIN's counties, and how many utilities were left out as not reporting lately.
    Odin(Vec<mapterm::outages::Area>, usize),
    /// Outlines of ODIN's counties, by area key.
    OdinOutlines(std::collections::HashMap<String, serde_json::Value>),
    /// Why a request failed.
    Error(String),
}

/// "WEST PENN POWER COMPANY" as "West Penn Power Company" (short words like "&" kept).
fn title_words(s: &str) -> String {
    if s.chars().any(|c| c.is_lowercase()) {
        return s.to_string();
    }
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A time with its weekday when it isn't today: "03:00", "Fri 03:00".
fn day_hhmm(t: f64) -> String {
    let day = |t: f64| ((t + zone().at(t).0 as f64) / 86400.0).floor();
    if day(t) == day(unix_now()) {
        hhmm(t)
    } else {
        let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(day(t) as i64).rem_euclid(7) as usize];
        format!("{wd} {}", hhmm(t))
    }
}

/// The --outages panel for the selected county, municipality or ZIP code.
fn draw_outage(f: &mut ratatui::Frame, area: Rect, o: &mapterm::outages::Area) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" power outage ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let (r, g, b) = mapterm::outages::rgb(o);
    let key = |k: &str| Span::styled(format!(" {k:<9}"), dim);
    let mut l = vec![Line::from(Span::styled(format!(" {}", o.title()), TStyle::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)))];
    let under = match &o.county {
        Some(c) => format!(" {c} County, {}", o.state),
        None if o.kind == "county" => format!(" {}", o.state),
        None => format!(" {}, {}", o.kind, o.state),
    };
    l.push(Line::from(Span::styled(under, dim)));
    l.push(Line::default());
    l.push(Line::from(vec![key("out"), Span::styled(format!("{} customers", thousands(o.out as usize)), TStyle::new().fg(Color::Rgb(r, g, b)))]));
    if let (Some(served), Some(share)) = (o.served, o.share()) {
        l.push(Line::from(vec![key("served"), Span::raw(format!("{} (estimated)", thousands(served as usize)))]));
        let pct = share * 100.0;
        l.push(Line::from(vec![key("share"), Span::raw(if pct > 0.0 && pct < 1.0 { format!("{pct:.1}%") } else { format!("{pct:.0}%") })]));
    } else {
        l.push(Line::from(vec![key("served"), Span::styled("not given for this area", dim)]));
    }
    if let (Some(c), Some(county)) = (o.county_out, &o.county) {
        l.push(Line::from(vec![key("county"), Span::raw(format!("{} out in {county} County", thousands(c as usize)))]));
    }
    for (i, (u, n)) in o.utilities.iter().take(6).enumerate() {
        l.push(Line::from(vec![
            key(if i == 0 { "utility" } else { "" }),
            Span::raw(format!("{} ", title_words(u))),
            Span::styled(thousands(*n as usize), dim),
        ]));
    }
    if let Some(t) = o.restore {
        l.push(Line::from(vec![key("back by"), Span::raw(format!("{} (the utility's estimate)", day_hhmm(t)))]));
    }
    if !o.causes.is_empty() {
        l.push(Line::from(vec![key("cause"), Span::raw(o.causes.join(", "))]));
    }
    if let Some(t) = o.updated {
        let ago = ((unix_now() - t) / 60.0).max(0.0).round();
        l.push(Line::from(vec![key("updated"), Span::raw(format!("{} · {ago:.0} min ago", hhmm(t)))]));
    }
    if let Some(d) = &o.disclaimer {
        l.push(Line::default());
        l.push(Line::from(Span::styled(format!(" {d}"), dim)));
    }
    l.push(Line::default());
    if o.source == "ODIN" {
        l.push(Line::from(Span::styled(
            " From ODIN (Oak Ridge National Laboratory): participating utilities only. Your utility's own outage map has the details.",
            dim,
        )));
    } else {
        l.push(Line::from(Span::styled(format!(" From {}: the utilities' reports to the state, every 15 minutes.", o.source), dim)));
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(" esc close", dim)));
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
}

/// The --sondes panel for the selected balloon: its readings, and where it's predicted to land.
#[allow(clippy::too_many_arguments)]
fn draw_balloon(
    f: &mut ratatui::Frame,
    area: Rect,
    (b, at): (&mapterm::sondehub::Balloon, mapterm::timeseries::Sample),
    pred: Option<&mapterm::sondehub::Prediction>,
    heard: &[(&mapterm::sondehub::Station, f64, Option<f64>)],
    from: Option<&mapterm::upperair::Site>,
    now: f64,
    follow: bool,
    me: Option<&mapterm::locate::Fix>,
) -> Vec<StationRow> {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Borders, Wrap};
    let dim = TStyle::new().fg(Color::DarkGray);
    let block = Block::new().borders(Borders::LEFT).border_style(dim).title(" weather balloon ");
    let inner = block.inner(area);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(block, area);
    let t = &b.latest;
    let key = |k: &str| Span::styled(format!(" {k:<13}"), dim);
    let mut l = vec![Line::from(vec![
        Span::styled(format!(" {} {}", b.glyph(), t.serial), TStyle::new().fg(Color::Rgb(127, 178, 255)).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {}", [t.manufacturer.as_deref(), t.kind.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ")), dim),
    ])];
    l.push(Line::default());
    if let Some(alt) = at.alt {
        l.push(Line::from(vec![key("altitude"), Span::raw(format!("{:.0} m", alt)), Span::styled(format!("  {:.0} ft", alt / 0.3048), dim)]));
    }
    if let Some(v) = t.vel_v {
        let what = if v > 0.5 {
            "climbing"
        } else if v < -0.5 {
            "falling"
        } else {
            "level"
        };
        l.push(Line::from(vec![key("vertical"), Span::raw(format!("{:+.1} m/s", v)), Span::styled(format!("  {what}"), dim)]));
    }
    if let Some(h) = t.vel_h {
        let dir = t.heading.map(|d| format!(" toward {:03.0}°", d.rem_euclid(360.0))).unwrap_or_default();
        l.push(Line::from(vec![key("drift"), Span::raw(format!("{:.0} km/h{dir}", h * 3.6))]));
    }
    if let Some(c) = t.temp {
        l.push(Line::from(vec![key("temperature"), Span::raw(format!("{c:.1} °C"))]));
    }
    if let Some(h) = t.humidity {
        l.push(Line::from(vec![key("humidity"), Span::raw(format!("{h:.0} %"))]));
    }
    if let Some(fq) = t.frequency {
        l.push(Line::from(vec![key("frequency"), Span::raw(format!("{fq:.3} MHz"))]));
    }
    if let Some(v) = t.batt {
        l.push(Line::from(vec![key("battery"), Span::raw(format!("{v:.1} V"))]));
    }
    // The stations hearing it now (green), then those that heard it earlier: how far, how
    // strongly. Only the callsign when no station gave its position.
    if heard.is_empty()
        && let Some(u) = &t.uploader
    {
        l.push(Line::from(vec![key("heard by"), Span::raw(u.clone())]));
    }
    // Each on one line (cut to fit), so a click's row is its station: a click flies there.
    let mut rows: Vec<(usize, StationRow)> = Vec::new();
    for (i, (s, when, snr)) in heard.iter().take(6).enumerate() {
        let km = mapterm::adsb::distance_nm((at.lon, at.lat), (s.lon, s.lat)) * 1.852;
        let snr = snr.map(|x| format!(" · {x:.0} dB")).unwrap_or_default();
        let now_ = now - when <= mapterm::sondehub::HEARING;
        let st = if now_ { TStyle::new().fg(Color::Rgb(0x7e, 0xe7, 0x87)) } else { TStyle::new() };
        let ago = if now_ { String::new() } else { format!(" · {}", mapterm::sessions::ago(*when, now)) };
        let name = format!("⌂ {}", s.callsign);
        let room = (inner.width as usize).saturating_sub(15 + name.chars().count());
        let rest: String = format!("  ≈ {km:.0} km{snr}{ago}").chars().take(room).collect();
        rows.push((l.len(), StationRow { row: 0, lon: s.lon, lat: s.lat, callsign: s.callsign.clone() }));
        l.push(Line::from(vec![key(if i == 0 { "heard by" } else { "" }), Span::styled(name, st), Span::styled(rest, dim)]));
    }
    if heard.len() > 6 {
        l.push(Line::from(vec![key(""), Span::styled(format!("and {} more", heard.len() - 6), dim)]));
    }
    if let Some(s) = from {
        l.push(Line::from(vec![key("launched from"), Span::raw(title_case(&s.name)), Span::styled(format!("  {}", s.wmo), dim)]));
    }
    l.push(Line::from(vec![key("position"), Span::raw(format!("{:.4}, {:.4}", at.lat, at.lon))]));
    l.push(Line::from(vec![key("last report"), Span::raw(format!("{:.0} s ago", (now - t.t).max(0.0)))]));
    l.push(Line::default());
    match pred {
        Some(p) => {
            if let (false, Some(burst)) = (p.descending, p.burst_alt) {
                l.push(Line::from(vec![key("bursts at"), Span::raw(format!("about {burst:.0} m"))]));
            }
            if let Some((lt, lon, lat)) = p.landing() {
                l.push(Line::from(vec![key("lands"), Span::raw(hhmm(lt)), Span::styled(format!("  in {:.0} min", ((lt - now) / 60.0).max(0.0)), dim)]));
                l.push(Line::from(vec![key("landing at"), Span::raw(format!("{lat:.4}, {lon:.4}"))]));
                if let Some(me) = me {
                    let km = mapterm::adsb::distance_nm((me.lon, me.lat), (lon, lat)) * 1.852;
                    l.push(Line::from(vec![key("from you"), Span::raw(format!("{km:.1} km"))]));
                }
            }
        }
        None => l.push(Line::from(Span::styled(" (landing prediction loading…)", dim))),
    }
    l.push(Line::default());
    l.push(Line::from(Span::styled(format!(" f {} · esc close", if follow { "stop following" } else { "follow" }), dim)));
    // Where each station's line lands: lines before it wrap by their width.
    let w = inner.width.max(1) as usize;
    let mut y = inner.y as usize;
    let mut at_line = 0;
    let mut out = Vec::new();
    for (n, mut r) in rows {
        while at_line < n {
            y += l[at_line].width().div_ceil(w).max(1);
            at_line += 1;
        }
        if y < inner.bottom() as usize {
            r.row = y as u16;
            out.push(r);
        }
    }
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), inner);
    out
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
    // What kind of aircraft, by its ADS-B category: a balloon stands out.
    if let Some(c) = a.category.as_deref().and_then(mapterm::adsb::category_name) {
        let st = if a.is_balloon() { TStyle::new().fg(Color::Rgb(0xff, 0x6b, 0x6b)) } else { dim };
        d.push(Line::from(Span::styled(format!(" {c}"), st)));
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
        // So do the other live layers: closures, gauges, fires, quakes.
        let live = id == CLOSURE_TILES
            || id == ROUTE_TILES
            || id == ROUTE
            || id == format!("{ROUTE}-line")
            || [CLOSURES, GAUGES, FIRES, QUAKES, TRAFFIC].iter().any(|l| id == *l || id.strip_prefix(l).is_some_and(|r| r.starts_with('-')))
            || [STATIONS, STATION_LINKS, NASA, NASA_TRACKS, LAUNCH_SITES, TOWERS].contains(&id.as_str())
            || id.starts_with(STATION_AREAS);
        if live || id == AIRCRAFT || id == TRAILS || id == SHIPS || id == SATS || id == ALERT_TILES || id == BALLOONS || ALERT_LAYERS.contains(&id.as_str()) {
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

/// The live layers that can be started while the viewer runs (from the `L` picker), by
/// their mapterm::basemaps::LIVE key, and the option that starts them at launch.
const STARTABLE: &[(&str, &str)] = &[
    ("aircraft", "--adsb"),
    ("balloons", "--sondes"),
    ("nasa", "--nasa-balloons"),
    ("towers", "--towers"),
    ("alerts", "--alerts"),
    ("quakes", "--quakes"),
    ("fires", "--fires"),
    ("gauges", "--floods"),
    ("tfrs", "--tfrs"),
    ("closures", "--closures"),
    ("traffic", "--traffic"),
    ("radar", "--radar"),
    ("satellites", "--sats"),
    ("ships", "--ais"),
    ("outages", "--outages"),
];

/// Where a layer started later goes: under the first layer drawing from one of `sources`
/// (what's drawn over it), else under the markers.
fn under(map: &Map, sources: &[&str]) -> Option<String> {
    map.layer_ids()
        .into_iter()
        .find(|id| *id == OVERLAYS || sources.contains(id) || map.layer(id).and_then(|l| l.source.as_deref()).is_some_and(|s| sources.contains(&s)))
        .map(str::to_string)
}

/// What moves (and is drawn over everything else live).
const MOVING: &[&str] = &[SHIPS, BALLOONS, TRAILS, AIRCRAFT, SATS];
/// What's drawn over the area layers (outages, restrictions, fire areas, alerts).
const OVER_AREAS: &[&str] = &[CLOSURES, CLOSURE_TILES, ROUTE, ROUTE_TILES, TRAFFIC, GAUGES, FIRES, QUAKES, SHIPS, BALLOONS, TRAILS, AIRCRAFT, SATS];

/// An empty GeoJSON source.
fn empty_source() -> Result<MapSource> {
    MapSource::geojson(&serde_json::json!({"type": "FeatureCollection", "features": []}))
}

/// A layer of areas (`id`, credited `credit`) and, where the terminal shows images, its tiles
/// (`tiles`: their id and source, and the braille layers they stand in for, kept hidden for
/// clicking), all under `before` (None: on top).
fn add_areas(map: &mut Map, id: &str, credit: &str, layers: Vec<StyleLayer>, tiles: Option<(&str, MapSource, &[&str])>, before: Option<&str>) -> Result<()> {
    map.add_source(id, empty_source()?)?;
    map.set_attribution(id, credit)?;
    map.add_layers(layers, before)?;
    if let Some((tid, src, hide)) = tiles {
        map.add_source(tid, src)?;
        map.add_layers(vec![StyleLayer::raster(tid, tid)], before)?;
        for h in hide {
            map.set_visibility(h, false)?;
        }
    }
    Ok(())
}

/// A layer of points (`id`, credited `credit`) under `before` (None: on top).
fn add_points(map: &mut Map, id: &str, credit: &str, layer: StyleLayer, before: Option<&str>) -> Result<()> {
    map.add_source(id, empty_source()?)?;
    map.set_attribution(id, credit)?;
    map.add_layers(vec![layer], before)
}

/// The road closures' layers: the lines under `lines_before` and, where the terminal shows
/// images, their tiles under `tiles_before` (the braille lines stay, hidden, for clicking).
fn add_closures(map: &mut Map, tiles: &Arc<mapterm::areas::AreaTiles>, tiles_before: Option<&str>, lines_before: Option<&str>) -> Result<()> {
    map.add_source(CLOSURES, empty_source()?)?;
    map.set_attribution(CLOSURES, mapterm::closures::CREDIT)?;
    if mapterm::kitty::supported() {
        map.add_source(CLOSURE_TILES, MapSource::raster(mapterm::areas::tile_source_to(tiles.clone(), "closures", mapterm::closures::CREDIT, 18)?))?;
        map.add_layers(vec![StyleLayer::raster(CLOSURE_TILES, CLOSURE_TILES)], tiles_before)?;
    }
    map.add_layers(mapterm::closures::layers(CLOSURES)?, lines_before)?;
    if mapterm::kitty::supported() {
        map.set_visibility(&format!("{CLOSURES}-line"), false)?;
    }
    Ok(())
}

/// The weather balloons over the stations receiving them and the lines between, all under
/// `before` (None: on top).
fn add_balloons(map: &mut Map, link_tiles: &Arc<mapterm::areas::AreaTiles>, before: Option<&str>) -> Result<()> {
    map.add_source(STATION_AREAS, empty_source()?)?;
    map.add_layers(mapterm::sondehub::area_layers(STATION_AREAS, STATION_AREAS), before)?;
    add_points(map, LAUNCH_SITES, mapterm::upperair::CREDIT, mapterm::upperair::layer(LAUNCH_SITES, LAUNCH_SITES), before)?;
    // Where the terminal shows images, the lines to the stations are smooth image lines; the
    // braille ones stay, hidden, for clicking.
    if mapterm::kitty::supported() {
        map.add_source(STATION_LINK_TILES, MapSource::raster(mapterm::areas::tile_source_to(link_tiles.clone(), "balloon-links", SONDE_CREDIT, 18)?))?;
        map.add_layers(vec![StyleLayer::raster(STATION_LINK_TILES, STATION_LINK_TILES)], before)?;
    }
    add_points(map, STATION_LINKS, SONDE_CREDIT, mapterm::sondehub::link_layer(STATION_LINKS, STATION_LINKS), before)?;
    if mapterm::kitty::supported() {
        map.set_visibility(STATION_LINKS, false)?;
    }
    add_points(map, STATIONS, SONDE_CREDIT, mapterm::sondehub::station_layer(STATIONS, STATIONS), before)?;
    add_points(map, BALLOONS, SONDE_CREDIT, mapterm::sondehub::layer(BALLOONS, BALLOONS), before)
}

/// NASA's balloons over their tracks, under `before` (None: on top).
fn add_nasa(map: &mut Map, before: Option<&str>) -> Result<()> {
    add_points(map, NASA_TRACKS, mapterm::csbf::CREDIT, mapterm::csbf::track_layer(NASA_TRACKS, NASA_TRACKS), before)?;
    add_points(map, NASA, mapterm::csbf::CREDIT, mapterm::csbf::layer(NASA, NASA), before)
}

/// What the NASA balloon task sends: the flights read, a flight's track, or what failed.
enum NasaMsg {
    Flights(Vec<mapterm::csbf::Flight>),
    Track(String, Vec<mapterm::csbf::TrackPoint>),
    Error(String),
}

/// --nasa-balloons: the flights the campaigns link to (looked for hourly), the pages of the
/// recent ones every 2 minutes (all of them after each look), and the recent flights' tracks
/// every 10 minutes (`all`: every flight's track, once).
fn spawn_nasa(tx: mpsc::UnboundedSender<NasaMsg>, all: bool) {
    use mapterm::csbf;
    tokio::spawn(async move {
        let mut urls: Vec<String> = Vec::new();
        let mut looked: Option<Instant> = None;
        let mut flights: std::collections::HashMap<String, csbf::Flight> = std::collections::HashMap::new();
        let mut tracked: std::collections::HashMap<String, Instant> = std::collections::HashMap::new();
        loop {
            let mut read_all = false;
            if looked.is_none_or(|t| t.elapsed() >= Duration::from_secs(csbf::DISCOVER)) {
                match csbf::discover().await {
                    Ok(u) => (urls, looked, read_all) = (u, Some(Instant::now()), true),
                    Err(e) => {
                        if tx.send(NasaMsg::Error(format!("{e:#}"))).is_err() {
                            return;
                        }
                    }
                }
            }
            let now = unix_now();
            for u in &urls {
                let recent = flights.get(u).is_none_or(|f| now - f.t <= csbf::RECENT);
                if (read_all || recent)
                    && let Ok(Some(f)) = csbf::flight(u).await
                {
                    flights.insert(u.clone(), f);
                }
            }
            if tx.send(NasaMsg::Flights(flights.values().cloned().collect())).is_err() {
                return;
            }
            for f in flights.values() {
                let recent = now - f.t <= csbf::RECENT;
                let due = match tracked.get(&f.id) {
                    None => recent || all,
                    Some(t) => recent && t.elapsed() >= Duration::from_secs(600),
                };
                if let (true, Some(url)) = (due, &f.track_url) {
                    tracked.insert(f.id.clone(), Instant::now());
                    if let Ok(pts) = csbf::track(url).await
                        && tx.send(NasaMsg::Track(f.id.clone(), pts)).is_err()
                    {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(csbf::REFRESH)).await;
        }
    });
}

/// The aircraft and their trails, from `feed`, under `before` (None: on top).
fn add_aircraft(map: &mut Map, feed: &mapterm::adsb::Feed, before: Option<&str>) -> Result<()> {
    map.add_source(AIRCRAFT, empty_source()?)?;
    map.add_source(TRAILS, empty_source()?)?;
    map.set_attribution(AIRCRAFT, &feed.attribution)?;
    map.add_layers(vec![mapterm::adsb::trail_layer(TRAILS, TRAILS), mapterm::adsb::layer(AIRCRAFT, AIRCRAFT)], before)
}

/// --alerts: the active alerts every 5 minutes (with their ETag), and the zone outlines they
/// need, one a second (cached ones at once).
fn spawn_alerts(
    natx: mpsc::UnboundedSender<Result<Vec<mapterm::nws::Alert>>>,
    mut nzreq_rx: mpsc::UnboundedReceiver<Vec<String>>,
    nztx: mpsc::UnboundedSender<(String, serde_json::Value)>,
) {
    tokio::spawn(async move {
        let mut etag: Option<String> = None;
        loop {
            let r = mapterm::nws::active(etag.as_deref()).await;
            let wait = if r.is_ok() { 300 } else { 600 };
            let send = match r {
                Ok(Some((list, tag))) => {
                    etag = tag;
                    Some(Ok(list))
                }
                Ok(None) => None,
                Err(e) => Some(Err(e)),
            };
            if let Some(m) = send
                && natx.send(m).is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    });
    tokio::spawn(async move {
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        loop {
            // Take the latest wanted list; cached outlines go straight back.
            while let Ok(list) = nzreq_rx.try_recv() {
                queue = list.into();
            }
            while let Some(z) = queue.front().cloned() {
                match mapterm::nws::cached_zone(&z) {
                    Some(g) => {
                        queue.pop_front();
                        if nztx.send((z, g)).is_err() {
                            return;
                        }
                    }
                    None => break,
                }
            }
            match queue.pop_front() {
                Some(z) => {
                    let g = mapterm::nws::zone_outline(&z).await.unwrap_or(serde_json::Value::Null);
                    if nztx.send((z, g)).is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                None => match nzreq_rx.recv().await {
                    Some(list) => queue = list.into(),
                    None => return,
                },
            }
        }
    });
}

/// --closures: every feed every 5 minutes (Virginia's with your SmarterRoads token).
fn spawn_closures(cltx: mpsc::UnboundedSender<(usize, std::result::Result<Vec<mapterm::closures::Closure>, String>)>, vdot_token: Option<String>) {
    tokio::spawn(async move {
        loop {
            for (i, feed) in mapterm::closures::FEEDS.iter().enumerate() {
                if feed.vdot && vdot_token.is_none() {
                    continue;
                }
                let r = mapterm::closures::fetch(feed, vdot_token.as_deref()).await.map_err(|e| format!("{e:#}"));
                if cltx.send((i, r)).is_err() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });
}

/// --tfrs: the standing airspace once, the TFRs every 5 minutes.
fn spawn_tfrs(ttx: mpsc::UnboundedSender<TfrMsg>) {
    let tx = ttx.clone();
    tokio::spawn(async move {
        let m = match mapterm::tfrs::standing().await {
            Ok(s) => TfrMsg::Standing(s),
            Err(e) => TfrMsg::Error(format!("{e:#}")),
        };
        let _ = tx.send(m);
    });
    let tx = ttx.clone();
    tokio::spawn(async move {
        loop {
            let (m, wait) = match mapterm::tfrs::tfrs().await {
                Ok(t) => (TfrMsg::Live(t), 300),
                Err(e) => (TfrMsg::Error(format!("{e:#}")), 600),
            };
            if tx.send(m).is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    });
}

/// --fires: incidents and perimeters every 5 minutes (each with its ETag).
fn spawn_fires(ftx: mpsc::UnboundedSender<FireMsg>) {
    tokio::spawn(async move {
        let (mut inc_tag, mut per_tag) = (None::<String>, None::<String>);
        loop {
            let mut failed = false;
            let m = match mapterm::fires::incidents(inc_tag.as_deref()).await {
                Ok(Some((f, t))) => {
                    inc_tag = t;
                    Some(FireMsg::Incidents(f))
                }
                Ok(None) => None,
                Err(e) => {
                    failed = true;
                    Some(FireMsg::Error(format!("{e:#}")))
                }
            };
            if let Some(m) = m
                && ftx.send(m).is_err()
            {
                return;
            }
            let m = match mapterm::fires::perimeters(per_tag.as_deref()).await {
                Ok(Some((p, t))) => {
                    per_tag = t;
                    Some(FireMsg::Perimeters(p))
                }
                Ok(None) => None,
                Err(e) => {
                    failed = true;
                    Some(FireMsg::Error(format!("{e:#}")))
                }
            };
            if let Some(m) = m
                && ftx.send(m).is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_secs(if failed { 600 } else { 300 })).await;
        }
    });
}

/// --quakes: the USGS feed for `period` once a minute (with If-Modified-Since).
fn spawn_quakes(qtx: mpsc::UnboundedSender<Result<Vec<mapterm::quakes::Quake>>>, period: mapterm::quakes::Period) {
    tokio::spawn(async move {
        let mut since: Option<String> = None;
        loop {
            let r = mapterm::quakes::fetch(period, since.as_deref()).await;
            let wait = if r.is_ok() { 60 } else { 300 };
            let send = match r {
                Ok(Some((q, modified))) => {
                    since = modified;
                    Some(Ok(q))
                }
                Ok(None) => None,
                Err(e) => Some(Err(e)),
            };
            if let Some(m) = send
                && qtx.send(m).is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    });
}

/// --traffic: CHART's speed sensors each minute.
fn spawn_traffic(traffictx: mpsc::UnboundedSender<Result<Vec<mapterm::traffic::Sensor>>>) {
    tokio::spawn(async move {
        loop {
            let r = mapterm::traffic::sensors().await;
            let wait = if r.is_ok() { mapterm::traffic::REFRESH } else { 300 };
            if traffictx.send(r).is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    });
}

/// Satellites' radios by NORAD number, from the SatNOGS DB.
type Radio = std::collections::HashMap<u64, Vec<mapterm::satnogs::Transmitter>>;
/// The radios loaded: them, their age in seconds and why some are missing; or what failed.
type RadioMsg = std::result::Result<(Radio, f64, Option<String>), String>;

/// --sats: the `groups`' satellites and their owners, then what they're for, then their radios;
/// again every 2 hours.
fn spawn_sats(
    groups: Vec<String>,
    otx: mpsc::UnboundedSender<(Vec<mapterm::satellites::Sat>, f64, Vec<String>)>,
    mtx: mpsc::UnboundedSender<(std::collections::HashMap<u64, mapterm::satellites::Mission>, Vec<String>)>,
    rtx: mpsc::UnboundedSender<RadioMsg>,
) {
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

/// --radar: RainViewer's scan list every 5 minutes (10 after a failure).
fn spawn_radar(rvtx: mpsc::UnboundedSender<Result<mapterm::rainviewer::Scans>>) {
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

/// --outages: each state's outlines once and counts every 5 minutes, and ODIN's counties
/// outside those states.
fn spawn_outages(outx: mpsc::UnboundedSender<OutageMsg>) {
    let odin_tx = outx.clone();
    tokio::spawn(async move {
        use mapterm::outages::{FEEDS, Level, counts, outlines};
        let mut tags: std::collections::HashMap<(usize, Level), Option<String>> = std::collections::HashMap::new();
        let mut have: std::collections::HashSet<(usize, Level)> = std::collections::HashSet::new();
        loop {
            let mut failed = false;
            // Every state's counties first, as they're shown first, then the finer areas.
            for level in [Level::County, Level::Local] {
                for (i, feed) in FEEDS.iter().enumerate().filter(|(_, f)| f.layer(level).is_some()) {
                    if !have.contains(&(i, level)) {
                        let m = match outlines(feed, level).await {
                            Ok(o) => {
                                have.insert((i, level));
                                OutageMsg::Outlines(i, level, o)
                            }
                            Err(e) => {
                                failed = true;
                                OutageMsg::Error(format!("{e:#}"))
                            }
                        };
                        if outx.send(m).is_err() {
                            return;
                        }
                    }
                    let tag = tags.get(&(i, level)).cloned().flatten();
                    let m = match counts(feed, level, tag.as_deref()).await {
                        Ok(Some((areas, t))) => {
                            tags.insert((i, level), t);
                            Some(OutageMsg::Counts(i, level, areas))
                        }
                        Ok(None) => None,
                        Err(e) => {
                            failed = true;
                            Some(OutageMsg::Error(format!("{e:#}")))
                        }
                    };
                    if let Some(m) = m
                        && outx.send(m).is_err()
                    {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(if failed { 900 } else { 300 })).await;
        }
    });
    // ODIN, for the counties outside the states above: the reporting utilities and the
    // outages every 5 minutes (the outages with their ETag), and outlines for counties
    // not seen before.
    let outx = odin_tx;
    tokio::spawn(async move {
        let skip: Vec<&str> = mapterm::outages::FEEDS.iter().map(|f| f.fips).collect();
        let mut outlines = mapterm::odin::cached_outlines();
        let geoms = |o: &std::collections::HashMap<String, mapterm::odin::Outline>| {
            o.iter().map(|(k, v)| (format!("odin:{k}"), v.geometry.clone())).collect::<std::collections::HashMap<_, _>>()
        };
        if !outlines.is_empty() && outx.send(OutageMsg::OdinOutlines(geoms(&outlines))).is_err() {
            return;
        }
        let (mut etag, mut reports, mut status) = (None::<String>, Vec::new(), std::collections::HashMap::new());
        loop {
            let mut errors = Vec::new();
            match mapterm::odin::status().await {
                Ok(s) => status = s,
                Err(e) => errors.push(format!("{e:#}")),
            }
            match mapterm::odin::reports(etag.as_deref()).await {
                Ok(Some((r, t))) => (reports, etag) = (r, t),
                Ok(None) => {}
                Err(e) => errors.push(format!("{e:#}")),
            }
            let (areas, _) = mapterm::odin::counties(&reports, &status, &outlines, unix_now(), &skip);
            let missing: Vec<String> =
                areas.iter().filter_map(|a| a.key.strip_prefix("odin:")).filter(|f| !outlines.contains_key(*f)).map(str::to_string).collect();
            if !missing.is_empty() {
                match mapterm::odin::fetch_outlines(&missing, &outlines).await {
                    Ok(got) => {
                        if outx.send(OutageMsg::OdinOutlines(geoms(&got))).is_err() {
                            return;
                        }
                        outlines.extend(got);
                    }
                    Err(e) => errors.push(format!("{e:#}")),
                }
            }
            // Again, with the names of the counties whose outlines just came.
            let (areas, stale) = mapterm::odin::counties(&reports, &status, &outlines, unix_now(), &skip);
            if outx.send(OutageMsg::Odin(areas, stale)).is_err() {
                return;
            }
            for e in errors {
                if outx.send(OutageMsg::Error(e)).is_err() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut a = args()?;
    // Live or hazard data on the map: the disclaimer shows with the credits.
    let live_data = a.adsb.is_some()
        || a.ais
        || a.sats.is_some()
        || a.sondes
        || a.nasa.is_some()
        || a.radar
        || a.alerts
        || a.outages
        || a.quakes.is_some()
        || a.fires
        || a.floods
        || a.tfrs
        || a.traffic
        || a.closures;
    // Road closures over rendered roads: Esri's Dark Gray Canvas underneath (quiet greys,
    // made for overlays, no labels of its own), with mapterm's road and place names on top;
    // unless another map was asked for.
    let closures_raster = (a.closures || a.traffic) && a.raster.is_none() && a.source.is_none() && a.style.is_none() && !a.light;
    if closures_raster {
        a.raster = Some("esri-dark".into());
    }
    let _ = ZONE.set(if a.utc { mapterm::clock::Zone::Utc } else { mapterm::clock::Zone::Local });
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
        if mapterm::msbuildings::is_spec(g) || mapterm::overture::is_spec(g) || (remote && (path.ends_with("collection.json") || (parquet && a.bbox.is_none())))
        {
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
        // Watching weather over the default basemap: the weather style; aircraft, ships or
        // satellites alone, the aviation style; unless --light.
        if (a.radar || a.alerts || a.outages || a.fires || a.floods) && a.source.is_none() && !a.light {
            map = Map::weather(src, schema)?;
        } else if a.closures && a.source.is_none() && !a.light {
            map = Map::roads(src, schema)?;
        } else if (a.adsb.is_some() || a.ais || a.sats.is_some() || a.sondes || a.quakes.is_some() || a.tfrs) && a.source.is_none() && !a.light {
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
    let default_dark = a.style.is_none() && a.source.is_none() && !a.light;
    if (a.radar || a.alerts || a.outages || a.fires || a.floods) && default_dark {
        styles.push(WEATHER.to_string());
    } else if a.closures && default_dark {
        styles.push(ROADS.to_string());
    } else if (a.adsb.is_some() || a.ais || a.sats.is_some() || a.sondes || a.quakes.is_some() || a.tfrs) && default_dark {
        styles.push(AVIATION.to_string());
    }
    for s in [BUILTIN, AVIATION, WEATHER, ROADS].into_iter().chain(stylejson::OPENFREEMAP_STYLES) {
        if !styles.iter().any(|x| x == s) {
            styles.push(s.to_string());
        }
    }
    let mut style_at = 0usize;
    // The base maps `b`, `m`, `M` and `'` switch between: the styles, then imagery (the
    // providers that need no key, or whose key is set; and what --raster named).
    let bases = mapterm::basemaps::entries(&styles, a.raster.as_deref(), |p| {
        mapterm::raster::find_key(None, p.key_env).ok().flatten().is_some() || (p.name == "mapbox" && mapterm::mapbox::access_token().is_some())
    });
    let mut base_at = a.raster.as_deref().and_then(|r| bases.iter().position(|e| e.base == mapterm::basemaps::Base::Imagery(r.to_string()))).unwrap_or(0);

    // Optional raster layer under the vector map; its layers are kept for the base map
    // switcher to put back.
    let mut start_imagery: ImageryLayers = Vec::new();
    let mut raster = match &a.raster {
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
                start_imagery.push((id, r.clone(), sl.min_zoom as f64));
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
            start_imagery.push((RASTER.to_string(), r.clone(), 0.0));
            map.add_source(RASTER, MapSource::raster(r.clone()))?;
            let bottom = map.layer_ids().first().map(|s| s.to_string());
            map.add_layers(vec![StyleLayer::raster(RASTER, RASTER)], bottom.as_deref())?;
            Some(r)
        }
        None => None,
    };
    // GeoJSON layers over the basemap, each in its own color.
    let mut legend: Vec<(String, Color)> = Vec::new();
    // Each data layer's source, beside `legend`: what was given, features, how it loads.
    let mut data_info: Vec<DataInfo> = Vec::new();
    let mut datasets: Vec<Loading> = Vec::new();
    let mut opened = opened.into_iter().peekable();
    for (i, g) in geo.into_iter().enumerate() {
        let color = g.color;
        let mut id = g.name.clone();
        if map.source(&id).is_some() {
            id = format!("{id}-{}", i + 1);
        }
        let spec = a.geojson.get(i).cloned().unwrap_or_default();
        let streamed = opened.peek().is_some_and(|(at, ..)| *at == i);
        data_info.push(DataInfo::new(&spec, (!streamed).then_some(g.features.len()), streamed));
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
    // Power outages (--outages): under the alerts.
    let outage_tiles = std::sync::Arc::new(mapterm::areas::AreaTiles::default());
    let add_outages = |map: &mut Map, before: Option<&str>| -> Result<()> {
        let credit = mapterm::outages::credit();
        let tiles = match mapterm::kitty::supported() {
            true => Some((OUTAGE_TILES, MapSource::raster(mapterm::areas::tile_source(outage_tiles.clone(), "outages", &credit)?), &OUTAGE_LAYERS[..])),
            false => None,
        };
        add_areas(map, OUTAGES, &credit, mapterm::outages::layers(OUTAGES)?, tiles, before)
    };
    if a.outages {
        add_outages(&mut map, None)?;
    }
    // Flight restrictions (--tfrs): over the outages and fires, under the alerts.
    let tfr_tiles = std::sync::Arc::new(mapterm::areas::AreaTiles::default());
    let add_tfrs = |map: &mut Map, before: Option<&str>| -> Result<()> {
        let tiles = match mapterm::kitty::supported() {
            true => Some((TFR_TILES, MapSource::raster(mapterm::areas::tile_source(tfr_tiles.clone(), "tfrs", mapterm::tfrs::CREDIT)?), &TFR_LAYERS[..])),
            false => None,
        };
        add_areas(map, TFRS, mapterm::tfrs::CREDIT, mapterm::tfrs::layers(TFRS)?, tiles, before)
    };
    if a.tfrs {
        add_tfrs(&mut map, None)?;
    }
    // Wildfire perimeters (--fires): over the outages, under the alerts.
    let fire_tiles = std::sync::Arc::new(mapterm::areas::AreaTiles::default());
    let add_fire_areas = |map: &mut Map, before: Option<&str>| -> Result<()> {
        let tiles = match mapterm::kitty::supported() {
            true => Some((FIRE_TILES, MapSource::raster(mapterm::areas::tile_source(fire_tiles.clone(), "fires", mapterm::fires::CREDIT)?), &FIRE_LAYERS[..])),
            false => None,
        };
        add_areas(map, FIRE_AREAS, mapterm::fires::CREDIT, mapterm::fires::perimeter_layers(FIRE_AREAS)?, tiles, before)
    };
    if a.fires {
        add_fire_areas(&mut map, None)?;
    }
    // Weather alerts (--alerts): under everything that moves.
    let alert_tiles = std::sync::Arc::new(mapterm::nws::AlertTiles::default());
    // Where the terminal shows images, the areas are tiles drawn here, moved like imagery;
    // the braille layers stay for clicking (the lookup covers hidden layers).
    let add_alerts = |map: &mut Map, before: Option<&str>| -> Result<()> {
        let tiles = match mapterm::kitty::supported() {
            true => Some((ALERT_TILES, MapSource::raster(mapterm::nws::tile_source(alert_tiles.clone())?), &ALERT_LAYERS[..])),
            false => None,
        };
        add_areas(map, ALERTS, mapterm::nws::CREDIT, mapterm::nws::layers(ALERTS)?, tiles, before)
    };
    if a.alerts {
        add_alerts(&mut map, None)?;
    }
    // Live aircraft (--adsb): a GeoJSON source the loop refills, over your layers.
    let mut adsb_feed = a.adsb.as_deref().map(mapterm::adsb::Feed::named).transpose()?;
    // adsb.lol's key, only ever sent to adsb.lol.
    if let Some(feed) = adsb_feed.as_mut().filter(|f| f.name == "adsb.lol") {
        feed.key = mapterm::raster::find_key(a.adsb_key_cmd.as_deref(), &["ADSB_LOL_KEY"])?;
    }
    // How often to ask (slowed further by the feed's quota), and how far in the past to draw:
    // a poll interval and a second, so frames fall between two real fixes.
    let mut adsb_every = adsb_feed.as_ref().map_or(Duration::from_secs(10), mapterm::adsb::Feed::every);
    let mut adsb_delay = adsb_every.as_secs_f64() + 1.0;
    if let Some(feed) = &adsb_feed {
        add_aircraft(&mut map, feed, None)?;
    }
    // Live ships (--ais), under the aircraft.
    let mut ais_key = if a.ais {
        let key = mapterm::raster::find_key(a.ais_key_cmd.as_deref(), &["AISSTREAM_API_KEY"])?;
        Some(key.context("--ais needs a free aisstream.io API key: sign up at https://aisstream.io, then set AISSTREAM_API_KEY or --ais-key-cmd")?)
    } else {
        None
    };
    if ais_key.is_some() {
        let above = map.layer_ids().into_iter().find(|id| *id == TRAILS).map(str::to_string);
        add_points(&mut map, SHIPS, "aisstream.io", mapterm::ais::layer(SHIPS, SHIPS), above.as_deref())?;
    }
    // Live weather balloons (--sondes), between the ships and the aircraft.
    if a.nasa.is_some() {
        let above = map.layer_ids().into_iter().find(|id| *id == TRAILS || *id == AIRCRAFT).map(str::to_string);
        add_nasa(&mut map, above.as_deref())?;
    }
    if a.towers {
        let above = under(&map, MOVING);
        add_points(&mut map, TOWERS, mapterm::towers::CREDIT, mapterm::towers::layer(TOWERS, TOWERS), above.as_deref())?;
    }
    // The lines from a selected balloon to the stations hearing it, as image tiles.
    let link_tiles = std::sync::Arc::new(mapterm::areas::AreaTiles::default());
    if a.sondes {
        let above = map.layer_ids().into_iter().find(|id| *id == TRAILS || *id == AIRCRAFT).map(str::to_string);
        add_balloons(&mut map, &link_tiles, above.as_deref())?;
    }
    // Earthquakes (--quakes): under everything that moves.
    if a.quakes.is_some() {
        let above = map.layer_ids().into_iter().find(|id| [TRAILS, AIRCRAFT, SHIPS, BALLOONS].contains(id)).map(str::to_string);
        add_points(&mut map, QUAKES, mapterm::quakes::CREDIT, mapterm::quakes::layer(QUAKES, QUAKES), above.as_deref())?;
    }
    // Traffic speeds (--traffic): over the closures and the route, under what moves.
    let add_traffic = |map: &mut Map| -> Result<()> {
        let empty = serde_json::json!({"type": "FeatureCollection", "features": []});
        map.add_source(TRAFFIC, MapSource::geojson(&empty)?)?;
        map.set_attribution(TRAFFIC, mapterm::traffic::CREDIT)?;
        let above = map.layer_ids().into_iter().find(|id| [TRAILS, AIRCRAFT, SHIPS, BALLOONS].contains(id)).map(str::to_string);
        map.add_layers(vec![mapterm::traffic::layer(TRAFFIC, TRAFFIC)], above.as_deref())?;
        Ok(())
    };
    // Road closures (--closures): under the live layers.
    let closure_tiles = std::sync::Arc::new(mapterm::areas::AreaTiles::default());
    // Where the terminal shows images, the lines are tiles drawn here, smooth and as wide as a
    // road; the braille lines stay for clicking (the lookup covers hidden layers).
    if a.closures {
        let above = map.layer_ids().into_iter().find(|id| [TRAILS, AIRCRAFT, SHIPS, BALLOONS].contains(id)).map(str::to_string);
        add_closures(&mut map, &closure_tiles, None, above.as_deref())?;
    }
    // A route (--route, or d after a search): over the closures, under the live layers.
    let route_tiles = std::sync::Arc::new(mapterm::areas::AreaTiles::default());
    {
        let empty = serde_json::json!({"type": "FeatureCollection", "features": []});
        map.add_source(ROUTE, MapSource::geojson(&empty)?)?;
        if mapterm::kitty::supported() {
            map.add_source(ROUTE_TILES, MapSource::raster(mapterm::areas::tile_source_to(route_tiles.clone(), "route", mapterm::route::CREDIT, 18)?))?;
            map.add_layer(mapterm::StyleLayer::raster(ROUTE_TILES, ROUTE_TILES))?;
        }
        let line =
            serde_json::json!({"id": format!("{ROUTE}-line"), "type": "line", "source": ROUTE, "paint": {"line-color": ["get", "color"], "line-width": 3}});
        let above = map.layer_ids().into_iter().find(|id| [TRAILS, AIRCRAFT, SHIPS, BALLOONS].contains(id)).map(str::to_string);
        map.add_layers(vec![mapterm::StyleLayer::from_json(&line)?.0], above.as_deref())?;
        if mapterm::kitty::supported() {
            map.set_visibility(&format!("{ROUTE}-line"), false)?;
        }
    }
    if a.traffic {
        add_traffic(&mut map)?;
    }
    // River gauges (--floods), with the quakes.
    if a.floods {
        let above = map.layer_ids().into_iter().find(|id| [TRAILS, AIRCRAFT, SHIPS, BALLOONS].contains(id)).map(str::to_string);
        add_points(&mut map, GAUGES, mapterm::floods::CREDIT, mapterm::floods::layer(GAUGES, GAUGES), above.as_deref())?;
    }
    // Wildfire incidents (--fires), with the quakes.
    if a.fires {
        let above = map.layer_ids().into_iter().find(|id| [TRAILS, AIRCRAFT, SHIPS, BALLOONS].contains(id)).map(str::to_string);
        add_points(&mut map, FIRES, mapterm::fires::CREDIT, mapterm::fires::layer(FIRES, FIRES), above.as_deref())?;
    }
    // Satellites (--sats), over everything else that moves.
    if a.sats.is_some() {
        add_points(&mut map, SATS, "CelesTrak", mapterm::satellites::layer(SATS, SATS), None)?;
    }
    // Highlights, the crosshair and the clicked spot, on top of everything.
    map.add_custom_layer(OVERLAYS, Markers::default(), None)?;
    let mut base = base_visibility(&map);
    let mut labels = !a.no_labels;
    // Vector overlay: 0 = everything (no fills over a raster), 1 = labels only, 2 = off.
    let mut vector: u8 = if closures_raster { 1 } else { 0 };
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
    // Imagery picked later (b, m) turns it on then.
    let mut kitty = (has_raster || a.radar) && mapterm::kitty::supported();
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
    let mut trails_kitty = (adsb_feed.is_some() || ais_key.is_some() || a.sats.is_some() || a.sondes) && mapterm::kitty::supported();
    if trails_kitty {
        let _ = map.set_visibility(TRAILS, false);
    }
    // Asked wherever images can show, for imagery the base map switcher may add later.
    let shm = (mapterm::kitty::supported() || pixels_on || trails_kitty) && mapterm::kitty::shm_supported();
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
    // The balloon panel's station lines (a click flies to the station).
    let mut station_rows: Vec<StationRow> = Vec::new();
    let mut panel_area = Rect::default();
    let mut last_panel = Rect::default();
    let mut last_switcher = Rect::default();
    // The timeline (with --radar or --alerts): where it was drawn, the span it shows, and
    // whether the user moved that span (then it no longer follows the data).
    let mut timeline_area = Rect::default();
    let mut timeline = mapterm::timeline::Timeline::new(unix_now() - 7200.0, unix_now() + 900.0);
    let mut timeline_moved = false;
    // Playing speeds, map seconds per second (Cesium's shuttle ring): 1200 plays two hours in
    // six seconds.
    const SPEEDS: [f64; 7] = [60.0, 150.0, 300.0, 600.0, 1200.0, 2400.0, 4800.0];
    let mut speed_i = 4usize;
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
    // Directions: the route, whether its panel shows, its ends (to ask again around closures
    // once they're in), and the answers.
    let router =
        a.router.clone().or_else(|| std::env::var("MAPTERM_ROUTER").ok().filter(|r| !r.is_empty())).unwrap_or_else(|| mapterm::route::PUBLIC.to_string());
    let mut route: Option<mapterm::route::Route> = None;
    // All the routes Valhalla gave (the best, then alternates), and which one is picked.
    let mut routes: Vec<mapterm::route::Route> = Vec::new();
    let mut route_pick = 0usize;
    let mut route_new = false;
    let mut route_panel = true;
    let mut route_ends: Option<((f64, f64), (f64, f64))> = None;
    let mut route_asking = false;
    // Asked again once, with the closed roads to avoid, when they arrive after it.
    let mut route_avoid_pending = false;
    let (routetx, mut routerx) = mpsc::unbounded_channel::<std::result::Result<(Vec<mapterm::route::Route>, (f64, f64), (f64, f64)), String>>();
    if let Some((from, to)) = a.route.clone() {
        let (tx, geo, cmd, mode, server) = (routetx.clone(), geocoder.clone(), a.location_cmd.clone(), a.route_mode, router.clone());
        route_asking = true;
        route_avoid_pending = a.closures;
        tokio::spawn(async move {
            let r = async {
                let f = route_end(&from, &geo, cmd.clone()).await?;
                let t = route_end(&to, &geo, cmd).await?;
                let r = mapterm::route::route(&server, f, t, mode, &[]).await.map_err(|e| format!("{e:#}"))?;
                Ok((r, f, t))
            }
            .await;
            let _ = tx.send(r);
        });
    }
    // --traffic: CHART's speed sensors each minute, and each route's time with them.
    let mut sensors: Vec<mapterm::traffic::Sensor> = Vec::new();
    let mut sensors_new = false;
    let mut route_traffic: Vec<Option<mapterm::traffic::Estimate>> = Vec::new();
    let (traffictx, mut trafficrx) = mpsc::unbounded_channel::<Result<Vec<mapterm::traffic::Sensor>>>();
    if a.traffic {
        spawn_traffic(traffictx.clone());
    }
    // Search results from the map's own data, waiting for the geocoder's.
    let mut search_local: Vec<mapterm::geocode::Place> = Vec::new();
    let (gtx, mut grx) = mpsc::unbounded_channel::<(String, Result<Vec<mapterm::geocode::Place>>)>();
    // Style switching (m) loads on a task; the pointer (dots in the map) is for c.
    let (stx, mut srx) = mpsc::unbounded_channel::<StyleDone>();
    // Base map switching: the one asked for (applied once nothing is loading), the one before
    // (for `'`), imagery loading, the switcher (b) and when its choice last moved (it previews
    // after a pause), and the imagery loaded so far, to put back without asking again.
    let mut want_base: Option<usize> = None;
    let mut base_prev: Option<usize> = None;
    let (itx, mut irx) = mpsc::unbounded_channel::<ImageryDone>();
    let mut imagery_loading: Option<String> = None;
    let mut switcher: Option<mapterm::basemaps::Switcher> = None;
    // A row picked (enter, space or a click), handled with everything else in the loop.
    let mut switcher_picks: Vec<mapterm::basemaps::Row> = Vec::new();
    // Sessions: one to open in place (its layers are running) once nothing is loading, the
    // command line to start again with (one that needs other layers), and S's name prompt.
    let mut restart_with: Option<mapterm::sessions::Session> = None;
    let mut naming: Option<String> = None;
    // B: saved places. The spot B was pressed on (the pointer, else the center), the name
    // prompt for saving it, the places listed, and a place to forget once ctrl-d is pressed
    // again.
    let mut place_spot = (0.0f64, 0.0f64, 0.0f64);
    let mut place_naming: Option<String> = None;
    let mut place_list: Vec<mapterm::places::Place> = Vec::new();
    let mut place_forget: Option<String> = None;
    // How each data source is doing (by live layer key): updated when, errors, how often.
    let mut status: std::collections::HashMap<&'static str, mapterm::sources::Status> = std::collections::HashMap::new();
    // Data layers (--geojson and the like) turned off in L: their style layers, to put back.
    let mut data_off: std::collections::HashMap<String, (Vec<StyleLayer>, Option<String>)> = std::collections::HashMap::new();
    // When this run started (a run in the history is created then).
    let run_started = unix_now();
    let mut session_list: Vec<(mapterm::sessions::Session, bool)> = Vec::new();
    // Imagery overlays (by their place in mapterm::basemaps::OVERLAYS): on, loading.
    let mut overlay_on = vec![false; mapterm::basemaps::OVERLAYS.len()];
    let mut overlay_loading: HashSet<usize> = HashSet::new();
    // Live layers turned off in the switcher: their style layers, kept to put back. Their data
    // keeps coming; their images (trails, satellite dots) are freed and not drawn.
    let mut live_off: std::collections::HashMap<&'static str, (Vec<StyleLayer>, Option<String>)> = std::collections::HashMap::new();
    let mut free_images: Vec<u32> = Vec::new();
    let (ovtx, mut ovrx) = mpsc::unbounded_channel::<(usize, Result<Arc<mapterm::raster::RasterSource>>)>();
    let mut switcher_rows: Vec<(u16, mapterm::basemaps::Row)> = Vec::new();
    let mut switcher_rect = Rect::default();
    let mut imagery_kept: std::collections::HashMap<String, ImageryLayers> = std::collections::HashMap::new();
    if let Some(spec) = &a.raster
        && !start_imagery.is_empty()
    {
        imagery_kept.insert(spec.clone(), start_imagery.clone());
    }
    let mut raster_id = start_imagery.last().map_or(RASTER.to_string(), |l| l.0.clone());
    let mut style_loading: Option<String> = None;
    let mut pointer: Option<(f64, f64)> = None;
    // Google's credits follow the view: once it has stayed on a new spot for a moment, ask
    // for the credits there (a quarter of the view's width counts as a new spot).
    let mut google = raster.as_ref().filter(|r| r.is_google()).cloned();
    let mut billed_raster = raster.as_ref().is_some_and(|r| r.is_google() || r.wants_key());
    // Imagery requests a minute: a bucket that holds a minute's worth and refills steadily.
    let mut limiter = imagery_limiter(a.raster_limit, billed_raster);
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
        tokio::spawn(mapterm::ais::stream(key, area_rx.clone(), stx_ais.clone()));
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
    let (rtx, mut rrx) = mpsc::unbounded_channel::<RadioMsg>();
    if let Some(groups) = a.sats.clone() {
        spawn_sats(groups, otx.clone(), mtx.clone(), rtx.clone());
    }
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
        spawn_radar(rvtx.clone());
    }
    // --outages: each level's outlines once (kept on disk for 30 days), then its counts every
    // 5 minutes (the state updates them every 15) with their ETag; counties first, as
    // they're shown first.
    let mut outages = mapterm::outages::Outages::default();
    // What the outage layer was last built from: new data since, the level, the selection.
    let mut outages_new = false;
    let mut outage_level: Option<mapterm::outages::Level> = None;
    let mut outage_shown_selected: Option<String> = None;
    let mut selected_outage: Option<String> = None;
    let (outx, mut outrx) = mpsc::unbounded_channel::<OutageMsg>();
    if a.outages {
        spawn_outages(outx.clone());
    }
    // --alerts: the NWS list every 5 minutes (with its ETag), and zone outlines one a second
    // (from the disk cache at once), the most severe alerts' first.
    let mut alerts: Vec<mapterm::nws::Alert> = Vec::new();
    let mut alert_zones: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    // What the alert layer was last built from: the list changed, outlines have arrived
    // since, and which alerts were in force (a hash of their ids).
    let mut alerts_list_new = false;
    let mut alerts_zones_new = false;
    // When the last outline arrived: from the disk cache they come all at once, so the areas
    // go up once they stop; from the NWS, one a second, so every 10 s.
    let mut alerts_zone_at = Instant::now();
    // The alert highlighted when the layer was last built.
    let mut alert_shown_selected: Option<String> = None;
    let mut alerts_drawn = Instant::now();
    let mut alerts_in_force: u64 = 0;

    let mut selected_alert: Option<String> = None;
    let (natx, mut narx) = mpsc::unbounded_channel::<Result<Vec<mapterm::nws::Alert>>>();
    let (nzreq, nzreq_rx) = mpsc::unbounded_channel::<Vec<String>>();
    let (nztx, mut nzrx) = mpsc::unbounded_channel::<(String, serde_json::Value)>();
    let mut nzreq_rx = Some(nzreq_rx);
    if a.alerts
        && let Some(rx) = nzreq_rx.take()
    {
        spawn_alerts(natx.clone(), rx, nztx.clone());
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
    // The key list (?), over the map.
    let mut show_help = false;
    // Image tiles still waiting to be stitched (smooth scrolling): keep frames coming.
    let mut stitching = false;
    let mut trails_cam: Option<(f64, f64, f64, Rect)> = None;
    // When the view last moved, when the trails were last drawn, and which trail images
    // (aircraft, ships) are up.
    let mut trails_moved = Instant::now();
    let mut trails_drawn = Instant::now() - Duration::from_secs(10);
    let mut trails_shown = (false, false, false, false);
    // --sondes: the balloons, the stream's task with the circle it follows and the balloon
    // whose prediction it follows, and the selected balloon.
    let mut balloons = mapterm::sondehub::Balloons::default();
    // The receiving stations heard before (kept 30 days), so they show before any balloon is.
    let mut stations_saved = Instant::now();
    // What the image lines to the stations were last drawn from.
    let mut links_drawn = String::new();
    // Where balloons are launched (NOAA's list, kept 30 days), and the one clicked.
    let mut launch_sites: Vec<mapterm::upperair::Site> = Vec::new();
    let mut selected_site: Option<String> = None;
    let (sitetx, mut siterx) = mpsc::unbounded_channel::<Result<Vec<mapterm::upperair::Site>>>();
    let load_sites = |tx: mpsc::UnboundedSender<Result<Vec<mapterm::upperair::Site>>>| {
        tokio::spawn(async move {
            let _ = tx.send(mapterm::upperair::load().await);
        });
    };
    if a.sondes {
        load_sites(sitetx.clone());
    }
    if a.sondes {
        balloons.load_stations(&mapterm::sondehub::stations_file(), unix_now());
        // Exact positions an earlier version kept: replaced by their areas at once.
        if balloons.stations_changed() {
            let _ = balloons.save_stations(&mapterm::sondehub::stations_file());
        }
        map.set_data(STATIONS, &balloons.station_points(unix_now()))?;
        map.set_data(STATION_AREAS, &balloons.station_areas(unix_now()))?;
    }
    let (sonde_area_tx, sonde_area_rx) = tokio::sync::watch::channel::<Option<mapterm::sondehub::Area>>(None);
    let (sonde_sel_tx, sonde_sel_rx) = tokio::sync::watch::channel::<Option<String>>(None);
    let (btx, mut brx) = mpsc::unbounded_channel::<mapterm::sondehub::Event>();
    // --nasa-balloons: the flights and their tracks, drawn again when either comes.
    let mut nasa_flights: Vec<mapterm::csbf::Flight> = Vec::new();
    let mut nasa_tracks: std::collections::HashMap<String, Vec<mapterm::csbf::TrackPoint>> = std::collections::HashMap::new();
    let mut nasa_new = false;
    let (nasatx, mut nasarx) = mpsc::unbounded_channel::<NasaMsg>();
    // --towers: the FCC's structures (read once, kept a week), and the view they were last
    // drawn for.
    let mut towers: Vec<mapterm::towers::Tower> = Vec::new();
    let mut towers_drawn: Option<(i32, i64, i64)> = None;
    let (towertx, mut towerrx) = mpsc::unbounded_channel::<Result<Vec<mapterm::towers::Tower>>>();
    let load_towers = |tx: mpsc::UnboundedSender<Result<Vec<mapterm::towers::Tower>>>| {
        tokio::spawn(async move {
            let _ = tx.send(mapterm::towers::load().await);
        });
    };
    if a.towers {
        load_towers(towertx.clone());
    }
    if let Some(all) = a.nasa {
        spawn_nasa(nasatx.clone(), all);
    }
    if a.sondes {
        tokio::spawn(mapterm::sondehub::stream(sonde_area_rx.clone(), sonde_sel_rx.clone(), btx.clone()));
    }
    let mut sonde_area: Option<(Instant, mapterm::sondehub::Area)> = None;
    let mut sonde_state = String::new();
    let mut balloons_drawn = Instant::now();
    let mut selected_balloon: Option<String> = None;
    // --quakes: the USGS feed once a minute (with If-Modified-Since).
    let mut quakes: Vec<mapterm::quakes::Quake> = Vec::new();
    let mut selected_quake: Option<String> = None;
    // --closures: every feed every 5 minutes; drawn again when one arrives, another is
    // selected, or a minute on (planned closures start).
    let mut closures: std::collections::HashMap<usize, Vec<mapterm::closures::Closure>> = std::collections::HashMap::new();
    let mut closures_new = false;
    let mut closures_drawn = Instant::now();
    let mut closures_shown_selected: Option<String> = None;
    let mut selected_closure: Option<String> = None;
    let mut closure_errors: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let (cltx, mut clrx) = mpsc::unbounded_channel::<(usize, std::result::Result<Vec<mapterm::closures::Closure>, String>)>();
    if a.closures {
        // Your own SmarterRoads token, for Virginia's work zones; only ever sent to VDOT's feed.
        spawn_closures(cltx.clone(), mapterm::raster::find_key(a.vdot_key_cmd.as_deref(), &["VDOT_SMARTERROADS_TOKEN"])?);
    }
    // --floods: the gauges around the view (asked for when the view leaves the area, or every
    // 10 minutes), and the selected one's details.
    let mut gauges: Vec<mapterm::floods::Gauge> = Vec::new();
    let mut gauges_area: Option<(Instant, (f64, f64, f64, f64))> = None;
    let mut gauges_asking = false;
    let mut gauges_new = false;
    let mut gauges_shown_selected: Option<String> = None;
    let mut selected_gauge: Option<String> = None;
    let mut gauge_details: std::collections::HashMap<String, Option<mapterm::floods::Detail>> = std::collections::HashMap::new();
    let (gatx, mut garx) = mpsc::unbounded_channel::<GaugeMsg>();
    // --tfrs: the TFRs every 5 minutes, the standing airspace once (kept a week), and the
    // selected TFR's NOTAM.
    let mut tfr_live: Vec<mapterm::tfrs::Restriction> = Vec::new();
    let mut tfr_standing: Vec<mapterm::tfrs::Restriction> = Vec::new();
    let mut tfrs_new = false;
    let mut tfrs_shown_selected: Option<String> = None;
    let mut selected_tfr: Option<String> = None;
    let mut tfr_notams: std::collections::HashMap<String, Option<mapterm::tfrs::Notam>> = std::collections::HashMap::new();
    let (ttx, mut trx) = mpsc::unbounded_channel::<TfrMsg>();
    if a.tfrs {
        spawn_tfrs(ttx.clone());
    }
    // --fires: incidents and perimeters every 5 minutes (each with its ETag).
    let mut fires: Vec<mapterm::fires::Fire> = Vec::new();
    let mut fire_perimeters: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    let mut selected_fire: Option<String> = None;
    let mut fires_new = false;
    let mut fires_shown_selected: Option<String> = None;
    let (ftx, mut frx) = mpsc::unbounded_channel::<FireMsg>();
    if a.fires {
        spawn_fires(ftx.clone());
    }
    let mut quakes_new = false;
    let mut quakes_drawn = Instant::now();
    let mut quakes_shown_selected: Option<String> = None;
    let (qtx, mut qrx) = mpsc::unbounded_channel::<Result<Vec<mapterm::quakes::Quake>>>();
    if let Some(period) = a.quakes {
        spawn_quakes(qtx.clone(), period);
    }
    // `g`: the location helper runs on a blocking task; its answer flies the map there.
    let (ltx, mut lrx) = mpsc::unbounded_channel::<Result<mapterm::locate::Fix>>();
    let mut locating = false;
    let mut credit_view: Option<(i32, i64, i64)> = None;
    let mut credit_seen: Option<((i32, i64, i64), Instant)> = None;
    let mut credit_pending = false;
    // The live layers running: each can be turned off and on.
    let mut live_running: Vec<usize> = mapterm::basemaps::LIVE
        .iter()
        .enumerate()
        .filter(|(_, l)| match l.key {
            "aircraft" => adsb_feed.is_some(),
            "ships" => ais_key.is_some(),
            "satellites" => a.sats.is_some(),
            "balloons" => a.sondes,
            "nasa" => a.nasa.is_some(),
            "towers" => a.towers,
            "radar" => radar.is_some(),
            "alerts" => a.alerts,
            "quakes" => a.quakes.is_some(),
            "fires" => a.fires,
            "gauges" => a.floods,
            "outages" => a.outages,
            "tfrs" => a.tfrs,
            "closures" => a.closures,
            "traffic" => a.traffic,
            _ => false,
        })
        .map(|(i, _)| i)
        .collect();
    // A session opened: its base map, overlays, layers turned off and overlay setting.
    let mut session_open: Option<mapterm::sessions::Session> = a.session.clone();
    // --resume: open on the sessions to pick one.
    if a.resume {
        session_list = mapterm::sessions::all();
        let rows = session_rows(&session_list);
        if rows.is_empty() {
            note = Some((Instant::now(), "no sessions yet: S saves one, and each run is kept when it ends".into()));
        } else {
            switcher = Some(mapterm::basemaps::Switcher::sessions(rows, true));
        }
    }
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
                .or_else(|| selected_balloon.as_deref().and_then(|s| balloons.get(s, unix_now() - SONDE_DELAY).map(|x| x.1)))
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
            // Smooth scrolling shifts image tiles by part of a cell: give fetched ones a strip
            // of their neighbors to shift into, a few at a time (a few ms a frame).
            map.stitch = smooth_on && shift_mode && kitty;
            stitching = map.stitch_some(Duration::from_millis(6));
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
                r.update(&mut map, clock_t, ahead, clock.is_playing(), &draw_cam, map_area);
            }
            // The alerts in force at the clock's time. Rebuilding the layer means every area's
            // outline again, so only when what's shown changes: a new list, another set in
            // force, or outlines that arrived meanwhile (once they stop, or every 10 s while
            // they keep coming).
            if a.alerts {
                let in_force = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    alerts.iter().filter(|al| al.in_force(clock_t)).for_each(|al| al.id.hash(&mut h));
                    h.finish()
                };
                let outlines_due = alerts_zones_new && (alerts_drawn.elapsed() >= Duration::from_secs(10) || alerts_zone_at.elapsed() >= Duration::from_millis(1500));
                if alerts_list_new || in_force != alerts_in_force || outlines_due || selected_alert != alert_shown_selected {
                    map.set_data(ALERTS, &mapterm::nws::features(&alerts, &alert_zones, clock_t, selected_alert.as_deref()))?;
                    alert_tiles.set(mapterm::nws::shapes(&alerts, &alert_zones, clock_t, selected_alert.as_deref()));
                    alert_shown_selected = selected_alert.clone();
                    // Drawn again from the new areas as the view asks for them.
                    map.put_raster_tile(ALERT_TILES, None, true);
                    (alerts_list_new, alerts_zones_new, alerts_drawn, alerts_in_force) = (false, false, Instant::now(), in_force);
                }
            }
            // The outages at the level the zoom shows: rebuilt when new figures or outlines
            // arrive, the level changes, or another area is selected.
            if a.outages {
                let level = mapterm::outages::Level::at(draw_cam.zoom);
                if outages_new || outage_level != Some(level) || selected_outage != outage_shown_selected {
                    map.set_data(OUTAGES, &outages.features(level, selected_outage.as_deref()))?;
                    outage_tiles.set(outages.shapes(level, selected_outage.as_deref()));
                    map.put_raster_tile(OUTAGE_TILES, None, true);
                    (outages_new, outage_level, outage_shown_selected) = (false, Some(level), selected_outage.clone());
                }
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
            // The selected balloon, the same way, and where it's predicted to go and land.
            if let Some((b, at)) = selected_balloon.as_deref().and_then(|s| balloons.get(s, unix_now() - SONDE_DELAY)) {
                if let Some(p) = balloons.prediction(&b.latest.serial)
                    && let Some((t, lon, lat)) = p.landing()
                {
                    let dim = Color::Rgb(127, 178, 255);
                    // Where the terminal shows images, the path is drawn there, smooth, with the
                    // satellite tracks; braille elsewhere.
                    if !trails_kitty {
                        overlays.push(Overlay::Line { pts: p.path.iter().map(|&(_, lon, lat, _)| (lon, lat)).collect(), color: dim });
                    }
                    // The label on a solid background, so it reads over any map or imagery.
                    let when = format!(" lands {} ", hhmm(t));
                    let tag = TStyle::new().fg(Color::Black).bg(dim).add_modifier(ratatui::style::Modifier::BOLD);
                    overlays.push(Overlay::Marker { lon, lat, glyph: "✕".into(), style: TStyle::new().fg(dim), label: Some((when, tag)), priority: 100 });
                }
                let st = TStyle::new().fg(Color::Black).bg(Color::Rgb(127, 178, 255)).add_modifier(ratatui::style::Modifier::BOLD);
                overlays.push(Overlay::Marker { lon: at.lon, lat: at.lat, glyph: b.glyph().into(), style: st, label: Some((b.latest.serial.clone(), st)), priority: 101 });
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
                let tl_rows = if timeline_on(&radar, a.alerts) && f.area().height >= 12 { 2 } else { 0 };
                let [main, tl, s] = Layout::vertical([Constraint::Min(1), Constraint::Length(tl_rows), Constraint::Length(status_rows)]).areas(f.area());
                timeline_area = tl;
                if tl_rows > 0 {
                    // It follows the data until moved: the radar's two hours to a little past
                    // now (alerts alone, an hour back to half a day ahead).
                    let now = unix_now();
                    if !timeline_moved {
                        match radar.as_ref().and_then(|r| r.span()) {
                            Some((start, _)) => timeline.set_span(start, now + 900.0),
                            None => timeline.set_span(now - 3600.0, now + 12.0 * 3600.0),
                        }
                    }
                    timeline.clear_highlights();
                    if let Some((start, stop)) = radar.as_ref().filter(|r| !r.is_off()).and_then(|r| r.span()) {
                        timeline.highlight(start, stop, Color::Rgb(0x22, 0x36, 0x4d));
                    }
                    if let Some(al) = selected_alert.as_ref().and_then(|id| alerts.iter().find(|a| &a.id == id)) {
                        let (r, g, b) = al.severity.rgb();
                        timeline.highlight(al.from.unwrap_or(f64::MIN), al.to.unwrap_or(f64::MAX), Color::Rgb(r / 3, g / 3, b / 3));
                    }
                    timeline.zone = zone();
                    timeline.render(f.buffer_mut(), tl, &clock, now);
                }
                let results = matches!(search, Search::Results { .. }) || finder.is_some();
                let plane = selected.as_deref().and_then(|h| aircraft.get(h, unix_now() - adsb_delay));
                let ship = selected_ship.and_then(|m| fleet.get(m, unix_now() - AIS_DELAY));
                let balloon = selected_balloon.as_deref().and_then(|s| balloons.get(s, unix_now() - SONDE_DELAY));
                let sat = sats.iter().find(|s| Some(s.norad) == selected_sat).and_then(|s| Some((s, s.at(unix_now())?)));
                let alert = selected_alert.as_ref().and_then(|id| alerts.iter().find(|a| &a.id == id));
                let outage = selected_outage.as_deref().and_then(|k| outages.find(k));
                let quake = selected_quake.as_deref().and_then(|id| quakes.iter().find(|q| q.id == id));
                let fire = selected_fire.as_deref().and_then(|id| fires.iter().find(|f| f.id == id));
                let show_route = (route_panel && !routes.is_empty()).then_some(routes.as_slice());
                let closure = selected_closure.as_deref().and_then(|k| closures.values().flatten().find(|c| c.key == k));
                let tfr = selected_tfr.as_deref().and_then(|k| tfr_live.iter().chain(&tfr_standing).find(|r| r.key == k));
                let gauge = selected_gauge.as_deref().and_then(|id| gauges.iter().find(|g| g.lid == id));
                let site = selected_site.as_deref().and_then(|id| launch_sites.iter().find(|s| s.id == id));
                let panel_w =
                    if results || (inspect && !picks.is_empty()) || plane.is_some() || ship.is_some() || sat.is_some() || alert.is_some() || outage.is_some() || quake.is_some() || fire.is_some() || gauge.is_some() || tfr.is_some() || closure.is_some() || show_route.is_some() || balloon.is_some() || site.is_some() {
                        52.min(main.width / 2)
                    } else {
                        0
                    };
                // The panel floats over the map's right side rather than taking room from it,
                // so opening one doesn't move the map.
                let m = main;
                let panel = Rect { x: main.right() - panel_w, width: panel_w, ..main };
                map_area = m;
                panel_area = panel;
                f.render_widget(map.view(&draw_cam), m);
                // Legends for what's colored, stacked up from the map's lower left.
                legend_rects.clear();
                if show_legend {
                    let mut legends: Vec<mapterm::legend::Legend> = Vec::new();
                    legends.extend(sat_legend.clone().filter(|_| !sats.is_empty() && !live_off.contains_key("satellites")));
                    if adsb_feed.is_some() && !aircraft.is_empty() && !live_off.contains_key("aircraft") {
                        legends.push(mapterm::adsb::legend(&aircraft));
                    }
                    if ais_key.is_some() && !fleet.is_empty() && !live_off.contains_key("ships") {
                        legends.push(mapterm::ais::legend(&fleet));
                    }
                    if a.outages && !live_off.contains_key("outages") {
                        legends.extend(outages.legends(mapterm::outages::Level::at(cam.zoom)));
                    }
                    if a.floods && !gauges.is_empty() && !live_off.contains_key("gauges") {
                        legends.push(mapterm::floods::legend(&gauges));
                    }
                    if a.closures && !closures.is_empty() && !live_off.contains_key("closures") {
                        let all: Vec<mapterm::closures::Closure> = closures.values().flatten().cloned().collect();
                        legends.push(mapterm::closures::legend(&all, unix_now()));
                    }
                    if a.tfrs && !(tfr_live.is_empty() && tfr_standing.is_empty()) && !live_off.contains_key("tfrs") {
                        let all: Vec<mapterm::tfrs::Restriction> = tfr_standing.iter().chain(&tfr_live).cloned().collect();
                        legends.push(mapterm::tfrs::legend(&all));
                    }
                    if a.fires && !fires.is_empty() && !live_off.contains_key("fires") {
                        legends.push(mapterm::fires::legend(&fires));
                    }
                    if a.quakes.is_some() && !quakes.is_empty() && !live_off.contains_key("quakes") {
                        legends.push(mapterm::quakes::legend(&quakes, unix_now()));
                    }
                    if a.traffic && !sensors.is_empty() && !live_off.contains_key("traffic") {
                        legends.push(mapterm::traffic::legend(&sensors));
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
                station_rows.clear();
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
                    _ if let Some(b) = balloon => {
                        let heard = balloons.heard_by(&b.0.latest.serial, unix_now());
                        let from = launch_site(&launch_sites, b.0);
                        station_rows = draw_balloon(f, panel, b, balloons.prediction(&b.0.latest.serial), &heard, from, unix_now(), follow, me.as_ref());
                        Vec::new()
                    }
                    _ if let Some(s) = site => {
                        // The balloons followed that went up from it.
                        let launched: Vec<(String, f64)> = balloons
                            .serials()
                            .filter_map(|sn| balloons.get(sn, unix_now() - SONDE_DELAY))
                            .filter(|(b, _)| launch_site(&launch_sites, b).is_some_and(|x| x.id == s.id))
                            .map(|(b, at)| (b.latest.serial.clone(), at.alt.unwrap_or(0.0)))
                            .collect();
                        draw_site(f, panel, s, &launched, unix_now());
                        Vec::new()
                    }
                    _ if let Some(s) = ship => {
                        draw_ship(f, panel, s, unix_now(), follow, me.as_ref());
                        Vec::new()
                    }
                    _ if let Some(g) = gauge => {
                        draw_gauge(f, panel, g, gauge_details.get(&g.lid).and_then(Option::as_ref));
                        Vec::new()
                    }
                    _ if let Some(c) = closure => {
                        draw_closure(f, panel, c);
                        Vec::new()
                    }
                    _ if let Some(r) = tfr => {
                        draw_tfr(f, panel, r, r.notam.as_ref().and_then(|n| tfr_notams.get(n)).and_then(Option::as_ref));
                        Vec::new()
                    }
                    _ if let Some(fi) = fire => {
                        draw_fire(f, panel, fi, fire_perimeters.contains_key(&fi.id));
                        Vec::new()
                    }
                    _ if let Some(q) = quake => {
                        draw_quake(f, panel, q);
                        Vec::new()
                    }
                    _ if let Some(al) = alert => {
                        draw_alert(f, panel, al);
                        Vec::new()
                    }
                    _ if let Some(o) = outage => {
                        draw_outage(f, panel, o);
                        Vec::new()
                    }
                    _ if let Some(r) = show_route => {
                        draw_route(f, panel, r, route_pick, &route_traffic);
                        Vec::new()
                    }
                    _ => Vec::new(),
                };
                if let Some(r) = box_rect {
                    f.render_widget(Block::bordered().border_style(TStyle::new().fg(Color::Yellow)), r);
                }
                if show_help {
                    let on = HelpOn {
                        radar: radar.is_some(),
                        alerts: a.alerts,
                        outages: a.outages,
                        live: adsb_feed.is_some() || ais_key.is_some() || a.sats.is_some(),
                        sats: a.sats.is_some(),
                    };
                    draw_help(f, main, on);
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
                // The imagery and overlays by name (their credits are with the others at the
                // end), and how they draw.
                let on: Vec<&str> = overlay_on.iter().zip(mapterm::basemaps::OVERLAYS).filter(|(on, _)| **on).map(|(_, o)| o.label).collect();
                if raster.is_some() || !on.is_empty() {
                    let mut parts: Vec<String> = Vec::new();
                    if raster.is_some() {
                        parts.push(bases.get(base_at).map_or("imagery".into(), |e| e.label.clone()));
                    }
                    if !on.is_empty() {
                        parts.push(format!("+ {}", on.join(", ")));
                    }
                    if !kitty {
                        parts.push("(cells)".into());
                    }
                    if raster.is_some() && vector > 0 {
                        parts.push(["", "· labels", "· raster only"][vector as usize].into());
                    }
                    spans.push(Span::styled(format!("◧ {} ", parts.join(" ")), TStyle::new().fg(Color::LightBlue)));
                }
                // Each data layer with how many of its features are in view (to compare layers).
                for (name, color) in &legend {
                    spans.push(Span::styled("● ", TStyle::new().fg(*color)));
                    match map.features_in_view(name, &draw_cam, map_area) {
                        Some(n) => spans.push(Span::styled(format!("{name} {} ", thousands(n)), dim)),
                        None => spans.push(Span::styled(format!("{name} "), dim)),
                    }
                }
                if a.sats.is_some() && !live_off.contains_key("satellites") {
                    let st = TStyle::new().fg(Color::Rgb(127, 178, 255));
                    let of = sat_filter.describe();
                    spans.push(Span::styled(format!("◇ {} sats{of} ", sats.len()), st));
                }
                if a.towers && !live_off.contains_key("towers") {
                    let text = if towers.is_empty() {
                        "† … ".to_string()
                    } else if cam.zoom < TOWERS_MIN_ZOOM {
                        "† zoom in for towers ".to_string()
                    } else {
                        let vp = Viewport::new(&cam, map_area);
                        let (w, n) = vp.cell_to_lonlat(0, 0);
                        let (e, s) = vp.cell_to_lonlat(map_area.width, map_area.height);
                        let inview = towers.iter().filter(|t| t.lon >= w && t.lon <= e && t.lat >= s && t.lat <= n).count();
                        format!("† {} towers ", thousands(inview))
                    };
                    spans.push(Span::styled(text, TStyle::new().fg(Color::Rgb(0x8a, 0xa4, 0xc8))));
                }
                if a.nasa.is_some() && !live_off.contains_key("nasa") {
                    let flying = nasa_flights.iter().filter(|f| unix_now() - f.t <= mapterm::csbf::RECENT).count();
                    let text = match (flying, status.get("nasa").and_then(|s| s.updated)) {
                        (_, None) => "◓ … ".to_string(),
                        (0, _) => "◓ no NASA balloons flying ".to_string(),
                        (n, _) => format!("◓ {n} NASA balloon{} ", if n == 1 { "" } else { "s" }),
                    };
                    spans.push(Span::styled(text, TStyle::new().fg(Color::Rgb(0xff, 0x6b, 0x6b))));
                }
                if a.sondes && !live_off.contains_key("balloons") {
                    let st = TStyle::new().fg(Color::Rgb(127, 178, 255));
                    spans.push(Span::styled(format!("▲ {} sondes{} ", balloons.len(), sonde_state), st));
                }
                if ais_key.is_some() && !live_off.contains_key("ships") {
                    let st = TStyle::new().fg(Color::Rgb(127, 178, 255));
                    let text = if cam.zoom < AIS_MIN_ZOOM { "⛴ zoom in for ships ".to_string() } else { format!("⛴ {}{} ", fleet.len(), ais_state) };
                    spans.push(Span::styled(text, st));
                }
                if adsb_feed.is_some() && !live_off.contains_key("aircraft") {
                    let st = TStyle::new().fg(Color::Rgb(127, 178, 255));
                    let text = match aircraft_retry.map(|t| t.saturating_duration_since(Instant::now())) {
                        _ if cam.zoom < ADSB_MIN_ZOOM => "✈ zoom in for aircraft ".to_string(),
                        Some(w) if !w.is_zero() => format!("✈ paused {} s ", w.as_secs() + 1),
                        _ => {
                            // Balloons and airships in the feed, counted on their own.
                            let lta_n = aircraft.all().filter(|a| a.is_balloon()).count();
                            let lta = if lta_n > 0 { format!(" · ◓ {lta_n} lighter than air") } else { String::new() };
                            match aircraft_quota {
                                Some(q) => format!("✈ {}{lta} · {} left ", aircraft.len(), q.remaining),
                                None => format!("✈ {}{lta} ", aircraft.len()),
                            }
                        }
                    };
                    spans.push(Span::styled(text, st));
                }
                if a.alerts {
                    let now_in = alerts.iter().filter(|al| al.in_force(clock_t)).count();
                    let want = alerts.iter().filter(|al| al.geometry.is_none()).flat_map(|al| &al.zones).collect::<std::collections::HashSet<_>>();
                    let have = want.iter().filter(|z| alert_zones.contains_key(**z)).count();
                    let more = if have < want.len() { format!(" (areas {have}/{})", want.len()) } else { String::new() };
                    spans.push(Span::styled(format!("⚠ {now_in}{more} "), TStyle::new().fg(Color::Rgb(0xff, 0xa6, 0x4d))));
                }
                if a.floods {
                    let st = TStyle::new().fg(Color::Rgb(0x4f, 0xa3, 0xff));
                    let flooding = gauges.iter().filter(|g| g.worst() >= mapterm::floods::Category::Minor).count();
                    let text = if cam.zoom < mapterm::floods::MIN_ZOOM {
                        "◆ zoom in for gauges ".to_string()
                    } else if gauges.is_empty() {
                        "◆ … ".to_string()
                    } else {
                        format!("◆ {} gauges{} ", gauges.len(), if flooding > 0 { format!(" · {flooding} flooding") } else { String::new() })
                    };
                    spans.push(Span::styled(text, st));
                }
                if let Some(r) = &route {
                    let st = TStyle::new().fg(Color::Rgb(0x22, 0xd3, 0xee));
                    let with = route_traffic.get(route_pick).cloned().flatten().map(|e| format!(" ({} with traffic)", mapterm::route::duration(e.seconds))).unwrap_or_default();
                    spans.push(Span::styled(format!("⮕ {} · {}{with} ", mapterm::route::distance(r.miles), mapterm::route::duration(r.seconds)), st));
                } else if route_asking {
                    spans.push(Span::styled("⮕ routing… ", TStyle::new().fg(Color::Rgb(0x22, 0xd3, 0xee))));
                }
                if a.traffic && !live_off.contains_key("traffic") {
                    let slow = sensors.iter().filter(|s| s.level() >= mapterm::traffic::Level::Heavy).count();
                    let text = if sensors.is_empty() { "● traffic … ".to_string() } else { format!("● {} speed sensors · {slow} under 35 mph ", sensors.len()) };
                    spans.push(Span::styled(text, TStyle::new().fg(Color::Rgb(0x22, 0xc5, 0x5e))));
                }
                if a.closures && !live_off.contains_key("closures") {
                    let st = TStyle::new().fg(Color::Rgb(0xff, 0x7a, 0x32));
                    let now = unix_now();
                    let shown = closures.values().flatten().filter(|c| c.shown(now));
                    let (planned, current): (Vec<_>, Vec<_>) = shown.partition(|c| c.planned(now));
                    let text = if closures.is_empty() { "━ … ".to_string() } else { format!("━ {} closures · {} planned ", current.len(), planned.len()) };
                    spans.push(Span::styled(text, st));
                }
                if a.tfrs && !live_off.contains_key("tfrs") {
                    let st = TStyle::new().fg(Color::Rgb(0xc8, 0x4d, 0xe8));
                    let text = if tfr_live.is_empty() { "⛔ … ".to_string() } else { format!("⛔ {} TFRs ", tfr_live.len()) };
                    spans.push(Span::styled(text, st));
                }
                if a.fires && !live_off.contains_key("fires") {
                    let st = TStyle::new().fg(Color::Rgb(0xff, 0x45, 0x3a));
                    let wild = fires.iter().filter(|f| f.kind != "RX").count();
                    let text = if fires.is_empty() { "✹ … ".to_string() } else { format!("✹ {wild} wildfires · {} perimeters ", fire_perimeters.len()) };
                    spans.push(Span::styled(text, st));
                }
                if let Some(period) = a.quakes.filter(|_| !live_off.contains_key("quakes")) {
                    let st = TStyle::new().fg(Color::Rgb(0xff, 0x9f, 0x0a));
                    let biggest = quakes.iter().filter(|q| q.mag.is_some()).max_by(|a, b| a.mag.unwrap_or(0.0).total_cmp(&b.mag.unwrap_or(0.0)));
                    let text = match biggest {
                        Some(q) => format!("◉ {} quakes ({}) · {} ", quakes.len(), period.label(), mapterm::quakes::mag_text(q)),
                        None => format!("◉ … ({}) ", period.label()),
                    };
                    spans.push(Span::styled(text, st));
                }
                if a.outages && !live_off.contains_key("outages") {
                    let st = TStyle::new().fg(Color::Rgb(0xff, 0xb4, 0x46));
                    let totals = outages.totals();
                    let text = if totals.is_empty() {
                        "⚡ … ".to_string()
                    } else {
                        let each: Vec<String> = totals.iter().map(|(st, n)| format!("{st} {}", thousands(*n as usize))).collect();
                        format!("⚡ {} out ", each.join(" · "))
                    };
                    spans.push(Span::styled(text, st));
                }
                // The clock: live, or stopped (or playing) at a time; then the radar scan shown
                // and how old it is (live, the latest scan is a few minutes old).
                if timeline_area.height > 0 {
                    let (badge, color) = timeline.badge(&clock);
                    let when = if clock.is_live() { String::new() } else { format!(" · {} ago", mapterm::sources::span((unix_now() - clock.time()).max(0.0))) };
                    spans.insert(0, Span::styled(format!(" {badge}{when}"), TStyle::new().fg(color).add_modifier(ratatui::style::Modifier::BOLD)));
                }
                if let Some(r) = radar.as_ref().filter(|r| !r.is_off()) {
                    match r.shown_time() {
                        Some(t) => {
                            let age = mapterm::sources::span((unix_now() - t).max(0.0));
                            spans.push(Span::styled(format!("☂ scan {} ({age} old) ", hhmm(t)), dim));
                        }
                        None => spans.push(Span::styled("☂ … ", dim)),
                    }
                }
                spans.push(Span::styled(format!("{frame_ms:.0} ms "), dim));
                // The base map switcher over everything on the map.
                (switcher_rect, switcher_rows) = match &switcher {
                    Some(sw) => {
                        let loading = style_loading.is_some() || imagery_loading.is_some() || want_base.is_some();
                        let ov: Vec<(bool, bool)> = overlay_on.iter().enumerate().map(|(i, on)| (*on, overlay_loading.contains(&i))).collect();
                        let radar_on = radar.as_ref().is_some_and(|r| !r.is_off());
                        let live_on = |i: usize| {
                            let key = mapterm::basemaps::LIVE[i].key;
                            if key == "radar" { radar_on } else { !live_off.contains_key(key) }
                        };
                        // Not started (enter starts it), waiting for its first data, on, off.
                        let live_state = |i: usize| -> (&'static str, Option<&'static str>) {
                            let key = mapterm::basemaps::LIVE[i].key;
                            if !live_running.contains(&i) {
                                ("○", Some("not started · enter starts it"))
                            } else if !live_on(i) {
                                ("☐", None)
                            } else if status.get(key).is_none_or(|s| s.updated.is_none()) && !matches!(key, "satellites" | "gauges") {
                                ("◌", Some("starting…"))
                            } else {
                                ("☑", None)
                            }
                        };
                        let data_on = |i: usize| legend.get(i).is_none_or(|(id, _)| !data_off.contains_key(id));
                        let now = unix_now();
                        let source = |row: mapterm::basemaps::Row| -> Option<(String, String)> {
                            use mapterm::basemaps::Row;
                            match row {
                                Row::Live(i) => {
                                    let key = mapterm::basemaps::LIVE[i].key;
                                    let d = mapterm::sources::live_desc(key)?;
                                    let st = status.get(key).cloned().unwrap_or_default();
                                    let n = st.features.map(|n| format!("{} · ", thousands(n))).unwrap_or_default();
                                    let off = if live_on(i) { "" } else { "off · " };
                                    // Tracks are drawn a little in the past, to move smoothly between reports.
                                    let latency = match key {
                                        "aircraft" => Some(adsb_delay),
                                        "ships" => Some(AIS_DELAY),
                                        "balloons" => Some(SONDE_DELAY),
                                        _ => None,
                                    };
                                    let behind = latency.map(|l| format!(" · drawn {} behind", mapterm::sources::span(l))).unwrap_or_default();
                                    Some((
                                        format!("{off}{n}{}", st.describe(now)),
                                        format!("{} {} · {}{behind} · {}", d.time.mark(), d.time.label(), d.origin, d.credit),
                                    ))
                                }
                                Row::Data(i) => {
                                    let (id, _) = legend.get(i)?;
                                    let d = data_info.get(i)?;
                                    let off = if data_on(i) { "" } else { "off · " };
                                    let n = d.features.map(|n| format!("{} · ", thousands(n))).unwrap_or_default();
                                    let credit = map.source(id).and_then(MapSource::attribution).map(|c| format!(" · {c}")).unwrap_or_default();
                                    let ago = mapterm::sessions::ago(d.loaded, now);
                                    Some((format!("{off}{n}{} · read {ago}", d.what), format!("· timeless · {}{credit}", short_path(&d.spec, 56))))
                                }
                                _ => None,
                            }
                        };
                        draw_switcher(f, map_area, sw, &bases, base_at, loading, labels, vector, &ov, &live_state, &data_on, show_legend, &source)
                    }
                    None => (Rect::default(), Vec::new()),
                };
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
                if route.is_some() && !credits.iter().any(|c| c == mapterm::route::CREDIT) {
                    credits.push(mapterm::route::CREDIT.to_string());
                }
                let mut credit_items = if credits.is_empty() { vec![basemap_name.clone()] } else { credits };
                // With a live or hazard layer on, mapterm's disclaimer after the credits.
                if live_data {
                    credit_items.push(DISCLAIMER.to_string());
                }
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
                // While naming a place, the status line is its prompt.
                if let Some(name) = &place_naming {
                    spans = vec![
                        Span::styled(" save place ", TStyle::new().fg(Color::Black).bg(Color::Rgb(0xfb, 0xbf, 0x24))),
                        Span::raw(format!(" > {name}")),
                        Span::styled("█", TStyle::new().fg(Color::Rgb(0xfb, 0xbf, 0x24))),
                        Span::styled(format!("  {:.4}, {:.4} · enter save · esc cancel", place_spot.1, place_spot.0), dim),
                    ];
                }
                // While naming a session, the status line is its prompt.
                if let Some(name) = &naming {
                    spans = vec![
                        Span::styled(" save session ", TStyle::new().fg(Color::Black).bg(Color::Rgb(127, 178, 255))),
                        Span::raw(format!(" > {name}")),
                        Span::styled("█", TStyle::new().fg(Color::Rgb(127, 178, 255))),
                        Span::styled("  enter save · esc cancel", dim),
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
                // mapterm's mark at the end of the credits, as on mapterm.com: "mt" in braille.
                let logo = logo_spans();
                let logo_w = if width >= 40 { LOGO_W as usize + 1 } else { 0 };
                let together = status_width + 2 + Span::raw(credits.as_str()).width() + 10 + logo_w <= width;
                // Otherwise as many lines as they take, broken between credits, up to half
                // the window.
                let lines = credit_lines(&credit_items, width.saturating_sub(logo_w));
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
                    spans.extend(credit_spans(&format!("· {credits} · ? keys · q quit"), dim));
                    let row = Rect { y: s.bottom() - 1, height: 1, ..s };
                    f.render_widget(Paragraph::new(Line::from(spans)), row);
                    if logo_w > 0 {
                        f.render_widget(Paragraph::new(Line::from(logo)), Rect { x: row.right() - LOGO_W, width: LOGO_W, ..row });
                    }
                } else {
                    let [above, line] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(s);
                    // Bottom-aligned, so a frame where the rows haven't caught up yet loses
                    // the top line, not the end of the last.
                    let shown = lines.len().min(above.height as usize);
                    let text: Vec<Line> = lines[lines.len() - shown..].iter().map(|l| Line::from(credit_spans(l, dim))).collect();
                    f.render_widget(Paragraph::new(text), Rect { y: above.bottom() - shown as u16, height: shown as u16, ..above });
                    if logo_w > 0 && shown > 0 {
                        f.render_widget(Paragraph::new(Line::from(logo)), Rect { x: above.right() - LOGO_W, y: above.bottom() - 1, width: LOGO_W, height: 1 });
                    }
                    spans.push(Span::styled("· ? keys · q quit", dim));
                    f.render_widget(Paragraph::new(Line::from(spans)), line);
                }
            })?;
            ctl.set_area(map_area);
            frame_ms = t0.elapsed().as_secs_f64() * 1000.0;
            if sync {
                let mut out = std::io::stdout();
                // Images of live layers just turned off.
                for id in free_images.drain(..) {
                    out.write_all(mapterm::kitty::free(id).as_bytes())?;
                }
                // What's drawn over the map (legends, the panel, the base map switcher): images
                // keep clear of it.
                let holes: Vec<Rect> = legend_rects
                    .iter()
                    .copied()
                    .chain((panel_area.width > 0).then_some(panel_area))
                    .chain((switcher_rect.width > 0).then_some(switcher_rect))
                    .collect();
                kstate.holes = holes.clone();
                // A panel opened or closed: the satellite and trail images take the new holes now.
                if panel_area != last_panel || switcher_rect != last_switcher {
                    last_panel = panel_area;
                    last_switcher = switcher_rect;
                    trails_dirty = true;
                    sat_dots_drawn = Instant::now() - Duration::from_secs(10);
                }
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
                        for (id, shown) in [(TRAIL_IMAGE, &mut trails_shown.0), (SHIP_TRAIL_IMAGE, &mut trails_shown.1), (PATH_IMAGE, &mut trails_shown.2), (BALLOON_TRAIL_IMAGE, &mut trails_shown.3)] {
                            if std::mem::take(shown) {
                                out.write_all(mapterm::kitty::free(id).as_bytes())?;
                            }
                        }
                    }
                    let settled = trails_moved.elapsed() >= Duration::from_millis(150) && !ctl.animating();
                    // Paths: the selected satellite's ground track, and the selected balloon's
                    // predicted flight to its landing.
                    let mut path_lines: Vec<mapterm::timeseries::Line> = sats
                        .iter()
                        .find(|s| Some(s.norad) == selected_sat)
                        .map(|s| s.track(unix_now(), s.period_min()).into_iter().map(|p| (p, (127, 178, 255))).collect())
                        .unwrap_or_default();
                    if let Some(p) = selected_balloon.as_deref().and_then(|s| balloons.prediction(s)) {
                        path_lines.push((p.path.iter().map(|&(_, lon, lat, _)| (lon, lat)).collect(), (127, 178, 255)));
                    }
                    let hidden =
                        (adsb_feed.is_some() && !trails_shown.0) || (ais_key.is_some() && !trails_shown.1) || (!path_lines.is_empty() && !trails_shown.2) || (a.sondes && !trails_shown.3);
                    if settled && ((trails_dirty && trails_drawn.elapsed() >= Duration::from_secs(1)) || (hidden && trails_dirty)) {
                        trails_dirty = false;
                        trails_drawn = Instant::now();
                        let t_trails = Instant::now();
                        let (w, h) = ((map_area.width as f64 * cpx.0).round() as u32, (map_area.height as f64 * cpx.1).round() as u32);
                        let vp = Viewport::new(&cam, map_area);
                        let now = unix_now();
                        let sets: [TrailSet; 3] = [
                            (a.sondes && !live_off.contains_key("balloons"), balloons.trail_paths().collect(), now - SONDE_DELAY, SONDE_TRAIL, BALLOON_TRAIL_IMAGE, -12, &mut trails_shown.3),
                            (
                                adsb_feed.is_some() && !live_off.contains_key("aircraft"),
                                mapterm::adsb::trail_paths(&aircraft).collect(),
                                now - adsb_delay,
                                ADSB_TRAIL,
                                TRAIL_IMAGE,
                                -10,
                                &mut trails_shown.0,
                            ),
                            (ais_key.is_some() && !live_off.contains_key("ships"), fleet.trail_paths().collect(), now - AIS_DELAY, AIS_TRAIL, SHIP_TRAIL_IMAGE, -11, &mut trails_shown.1),
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
                            punch(&mut rgba, w, map_area, &holes, cpx);
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
                            punch(&mut rgba, w, map_area, &holes, cpx);
                            out.write_all(
                                mapterm::kitty::show_rgba(PATH_IMAGE, w, h, &rgba, map_area.x, map_area.y, map_area.width, map_area.height, -9, shm).as_bytes(),
                            )?;
                            trails_shown.2 = true;
                        }
                        dlog(|| format!("trails: {w}x{h} px, {:.1} ms, shown {trails_shown:?}", t_trails.elapsed().as_secs_f64() * 1000.0));
                    }
                    // (The images drawn here use the true camera, not the one on the cell grid:
                    // with smooth scrolling they land where the shifted text does.)
                    // Satellites as dots with their names, five times a second once the map is
                    // still; the cell glyphs and text names stand in while it moves. Full
                    // resolution when there are names to draw (text needs it), else half,
                    // scaled up by the terminal (dots don't).
                    let want_dots = !sats.is_empty() && settled && !live_off.contains_key("satellites");
                    if want_dots && sat_dots_drawn.elapsed() >= sat_every {
                        sat_dots_drawn = Instant::now();
                        // Full resolution when there's text to draw: names, or the selected one's.
                        let named = sats.iter().any(|s| !s.is_starlink());
                        let scale = if named || selected_sat.is_some() { 1.0 } else { 0.5 };
                        let (w, h) = ((map_area.width as f64 * cpx.0 * scale).round() as u32, (map_area.height as f64 * cpx.1 * scale).round() as u32);
                        let vp = Viewport::new(&cam, map_area);
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
                        punch(&mut rgba, w, map_area, &holes, (cpx.0 * scale, cpx.1 * scale));
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
                    // Smooth scrolling: the terminal shifts the text by part of a cell; the
                    // image tiles go with it.
                    kstate.shift = if smooth_on && shift_mode { (ox as f64 / 1000.0 * cpx.0, oy as f64 / 1000.0 * cpx.1) } else { (0.0, 0.0) };
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
                if show_help {
                    // Images sit under the text: take them down so the key list reads.
                    out.write_all(mapterm::kitty::CLEAR_PLACEMENTS.as_bytes())?;
                }
                crossterm::execute!(out, crossterm::terminal::EndSynchronizedUpdate)?;
            }

            // Fetch what the view needs that we don't have yet.
            // While the camera moves, paid imagery (keyed providers, billed per tile) loads for
            // where it will stop (a jump's destination, the end of a zoom or glide), not for
            // every view on the way.
            let target = ctl.target().filter(|_| billed_raster);
            let mut reqs: Vec<_> = map.tile_requests(&draw_cam, map_area).into_iter().filter(|r| target.is_none() || r.source != raster_id).collect();
            if let Some(t) = target {
                reqs.extend(map.tile_requests(&t, map_area).into_iter().filter(|r| r.source == raster_id));
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
                if req.source == raster_id && throttled_at.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
                    continue;
                }
                if radar.as_ref().is_some_and(|r| r.owns(&req.source)) && !radar_bucket.take() {
                    held_back = true;
                    continue;
                }
                if req.source == raster_id
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

            // --sondes: follow a circle around the view (1.5 times the view, 100 to 800 km), a
            // new one once the view leaves it, asking SondeHub's API at most once a minute.
            if a.sondes && map_area.width > 0 && !ctl.animating() {
                let (lon, lat, view_nm) = view_circle(&cam, map_area);
                let view_m = view_nm * 1852.0;
                let inside = sonde_area.is_some_and(|(_, (alat, alon, r))| mapterm::adsb::distance_nm((lon, lat), (alon, alat)) * 1852.0 + view_m <= r);
                if !inside && sonde_area.is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(60)) {
                    let area = (lat, lon, (view_m * 1.5).clamp(100_000.0, 800_000.0));
                    dlog(|| format!("sondes: area {area:?}"));
                    sonde_area = Some((Instant::now(), area));
                    let _ = sonde_area_tx.send(Some(area));
                }
                sonde_sel_tx.send_if_modified(|v| {
                    let changed = *v != selected_balloon;
                    if changed {
                        v.clone_from(&selected_balloon);
                    }
                    changed
                });
            }
            // Quakes: again when the feed changes, another is selected, or a minute on (their
            // colors go by age).
            if a.quakes.is_some() && (quakes_new || selected_quake != quakes_shown_selected || quakes_drawn.elapsed() >= Duration::from_secs(60)) {
                map.set_data(QUAKES, &mapterm::quakes::points(&quakes, unix_now(), selected_quake.as_deref()))?;
                (quakes_new, quakes_drawn, quakes_shown_selected) = (false, Instant::now(), selected_quake.clone());
            }
            // Traffic: drawn when new readings come; each route's time with them again.
            if sensors_new || route_new {
                if sensors_new {
                    map.set_data(TRAFFIC, &mapterm::traffic::points(&sensors))?;
                }
                route_traffic = routes.iter().map(|r| mapterm::traffic::estimate(r, &sensors)).collect();
                sensors_new = false;
            }
            // The route: drawn when a new one comes (or it's cleared).
            if route_new {
                // The others dim, the picked one bright and on top.
                let order: Vec<usize> = (0..routes.len()).filter(|i| *i != route_pick).chain((route_pick < routes.len()).then_some(route_pick)).collect();
                let features: Vec<serde_json::Value> = order
                    .iter()
                    .map(|&i| serde_json::json!({"type": "Feature", "geometry": routes[i].geometry(), "properties": {"color": if i == route_pick { "#22d3ee" } else { "#64748b" }}}))
                    .collect();
                map.set_data(ROUTE, &serde_json::json!({"type": "FeatureCollection", "features": features}))?;
                let shapes: Vec<mapterm::areas::Shape> = order
                    .iter()
                    .map(|&i| {
                        let mut shape = if i == route_pick { mapterm::areas::Shape::lines((0x22, 0xd3, 0xee), 5.0, false) } else { mapterm::areas::Shape::lines((0x64, 0x74, 0x8b), 4.0, false) };
                        shape.add(&routes[i].geometry());
                        shape
                    })
                    .collect();
                route_tiles.set(shapes);
                map.put_raster_tile(ROUTE_TILES, None, true);
                route_new = false;
            }
            // Asked again, avoiding the closed roads, once they've come in.
            if route_avoid_pending && !route_asking && !closures.is_empty()
                && let Some((f, t)) = route_ends
            {
                route_avoid_pending = false;
                let all: Vec<mapterm::closures::Closure> = closures.values().flatten().cloned().collect();
                let avoid = mapterm::route::avoid_closures(&all, unix_now(), f, t);
                if !avoid.is_empty() {
                    route_asking = true;
                    let (tx, mode, server) = (routetx.clone(), route.as_ref().map_or(a.route_mode, |r| r.mode), router.clone());
                    tokio::spawn(async move {
                        let r = mapterm::route::route(&server, f, t, mode, &avoid).await.map(|r| (r, f, t)).map_err(|e| format!("{e:#}"));
                        let _ = tx.send(r);
                    });
                }
            }
            // Closures: again when new ones come, another is selected, or a minute on.
            if a.closures && (closures_new || selected_closure != closures_shown_selected || closures_drawn.elapsed() >= Duration::from_secs(60)) {
                let all: Vec<mapterm::closures::Closure> = closures.values().flatten().cloned().collect();
                map.set_data(CLOSURES, &mapterm::closures::features(&all, unix_now(), selected_closure.as_deref()))?;
                closure_tiles.set(mapterm::closures::shapes(&all, unix_now(), selected_closure.as_deref()));
                map.put_raster_tile(CLOSURE_TILES, None, true);
                (closures_new, closures_drawn, closures_shown_selected) = (false, Instant::now(), selected_closure.clone());
            }
            // Gauges: again when new ones come or another is selected.
            if a.floods && (gauges_new || selected_gauge != gauges_shown_selected) {
                map.set_data(GAUGES, &mapterm::floods::points(&gauges, selected_gauge.as_deref()))?;
                (gauges_new, gauges_shown_selected) = (false, selected_gauge.clone());
                // The selected gauge's details, once.
                if let Some(lid) = selected_gauge.clone().filter(|l| !gauge_details.contains_key(l)) {
                    gauge_details.insert(lid.clone(), None);
                    let tx = gatx.clone();
                    tokio::spawn(async move {
                        let r = mapterm::floods::detail(&lid).await.map_err(|e| format!("{e:#}"));
                        let _ = tx.send(GaugeMsg::Detail(lid, r));
                    });
                }
            }
            // Flight restrictions: again when new ones come or another is selected; the
            // selected TFR's NOTAM once.
            if a.tfrs && (tfrs_new || selected_tfr != tfrs_shown_selected) {
                let all: Vec<mapterm::tfrs::Restriction> = tfr_standing.iter().chain(&tfr_live).cloned().collect();
                map.set_data(TFRS, &mapterm::tfrs::features(&all, selected_tfr.as_deref()))?;
                tfr_tiles.set(mapterm::tfrs::shapes(&all, selected_tfr.as_deref()));
                map.put_raster_tile(TFR_TILES, None, true);
                (tfrs_new, tfrs_shown_selected) = (false, selected_tfr.clone());
                let notam = selected_tfr.as_deref().and_then(|k| all.iter().find(|r| r.key == k)).filter(|r| r.kind.is_tfr()).and_then(|r| r.notam.clone());
                if let Some(n) = notam.filter(|n| !tfr_notams.contains_key(n)) {
                    tfr_notams.insert(n.clone(), None);
                    let tx = ttx.clone();
                    tokio::spawn(async move {
                        let r = mapterm::tfrs::notam_detail(&n).await.map_err(|e| format!("{e:#}"));
                        let _ = tx.send(TfrMsg::Notam(n, r));
                    });
                }
            }
            // Fires: again when new data comes or another is selected.
            if a.fires && (fires_new || selected_fire != fires_shown_selected) {
                map.set_data(FIRES, &mapterm::fires::points(&fires, selected_fire.as_deref()))?;
                map.set_data(FIRE_AREAS, &mapterm::fires::perimeter_features(&fires, &fire_perimeters, selected_fire.as_deref()))?;
                fire_tiles.set(mapterm::fires::shapes(&fires, &fire_perimeters, selected_fire.as_deref()));
                map.put_raster_tile(FIRE_TILES, None, true);
                (fires_new, fires_shown_selected) = (false, selected_fire.clone());
            }
            // Towers around the view, drawn again when it moves a quarter of its width (or the
            // zoom changes); none zoomed out.
            if a.towers && !towers.is_empty() && map_area.width > 0 {
                let vp = Viewport::new(&cam, map_area);
                let (west, north) = vp.cell_to_lonlat(0, 0);
                let (east, south) = vp.cell_to_lonlat(map_area.width, map_area.height);
                let step = ((east - west).abs() / 4.0).max(1e-9);
                let key = if cam.zoom >= TOWERS_MIN_ZOOM { (cam.zoom.floor() as i32, (cam.lon / step).round() as i64, (cam.lat / step).round() as i64) } else { (-1, 0, 0) };
                if towers_drawn != Some(key) {
                    towers_drawn = Some(key);
                    let (dx, dy) = ((east - west).abs() / 2.0, (north - south).abs() / 2.0);
                    let pts = if key.0 < 0 { serde_json::json!({"type": "FeatureCollection", "features": []}) } else { mapterm::towers::points(&towers, (west - dx, south - dy, east + dx, north + dy)) };
                    map.set_data(TOWERS, &pts)?;
                }
            }
            if let (Some(all), true) = (a.nasa, nasa_new) {
                nasa_new = false;
                let now = unix_now();
                map.set_data(NASA, &mapterm::csbf::points(&nasa_flights, now, all))?;
                let shown: HashSet<&String> = nasa_flights.iter().filter(|f| all || now - f.t <= mapterm::csbf::RECENT).map(|f| &f.id).collect();
                map.set_data(NASA_TRACKS, &mapterm::csbf::track_lines(nasa_tracks.iter().filter(|(id, _)| shown.contains(id))))?;
            }
            if a.sondes && !balloons.is_empty() && balloons_drawn.elapsed() >= Duration::from_millis(250) {
                balloons_drawn = Instant::now();
                balloons.prune(unix_now());
                if selected_balloon.as_deref().is_some_and(|s| balloons.get(s, unix_now()).is_none()) {
                    selected_balloon = None;
                    follow = false;
                }
                map.set_data(BALLOONS, &balloons.points(unix_now() - SONDE_DELAY))?;
                map.set_data(STATIONS, &balloons.station_points(unix_now()))?;
                map.set_data(STATION_AREAS, &balloons.station_areas(unix_now()))?;
                let links = match selected_balloon.as_deref() {
                    Some(sn) => balloons.links(sn, unix_now() - SONDE_DELAY, unix_now()),
                    None => serde_json::json!({"type": "FeatureCollection", "features": []}),
                };
                map.set_data(STATION_LINKS, &links)?;
                // The image lines again only when they'd look different: another balloon,
                // other stations, or the balloon a kilometer on.
                let key = links["features"]
                    .as_array()
                    .map(|fs| {
                        fs.iter()
                            .map(|f| {
                                let c = &f["geometry"]["coordinates"][0];
                                format!("{}@{:.2},{:.2}", f["properties"]["callsign"], c[0].as_f64().unwrap_or(0.0), c[1].as_f64().unwrap_or(0.0))
                            })
                            .collect::<Vec<_>>()
                            .join(";")
                    })
                    .unwrap_or_default();
                if key != links_drawn {
                    let mut shape = mapterm::areas::Shape::lines(STATION_LINK_RGB, 2.0, false);
                    for f in links["features"].as_array().into_iter().flatten() {
                        shape.add(&f["geometry"]);
                    }
                    link_tiles.set(vec![shape]);
                    map.put_raster_tile(STATION_LINK_TILES, None, true);
                    links_drawn = key;
                }
                // New or moved stations, kept for next time (at most every 5 minutes).
                if balloons.stations_changed() && stations_saved.elapsed() >= Duration::from_secs(300) {
                    stations_saved = Instant::now();
                    if let Err(e) = balloons.save_stations(&mapterm::sondehub::stations_file()) {
                        dlog(|| format!("sondes: saving stations: {e:#}"));
                    }
                }
                trails_dirty = true;
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
            // --floods: the gauges in a box around the view (half a view more on each side),
            // again when the view leaves it or every 10 minutes; none when zoomed out.
            if a.floods && map_area.width > 0 && !ctl.animating() && !gauges_asking && cam.zoom >= mapterm::floods::MIN_ZOOM {
                let vp = Viewport::new(&cam, map_area);
                let (w, n) = vp.cell_to_lonlat(0, 0);
                let (e, s) = vp.cell_to_lonlat(map_area.width, map_area.height);
                let inside = gauges_area.is_some_and(|(_, b)| w >= b.0 && s >= b.1 && e <= b.2 && n <= b.3);
                let stale = gauges_area.is_some_and(|(at, _)| at.elapsed() >= Duration::from_secs(mapterm::floods::REFRESH));
                if !inside || stale {
                    let (dx, dy) = ((e - w) / 2.0, (n - s) / 2.0);
                    let b = ((w - dx).max(-180.0), (s - dy).max(-90.0), (e + dx).min(180.0), (n + dy).min(90.0));
                    gauges_area = Some((Instant::now(), b));
                    gauges_asking = true;
                    let tx = gatx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(GaugeMsg::Gauges(mapterm::floods::gauges(b).await.map_err(|e| format!("{e:#}"))));
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

            // A session opened in place: the view, then the rest as switcher picks.
            if let Some(sn) = session_open.take() {
                use mapterm::basemaps::{OVERLAYS, Row};
                ctl.fly_to(Camera::new(sn.lon, sn.lat, sn.zoom));
                if let Some(b) = &sn.base
                    && let Some(i) = bases.iter().position(|e| e.base.spec() == b)
                {
                    want_base = Some(i);
                }
                for (i, o) in OVERLAYS.iter().enumerate() {
                    if overlay_on[i] != sn.overlays.iter().any(|x| x == o.preset) && !overlay_loading.contains(&i) {
                        switcher_picks.push(Row::Overlay(i));
                    }
                }
                for &i in &live_running {
                    let key = mapterm::basemaps::LIVE[i].key;
                    if key != "radar" && live_off.contains_key(key) != sn.live_off.iter().any(|k| k == key) {
                        switcher_picks.push(Row::Live(i));
                    }
                }
                vector = sn.vector;
                apply_toggles(&mut map, &base, labels, vector, raster.is_some());
                if switcher.as_ref().is_some_and(|s| s.kind == mapterm::basemaps::Picker::Sessions) {
                    switcher = None;
                }
                note = Some((Instant::now(), format!("session {}", sn.name)));
            }
            // Start again with another session's layers.
            if restart_with.is_some() {
                return Ok(());
            }
            // What was picked in the switcher.
            for row in std::mem::take(&mut switcher_picks) {
                use mapterm::basemaps::{OVERLAYS, Row};
                match row {
                    Row::Session(i) => {
                        if let Some((sn, past)) = session_list.get(i) {
                            if !past {
                                let _ = mapterm::sessions::open(&sn.name, unix_now());
                            }
                            if sn.args == a.kept {
                                session_open = Some(sn.clone());
                            } else {
                                // Other layers: start again with them.
                                restart_with = Some(sn.clone());
                            }
                        }
                    }
                    Row::Base(i) => want_base = Some(i),
                    Row::SaveAs => {}
                    Row::Place(i) => {
                        if let Some(p) = place_list.get(i) {
                            follow = false;
                            ctl.fly_to(Camera::new(p.lon, p.lat, p.zoom));
                            mapterm::places::visit(&mapterm::places::file(), &p.name, unix_now());
                            note = Some((Instant::now(), format!("⚑ {}", p.name)));
                        }
                    }
                    Row::Legends => show_legend = !show_legend,
                    Row::Data(i) => {
                        if let Some((id, _)) = legend.get(i) {
                            match data_off.remove(id) {
                                Some(saved) => {
                                    if let Err(e) = show_live(&mut map, saved) {
                                        note = Some((Instant::now(), format!("{id}: {e:#}")));
                                    }
                                }
                                None => {
                                    let saved = hide_sources(&mut map, &[id.as_str()]);
                                    data_off.insert(id.clone(), saved);
                                }
                            }
                        }
                    }
                    Row::Labels => labels = !labels,
                    Row::Vector => vector = (vector + 1) % 3,
                    // A live layer not running: start it, as its option would have at launch.
                    Row::Live(i) if !live_running.contains(&i) => {
                        let key = mapterm::basemaps::LIVE[i].key;
                        let started = (|| -> Result<()> {
                            match key {
                                "aircraft" => {
                                    let mut feed = mapterm::adsb::Feed::named("lol")?;
                                    feed.key = mapterm::raster::find_key(a.adsb_key_cmd.as_deref(), &["ADSB_LOL_KEY"])?;
                                    adsb_every = feed.every();
                                    adsb_delay = adsb_every.as_secs_f64() + 1.0;
                                    let before = under(&map, &[SATS]);
                                    add_aircraft(&mut map, &feed, before.as_deref())?;
                                    if mapterm::kitty::supported() {
                                        trails_kitty = true;
                                        let _ = map.set_visibility(TRAILS, false);
                                    }
                                    adsb_feed = Some(feed);
                                }
                                "towers" => {
                                    a.towers = true;
                                    let before = under(&map, MOVING);
                                    add_points(&mut map, TOWERS, mapterm::towers::CREDIT, mapterm::towers::layer(TOWERS, TOWERS), before.as_deref())?;
                                    load_towers(towertx.clone());
                                }
                                "nasa" => {
                                    a.nasa = Some(false);
                                    let before = under(&map, &[TRAILS, AIRCRAFT, SATS]);
                                    add_nasa(&mut map, before.as_deref())?;
                                    spawn_nasa(nasatx.clone(), false);
                                }
                                "balloons" => {
                                    a.sondes = true;
                                    let before = under(&map, &[TRAILS, AIRCRAFT, SATS]);
                                    add_balloons(&mut map, &link_tiles, before.as_deref())?;
                                    load_sites(sitetx.clone());
                                    balloons.load_stations(&mapterm::sondehub::stations_file(), unix_now());
                                    if balloons.stations_changed() {
                                        let _ = balloons.save_stations(&mapterm::sondehub::stations_file());
                                    }
                                    map.set_data(STATIONS, &balloons.station_points(unix_now()))?;
                                    map.set_data(STATION_AREAS, &balloons.station_areas(unix_now()))?;
                                    tokio::spawn(mapterm::sondehub::stream(sonde_area_rx.clone(), sonde_sel_rx.clone(), btx.clone()));
                                    trails_kitty |= mapterm::kitty::supported();
                                }
                                "alerts" => {
                                    a.alerts = true;
                                    let before = under(&map, OVER_AREAS);
                                    add_alerts(&mut map, before.as_deref())?;
                                    if let Some(rx) = nzreq_rx.take() {
                                        spawn_alerts(natx.clone(), rx, nztx.clone());
                                    }
                                }
                                "quakes" => {
                                    let period = mapterm::quakes::Period::default();
                                    a.quakes = Some(period);
                                    let before = under(&map, MOVING);
                                    add_points(&mut map, QUAKES, mapterm::quakes::CREDIT, mapterm::quakes::layer(QUAKES, QUAKES), before.as_deref())?;
                                    spawn_quakes(qtx.clone(), period);
                                }
                                "fires" => {
                                    a.fires = true;
                                    let below = under(&map, OVER_AREAS);
                                    add_fire_areas(&mut map, below.as_deref())?;
                                    let before = under(&map, MOVING);
                                    add_points(&mut map, FIRES, mapterm::fires::CREDIT, mapterm::fires::layer(FIRES, FIRES), before.as_deref())?;
                                    spawn_fires(ftx.clone());
                                }
                                "gauges" => {
                                    a.floods = true;
                                    let before = under(&map, MOVING);
                                    add_points(&mut map, GAUGES, mapterm::floods::CREDIT, mapterm::floods::layer(GAUGES, GAUGES), before.as_deref())?;
                                }
                                "tfrs" => {
                                    a.tfrs = true;
                                    let before = under(&map, OVER_AREAS);
                                    add_tfrs(&mut map, before.as_deref())?;
                                    spawn_tfrs(ttx.clone());
                                }
                                "closures" => {
                                    let token = mapterm::raster::find_key(a.vdot_key_cmd.as_deref(), &["VDOT_SMARTERROADS_TOKEN"])?;
                                    a.closures = true;
                                    let before = under(&map, MOVING);
                                    add_closures(&mut map, &closure_tiles, before.as_deref(), before.as_deref())?;
                                    spawn_closures(cltx.clone(), token);
                                }
                                "traffic" => {
                                    a.traffic = true;
                                    add_traffic(&mut map)?;
                                    spawn_traffic(traffictx.clone());
                                }
                                "radar" => {
                                    a.radar = true;
                                    radar = Some(mapterm::timelayer::TimeRaster::new("radar", 0.8));
                                    spawn_radar(rvtx.clone());
                                }
                                "satellites" => {
                                    let groups = vec!["active".to_string()];
                                    a.sats = Some(groups.clone());
                                    let before = under(&map, &[]);
                                    add_points(&mut map, SATS, "CelesTrak", mapterm::satellites::layer(SATS, SATS), before.as_deref())?;
                                    spawn_sats(groups, otx.clone(), mtx.clone(), rtx.clone());
                                    trails_kitty |= mapterm::kitty::supported();
                                }
                                "ships" => {
                                    let key = mapterm::raster::find_key(a.ais_key_cmd.as_deref(), &["AISSTREAM_API_KEY"])?.context(
                                        "ships need a free aisstream.io API key: sign up at aisstream.io, then set AISSTREAM_API_KEY or start with --ais-key-cmd",
                                    )?;
                                    a.ais = true;
                                    let before = under(&map, &[BALLOONS, TRAILS, AIRCRAFT, SATS]);
                                    add_points(&mut map, SHIPS, "aisstream.io", mapterm::ais::layer(SHIPS, SHIPS), before.as_deref())?;
                                    tokio::spawn(mapterm::ais::stream(key.clone(), area_rx.clone(), stx_ais.clone()));
                                    ais_key = Some(key);
                                    trails_kitty |= mapterm::kitty::supported();
                                }
                                "outages" => {
                                    a.outages = true;
                                    let before = under(&map, OVER_AREAS);
                                    add_outages(&mut map, before.as_deref())?;
                                    spawn_outages(outx.clone());
                                }
                                _ => bail!("{key} can't be started yet: start mapterm with it"),
                            }
                            Ok(())
                        })();
                        match started {
                            Ok(()) => {
                                live_running.push(i);
                                // Sessions saved from here start it too.
                                if let Some((_, flag)) = STARTABLE.iter().find(|(k, _)| *k == key)
                                    && !a.kept.iter().any(|x| x == flag)
                                {
                                    a.kept.push(flag.to_string());
                                }
                                // Area tiles and trails show as images where the terminal can.
                                if !kitty && mapterm::kitty::supported() {
                                    kitty = true;
                                    map.raster_mode = RasterMode::Kitty { cell_px: cpx };
                                    kstate.shm = shm;
                                }
                                note = Some((Instant::now(), format!("started {}", mapterm::basemaps::LIVE[i].label)));
                            }
                            Err(e) => note = Some((Instant::now(), format!("{}: {e:#}", mapterm::basemaps::LIVE[i].label))),
                        }
                    }
                    Row::Live(i) if mapterm::basemaps::LIVE[i].key == "radar" => {
                        if let Some(r) = radar.as_mut() {
                            let off = !r.is_off();
                            r.set_off(&mut map, off);
                            if off {
                                clock.go_live(unix_now());
                            }
                        }
                    }
                    Row::Live(i) => {
                        let key = mapterm::basemaps::LIVE[i].key;
                        match live_off.remove(key) {
                            Some(saved) => {
                                if let Err(e) = show_live(&mut map, saved) {
                                    note = Some((Instant::now(), format!("{key}: {e:#}")));
                                }
                            }
                            None => {
                                live_off.insert(key, hide_live(&mut map, key));
                                free_images.extend(live_images(key));
                                // Nothing hidden stays chosen.
                                match key {
                                    "aircraft" => selected = None,
                                    "ships" => selected_ship = None,
                                    "satellites" => selected_sat = None,
                                    "balloons" => selected_balloon = None,
                                    "alerts" => selected_alert = None,
                                    "quakes" => selected_quake = None,
                                    "fires" => selected_fire = None,
                                    "gauges" => selected_gauge = None,
                                    "outages" => selected_outage = None,
                                    "tfrs" => selected_tfr = None,
                                    "closures" => selected_closure = None,
                                    _ => {}
                                }
                            }
                        }
                        trails_dirty = true;
                        sat_dots_drawn = Instant::now() - Duration::from_secs(10);
                    }
                    Row::Overlay(i) if overlay_on[i] => {
                        let id = overlay_id(i);
                        map.remove_layers(&[id.as_str()]);
                        map.remove_source(&id);
                        overlay_on[i] = false;
                    }
                    Row::Overlay(i) => {
                        if overlay_loading.insert(i) {
                            let (ovtx, cache, o) = (ovtx.clone(), a.raster_cache, OVERLAYS[i]);
                            note = Some((Instant::now(), format!("loading {}…", o.label)));
                            tokio::spawn(async move {
                                let r = open_preset(o.preset, None, cache).await.map(|mut r| {
                                    r.overlay = true;
                                    r.opacity = o.opacity;
                                    r.shade = o.shade;
                                    Arc::new(r)
                                });
                                let _ = ovtx.send((i, r));
                            });
                        }
                    }
                }
                base = base_visibility(&map);
                apply_toggles(&mut map, &base, labels, vector, raster.is_some());
            }
            // Switch base maps, one at a time: a style swaps the vector map (and takes away any
            // imagery); imagery goes under the style's roads and names.
            if let Some(i) = want_base.filter(|_| style_loading.is_none() && imagery_loading.is_none()) {
                want_base = None;
                if i != base_at && i < bases.len() {
                    base_prev = Some(base_at);
                    base_at = i;
                    match bases[i].base.clone() {
                        mapterm::basemaps::Base::Style(spec) => {
                            vector = 0;
                            if raster.is_some() {
                                imagery_loading = Some(String::new());
                                let _ = itx.send((String::new(), Ok(Vec::new())));
                            }
                            if styles[style_at] != spec {
                                style_at = styles.iter().position(|s| *s == spec).unwrap_or(style_at);
                                let (source, dark, cam, stx) = (builtin_source.clone(), theme.map_or(!a.light, |t| t.1), ctl.cam, stx.clone());
                                let sp = spec.clone();
                                tokio::spawn(async move {
                                    let r = load_basemap(&sp, &source, dark, cam).await;
                                    let _ = stx.send((sp, r));
                                });
                                note = Some((Instant::now(), format!("loading style {spec}…")));
                                style_loading = Some(spec);
                            } else {
                                apply_toggles(&mut map, &base, labels, vector, raster.is_some());
                            }
                        }
                        mapterm::basemaps::Base::Imagery(spec) => {
                            imagery_loading = Some(spec.clone());
                            match imagery_kept.get(&spec) {
                                Some(l) => {
                                    let _ = itx.send((spec, Ok(l.clone())));
                                }
                                None => {
                                    note = Some((Instant::now(), format!("loading imagery {spec}…")));
                                    let (itx, cache, cam) = (itx.clone(), a.raster_cache, ctl.cam);
                                    tokio::spawn(async move {
                                        let r = open_imagery(spec.clone(), cache, cam).await;
                                        let _ = itx.send((spec, r));
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // ~60 fps while animating, otherwise wake only for input, tiles, or a slow tick.
            let idle = if ctl.animating() || pixels_pending > 0 || !demo.is_empty() || clock.is_playing() || stitching {
                Duration::from_millis(16)
            } else if sat_dots {
                // Satellite dots redraw up to 15 times a second (see sat_every).
                (sat_every / 2).max(Duration::from_millis(16))
            } else if !aircraft.is_empty() || !fleet.is_empty() || !sats.is_empty() || !balloons.is_empty() {
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
                        // A base map switched away while it loaded: nowhere to put it.
                        if map.source(&source).is_none() {
                            continue;
                        }
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
                Some(r) = narx.recv() => {
                    match r {
                        Ok(list) => {
                            status.entry("alerts").or_default().ok(unix_now(), Some(list.len()));
                            dlog(|| format!("alerts: {} active", list.len()));
                            alerts = list;
                            let _ = nzreq.send(mapterm::nws::zones_needed(&alerts, &alert_zones));
                            alerts_list_new = true;
                            if selected_alert.as_ref().is_some_and(|id| !alerts.iter().any(|a| &a.id == id)) {
                                selected_alert = None;
                            }
                        }
                        Err(e) => {
                            status.entry("alerts").or_default().fail(unix_now(), format!("{e:#}"));
                            dlog(|| format!("alerts: {e:#}"));
                            note = Some((Instant::now(), format!("alerts: {e:#}")));
                        }
                    }
                }
                Some(m) = garx.recv() => {
                    match m {
                        GaugeMsg::Gauges(Ok(g)) => {
                            status.entry("gauges").or_default().ok(unix_now(), Some(g.len()));
                            dlog(|| format!("floods: {} gauges", g.len()));
                            gauges = g;
                            gauges_new = true;
                            gauges_asking = false;
                        }
                        GaugeMsg::Gauges(Err(e)) => {
                            status.entry("gauges").or_default().fail(unix_now(), e.to_string());
                            dlog(|| format!("floods: {e}"));
                            note = Some((Instant::now(), format!("gauges: {e}")));
                            gauges_asking = false;
                        }
                        GaugeMsg::Detail(lid, Ok(d)) => {
                            gauge_details.insert(lid, Some(d));
                        }
                        GaugeMsg::Detail(lid, Err(e)) => {
                            dlog(|| format!("floods: {lid}: {e}"));
                            gauge_details.remove(&lid);
                            note = Some((Instant::now(), format!("gauge {lid}: {e}")));
                        }
                    }
                }
                Some(r) = trafficrx.recv() => {
                    match r {
                        Ok(s) => {
                            status.entry("traffic").or_default().ok(unix_now(), Some(s.len()));
                            dlog(|| format!("traffic: {} sensors", s.len()));
                            sensors = s;
                            sensors_new = true;
                        }
                        Err(e) => {
                            status.entry("traffic").or_default().fail(unix_now(), format!("{e:#}"));
                            dlog(|| format!("traffic: {e:#}"));
                            note = Some((Instant::now(), format!("traffic: {e:#}")));
                        }
                    }
                }
                Some(r) = routerx.recv() => {
                    route_asking = false;
                    match r {
                        Ok((all, f, t)) if !all.is_empty() => {
                            for r in &all {
                                dlog(|| format!("route: {:.1} mi, {:.0} s, {} steps, avoiding {}", r.miles, r.seconds, r.steps.len(), r.avoided));
                            }
                            // Fit the map to all of them.
                            let r = mapterm::route::Route { line: all.iter().flat_map(|r| r.line.iter().copied()).collect(), ..Default::default() };
                            // Fit the map to it.
                            if let Some(b) = r.bounds() {
                                let p = mapterm::geocode::Place {
                                    name: String::new(),
                                    context: String::new(),
                                    kind: "route".into(),
                                    lon: (b.0 + b.2) / 2.0,
                                    lat: (b.1 + b.3) / 2.0,
                                    bounds: Some(b),
                                };
                                let (lon, lat, z) = p.view(map_area.width, map_area.height);
                                ctl.fly_to(Camera::new(lon, lat, z));
                            }
                            route_pick = 0;
                            route = Some(all[0].clone());
                            routes = all;
                            route_ends = Some((f, t));
                            route_new = true;
                            route_panel = true;
                        }
                        Ok(_) => note = Some((Instant::now(), "route: none found".into())),
                        Err(e) => {
                            dlog(|| format!("route: {e}"));
                            note = Some((Instant::now(), format!("route: {e}")));
                        }
                    }
                }
                Some((i, r)) = clrx.recv() => {
                    let feed = &mapterm::closures::FEEDS[i];
                    match r {
                        Ok(c) => {
                            dlog(|| format!("closures: {}: {}", feed.id, c.len()));
                            closures.insert(i, c);
                            closure_errors.remove(&i);
                            status.entry("closures").or_default().ok(unix_now(), Some(closures.values().map(Vec::len).sum()));
                            closures_new = true;
                        }
                        Err(e) => {
                            dlog(|| format!("closures: {}: {e}", feed.id));
                            status.entry("closures").or_default().fail(unix_now(), format!("{}: {e}", feed.source));
                            // Once per feed until it works again, not every 5 minutes.
                            if closure_errors.insert(i) {
                                note = Some((Instant::now(), format!("closures: {}: {e}", feed.source)));
                            }
                        }
                    }
                }
                Some(m) = trx.recv() => {
                    match m {
                        TfrMsg::Live(t) => {
                            dlog(|| format!("tfrs: {}", t.len()));
                            tfr_live = t;
                            tfrs_new = true;
                            status.entry("tfrs").or_default().ok(unix_now(), Some(tfr_live.len() + tfr_standing.len()));
                        }
                        TfrMsg::Standing(s) => {
                            dlog(|| format!("tfrs: {} standing", s.len()));
                            tfr_standing = s;
                            tfrs_new = true;
                        }
                        TfrMsg::Notam(n, Ok(d)) => {
                            tfr_notams.insert(n, Some(d));
                        }
                        TfrMsg::Notam(n, Err(e)) => {
                            tfr_notams.remove(&n);
                            note = Some((Instant::now(), format!("NOTAM {n}: {e}")));
                        }
                        TfrMsg::Error(e) => {
                            status.entry("tfrs").or_default().fail(unix_now(), e.to_string());
                            dlog(|| format!("tfrs: {e}"));
                            note = Some((Instant::now(), format!("flight restrictions: {e}")));
                        }
                    }
                }
                Some(m) = frx.recv() => {
                    match m {
                        FireMsg::Incidents(f) => {
                            dlog(|| format!("fires: {} incidents", f.len()));
                            fires = f;
                            status.entry("fires").or_default().ok(unix_now(), Some(fires.len()));
                            if selected_fire.as_ref().is_some_and(|id| !fires.iter().any(|f| &f.id == id)) {
                                selected_fire = None;
                            }
                        }
                        FireMsg::Perimeters(p) => {
                            dlog(|| format!("fires: {} perimeters", p.len()));
                            fire_perimeters = p;
                        }
                        FireMsg::Error(e) => {
                            status.entry("fires").or_default().fail(unix_now(), e.to_string());
                            dlog(|| format!("fires: {e}"));
                            note = Some((Instant::now(), format!("fires: {e}")));
                        }
                    }
                    fires_new = true;
                }
                Some(r) = qrx.recv() => {
                    match r {
                        Ok(q) => {
                            status.entry("quakes").or_default().ok(unix_now(), Some(q.len()));
                            dlog(|| format!("quakes: {}", q.len()));
                            quakes = q;
                            quakes_new = true;
                            if selected_quake.as_ref().is_some_and(|id| !quakes.iter().any(|q| &q.id == id)) {
                                selected_quake = None;
                            }
                        }
                        Err(e) => {
                            status.entry("quakes").or_default().fail(unix_now(), format!("{e:#}"));
                            dlog(|| format!("quakes: {e:#}"));
                            note = Some((Instant::now(), format!("quakes: {e:#}")));
                        }
                    }
                }
                Some(m) = outrx.recv() => {
                    match m {
                        OutageMsg::Outlines(f, level, o) => outages.set_outlines(f, level, o),
                        OutageMsg::Counts(f, level, v) => {
                            status.entry("outages").or_default().ok(unix_now(), None);
                            dlog(|| format!("outages: {} {:?}: {} areas, {} out", mapterm::outages::FEEDS[f].state, level, v.len(), v.iter().map(|c| c.out).sum::<u64>()));
                            outages.set_counts(f, level, v);
                        }
                        OutageMsg::Odin(v, stale) => {
                            dlog(|| format!("outages: ODIN: {} counties, {} out, {stale} utilities not reporting", v.len(), v.iter().map(|c| c.out).sum::<u64>()));
                            outages.set_odin(v, stale);
                        }
                        OutageMsg::OdinOutlines(o) => outages.add_odin_outlines(o),
                        OutageMsg::Error(e) => {
                            status.entry("outages").or_default().fail(unix_now(), e.to_string());
                            dlog(|| format!("outages: {e}"));
                            note = Some((Instant::now(), format!("outages: {e}")));
                        }
                    }
                    outages_new = true;
                }
                Some((z, g)) = nzrx.recv() => {
                    alert_zones.insert(z, g);
                    alerts_zones_new = true;
                    alerts_zone_at = Instant::now();
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
                            status.entry("radar").or_default().ok(unix_now(), Some(frames.len()));
                            dlog(|| format!("radar: {} scans, the latest {}", frames.len(), scans.past.last().map_or(0, |s| s.time)));
                            if let Some(r) = radar.as_mut() {
                                // Under the alert areas, where they're tiles.
                                let before = map.source(ALERT_TILES).is_some().then_some(ALERT_TILES);
                                r.set_frames(&mut map, frames, mapterm::rainviewer::SCAN_SECS, before)?;
                            }
                        }
                        Ok(_) => status.entry("radar").or_default().heard(unix_now(), None),
                        Err(e) => {
                            status.entry("radar").or_default().fail(unix_now(), format!("{e:#}"));
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
                        status.entry("satellites").or_default().ok(unix_now(), Some(loaded.len()));
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
                Some(r) = siterx.recv() => {
                    match r {
                        Ok(v) => {
                            dlog(|| format!("launch sites: {}", v.len()));
                            map.set_data(LAUNCH_SITES, &mapterm::upperair::points(&v))?;
                            launch_sites = v;
                        }
                        Err(e) => note = Some((Instant::now(), format!("launch sites: {e:#}"))),
                    }
                }
                Some(r) = towerrx.recv() => {
                    match r {
                        Ok(v) => {
                            status.entry("towers").or_default().ok(unix_now(), Some(v.len()));
                            dlog(|| format!("towers: {}", v.len()));
                            towers = v;
                            towers_drawn = None;
                        }
                        Err(e) => {
                            status.entry("towers").or_default().fail(unix_now(), format!("{e:#}"));
                            note = Some((Instant::now(), format!("towers: {e:#}")));
                        }
                    }
                }
                Some(m) = nasarx.recv() => {
                    match m {
                        NasaMsg::Flights(f) => {
                            let flying = f.iter().filter(|f| unix_now() - f.t <= mapterm::csbf::RECENT).count();
                            status.entry("nasa").or_default().ok(unix_now(), Some(flying));
                            dlog(|| format!("nasa: {} flights, {flying} recent", f.len()));
                            nasa_flights = f;
                        }
                        NasaMsg::Track(id, pts) => {
                            nasa_tracks.insert(id, pts);
                        }
                        NasaMsg::Error(e) => {
                            dlog(|| format!("nasa: {e}"));
                            status.entry("nasa").or_default().fail(unix_now(), e);
                        }
                    }
                    nasa_new = true;
                }
                Some(ev) = brx.recv() => {
                    match ev {
                        mapterm::sondehub::Event::Snapshot(list) => {
                            dlog(|| format!("sondes: {} near", list.len()));
                            let now = unix_now();
                            for t in list {
                                balloons.apply(t, now);
                            }
                            status.entry("balloons").or_default().heard(now, Some(balloons.len()));
                        }
                        mapterm::sondehub::Event::Update(t) => {
                            balloons.apply(*t, unix_now());
                            status.entry("balloons").or_default().heard(unix_now(), Some(balloons.len()));
                        }
                        mapterm::sondehub::Event::Prediction(p) => balloons.set_prediction(p),
                        mapterm::sondehub::Event::Following(n) => {
                            dlog(|| format!("sondes: following {n}"));
                            sonde_state.clear();
                        }
                        mapterm::sondehub::Event::Retrying(e, wait) => {
                            dlog(|| format!("sondes: {e}; again in {} s", wait.as_secs()));
                            sonde_state = format!(" · reconnecting in {} s", wait.as_secs());
                            status.entry("balloons").or_default().fail(unix_now(), e.to_string());
                            note = Some((Instant::now(), format!("SondeHub: {e}")));
                        }
                    }
                }
                Some(ev) = srx_ais.recv() => {
                    match ev {
                        mapterm::ais::Event::Update(u) => {
                            fleet.apply(u, unix_now());
                            status.entry("ships").or_default().heard(unix_now(), Some(fleet.len()));
                        }
                        mapterm::ais::Event::Connected(b) => {
                            ais_state.clear();
                            fleet.prune(unix_now(), Some(b));
                            dlog(|| format!("ais: streaming {b:?}"));
                        }
                        mapterm::ais::Event::Retrying(e, wait) => {
                            dlog(|| format!("ais: {e}; again in {} s", wait.as_secs()));
                            ais_state = format!(" · reconnecting in {} s", wait.as_secs());
                            status.entry("ships").or_default().fail(unix_now(), e.to_string());
                            note = Some((Instant::now(), format!("aisstream.io: {e}")));
                        }
                        mapterm::ais::Event::Stopped(e) => {
                            dlog(|| format!("ais: stopped: {e}"));
                            ais_state = " · stopped".into();
                            status.entry("ships").or_default().fail(unix_now(), e.clone());
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
                            status.entry("aircraft").or_default().ok(unix_now(), Some(aircraft.len()));
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
                            status.entry("aircraft").or_default().fail(unix_now(), format!("{e:#} (again in {} s)", wait.as_secs()));
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
                        let _ = map.set_attribution(&raster_id, &text);
                    }
                }
                Some((i, r)) = ovrx.recv() => {
                    overlay_loading.remove(&i);
                    let id = overlay_id(i);
                    let added = r.and_then(|src| {
                        map.add_source(&id, MapSource::raster(src))?;
                        // Over the imagery and the overlays before it, under everything else.
                        let before = map.layer_ids().into_iter().find(|l| !is_imagery(l) && !l.starts_with(OVERLAY_PREFIX)).map(str::to_string);
                        map.add_layers(vec![StyleLayer::raster(&id, &id)], before.as_deref())
                    });
                    match added {
                        Ok(()) => {
                            overlay_on[i] = true;
                            if !kitty && mapterm::kitty::supported() {
                                kitty = true;
                                map.raster_mode = RasterMode::Kitty { cell_px: cpx };
                                kstate.shm = shm;
                            }
                            base = base_visibility(&map);
                            apply_toggles(&mut map, &base, labels, vector, raster.is_some());
                        }
                        Err(e) => {
                            map.remove_source(&id);
                            note = Some((Instant::now(), format!("{} failed: {e:#}", mapterm::basemaps::OVERLAYS[i].label)));
                        }
                    }
                }
                Some((spec, r)) = irx.recv() => {
                    imagery_loading = None;
                    let r = r.and_then(|layers| set_imagery(&mut map, &layers).map(|_| layers));
                    match r {
                        Ok(layers) => {
                            if !spec.is_empty() {
                                imagery_kept.insert(spec.clone(), layers.clone());
                                note = Some((Instant::now(), format!("imagery: {spec}")));
                            }
                            raster = layers.last().map(|l| l.1.clone());
                            raster_id = layers.last().map_or(RASTER.to_string(), |l| l.0.clone());
                            google = raster.as_ref().filter(|r| r.is_google()).cloned();
                            credit_view = None;
                            billed_raster = raster.as_ref().is_some_and(|r| r.is_google() || r.wants_key());
                            limiter = imagery_limiter(a.raster_limit, billed_raster);
                            // Imagery shows as images where the terminal can.
                            if raster.is_some() && !kitty && mapterm::kitty::supported() {
                                kitty = true;
                                map.raster_mode = RasterMode::Kitty { cell_px: cpx };
                                kstate.shm = shm;
                            }
                            base = base_visibility(&map);
                            apply_toggles(&mut map, &base, labels, vector, raster.is_some());
                        }
                        Err(e) => {
                            note = Some((Instant::now(), format!("imagery {spec} failed: {e:#}")));
                            // Back to what was showing.
                            if let Some(p) = base_prev.take() {
                                base_at = p;
                            }
                        }
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
                        // What's on the map first, then the geocoder's; the map's alone if the
                        // geocoder failed.
                        let local = std::mem::take(&mut search_local);
                        let r = match r {
                            Ok(places) => Ok(local.into_iter().chain(places).collect::<Vec<_>>()),
                            Err(_) if !local.is_empty() => Ok(local),
                            Err(e) => Err(e),
                        };
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
                                // B's name prompt (saving the spot as a place) takes every key.
                                if let Some(name) = place_naming.as_mut() {
                                    match k.code {
                                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                                        KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => name.clear(),
                                        KeyCode::Esc => place_naming = None,
                                        KeyCode::Enter => {
                                            let (lon, lat, zoom) = place_spot;
                                            match mapterm::places::add(&mapterm::places::file(), name, lat, lon, zoom, unix_now()) {
                                                Ok(_) => {
                                                    note = Some((Instant::now(), format!("saved place {} · B finds it", name.trim())));
                                                    place_naming = None;
                                                }
                                                Err(e) => note = Some((Instant::now(), format!("{e:#}"))),
                                            }
                                        }
                                        KeyCode::Char(c) => name.push(c),
                                        KeyCode::Backspace => {
                                            name.pop();
                                        }
                                        _ => {}
                                    }
                                    continue;
                                }
                                // S's name prompt takes every key while it's open.
                                if let Some(name) = naming.as_mut() {
                                    match k.code {
                                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                                        KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => name.clear(),
                                        KeyCode::Esc => naming = None,
                                        KeyCode::Enter => {
                                            let base_spec = bases.get(base_at).map(|e| e.base.spec().to_string());
                                            let sn = session_now(name.trim(), &a.kept, &ctl.cam, base_spec, &overlay_on, &live_off, vector, unix_now());
                                            match sn.save() {
                                                Ok(_) => {
                                                    note = Some((Instant::now(), format!("saved session {} · mapterm --session {:?}", sn.name, sn.name)));
                                                    naming = None;
                                                }
                                                Err(e) => note = Some((Instant::now(), format!("{e:#}"))),
                                            }
                                        }
                                        KeyCode::Char(c) => name.push(c),
                                        KeyCode::Backspace => {
                                            name.pop();
                                        }
                                        _ => {}
                                    }
                                    continue;
                                }
                                // The base map switcher takes every key while it's open, as fzf:
                                // typing filters, up/down (ctrl-n/p, ctrl-j/k) choose and preview,
                                // enter keeps, space toggles or picks and stays, esc goes back.
                                if let Some(sw) = switcher.as_mut() {
                                    use mapterm::basemaps::Row;
                                    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                                    let mut refilter = false;
                                    let mut close = false;
                                    match k.code {
                                        KeyCode::Char('c') if ctrl => return Ok(()),
                                        KeyCode::Char('u') if ctrl => {
                                            sw.query.clear();
                                            refilter = true;
                                        }
                                        KeyCode::Char('n' | 'j') if ctrl => sw.step(true),
                                        KeyCode::Char('p' | 'k') if ctrl => sw.step(false),
                                        KeyCode::Esc => close = true,
                                        KeyCode::Enter if sw.row() == Some(Row::SaveAs) && sw.kind == mapterm::basemaps::Picker::Places => {
                                            // Save the spot under what's typed, or ask for a name.
                                            let typed = sw.query.trim().to_string();
                                            if typed.is_empty() {
                                                place_naming = Some(String::new());
                                            } else {
                                                let (lon, lat, zoom) = place_spot;
                                                note = Some((Instant::now(), match mapterm::places::add(&mapterm::places::file(), &typed, lat, lon, zoom, unix_now()) {
                                                    Ok(_) => format!("saved place {typed} · B finds it"),
                                                    Err(e) => format!("{e:#}"),
                                                }));
                                            }
                                            close = true;
                                        }
                                        // ctrl-d twice forgets a place (there's no undo).
                                        KeyCode::Char('d') if ctrl && sw.kind == mapterm::basemaps::Picker::Places => {
                                            if let Some(Row::Place(i)) = sw.row()
                                                && let Some(p) = place_list.get(i)
                                            {
                                                if place_forget.as_deref() == Some(p.name.as_str()) {
                                                    let name = p.name.clone();
                                                    match mapterm::places::remove(&mapterm::places::file(), &name) {
                                                        Ok(v) => {
                                                            place_list = v;
                                                            sw.sessions = place_rows(&place_list);
                                                            sw.total = place_list.len();
                                                            refilter = true;
                                                            note = Some((Instant::now(), format!("forgot {name}")));
                                                        }
                                                        Err(e) => note = Some((Instant::now(), format!("{e:#}"))),
                                                    }
                                                    place_forget = None;
                                                } else {
                                                    note = Some((Instant::now(), format!("ctrl-d again forgets {}", p.name)));
                                                    place_forget = Some(p.name.clone());
                                                }
                                            }
                                        }
                                        KeyCode::Enter if sw.row() == Some(Row::SaveAs) => {
                                            // Save under what's typed, or ask for a name.
                                            let typed = sw.query.trim().to_string();
                                            let base_spec = bases.get(base_at).map(|e| e.base.spec().to_string());
                                            if mapterm::sessions::valid_name(&typed) {
                                                let sn = session_now(&typed, &a.kept, &ctl.cam, base_spec, &overlay_on, &live_off, vector, unix_now());
                                                note = Some((Instant::now(), match sn.save() {
                                                    Ok(_) => format!("saved session {} · mapterm --session {:?}", sn.name, sn.name),
                                                    Err(e) => format!("{e:#}"),
                                                }));
                                            } else {
                                                naming = Some(a.session.as_ref().map(|s| s.name.clone()).filter(|n| n != "run" && typed.is_empty()).unwrap_or(typed));
                                            }
                                            close = true;
                                        }
                                        KeyCode::Enter => {
                                            let row = sw.row();
                                            if matches!(row, Some(Row::Base(_) | Row::Session(_) | Row::Place(_))) {
                                                close = true;
                                            }
                                            switcher_picks.extend(row);
                                        }
                                        // Space picks or toggles; once something is typed it's part of
                                        // the query (as in fzf), so "power out" finds power outages.
                                        KeyCode::Char(' ') if sw.query.is_empty() => switcher_picks.extend(sw.row()),
                                        KeyCode::Down | KeyCode::Tab => sw.step(true),
                                        KeyCode::Up | KeyCode::BackTab => sw.step(false),
                                        KeyCode::Char(c) if !ctrl => {
                                            sw.query.push(c);
                                            refilter = true;
                                        }
                                        KeyCode::Backspace => {
                                            sw.query.pop();
                                            refilter = true;
                                        }
                                        _ => {}
                                    }
                                    if refilter {
                                        sw.refilter(&bases);
                                    }
                                    if close {
                                        switcher = None;
                                    }
                                    continue;
                                }
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
                                            // Addresses, streets and names already on the map, with the geocoder's.
                                            let feats: Vec<&mapterm::geojson::Feature> = legend.iter().flat_map(|(id, _)| map.data_features(id)).collect();
                                            // Saved places first.
                                            search_local = mapterm::places::search(&mapterm::places::load(), &query, 3);
                                            search_local.extend(mapterm::geocode::search_features(feats, &query, 8));
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
                                if show_help && matches!(k.code, KeyCode::Esc | KeyCode::Char('?')) {
                                    show_help = false;
                                    // The images it hid come back on the next frames.
                                    trails_dirty = true;
                                    sats_drawn = Instant::now() - Duration::from_secs(10);
                                    continue;
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
                                            clock.play(start, stop, mapterm::clock::Range::Loop, SPEEDS[speed_i]);
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
                                    KeyCode::Char(c @ ('<' | '>')) if radar.as_ref().is_some_and(|r| !r.frames().is_empty()) => {
                                        // Play backward or forward (Cesium's reverse button).
                                        let back = c == '<';
                                        if let Some((start, stop)) = radar.as_ref().and_then(|r| r.span()) {
                                            let speed = SPEEDS[speed_i] * if back { -1.0 } else { 1.0 };
                                            if clock.is_live() {
                                                clock.seek(stop);
                                            }
                                            clock.play(start, stop, mapterm::clock::Range::Loop, speed);
                                            note = Some((Instant::now(), format!("radar playing {} · space pauses", if back { "backward" } else { "forward" })));
                                        }
                                    }
                                    KeyCode::Char(c @ ('[' | ']')) if radar.is_some() => {
                                        // Slower or faster (Cesium's shuttle ring).
                                        speed_i = if c == '[' { speed_i.saturating_sub(1) } else { (speed_i + 1).min(SPEEDS.len() - 1) };
                                        clock.set_speed(SPEEDS[speed_i]);
                                        note = Some((Instant::now(), format!("speed {}× (two hours in {:.0} s)", SPEEDS[speed_i], 7200.0 / SPEEDS[speed_i])));
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
                                    KeyCode::Char('?') => show_help = true,
                                    // K: the legends on or off (also a row in L).
                                    KeyCode::Char('K') => {
                                        show_legend = !show_legend;
                                        note = Some((Instant::now(), if show_legend { "legends on · K hides them" } else { "legends off · K shows them" }.into()));
                                    }
                                    // D: the data sources, how each is doing.
                                    KeyCode::Char('D') => {
                                        finder = None;
                                        let data: Vec<String> = legend.iter().map(|(id, _)| id.clone()).collect();
                                        switcher = Some(mapterm::basemaps::Switcher::sources(&live_running, data));
                                    }
                                    // L: what's over the base map, on and off.
                                    KeyCode::Char('L') => {
                                        finder = None;
                                        let data: Vec<String> = legend.iter().map(|(id, _)| id.clone()).collect();
                                        // The live layers running, and those that can be started.
                                        let listed: Vec<usize> = (0..mapterm::basemaps::LIVE.len())
                                            .filter(|i| live_running.contains(i) || STARTABLE.iter().any(|(k, _)| *k == mapterm::basemaps::LIVE[*i].key))
                                            .collect();
                                        switcher = Some(mapterm::basemaps::Switcher::layers(&listed, data));
                                    }
                                    KeyCode::Char('p') if !sats.is_empty() || !aircraft.is_empty() || !fleet.is_empty() => {
                                        finder = Some(Finder { query: String::new(), sel: 0, hits: find("", &sats, &sats_hidden, &sat_filter, &aircraft, &fleet) });
                                    }
                                    KeyCode::Char('/') => {
                                        search = Search::Typing(String::new());
                                    }
                                    // Directions to the place found with /, from your location (else
                                    // the map's center).
                                    KeyCode::Char('d') if found.is_some() && !route_asking => {
                                        let to = found.as_ref().map(|p| (p.lon, p.lat)).expect("found");
                                        let from = me.as_ref().map_or((cam.lon, cam.lat), |m| (m.lon, m.lat));
                                        let all: Vec<mapterm::closures::Closure> = closures.values().flatten().cloned().collect();
                                        let avoid = mapterm::route::avoid_closures(&all, unix_now(), from, to);
                                        route_asking = true;
                                        note = Some((Instant::now(), format!("routing from {}…", if me.is_some() { "your location" } else { "the map's center" })));
                                        let (tx, mode, server) = (routetx.clone(), a.route_mode, router.clone());
                                        tokio::spawn(async move {
                                            let r = mapterm::route::route(&server, from, to, mode, &avoid).await.map(|r| (r, from, to)).map_err(|e| format!("{e:#}"));
                                            let _ = tx.send(r);
                                        });
                                    }
                                    KeyCode::Char(c @ '1'..='3') if (c as usize - '1' as usize) < routes.len() && routes.len() > 1 => {
                                        route_pick = c as usize - '1' as usize;
                                        route = Some(routes[route_pick].clone());
                                        route_panel = true;
                                        route_new = true;
                                    }
                                    KeyCode::Char('x') if route.is_some() => {
                                        routes.clear();
                                        route_pick = 0;
                                        route = None;
                                        route_ends = None;
                                        route_new = true;
                                    }
                                    KeyCode::Esc if selected_site.is_some() && !inspect => selected_site = None,
                                    KeyCode::Esc if route.is_some() && route_panel && selected.is_none() && selected_ship.is_none() && selected_sat.is_none() && selected_alert.is_none() && selected_outage.is_none() && selected_quake.is_none() && selected_fire.is_none() && selected_gauge.is_none() && selected_tfr.is_none() && selected_closure.is_none() && selected_balloon.is_none() && !inspect && !matches!(search, Search::Results { .. }) => {
                                        route_panel = false;
                                    }
                                    KeyCode::Char('g') if !locating => {
                                        locating = true;
                                        note = Some((Instant::now(), "locating…".into()));
                                        let (tx, cmd) = (ltx.clone(), a.location_cmd.clone());
                                        tokio::task::spawn_blocking(move || {
                                            let _ = tx.send(mapterm::locate::locate(cmd.as_deref()));
                                        });
                                    }
                                    KeyCode::Esc if (selected.is_some() || selected_ship.is_some() || selected_sat.is_some() || selected_alert.is_some() || selected_outage.is_some() || selected_quake.is_some() || selected_fire.is_some() || selected_gauge.is_some() || selected_tfr.is_some() || selected_closure.is_some() || selected_balloon.is_some()) && !inspect => {
                                        selected_gauge = None;
                                        selected_tfr = None;
                                        selected_closure = None;
                                        selected_fire = None;
                                        selected_quake = None;
                                        selected_alert = None;
                                        selected_outage = None;
                                        selected_balloon = None;
                                        selected = None;
                                        selected_ship = None;
                                        selected_sat = None;
                                        follow = false;
                                    }
                                    KeyCode::Char('f') if selected.is_some() || selected_ship.is_some() || selected_sat.is_some() || selected_balloon.is_some() => {
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
                                    // --sondes alone: Tab steps through the balloons the same way.
                                    KeyCode::Tab | KeyCode::BackTab if !inspect && aircraft.is_empty() && fleet.is_empty() && sats.is_empty() && !balloons.is_empty() => {
                                        let order = balloons.by_distance(unix_now() - SONDE_DELAY, ctl.cam.lon, ctl.cam.lat);
                                        let at = selected_balloon.as_ref().and_then(|s| order.iter().position(|x| x == s));
                                        let next = match (at, k.code) {
                                            (None, _) => 0,
                                            (Some(i), KeyCode::BackTab) => (i + order.len() - 1) % order.len(),
                                            (Some(i), _) => (i + 1) % order.len(),
                                        };
                                        if order.get(next) != selected_balloon.as_ref() {
                                            follow = false;
                                        }
                                        selected_balloon = order.get(next).cloned();
                                    }
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
                                    // m / M: the next or previous base map; ': the one before; b: the
                                    // switcher.
                                    KeyCode::Char(c @ ('m' | 'M')) => {
                                        let i = mapterm::basemaps::step(bases.len(), want_base.unwrap_or(base_at), c == 'm');
                                        want_base = Some(i);
                                        note = Some((Instant::now(), format!("base map: {}", bases[i].label)));
                                    }
                                    KeyCode::Char('\'') => match base_prev {
                                        Some(p) => want_base = Some(p),
                                        None => note = Some((Instant::now(), "' goes back to the base map before; there's none yet".into())),
                                    },
                                    // S: save what's shown as a named session (the one opened, by default).
                                    // B: saved places, to fly to one or save this spot (the pointer's,
                                    // else the center).
                                    KeyCode::Char('B') => {
                                        finder = None;
                                        let (lon, lat) = match pointer {
                                            Some(d) => Viewport::new(&last_cam, map_area).dot_to_lonlat(d.0, d.1),
                                            None => (ctl.cam.lon, ctl.cam.lat),
                                        };
                                        place_spot = (lon, lat, ctl.cam.zoom);
                                        place_list = mapterm::places::load();
                                        place_forget = None;
                                        switcher = Some(mapterm::basemaps::Switcher::places(place_rows(&place_list)));
                                    }
                                    // S: the sessions, to open one or save this.
                                    KeyCode::Char('S') => {
                                        finder = None;
                                        session_list = mapterm::sessions::all();
                                        switcher = Some(mapterm::basemaps::Switcher::sessions(session_rows(&session_list), false));
                                    }
                                    KeyCode::Char('b') => {
                                        finder = None;
                                        switcher = Some(mapterm::basemaps::Switcher::base(&bases, base_at));
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
                                // On the timeline: click or drag to move the clock there (at or past
                                // now: live), the wheel zooms it about the pointer, shift+wheel pans.
                                if timeline_area.height > 0 && timeline_area.contains(pos) {
                                    let bar = Rect { y: timeline_area.y + 1, height: 1, ..timeline_area };
                                    let at = timeline.time_at(bar, cell.0);
                                    let shift = m.modifiers.contains(KeyModifiers::SHIFT);
                                    match m.kind {
                                        event::MouseEventKind::Down(event::MouseButton::Left) | event::MouseEventKind::Drag(event::MouseButton::Left) => {
                                            if let Some(t) = at {
                                                if t >= unix_now() - 30.0 {
                                                    clock.go_live(unix_now());
                                                } else {
                                                    clock.seek(t);
                                                }
                                            }
                                        }
                                        event::MouseEventKind::ScrollUp | event::MouseEventKind::ScrollDown => {
                                            let out = m.kind == event::MouseEventKind::ScrollDown;
                                            if shift {
                                                timeline.pan((timeline.stop - timeline.start) * if out { 0.1 } else { -0.1 });
                                            } else if let Some(t) = at {
                                                timeline.zoom(if out { 1.25 } else { 0.8 }, t);
                                            }
                                            timeline_moved = true;
                                        }
                                        event::MouseEventKind::ScrollLeft | event::MouseEventKind::ScrollRight => {
                                            let back = m.kind == event::MouseEventKind::ScrollLeft;
                                            timeline.pan((timeline.stop - timeline.start) * if back { -0.1 } else { 0.1 });
                                            timeline_moved = true;
                                        }
                                        _ => {}
                                    }
                                    continue;
                                }
                                // The switcher: a click picks a row (and previews it); wheel
                                // moves; the map under it sees nothing.
                                if let Some(sw) = switcher.as_mut()
                                    && switcher_rect.contains(pos)
                                {
                                    match m.kind {
                                        event::MouseEventKind::Down(event::MouseButton::Left) => {
                                            if let Some(&(_, row)) = switcher_rows.iter().find(|(r, _)| *r == cell.1)
                                                && let Some(n) = sw.rows.iter().position(|r| *r == row)
                                            {
                                                sw.sel = n;
                                                switcher_picks.push(row);
                                            }
                                        }
                                        event::MouseEventKind::ScrollDown | event::MouseEventKind::ScrollUp => {
                                            sw.step(m.kind == event::MouseEventKind::ScrollDown);
                                        }
                                        _ => {}
                                    }
                                    continue;
                                }
                                // Clicks in the inspector select a feature; the map never sees them.
                                if panel_area.width > 0 && panel_area.contains(pos) {
                                    // A receiving station in the balloon panel: fly to it.
                                    if m.kind == event::MouseEventKind::Down(event::MouseButton::Left)
                                        && let Some(st) = station_rows.iter().find(|r| r.row == cell.1)
                                    {
                                        follow = false;
                                        ctl.fly_to(beside_panel(Camera::new(st.lon, st.lat, ctl.cam.zoom.max(9.0)), map_area, panel_area));
                                        note = Some((Instant::now(), format!("⌂ {} · receiving station", st.callsign)));
                                        continue;
                                    }
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
                                    } else if adsb_feed.is_some() || ais_key.is_some() || a.sats.is_some() || a.alerts || a.outages || a.sondes || a.quakes.is_some() || a.fires || a.floods || a.tfrs || a.closures {
                                        // A click on (or next to) an aircraft, else a balloon, else a
                                        // ship, selects it.
                                        let vp = Viewport::new(&last_cam, map_area);
                                        let dot = vp.dot_ll(k.lon, k.lat);
                                        selected_site = None;
                                        if let Some(hex) = aircraft.nearest(unix_now() - adsb_delay, &vp, dot, 6.0) {
                                            if selected.as_deref() != Some(hex.as_str()) {
                                                follow = false;
                                            }
                                            selected = Some(hex);
                                            selected_ship = None;
                                            selected_sat = None;
                                            selected_balloon = None;
                                        } else if let Some(serial) = balloons.nearest(unix_now() - SONDE_DELAY, &vp, dot, 6.0) {
                                            if selected_balloon.as_deref() != Some(serial.as_str()) {
                                                follow = false;
                                            }
                                            selected_balloon = Some(serial);
                                            selected = None;
                                            selected_ship = None;
                                            selected_sat = None;
                                        } else if let Some((_, mmsi)) = fleet.nearest(unix_now() - AIS_DELAY, &vp, dot, 6.0) {
                                            if selected_ship != Some(mmsi) {
                                                follow = false;
                                            }
                                            selected_ship = Some(mmsi);
                                            selected_sat = None;
                                            selected = None;
                                            selected_balloon = None;
                                        } else if let Some(norad) = mapterm::satellites::nearest(&sats, unix_now(), &vp, dot, 6.0) {
                                            if selected_sat != Some(norad) {
                                                follow = false;
                                            }
                                            selected_sat = Some(norad);
                                            selected = None;
                                            selected_ship = None;
                                            selected_alert = None;
                                        } else if let Some(id) = mapterm::floods::nearest(&gauges, &vp, dot, 4.0) {
                                            selected_gauge = Some(id);
                                            (selected, selected_ship, selected_sat, selected_balloon, selected_alert, selected_outage, selected_quake, selected_fire) =
                                                (None, None, None, None, None, None, None, None);
                                            follow = false;
                                        } else if let Some(id) = mapterm::fires::nearest(&fires, &vp, dot, 4.0).or_else(|| {
                                            // Or the perimeter under the click.
                                            map.query_features(&last_cam, map_area, dot)
                                                .iter()
                                                .filter(|p| p.source.as_deref() == Some(FIRE_AREAS))
                                                .find_map(|p| p.prop("id").and_then(|v| v.as_str()).map(str::to_string))
                                        }) {
                                            selected_fire = Some(id);
                                            selected_gauge = None;
                                            (selected, selected_ship, selected_sat, selected_balloon, selected_alert, selected_outage, selected_quake) =
                                                (None, None, None, None, None, None, None);
                                            follow = false;
                                        } else if let Some(id) = mapterm::quakes::nearest(&quakes, unix_now(), &vp, dot, 4.0) {
                                            selected_fire = None;
                                            selected_quake = Some(id);
                                            (selected, selected_ship, selected_sat, selected_balloon, selected_alert, selected_outage) = (None, None, None, None, None, None);
                                            follow = false;
                                        } else if let Some(id) = a
                                            .sondes
                                            .then(|| mapterm::upperair::nearest(&launch_sites, &vp, dot, 4.0).map(|s| s.id.clone()))
                                            .flatten()
                                        {
                                            // A weather balloon launch site.
                                            selected_site = Some(id);
                                            (selected, selected_ship, selected_sat, selected_balloon, selected_alert, selected_outage, selected_quake, selected_fire) =
                                                (None, None, None, None, None, None, None, None);
                                            follow = false;
                                        } else if let Some(k) = a
                                            .closures
                                            .then(|| map.query_features(&last_cam, map_area, dot))
                                            .and_then(|hits| hits.iter().filter(|p| p.source.as_deref() == Some(CLOSURES)).find_map(|p| p.prop("key").and_then(|v| v.as_str()).map(str::to_string)))
                                        {
                                            selected_closure = Some(k);
                                            (selected, selected_ship, selected_sat, selected_balloon, selected_alert, selected_outage, selected_quake, selected_fire, selected_tfr) =
                                                (None, None, None, None, None, None, None, None, None);
                                            follow = false;
                                        } else if a.alerts || a.outages || a.tfrs {
                                            selected_closure = None;
                                            selected_quake = None;
                                            selected_fire = None;
                                            let hits = map.query_features(&last_cam, map_area, dot);
                                            // A flight restriction under the click (TFRs over standing airspace;
                                            // its outline, braille or not, is what's picked).
                                            selected_tfr = hits
                                                .iter()
                                                .filter(|p| p.source.as_deref() == Some(TFRS))
                                                .filter_map(|p| p.prop("key").and_then(|v| v.as_str()))
                                                .max_by_key(|k| !k.starts_with("stadium:") && !k.starts_with("rules:"))
                                                .map(str::to_string)
                                                .or_else(|| {
                                                    // Inside an area (its outline isn't under the click): the
                                                    // smallest restriction containing the spot.
                                                    let (lon, lat) = (k.lon, k.lat);
                                                    tfr_live
                                                        .iter()
                                                        .chain(&tfr_standing)
                                                        .filter(|r| mapterm::tfrs::contains(&r.geometry, lon, lat))
                                                        .min_by(|a, b| mapterm::tfrs::extent(&a.geometry).total_cmp(&mapterm::tfrs::extent(&b.geometry)))
                                                        .map(|r| r.key.clone())
                                                });
                                            // An alert area under the click: the most severe.
                                            selected_alert = hits
                                                .iter()
                                                .filter(|p| p.source.as_deref() == Some(ALERTS))
                                                .filter_map(|p| p.prop("id").and_then(|v| v.as_str()))
                                                .filter_map(|id| alerts.iter().find(|a| a.id == id))
                                                .max_by_key(|a| a.severity)
                                                .map(|a| a.id.clone());
                                            // Else the outage area under it, at the level shown.
                                            let level = mapterm::outages::Level::at(last_cam.zoom);
                                            selected_outage = if selected_alert.is_some() {
                                                None
                                            } else {
                                                hits.iter()
                                                    .filter(|p| p.source.as_deref() == Some(OUTAGES))
                                                    .filter(|p| p.prop("level").and_then(|v| v.as_str()) == Some(level.name()))
                                                    .filter_map(|p| p.prop("key").and_then(|v| v.as_str()))
                                                    .next()
                                                    .map(str::to_string)
                                            };
                                            if selected_alert.is_some() || selected_outage.is_some() {
                                                (selected, selected_ship, selected_sat) = (None, None, None);
                                            }
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
    // The balloon stations heard this run, for next time.
    if a.sondes && balloons.stations_changed() {
        let _ = balloons.save_stations(&mapterm::sondehub::stations_file());
    }
    // Keep this run in the history, for --continue and --resume.
    let sn = session_now(
        a.session.as_ref().map_or("run", |s| s.name.as_str()),
        &a.kept,
        &ctl.cam,
        bases.get(base_at).map(|e| e.base.spec().to_string()),
        &overlay_on,
        &live_off,
        vector,
        run_started,
    );
    if let Err(e) = sn.save_history() {
        dlog(|| format!("history: {e:#}"));
    }
    // A session with other layers: this program again, continuing it (it goes in the history
    // as the latest run, after this one).
    if let Some(mut sn) = restart_with {
        use std::os::unix::process::CommandExt;
        sn.saved_at = unix_now() + 0.001;
        sn.save_history()?;
        let exe = std::env::current_exe().context("finding mapterm to start again")?;
        let e = std::process::Command::new(exe).arg("--continue").exec();
        return Err(e).context("starting mapterm again");
    }
    result
}

/// What the viewer shows now, as a session called `name`.
#[allow(clippy::too_many_arguments)]
fn session_now(
    name: &str,
    kept: &[String],
    cam: &Camera,
    base: Option<String>,
    overlay_on: &[bool],
    live_off: &std::collections::HashMap<&'static str, (Vec<StyleLayer>, Option<String>)>,
    vector: u8,
    created: f64,
) -> mapterm::sessions::Session {
    let mut off: Vec<String> = live_off.keys().map(|k| k.to_string()).collect();
    off.sort();
    mapterm::sessions::Session {
        name: name.to_string(),
        args: kept.to_vec(),
        lon: cam.lon,
        lat: cam.lat,
        zoom: cam.zoom,
        base,
        overlays: overlay_on.iter().zip(mapterm::basemaps::OVERLAYS).filter(|(on, _)| **on).map(|(_, o)| o.preset.to_string()).collect(),
        live_off: off,
        vector,
        saved_at: unix_now(),
        created_at: created,
        viewed_at: unix_now(),
    }
}

/// The places picker's rows: each place's name, and when it was saved and visited, where.
fn place_rows(list: &[mapterm::places::Place]) -> Vec<(String, String)> {
    use mapterm::sessions::ago;
    let now = unix_now();
    list.iter()
        .map(|p| {
            let visited = if p.visited > p.created { format!(" · visited {}", ago(p.visited, now)) } else { String::new() };
            (p.name.clone(), format!("saved {}{visited} · {:.4}, {:.4} · z{:.0}", ago(p.created, now), p.lat, p.lon, p.zoom))
        })
        .collect()
}

/// The switcher's rows for sessions: a name (or when, for a recent run) and what it shows.
fn session_rows(list: &[(mapterm::sessions::Session, bool)]) -> Vec<(String, String)> {
    let now = unix_now();
    list.iter()
        .map(|(s, past)| {
            use mapterm::sessions::ago;
            let name = match (*past, s.name.as_str()) {
                (true, "run") => format!("run {}", ago(s.saved_at, now)),
                (true, n) => format!("{n} · run {}", ago(s.saved_at, now)),
                (false, n) => n.to_string(),
            };
            let opened = if s.viewed_at > 0.0 { ago(s.viewed_at, now) } else { "never".into() };
            let detail = if *past {
                format!("{} · started {}", s.summary(), ago(s.created_at, now))
            } else {
                format!("{} · made {} · opened {opened}", s.summary(), ago(s.created_at, now))
            };
            (name, detail)
        })
        .collect()
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

#[cfg(test)]
mod data_info_tests {
    use super::{human_bytes, short_path};

    #[test]
    fn paths_keep_their_file_names() {
        assert_eq!(short_path("/tmp/a.geojson", 40), "/tmp/a.geojson");
        let long = format!("/data/{}/parcels.geojson", "x".repeat(60));
        let s = short_path(&long, 30);
        assert_eq!(s.chars().count(), 30);
        assert!(s.starts_with('…') && s.ends_with("/parcels.geojson"));
    }

    #[test]
    fn sizes() {
        assert_eq!(human_bytes(268), "268 B");
        assert_eq!(human_bytes(2 * 1024 * 1024 + 100_000), "2.1 MB");
    }
}
