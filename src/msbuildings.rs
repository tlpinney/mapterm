//! Microsoft's Global ML Building Footprints, as Microsoft keeps them up to date
//! (github.com/microsoft/GlobalMLBuildingFootprints; refreshed every few weeks, with heights
//! where known). A `dataset-links.csv` lists one gzipped GeoJSON-lines file per zoom-9
//! quadkey (about 78 km across, up to a few hundred MB).
//!
//! Gzip can't be read in parts, so the first time the view touches a tile its file is
//! downloaded once and split on disk into 16 x 16 pieces (zoom 13, a few km across) under
//! `~/.cache/mapterm/msbuildings`; panning then reads only the pieces in view. Used through
//! [`crate::geoparquet::Dataset`], which loads them as the map moves.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};

use crate::geoparquet::Bbox;

/// The current release's index (Microsoft moves it with each refresh; the README's list of
/// updates links the newest).
pub const LATEST: &str = "https://bfppub.blob.core.windows.net/$web/2026-08-13/dataset-links.csv";

/// Pieces per tile side: zoom-9 tiles split at zoom 13.
pub const SPLIT: u32 = 16;

/// A tile in the index.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    /// Region name (country or area), as Microsoft lists it.
    pub location: String,
    /// Bing quadkey.
    pub quadkey: String,
    /// Its gzipped GeoJSON-lines file.
    pub url: String,
}

/// Whether `spec` names Microsoft's footprints: `microsoft-buildings`, or a
/// `dataset-links.csv` URL.
pub fn is_spec(spec: &str) -> bool {
    spec.eq_ignore_ascii_case("microsoft-buildings") || spec.split(['?', '#']).next().unwrap_or(spec).ends_with("dataset-links.csv")
}

/// The index's URL for a spec ([`LATEST`] for `microsoft-buildings`).
pub fn index_url(spec: &str) -> &str {
    if spec.eq_ignore_ascii_case("microsoft-buildings") { LATEST } else { spec }
}

/// Parse `dataset-links.csv` (Location,QuadKey,Url,Size,UploadDate).
pub fn parse_index(csv: &str) -> Result<Vec<Tile>> {
    let mut lines = csv.lines();
    let head = lines.next().context("empty index")?;
    let cols: Vec<&str> = head.split(',').map(str::trim).collect();
    let at = |n: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(n)).with_context(|| format!("index without a {n} column"));
    let (loc, qk, url) = (at("Location")?, at("QuadKey")?, at("Url")?);
    let mut out = Vec::new();
    for l in lines.filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = l.split(',').collect();
        let (Some(location), Some(quadkey), Some(url)) = (f.get(loc), f.get(qk), f.get(url)) else { continue };
        if quadkey_tile(quadkey).is_none() || !url.starts_with("https://") {
            continue;
        }
        out.push(Tile { location: location.to_string(), quadkey: quadkey.to_string(), url: url.to_string() });
    }
    if out.is_empty() {
        bail!("no tiles in the index");
    }
    Ok(out)
}

/// A quadkey's tile (zoom, x, y).
pub fn quadkey_tile(qk: &str) -> Option<(u8, u32, u32)> {
    if qk.is_empty() || qk.len() > 24 {
        return None;
    }
    let (mut x, mut y) = (0u32, 0u32);
    for c in qk.bytes() {
        let d = c.checked_sub(b'0').filter(|d| *d < 4)? as u32;
        x = x << 1 | (d & 1);
        y = y << 1 | (d >> 1);
    }
    Some((qk.len() as u8, x, y))
}

/// A tile's lon/lat box.
pub fn tile_bbox(z: u8, x: u32, y: u32) -> Bbox {
    let n = (1u64 << z) as f64;
    let (w, nth) = crate::render::to_lonlat(x as f64 / n, y as f64 / n);
    let (e, s) = crate::render::to_lonlat((x + 1) as f64 / n, (y + 1) as f64 / n);
    [w, s, e, nth]
}

/// The boxes of a tile's pieces (row by row from the north-west), padded a little: a building
/// is filed by its first corner and may reach over the edge.
pub fn piece_boxes(z: u8, x: u32, y: u32) -> Vec<Bbox> {
    let mut out = Vec::new();
    for py in 0..SPLIT {
        for px in 0..SPLIT {
            let b = tile_bbox(z + 4, x * SPLIT + px, y * SPLIT + py);
            let pad = (b[2] - b[0]) * 0.05;
            out.push([b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]);
        }
    }
    out
}

/// Where a tile's pieces are kept.
pub fn tile_dir(url: &str) -> PathBuf {
    crate::source::cache_root().join("msbuildings").join(crate::source::hash(url))
}

/// The pieces a split tile has (true where it holds buildings), if it's been split.
pub fn split_done(dir: &Path) -> Option<Vec<bool>> {
    let b = std::fs::read(dir.join("pieces.json")).ok()?;
    let v: Json = serde_json::from_slice(&b).ok()?;
    let have: Vec<bool> = v["have"].as_array()?.iter().map(|x| x.as_bool().unwrap_or(false)).collect();
    (have.len() == (SPLIT * SPLIT) as usize).then_some(have)
}

/// Split a downloaded tile (`gz`, zoom-9 tile z/x/y) into pieces in `dir`, each building
/// filed by its first corner; returns which pieces hold buildings.
pub fn split(gz: &Path, dir: &Path, (z, x, y): (u8, u32, u32)) -> Result<Vec<bool>> {
    std::fs::create_dir_all(dir)?;
    let file = std::fs::File::open(gz)?;
    let lines = std::io::BufReader::new(flate2::read::MultiGzDecoder::new(std::io::BufReader::new(file))).lines();
    let n = ((1u64 << z) * SPLIT as u64) as f64;
    let mut out: Vec<Option<flate2::write::GzEncoder<std::io::BufWriter<std::fs::File>>>> = (0..SPLIT * SPLIT).map(|_| None).collect();
    for line in lines {
        let line = line.context("reading the tile")?;
        let Some((lon, lat)) = first_corner(&line) else { continue };
        let (wx, wy) = crate::render::to_world(lon, lat);
        let (px, py) = ((wx * n).floor() as i64 - (x * SPLIT) as i64, (wy * n).floor() as i64 - (y * SPLIT) as i64);
        let (px, py) = (px.clamp(0, SPLIT as i64 - 1) as usize, py.clamp(0, SPLIT as i64 - 1) as usize);
        let w = &mut out[py * SPLIT as usize + px];
        if w.is_none() {
            let f = std::fs::File::create(dir.join(format!("{}.jsonl.gz", py * SPLIT as usize + px)))?;
            *w = Some(flate2::write::GzEncoder::new(std::io::BufWriter::new(f), flate2::Compression::fast()));
        }
        let w = w.as_mut().expect("just made");
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
    }
    let mut have = Vec::with_capacity(out.len());
    for w in out {
        have.push(w.is_some());
        if let Some(w) = w {
            w.finish()?.flush()?;
        }
    }
    std::fs::write(dir.join("pieces.json"), serde_json::to_vec(&json!({"have": have}))?)?;
    Ok(have)
}

/// A line's first coordinate pair, found without parsing the whole line.
fn first_corner(line: &str) -> Option<(f64, f64)> {
    let at = line.find("\"coordinates\"")?;
    let rest = &line[at + 13..];
    let start = rest.find(|c: char| c == '-' || c.is_ascii_digit())?;
    let mut nums = rest[start..].split(|c: char| c == ',' || c == ']' || c.is_whitespace()).filter(|s| !s.is_empty());
    let lon: f64 = nums.next()?.parse().ok()?;
    let lat: f64 = nums.next()?.parse().ok()?;
    (lon.is_finite() && lat.is_finite()).then_some((lon, lat))
}

/// A piece's buildings, as GeoJSON features. An unknown height (Microsoft writes -1) is
/// left out.
pub fn read_piece(dir: &Path, piece: usize) -> Result<Vec<crate::geojson::Feature>> {
    let path = dir.join(format!("{piece}.jsonl.gz"));
    let f = std::fs::File::open(&path).with_context(|| format!("{}", path.display()))?;
    let mut features = Vec::new();
    for line in std::io::BufReader::new(flate2::read::MultiGzDecoder::new(f)).lines() {
        let line = line?;
        let Ok(mut v) = serde_json::from_str::<Json>(&line) else { continue };
        if v["properties"]["height"].as_f64().is_some_and(|h| h < 0.0)
            && let Some(p) = v["properties"].as_object_mut()
        {
            p.remove("height");
        }
        features.push(v);
    }
    Ok(crate::geojson::features(&json!({"type": "FeatureCollection", "features": features}))?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quadkeys() {
        assert_eq!(quadkey_tile("032010032"), Some((9, 146, 195)), "DC");
        assert_eq!(quadkey_tile(""), None);
        assert_eq!(quadkey_tile("0124"), None);
        let b = tile_bbox(9, 146, 195);
        assert!(b[0] < -77.03 && b[2] > -77.03 && b[1] < 38.9 && b[3] > 38.9, "{b:?}");
        assert_eq!(piece_boxes(9, 146, 195).len(), 256);
    }

    #[test]
    fn index_and_pieces() {
        let csv = "Location,QuadKey,Url,Size,UploadDate\nUnitedStates,032010032,https://x/a.csv.gz,77.5MB,2026-08-13\nBad,9,https://x/b,1KB,x\n";
        let t = parse_index(csv).unwrap();
        assert_eq!(t, vec![Tile { location: "UnitedStates".into(), quadkey: "032010032".into(), url: "https://x/a.csv.gz".into() }]);
        assert!(is_spec("microsoft-buildings") && is_spec("https://h/$web/2026-08-13/dataset-links.csv") && !is_spec("x.parquet"));
        // Split a tiny tile and read a piece back.
        let dir = std::env::temp_dir().join(format!("mapterm-msb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gz = dir.join("t.gz");
        let mut e = flate2::write::GzEncoder::new(std::fs::File::create(&gz).unwrap(), flate2::Compression::fast());
        for (lon, lat, h) in [(-77.03, 38.90, 12.5), (-77.031, 38.901, -1.0)] {
            writeln!(e, r#"{{"type": "Feature", "properties": {{"height": {h}, "confidence": 0.9}}, "geometry": {{"type": "Polygon", "coordinates": [[[{lon}, {lat}], [{}, {lat}], [{}, {}], [{lon}, {lat}]]]}}}}"#, lon + 0.0001, lon + 0.0001, lat + 0.0001).unwrap();
        }
        e.finish().unwrap();
        let pieces = dir.join("p");
        let have = split(&gz, &pieces, (9, 146, 195)).unwrap();
        assert_eq!(have.iter().filter(|h| **h).count(), 1, "both buildings in one piece");
        assert_eq!(split_done(&pieces), Some(have.clone()));
        let p = have.iter().position(|h| *h).unwrap();
        let f = read_piece(&pieces, p).unwrap();
        assert_eq!(f.len(), 2);
        let boxes = piece_boxes(9, 146, 195);
        assert!(boxes[p][0] < -77.03 && boxes[p][2] > -77.03, "filed in the right piece");
        for name in std::fs::read_dir(&pieces).unwrap().flatten() {
            let _ = std::fs::remove_file(name.path());
        }
        let _ = (std::fs::remove_dir(&pieces), std::fs::remove_file(&gz), std::fs::remove_dir(&dir));
    }
}
