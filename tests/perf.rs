//! Performance tests, run nightly (`cargo test --release --test perf -- --ignored
//! --test-threads=1`): they drive the map over pinned tiles (`tests/fixtures/perf-dc.mbtiles`)
//! and synthetic live layers, and compare with `tests/perf-baseline.json`.
//!
//! - Bytes sent to the terminal are the same on every machine, so they're checked strictly:
//!   more than 25% over the baseline fails. They catch redraws that send far more than they
//!   need (a full-screen image every pan frame, say).
//! - Times vary between machines, so they're checked only against a baseline from the same
//!   platform, and only fail past twice the baseline: a real slowdown, not noise.
//!
//! `MAPTERM_PERF_UPDATE=1` writes this run's numbers as the new baseline (times under this
//! platform). Results also go to `target/perf-results.json` and, in GitHub Actions, the run
//! summary.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mapterm::{Camera, Map, MapSource, Source, stylejson::Schema};
use ratatui::{Terminal, TerminalOptions, backend::CrosstermBackend, layout::Rect};
use serde_json::{Value as Json, json};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/perf-dc.mbtiles");
const BASELINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/perf-baseline.json");
/// The terminal the map is drawn into.
const AREA: Rect = Rect { x: 0, y: 0, width: 160, height: 48 };

/// What one test measured: bytes (strict) and milliseconds (loose, per platform).
#[derive(Default)]
struct Metrics {
    bytes: BTreeMap<String, f64>,
    ms: BTreeMap<String, f64>,
}

/// All tests' metrics, checked and written once each test has added its own.
static RESULTS: Mutex<Vec<(String, Metrics)>> = Mutex::new(Vec::new());

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Compare `m` with the baseline (or record it with MAPTERM_PERF_UPDATE=1), report it, and
/// fail on a regression.
fn check(test: &str, m: Metrics) {
    let update = std::env::var("MAPTERM_PERF_UPDATE").is_ok_and(|v| v == "1");
    let mut base: Json = std::fs::read(BASELINE).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
    let plat = platform();
    let mut failures = Vec::new();
    let mut table = format!("### {test}\n\n| metric | this run | baseline | limit |\n|---|---|---|---|\n");
    for (k, v) in &m.bytes {
        let b = base["bytes"][test][k].as_f64();
        let limit = b.map(|b| b * 1.25 + 2048.0);
        if let Some(l) = limit.filter(|l| *v > *l) {
            failures.push(format!("{k}: {v:.0} bytes, over {l:.0} (baseline {:.0})", b.unwrap_or(0.0)));
        }
        table += &format!("| {k} | {v:.0} B | {} | {} |\n", b.map_or("-".into(), |b| format!("{b:.0} B")), limit.map_or("-".into(), |l| format!("{l:.0} B")));
    }
    for (k, v) in &m.ms {
        let b = base["ms"][&plat][test][k].as_f64();
        let limit = b.map(|b| b * 2.0 + 1.0);
        if let Some(l) = limit.filter(|l| *v > *l) {
            failures.push(format!("{k}: {v:.2} ms, over {l:.2} (baseline {:.2} on {plat})", b.unwrap_or(0.0)));
        }
        table +=
            &format!("| {k} | {v:.2} ms | {} | {} |\n", b.map_or("-".into(), |b| format!("{b:.2} ms")), limit.map_or("-".into(), |l| format!("{l:.2} ms")));
    }
    println!("{table}");
    if let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY")
        && let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(path)
    {
        let _ = writeln!(f, "{table}");
    }
    if update {
        for (k, v) in &m.bytes {
            base["bytes"][test][k] = json!(v.round());
        }
        for (k, v) in &m.ms {
            base["ms"][&plat][test][k] = json!((v * 100.0).round() / 100.0);
        }
        std::fs::write(BASELINE, serde_json::to_string_pretty(&base).unwrap() + "\n").unwrap();
    }
    let mut all = RESULTS.lock().unwrap();
    all.push((test.to_string(), m));
    let out: BTreeMap<&str, Json> = all.iter().map(|(t, m)| (t.as_str(), json!({"bytes": m.bytes, "ms": m.ms}))).collect();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/target");
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(format!("{dir}/perf-results.json"), serde_json::to_string_pretty(&json!({"platform": plat, "tests": out})).unwrap());
    assert!(update || failures.is_empty(), "{test} got slower or heavier:\n  {}", failures.join("\n  "));
}

/// Percentile `p` (0 to 100) of `v`.
fn percentile(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[((s.len() - 1) as f64 * p / 100.0).round() as usize]
}

/// A writer that only counts what the terminal would receive.
#[derive(Clone, Default)]
struct Counter(Arc<AtomicUsize>);

impl Write for Counter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.fetch_add(b.len(), Ordering::Relaxed);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn terminal(counter: &Counter) -> Terminal<CrosstermBackend<Counter>> {
    Terminal::with_options(CrosstermBackend::new(counter.clone()), TerminalOptions { viewport: ratatui::Viewport::Fixed(AREA) }).unwrap()
}

/// The pinned map, drawn with the built-in dark style.
async fn fixture_map() -> Map {
    let src = Source::open(FIXTURE).await.expect("the perf fixture");
    Map::terminal(src, Schema::OpenMapTiles, true).unwrap()
}

/// The camera path: pan east and north at zoom 12, zoom in to 14 over the Mall, and back out.
fn path() -> Vec<Camera> {
    let mut v = Vec::new();
    let (mut lon, mut lat) = (-77.06, 38.88);
    for _ in 0..40 {
        lon += 0.0015;
        v.push(Camera::new(lon, lat, 12.0));
    }
    for _ in 0..40 {
        lat += 0.001;
        v.push(Camera::new(lon, lat, 12.0));
    }
    let (mall_lon, mall_lat) = (-77.03, 38.89);
    for i in 0..=40 {
        let f = i as f64 / 40.0;
        v.push(Camera::new(lon + (mall_lon - lon) * f, lat + (mall_lat - lat) * f, 12.0 + 2.0 * f));
    }
    for i in 0..=40 {
        v.push(Camera::new(mall_lon, mall_lat, 14.0 - 2.0 * i as f64 / 40.0));
    }
    v
}

/// Draw the camera path over the pinned tiles (loaded beforehand, so only drawing is timed).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "nightly performance test"]
async fn render_path() {
    let mut map = fixture_map().await;
    let cams = path();
    for c in &cams {
        let _ = map.load_tiles(c, AREA).await;
    }
    let counter = Counter::default();
    let mut term = terminal(&counter);
    let (mut ms, mut bytes) = (Vec::new(), Vec::new());
    for c in &cams {
        let before = counter.0.load(Ordering::Relaxed);
        let t = Instant::now();
        term.draw(|f| f.render_widget(map.view(c), AREA)).unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        bytes.push((counter.0.load(Ordering::Relaxed) - before) as f64);
    }
    // Held still: frames after the first should send next to nothing.
    let still = cams.last().unwrap();
    let before = counter.0.load(Ordering::Relaxed);
    for _ in 0..20 {
        term.draw(|f| f.render_widget(map.view(still), AREA)).unwrap();
    }
    let still_bytes = (counter.0.load(Ordering::Relaxed) - before) as f64 / 20.0;
    let mut m = Metrics::default();
    m.bytes.insert("total over the path".into(), bytes.iter().sum());
    m.bytes.insert("largest frame".into(), percentile(&bytes, 100.0));
    m.bytes.insert("per frame held still".into(), still_bytes);
    m.ms.insert("frame p50".into(), percentile(&ms, 50.0));
    m.ms.insert("frame p95".into(), percentile(&ms, 95.0));
    check("render_path", m);
}

/// A small deterministic random sequence (no dependency, same numbers everywhere).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// `n` aircraft in readsb JSON around DC, `step` updates along their tracks.
fn aircraft_json(n: usize, step: f64) -> Vec<u8> {
    let mut r = Lcg(7);
    let ac: Vec<Json> = (0..n)
        .map(|i| {
            let (lon, lat, track, gs) = (-77.25 + r.next() * 0.45, 38.7 + r.next() * 0.4, r.next() * 360.0, 150.0 + r.next() * 350.0);
            let d = gs / 3600.0 * 10.0 * step / 60.0;
            json!({"hex": format!("{i:06x}"), "flight": format!("TST{i}"), "t": "B738",
                   "lat": lat + d * track.to_radians().cos(), "lon": lon + d * track.to_radians().sin() / 38.9f64.to_radians().cos(),
                   "alt_baro": 1000.0 + r.next() * 38000.0, "gs": gs, "track": track, "seen_pos": 0.5})
        })
        .collect();
    serde_json::to_vec(&json!({"ac": ac})).unwrap()
}

/// 300 aircraft and 500 ships on the map: updates, GeoJSON, drawing, and trail images.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "nightly performance test"]
async fn live_layers() {
    let mut map = fixture_map().await;
    let cam = Camera::new(-77.03, 38.89, 12.0);
    let _ = map.load_tiles(&cam, AREA).await;
    let empty = json!({"type": "FeatureCollection", "features": []});
    map.add_source("aircraft", MapSource::geojson(&empty).unwrap()).unwrap();
    map.add_source("ships", MapSource::geojson(&empty).unwrap()).unwrap();
    map.add_layers(vec![mapterm::ais::layer("ships", "ships"), mapterm::adsb::layer("aircraft", "aircraft")], None).unwrap();

    // Twelve rounds of updates, 10 s apart, so the trails have points.
    let mut tracks = mapterm::adsb::Tracks::default();
    let mut fleet = mapterm::ais::Fleet::default();
    let mut r = Lcg(11);
    let ships: Vec<(u32, f64, f64, f64, f64)> =
        (0..500).map(|i| (300_000_000 + i, -77.1 + r.next() * 0.2, 38.8 + r.next() * 0.15, r.next() * 360.0, r.next() * 15.0)).collect();
    let (mut update_ms, mut t) = (Vec::new(), 1_000_000.0);
    for step in 0..12 {
        t += 10.0;
        let body = aircraft_json(300, step as f64);
        let s = Instant::now();
        tracks.update(mapterm::adsb::parse(&body).unwrap(), t);
        for &(mmsi, lon, lat, cog, sog) in &ships {
            let d = sog / 3600.0 * 10.0 * step as f64 / 60.0;
            fleet.apply(
                mapterm::ais::Update::Position {
                    mmsi,
                    lat: lat + d * cog.to_radians().cos(),
                    lon: lon + d * cog.to_radians().sin(),
                    sog: Some(sog),
                    cog: Some(cog),
                    heading: None,
                    status: Some(0),
                    name: Some(format!("SHIP {mmsi}")),
                },
                t,
            );
        }
        update_ms.push(s.elapsed().as_secs_f64() * 1000.0);
    }

    // The viewer's 4-a-second refresh: GeoJSON for both layers, then a frame.
    let counter = Counter::default();
    let mut term = terminal(&counter);
    let (mut data_ms, mut frame_ms) = (Vec::new(), Vec::new());
    for i in 0..40 {
        let now = t - 11.0 + i as f64 * 0.25;
        let s = Instant::now();
        map.set_data("aircraft", &tracks.points(now)).unwrap();
        map.set_data("ships", &fleet.points(now)).unwrap();
        data_ms.push(s.elapsed().as_secs_f64() * 1000.0);
        let s = Instant::now();
        term.draw(|f| f.render_widget(map.view(&cam), AREA)).unwrap();
        frame_ms.push(s.elapsed().as_secs_f64() * 1000.0);
    }

    // One trail image of each, at a typical retina cell size.
    let vp = mapterm::Viewport::new(&cam, AREA);
    let (cell, w, h) = ((18.0, 36.0), 160 * 18, 48 * 36);
    let s = Instant::now();
    let air = mapterm::timeseries::render_trails(mapterm::adsb::trail_paths(&tracks), t - 11.0, 90.0, &vp, w, h, cell);
    let air_ms = s.elapsed().as_secs_f64() * 1000.0;
    let s = Instant::now();
    let sea = mapterm::timeseries::render_trails(fleet.trail_paths(), t - 10.0, 600.0, &vp, w, h, cell);
    let sea_ms = s.elapsed().as_secs_f64() * 1000.0;
    assert!(air.chunks(4).any(|p| p[3] > 0) && sea.chunks(4).any(|p| p[3] > 0), "trails drawn");

    let mut m = Metrics::default();
    m.bytes.insert("per refresh frame".into(), counter.0.load(Ordering::Relaxed) as f64 / 40.0);
    m.ms.insert("update 300 aircraft + 500 ships".into(), percentile(&update_ms, 50.0));
    m.ms.insert("GeoJSON refresh p50".into(), percentile(&data_ms, 50.0));
    m.ms.insert("frame with live layers p95".into(), percentile(&frame_ms, 95.0));
    m.ms.insert("aircraft trail image".into(), air_ms);
    m.ms.insert("ship trail image".into(), sea_ms);
    check("live_layers", m);
}

/// A local ADS-B feed for the viewer: the same 60 aircraft, a little further on each request.
fn fake_feed() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut s, &mut buf);
            let body = aircraft_json(60, n as f64);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        }
    });
    port
}

/// The viewer itself on a pseudo-terminal, over the pinned tiles with a local ADS-B feed:
/// pans after the trails are up. Trail images must not be redrawn while the map moves
/// (each is a full-screen image the terminal must take in), and must come back after.
#[test]
#[ignore = "nightly performance test"]
fn viewer_pans() {
    let dir = std::env::temp_dir().join(format!("mapterm-perf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (log, typescript) = (dir.join("viewer.log"), dir.join("typescript"));
    let feed = format!("http://127.0.0.1:{}/{{lat}}/{{lon}}/{{nm}}", fake_feed());
    // Two polls (10 s apart) before the pans, so the trails have points; pans at 25 to 27 s.
    let demo = "right@25,right@25.4,right@25.8,left@26.2,down@26.6,up@27,q@32";
    let cmd = format!(
        "stty cols 120 rows 40; MAPTERM_LOG='{}' MAPTERM_DEMO='{demo}' MAPTERM_GRAPHICS=kitty MAPTERM_KITTY_MEDIUM=direct '{}' '{FIXTURE}' --adsb '{feed}' --at 38.89,-77.03 -z 12",
        log.display(),
        env!("CARGO_BIN_EXE_mapterm"),
    );
    // `script` gives the viewer a terminal; its BSD and util-linux flags differ.
    let mut script = std::process::Command::new("script");
    if cfg!(target_os = "macos") {
        script.args(["-q", typescript.to_str().unwrap(), "sh", "-c", &cmd]);
    } else {
        script.args(["-q", "-e", "-c", &format!("sh -c \"{}\"", cmd.replace('"', "\\\"")), typescript.to_str().unwrap()]);
    }
    let status = script.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).status().expect("running script(1)");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(status.success() && text.contains("demo: right"), "the viewer didn't run its script ({status}):\n{text}");
    let times = |pat: &str| -> Vec<f64> { text.lines().filter(|l| l.contains(pat)).filter_map(|l| l.split_whitespace().next()?.parse().ok()).collect() };
    let trails = times("trails:");
    let during = trails.iter().filter(|t| (25.0..27.1).contains(*t)).count();
    let after = trails.iter().filter(|t| **t > 27.1).count();
    let polls = times("adsb: asking").len();
    let sent = std::fs::metadata(&typescript).map(|m| m.len()).unwrap_or(0) as f64;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(polls >= 2, "the fake feed was asked {polls} times");
    assert!(after >= 1, "the trail image didn't come back after the pans (trail draws at {trails:?})");
    assert!(during <= 3, "trail images drawn {during} times during 2 s of panning (at {trails:?})");
    let mut m = Metrics::default();
    m.bytes.insert("sent to the terminal over the run".into(), sent);
    check("viewer_pans", m);
}
