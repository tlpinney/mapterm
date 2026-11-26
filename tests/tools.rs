//! Tools for working on mapterm, not tests: run one on purpose, by name.
//!
//! - `label_churn` measures how much labels flicker while the map moves: it flies one camera
//!   path (zoom in, pan, zoom out) and counts, frame to frame, labels that appear, vanish
//!   while still on screen, or hop to another spot; once with labels placed from scratch
//!   every frame, once sticky. `MAPTERM_STYLE` picks the style (default liberty).
//!
//!       cargo test --release --test tools label_churn -- --ignored --nocapture
//!
//! - `make_perf_fixture` rebuilds `tests/fixtures/perf-dc.mbtiles`, the map tiles the
//!   nightly performance test draws: OpenFreeMap's OpenMapTiles over central Washington, DC,
//!   zooms 10 to 14. Pinned in the repo so the numbers compare from night to night (the live
//!   tiles change every week); after rebuilding it, record a new baseline with
//!   `MAPTERM_PERF_UPDATE=1` (see `tests/perf.rs`).
//!
//!       cargo test --release --test tools make_perf_fixture -- --ignored --nocapture
//!
//! Both need the network. They're ignored tests in their own file so that neither the usual
//! `cargo test` nor the nightly performance run (`--test perf -- --ignored`) runs them.

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a tool: measures label flicker over a camera path (needs the network)"]
async fn label_churn() {
    label_churn::run().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a tool: rebuilds tests/fixtures/perf-dc.mbtiles from OpenFreeMap"]
async fn make_perf_fixture() {
    perf_fixture::run().await.unwrap();
}

mod label_churn {
    use mapterm::{Camera, LabelPlacement, Map, Viewport};
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    #[derive(Default, Debug)]
    struct Churn {
        frames: usize,
        labels: usize,
        appeared: usize,
        vanished: usize,
        /// Vanished while still offered: lost the collision.
        lost: usize,
        moved: usize,
    }

    pub async fn run() -> anyhow::Result<()> {
        let style = std::env::var("MAPTERM_STYLE").unwrap_or_else(|_| "liberty".into());
        let area = Rect::new(0, 0, 160, 45);
        // Georgetown: zoom 16 -> 17, pan east, zoom out to 15.5.
        let (lon, lat) = (-77.0650, 38.9045);
        let mut path = Vec::new();
        for i in 0..=40 {
            path.push(Camera::new(lon, lat, 16.0 + i as f64 / 40.0));
        }
        let mut cam = Camera::new(lon, lat, 17.0);
        for _ in 0..60 {
            cam.pan_dots(3.0, 0.0);
            path.push(cam);
        }
        for i in 0..=40 {
            path.push(Camera::new(cam.lon, cam.lat, 17.0 - 1.5 * i as f64 / 40.0));
        }
        for placement in [LabelPlacement::Fresh, LabelPlacement::Sticky] {
            let mut map = Map::from_style(&style).await?;
            map.label_placement = placement;
            let mut c = Churn::default();
            // Per phase: zoom in (frames 1-40), pan (41-100), zoom out (101-141).
            let mut phases = [Churn::default(), Churn::default(), Churn::default()];
            let mut prev: Vec<(String, f64, f64)> = Vec::new();
            for cam in &path {
                let _ = map.load_tiles(cam, area).await;
                let mut buf = Buffer::empty(area);
                map.view(cam).render(area, &mut buf);
                let now = map.placed_labels();
                let vp = Viewport::new(cam, area);
                let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
                let on = |p: (f64, f64)| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < w && p.1 < h;
                let dots =
                    |v: &[(String, f64, f64)]| -> Vec<(String, (f64, f64))> { v.iter().map(|(k, lon, lat)| (k.clone(), vp.dot_ll(*lon, *lat))).collect() };
                let (before, after) = (dots(&prev), dots(&now));
                let (a0, v0, l0, m0) = (c.appeared, c.vanished, c.lost, c.moved);
                if c.frames > 0 {
                    for (k, p) in &after {
                        let same: Vec<f64> = before.iter().filter(|(q, _)| q == k).map(|(_, b)| (p.0 - b.0).hypot(p.1 - b.1)).collect();
                        match same.iter().copied().reduce(f64::min) {
                            None => c.appeared += 1,
                            Some(d) if d > 3.0 => c.moved += 1,
                            _ => {}
                        }
                    }
                    let offered = map.offered_labels();
                    for (k, p) in &before {
                        if on(*p) && !after.iter().any(|(q, _)| q == k) {
                            c.vanished += 1;
                            if offered.contains(k) {
                                c.lost += 1;
                            }
                        }
                    }
                }
                if c.frames > 0 {
                    let ph = &mut phases[if c.frames <= 40 {
                        0
                    } else if c.frames <= 100 {
                        1
                    } else {
                        2
                    }];
                    ph.frames += 1;
                    ph.appeared += c.appeared - a0;
                    ph.vanished += c.vanished - v0;
                    ph.lost += c.lost - l0;
                    ph.moved += c.moved - m0;
                }
                c.frames += 1;
                c.labels += now.len();
                prev = now;
            }
            let per = |n: usize| n as f64 / (c.frames - 1) as f64;
            println!(
                "{placement:?}: {} frames, {:.0} labels a frame; per frame: {:.2} appeared, {:.2} vanished on screen ({:.2} lost a collision), {:.2} moved",
                c.frames,
                c.labels as f64 / c.frames as f64,
                per(c.appeared),
                per(c.vanished),
                per(c.lost),
                per(c.moved)
            );
            for (name, p) in ["zoom in", "pan", "zoom out"].iter().zip(&phases) {
                println!("{}", phase_line(name, p));
            }
        }
        Ok(())
    }

    fn phase_line(name: &str, p: &Churn) -> String {
        let per = |n: usize| n as f64 / p.frames.max(1) as f64;
        format!("  {name:<9} appeared {:.2}  vanished {:.2} (lost {:.2})  moved {:.2}", per(p.appeared), per(p.vanished), per(p.lost), per(p.moved))
    }
}

mod perf_fixture {
    use anyhow::{Context, Result};

    /// The area the perf test's camera path stays inside (lon/lat), and its zooms.
    const WEST: f64 = -77.12;
    const SOUTH: f64 = 38.84;
    const EAST: f64 = -76.93;
    const NORTH: f64 = 38.96;
    const ZOOMS: std::ops::RangeInclusive<u8> = 10..=14;
    /// From zoom 13 only this core (around the National Mall), where the path zooms in: the
    /// detailed tiles are most of the bytes.
    const CORE: (f64, f64, f64, f64) = (-77.055, 38.88, -77.005, 38.905);
    const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/perf-dc.mbtiles");

    /// The tile column and row (from the top) at zoom `z` holding (lon, lat).
    fn tile_of(lon: f64, lat: f64, z: u8) -> (u32, u32) {
        let n = (1u32 << z) as f64;
        let x = ((lon + 180.0) / 360.0 * n).floor() as u32;
        let r = lat.to_radians();
        let y = ((1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * n).floor() as u32;
        (x, y)
    }

    pub async fn run() -> Result<()> {
        let src = mapterm::Source::open(mapterm::source::OPENFREEMAP).await.context("OpenFreeMap")?;
        let _ = std::fs::remove_file(OUT);
        let c = rusqlite::Connection::open(OUT)?;
        c.execute_batch("CREATE TABLE metadata (name TEXT, value TEXT); CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB); CREATE UNIQUE INDEX tile_index ON tiles (zoom_level, tile_column, tile_row);")?;
        let meta = [
            ("name", "mapterm perf fixture: central Washington, DC"),
            ("format", "pbf"),
            ("minzoom", "10"),
            ("maxzoom", "14"),
            ("bounds", &format!("{WEST},{SOUTH},{EAST},{NORTH}")),
            ("center", "-77.03,38.9,12"),
            ("attribution", "OpenFreeMap © OpenMapTiles Data from OpenStreetMap"),
        ];
        for (k, v) in meta {
            c.execute("INSERT INTO metadata VALUES (?1, ?2)", (k, v))?;
        }
        let (mut count, mut bytes) = (0, 0);
        for z in ZOOMS {
            let (w, s, e, n) = if z >= 13 { CORE } else { (WEST, SOUTH, EAST, NORTH) };
            let (x0, y0) = tile_of(w, n, z);
            let (x1, y1) = tile_of(e, s, z);
            for x in x0..=x1 {
                for y in y0..=y1 {
                    let Some(tile) = src.tile(z, x, y).await? else { continue };
                    // Stored gzipped, as vector MBTiles usually are.
                    let data = if tile.starts_with(&[0x1f, 0x8b]) {
                        tile
                    } else {
                        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
                        std::io::Write::write_all(&mut gz, &tile)?;
                        gz.finish()?
                    };
                    bytes += data.len();
                    count += 1;
                    let row = (1i64 << z) - 1 - y as i64;
                    c.execute("INSERT INTO tiles VALUES (?1, ?2, ?3, ?4)", (z as i64, x as i64, row, data))?;
                }
            }
        }
        c.execute_batch("VACUUM")?;
        println!("{OUT}: {count} tiles, {:.1} MB", bytes as f64 / 1e6);
        Ok(())
    }
}
