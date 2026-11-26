//! Builds `tests/fixtures/perf-dc.mbtiles`, the map tiles the nightly performance test draws:
//! OpenFreeMap's OpenMapTiles over central Washington, DC, zooms 10 to 14. Pinned in the repo
//! so the numbers compare from night to night (the live tiles change every week).
//!
//! ```sh
//! cargo run --release --example make_perf_fixture
//! ```

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
const OUT: &str = "tests/fixtures/perf-dc.mbtiles";

/// The tile column and row (from the top) at zoom `z` holding (lon, lat).
fn tile_of(lon: f64, lat: f64, z: u8) -> (u32, u32) {
    let n = (1u32 << z) as f64;
    let x = ((lon + 180.0) / 360.0 * n).floor() as u32;
    let r = lat.to_radians();
    let y = ((1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * n).floor() as u32;
    (x, y)
}

#[tokio::main]
async fn main() -> Result<()> {
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
