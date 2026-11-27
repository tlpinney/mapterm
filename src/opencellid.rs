//! Mobile network cells from [OpenCelliD](https://opencellid.org) (CC BY-SA 4.0): where each
//! cell (a radio on a tower, one of several per mast) is estimated to be, from crowd-sourced
//! measurements, with its network, radio (GSM, UMTS, LTE, 5G NR) and range.
//!
//! OpenCelliD's live API is too tight for a map (4 km², 50 cells a request, 1,000 requests a
//! day), so mapterm uses its per-country files instead: `mapterm --cells-download us` fetches
//! them with your own key (from `--opencellid-key-cmd`, else `OPENCELLID_KEY`), at most once
//! in 12 hours each (OpenCelliD allows two downloads of a file a day), into the cache; `--cells`
//! reads them. Positions are estimates, not surveyed masts.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::Read;
use std::path::PathBuf;

/// The credit OpenCelliD asks for (CC BY-SA 4.0).
pub const CREDIT: &str = "Cell towers: OpenCelliD, CC BY-SA 4.0";
/// A file isn't downloaded again sooner than this (OpenCelliD allows two a day).
const MIN_AGAIN: std::time::Duration = std::time::Duration::from_secs(12 * 3600);
/// The US's mobile country codes (316 has no file).
pub const US: &[u16] = &[310, 311, 312, 313, 314, 315];

/// A cell's radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Radio {
    /// 2G.
    Gsm,
    /// 3G.
    Umts,
    /// 3G (CDMA networks).
    Cdma,
    /// 4G.
    Lte,
    /// 5G.
    Nr,
    /// Narrowband IoT.
    NbIot,
}

impl Radio {
    fn parse(s: &str) -> Option<Radio> {
        Some(match s {
            "GSM" => Radio::Gsm,
            "UMTS" => Radio::Umts,
            "CDMA" => Radio::Cdma,
            "LTE" => Radio::Lte,
            "NR" => Radio::Nr,
            "NBIOT" => Radio::NbIot,
            _ => return None,
        })
    }

    /// Its name.
    pub fn name(self) -> &'static str {
        match self {
            Radio::Gsm => "GSM (2G)",
            Radio::Umts => "UMTS (3G)",
            Radio::Cdma => "CDMA (3G)",
            Radio::Lte => "LTE (4G)",
            Radio::Nr => "5G NR",
            Radio::NbIot => "NB-IoT",
        }
    }

    /// Its color: 5G magenta, LTE blue, 3G green, 2G and the rest grey.
    pub fn color(self) -> &'static str {
        match self {
            Radio::Nr => "#e879f9",
            Radio::Lte => "#60a5fa",
            Radio::Umts | Radio::Cdma => "#4ade80",
            Radio::Gsm | Radio::NbIot => "#9ca3af",
        }
    }
}

/// A cell, as OpenCelliD estimates it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Its radio.
    pub radio: Radio,
    /// Mobile country code.
    pub mcc: u16,
    /// Mobile network code (the carrier).
    pub mnc: u16,
    /// Location (or tracking) area code.
    pub area: u32,
    /// Cell id.
    pub cell: u64,
    /// Degrees (estimated).
    pub lon: f32,
    /// Degrees (estimated).
    pub lat: f32,
    /// How far it's heard, meters (estimated).
    pub range: u32,
    /// How many measurements the estimate rests on.
    pub samples: u32,
    /// When it was last seen (Unix seconds).
    pub updated: i64,
}

/// The carrier of a US network code, for the big ones (about 95% of US cells); None for the
/// rest, shown by their code rather than guessed.
pub fn carrier(mcc: u16, mnc: u16) -> Option<&'static str> {
    Some(match (mcc, mnc) {
        (310, 260 | 240) => "T-Mobile",
        (310, 410) => "AT&T",
        (313, 100) => "AT&T FirstNet",
        (311, 480) => "Verizon",
        (311, 580) => "UScellular",
        (313, 340) => "Dish (Boost)",
        _ => return None,
    })
}

/// The cells in an OpenCelliD CSV (`radio,mcc,net,area,cell,unit,lon,lat,range,samples,
/// changeable,created,updated,averageSignal`), sorted by longitude for area lookups.
pub fn parse(csv: &str) -> Vec<Cell> {
    let mut v: Vec<Cell> = csv
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            if f.len() < 13 {
                return None;
            }
            let (lon, lat): (f32, f32) = (f[6].parse().ok()?, f[7].parse().ok()?);
            if lat.abs() > 90.0 || lon.abs() > 180.0 || (lat == 0.0 && lon == 0.0) {
                return None;
            }
            Some(Cell {
                radio: Radio::parse(f[0])?,
                mcc: f[1].parse().ok()?,
                mnc: f[2].parse().ok()?,
                area: f[3].parse().unwrap_or(0),
                cell: f[4].parse().unwrap_or(0),
                lon,
                lat,
                range: f[8].parse().unwrap_or(0),
                samples: f[9].parse().unwrap_or(0),
                updated: f[12].parse().unwrap_or(0),
            })
        })
        .collect();
    v.sort_by(|a, b| a.lon.total_cmp(&b.lon));
    v
}

/// Where the files are kept.
pub fn dir() -> PathBuf {
    crate::source::cache_root().join("opencellid")
}

/// Download the files for `mccs` with `key` into [`dir`], each at most once in 12 hours.
/// Returns, for each, what happened.
pub async fn download(mccs: &[u16], key: &str) -> Vec<(u16, Result<String>)> {
    let mut out = Vec::new();
    for &m in mccs {
        out.push((m, download_one(m, key).await));
    }
    out
}

async fn download_one(mcc: u16, key: &str) -> Result<String> {
    let path = dir().join(format!("{mcc}.csv.gz"));
    if let Some(age) = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok())
        && age < MIN_AGAIN
    {
        return Ok(format!("kept (downloaded {} ago; OpenCelliD allows two a day)", crate::sources::span(age.as_secs_f64())));
    }
    let url = format!("https://opencellid.org/ocid/downloads?token={key}&type=mcc&file={mcc}.csv.gz");
    let got = crate::source::get_limited(&crate::source::http_client(), &url, &[], 2 << 30).await;
    // The URL holds the key: errors are reported without it.
    let bytes = match got {
        Ok(Some(b)) => b,
        Ok(None) => bail!("no file for MCC {mcc}"),
        Err(e) => bail!("{}", format!("{e:#}").replace(key, "<key>")),
    };
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        // OpenCelliD answers errors (a bad key, too many downloads) in JSON or text.
        bail!("OpenCelliD: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).replace(key, "<key>").trim());
    }
    std::fs::create_dir_all(dir())?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(format!("{:.1} MB", bytes.len() as f64 / 1e6))
}

/// The cells in the downloaded files, sorted by longitude (reading them is CPU work: call
/// from a blocking task).
pub fn load() -> Result<Vec<Cell>> {
    let mut all = Vec::new();
    let entries = std::fs::read_dir(dir()).with_context(|| "no cells downloaded yet: mapterm --cells-download us")?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "gz") {
            let mut s = String::new();
            flate2::read::GzDecoder::new(std::fs::File::open(&p)?).read_to_string(&mut s).with_context(|| format!("reading {}", p.display()))?;
            all.extend(parse(&s));
        }
    }
    if all.is_empty() {
        bail!("no cells downloaded yet: mapterm --cells-download us");
    }
    all.sort_by(|a, b| a.lon.total_cmp(&b.lon));
    Ok(all)
}

/// The cells in (west, south, east, north), at most `max` (the most measured first).
pub fn in_view(cells: &[Cell], bounds: (f64, f64, f64, f64), max: usize) -> Vec<&Cell> {
    let (w, s, e, n) = bounds;
    let start = cells.partition_point(|c| (c.lon as f64) < w);
    let end = cells.partition_point(|c| (c.lon as f64) <= e);
    let mut v: Vec<&Cell> = cells[start..end].iter().filter(|c| (c.lat as f64) >= s && (c.lat as f64) <= n).collect();
    if v.len() > max {
        v.sort_by_key(|c| std::cmp::Reverse(c.samples));
        v.truncate(max);
    }
    v
}

/// Cells as GeoJSON points: ▴ in their radio's color, the carrier from zoom 15.
pub fn points(cells: &[&Cell]) -> Value {
    let features: Vec<Value> = cells
        .iter()
        .map(|c| {
            let net = carrier(c.mcc, c.mnc).map(str::to_string).unwrap_or_else(|| format!("{}-{}", c.mcc, c.mnc));
            json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [c.lon, c.lat]},
                "properties": {
                    "glyph": "▴", "color": c.radio.color(), "label": net, "radio": c.radio.name(), "network": net,
                    "mcc": c.mcc, "mnc": c.mnc, "area": c.area, "cell": c.cell, "range_m": c.range, "samples": c.samples,
                    "kind": "mobile cell (estimated position)",
                }
            })
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// The style layer: ▴ colored by radio, the carrier from zoom 15.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source)
        .glyph(json!(["get", "glyph"]))
        .text(json!(["step", ["zoom"], "", 15, ["get", "label"]]))
        .color("#9ca3af")
        .paint("icon-color", json!(["get", "color"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "\
GSM,310,260,307,17022,0,-120.2461,36.1916,11049,9,1,1363987473,1751301121,0
LTE,310,410,21250,45008913,0,-83.6967,41.6888,4374,34,1,1372103025,1794679019,0
NR,311,480,1,99,0,-77.41,39.41,500,120,1,1700000000,1794000000,0
WIFI,310,1,1,1,0,-77.0,39.0,1,1,1,1,1,0
LTE,310,410,1,1,0,0,0,1,1,1,1,1,0
";

    #[test]
    fn reads_cells() {
        let c = parse(CSV);
        assert_eq!(c.len(), 3, "an unknown radio and 0,0 left out");
        assert!(c.windows(2).all(|w| w[0].lon <= w[1].lon), "by longitude");
        let nr = c.iter().find(|x| x.radio == Radio::Nr).unwrap();
        assert_eq!((nr.mcc, nr.mnc, nr.range, nr.samples), (311, 480, 500, 120));
        assert_eq!(carrier(311, 480), Some("Verizon"));
        assert_eq!(carrier(310, 410), Some("AT&T"));
        assert_eq!(carrier(310, 260), Some("T-Mobile"));
        assert_eq!(carrier(310, 999), None);
    }

    #[test]
    fn cells_in_view() {
        let c = parse(CSV);
        let v = in_view(&c, (-78.0, 39.0, -77.0, 40.0), 10);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].radio, Radio::Nr);
        let all = in_view(&c, (-180.0, -90.0, 180.0, 90.0), 2);
        assert_eq!(all.len(), 2, "capped");
        assert_eq!(all[0].samples, 120, "the most measured kept");
        let p = points(&v);
        assert_eq!(p["features"][0]["properties"]["network"], "Verizon");
        assert_eq!(p["features"][0]["properties"]["color"], "#e879f9");
    }
}
