//! Antenna structures: the FCC's [Antenna Structure Registration](https://www.fcc.gov/wireless/systems-utilities/antenna-structure-registration)
//! (ASR) database, the towers and other structures registered with the FCC (tall ones, or
//! ones near airports): where each is, exactly, how tall, what kind, and who owns it. US
//! government data (public domain); a weekly file of every registration, ~38 MB zipped.
//!
//! The zip holds pipe-separated tables joined by each registration's system id: `CO` its
//! coordinates, `RA` the registration (status, structure type, heights, address), `EN` its
//! entities (the owner, `O`). Only constructed structures are kept.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

/// The weekly file of every registration.
pub const URL: &str = "https://data.fcc.gov/download/pub/uls/complete/r_tower.zip";
/// The credit.
pub const CREDIT: &str = "Antenna structures: FCC ASR";
/// How long the file is kept before it's read again (the FCC builds it weekly).
const KEEP: std::time::Duration = std::time::Duration::from_secs(7 * 86400);

/// A registered structure.
#[derive(Clone, Debug, PartialEq)]
pub struct Tower {
    /// The FCC registration number.
    pub reg: String,
    /// Degrees.
    pub lat: f64,
    /// Degrees.
    pub lon: f64,
    /// Overall height above ground, meters.
    pub height: Option<f64>,
    /// Ground elevation, meters.
    pub ground: Option<f64>,
    /// Its structure type code (`TOWER`, `MTOWER`, `POLE`, ...).
    pub kind: String,
    /// City and state, as registered.
    pub place: String,
    /// The owner.
    pub owner: Option<String>,
}

impl Tower {
    /// Its structure type in words.
    pub fn kind_name(&self) -> &str {
        kind_name(&self.kind)
    }
}

/// A structure type code in words (the common ones; others as they are).
pub fn kind_name(code: &str) -> &str {
    match code {
        "TOWER" => "tower",
        "LTOWER" => "lattice tower",
        "MTOWER" => "monopole",
        "GTOWER" => "guyed tower",
        "2TOWER" => "two towers",
        "POLE" => "pole",
        "UPOLE" => "utility pole",
        "BPOLE" => "pole on a building",
        "MAST" => "mast",
        "BMAST" => "mast on a building",
        "B" => "building",
        "BANT" => "antenna on a building",
        "BTWR" => "tower on a building",
        "TANK" => "water tank",
        "SILO" => "silo",
        "TREE" => "tower disguised as a tree",
        "RIG" => "oil or gas rig",
        "PIPE" => "pipe",
        "NNTANN" => "antenna on a structure",
        c if c.contains("TA") => "array of towers",
        "" => "structure",
        c => c,
    }
}

/// The entries of a zip file (stored or deflated) that `want` accepts, by name. Enough of the
/// format for the FCC's files: read from the central directory, no zip64, no encryption.
pub fn unzip(data: &[u8], want: impl Fn(&str) -> bool) -> Result<HashMap<String, Vec<u8>>> {
    let le16 = |b: &[u8], i: usize| -> Option<usize> { Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize) };
    let le32 = |b: &[u8], i: usize| -> Option<usize> { Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize) };
    // The end-of-central-directory record, within the last 64 KiB (and its comment).
    let eocd = (0..data.len().saturating_sub(21)).rev().take(70_000).find(|&i| data[i..].starts_with(b"PK\x05\x06")).context("not a zip file")?;
    let count = le16(data, eocd + 10).context("zip: short")?;
    let mut at = le32(data, eocd + 16).context("zip: short")?;
    let mut out = HashMap::new();
    for _ in 0..count {
        if !data.get(at..).is_some_and(|d| d.starts_with(b"PK\x01\x02")) {
            bail!("zip: bad central directory");
        }
        let method = le16(data, at + 10).context("zip: short")?;
        let size = le32(data, at + 20).context("zip: short")?;
        let (name_len, extra_len, comment_len) =
            (le16(data, at + 28).context("zip")?, le16(data, at + 30).context("zip")?, le16(data, at + 32).context("zip")?);
        let local = le32(data, at + 42).context("zip: short")?;
        let name = String::from_utf8_lossy(data.get(at + 46..at + 46 + name_len).context("zip: short")?).to_string();
        at += 46 + name_len + extra_len + comment_len;
        if !want(&name) {
            continue;
        }
        // The data follows the local header (its own name and extra field lengths).
        let start = local + 30 + le16(data, local + 26).context("zip")? + le16(data, local + 28).context("zip")?;
        let raw = data.get(start..start + size).context("zip: entry past the end")?;
        let bytes = match method {
            0 => raw.to_vec(),
            8 => {
                let mut v = Vec::new();
                flate2::read::DeflateDecoder::new(raw).read_to_end(&mut v).with_context(|| format!("zip: inflating {name}"))?;
                v
            }
            m => bail!("zip: {name} uses compression {m}"),
        };
        out.insert(name, bytes);
    }
    Ok(out)
}

/// The constructed structures from the registration tables (`CO`, `RA`, `EN` text).
pub fn parse(co: &str, ra: &str, en: &str) -> Vec<Tower> {
    // Each registration's coordinates (the structure's own, type T): by system id.
    let mut at: HashMap<&str, (f64, f64)> = HashMap::new();
    for l in co.lines() {
        let f: Vec<&str> = l.split('|').collect();
        if f.len() < 16 || f[5] != "T" {
            continue;
        }
        let secs = |s: &str| s.trim().parse::<f64>().ok();
        let (Some(lat), Some(lon)) = (secs(f[10]), secs(f[15])) else { continue };
        let lat = if f[9] == "S" { -lat } else { lat } / 3600.0;
        let lon = if f[14] == "W" { -lon } else { lon } / 3600.0;
        if lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat, lon) != (0.0, 0.0) {
            at.entry(f[4]).or_insert((lat, lon));
        }
    }
    // Owners, by system id.
    let mut owner: HashMap<&str, &str> = HashMap::new();
    for l in en.lines() {
        let f: Vec<&str> = l.split('|').collect();
        if f.len() > 9 && f[5] == "O" && !f[9].trim().is_empty() {
            owner.insert(f[4], f[9].trim());
        }
    }
    let num = |s: &str| s.trim().parse::<f64>().ok().filter(|v| *v > 0.0);
    ra.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').collect();
            // Constructed ones (not only granted, cancelled or dismantled).
            if f.len() < 33 || f[8] != "C" {
                return None;
            }
            let &(lat, lon) = at.get(f[4])?;
            let place = [f[24].trim(), f[25].trim()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(", ");
            Some(Tower {
                reg: f[3].trim().to_string(),
                lat,
                lon,
                height: num(f[30]),
                ground: num(f[29]),
                kind: f[32].trim().to_string(),
                place,
                owner: owner.get(f[4]).map(|s| s.to_string()),
            })
        })
        .collect()
}

/// The constructed structures from the FCC's zip.
pub fn from_zip(data: &[u8]) -> Result<Vec<Tower>> {
    let files = unzip(data, |n| matches!(n, "CO.dat" | "RA.dat" | "EN.dat"))?;
    let text = |n: &str| files.get(n).map(|b| String::from_utf8_lossy(b).to_string()).with_context(|| format!("the FCC zip has no {n}"));
    Ok(parse(&text("CO.dat")?, &text("RA.dat")?, &text("EN.dat")?))
}

/// Where the zip is kept.
fn cache_file() -> PathBuf {
    crate::source::cache_root().join("fcc").join("r_tower.zip")
}

/// The constructed structures: from the copy on disk while it's under a week old, else from
/// the FCC (kept for next time; the old copy if the FCC can't be reached).
pub async fn load() -> Result<Vec<Tower>> {
    let path = cache_file();
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < KEEP);
    let data = match fresh.then(|| std::fs::read(&path).ok()).flatten() {
        Some(d) => d,
        None => match crate::source::get_limited(&crate::source::http_client(), URL, &[], 200 << 20).await {
            Ok(Some(b)) => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, &b);
                b
            }
            other => std::fs::read(&path).map_err(|_| other.err().unwrap_or_else(|| anyhow::anyhow!("not found"))).context("the FCC's antenna structures")?,
        },
    };
    tokio::task::spawn_blocking(move || from_zip(&data)).await?
}

/// The structures in (west, south, east, north) as GeoJSON points: † with the height.
pub fn points(towers: &[Tower], bounds: (f64, f64, f64, f64)) -> Value {
    let (w, s, e, n) = bounds;
    let features: Vec<Value> = towers
        .iter()
        .filter(|t| t.lon >= w && t.lon <= e && t.lat >= s && t.lat <= n)
        .map(|t| {
            json!({
                "type": "Feature",
                "id": format!("asr:{}", t.reg),
                "geometry": {"type": "Point", "coordinates": [t.lon, t.lat]},
                "properties": {
                    "glyph": "†", "label": t.height.map(|h| format!("{h:.0} m")).unwrap_or_default(),
                    "registration": t.reg, "structure": t.kind_name(), "height_m": t.height, "ground_m": t.ground,
                    "place": t.place, "owner": t.owner, "kind": "antenna structure",
                }
            })
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// The style layer: † in steel blue, the height from zoom 12.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source).glyph(json!(["get", "glyph"])).text(json!(["step", ["zoom"], "", 12, ["get", "label"]])).color("#8aa4c8")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CO: &str =
        "CO|REG|A1377749|1000131|96999|T|39|49|54.0|N|143394.0|89|38|52.0|W|322732.0||\nCO|REG|A0000001|1000999|90000|T|1|0|0.0|N|3600.0|1|0|0.0|E|3600.0||\n";
    const RA: &str = "RA|REG|A1377749|1000131|96999|OC|AU|I|C|01/23/2026|02/19/2026|02/19/2026|07/25/1996||02/19/2026|C||Bridget||Carter||Sr. Specialist|Y|600 FT NE FROM THE INT|SPRINGFIELD|IL|17167|62702|31.0|178.0|34.7|212.7|POLE|07/09/1996|96-AGL-1964-OE||1|NONE|1||N|N|02/19/2026||||||\nRA|REG|A0000001|1000999|90000|OC|AU|I|G|01/23/2026|||||||||||||Y|x|NOWHERE|KS|1|1|1.0|300.0|10.0|310.0|TOWER|||||||||||||||\n";
    const EN: &str = "EN|REG|A1377749|1000131|96999|O|L||L00008376|American Towers LLC|||||7819264500||x@y.com|10 Presidential Way|||Woburn|MA|01801|FAA|0011498342\nEN|REG|A1377749|1000131|96999|R||||Someone Else|||||1||||||||||\n";

    #[test]
    fn joins_the_tables() {
        let t = parse(CO, RA, EN);
        assert_eq!(t.len(), 1, "only the constructed one");
        let t = &t[0];
        assert_eq!(t.reg, "1000131");
        assert!((t.lat - 143394.0 / 3600.0).abs() < 1e-9 && (t.lon - -322732.0 / 3600.0).abs() < 1e-9);
        assert_eq!((t.height, t.ground, t.kind_name()), (Some(34.7), Some(178.0), "pole"));
        assert_eq!((t.place.as_str(), t.owner.as_deref()), ("SPRINGFIELD, IL", Some("American Towers LLC")));
        assert_eq!(kind_name("3TA2"), "array of towers");
    }

    #[test]
    fn reads_a_zip() {
        // A zip made the way the FCC's is: deflated entries and a central directory.
        use std::io::Write;
        let mut z = Vec::new();
        let mut central = Vec::new();
        for (name, body) in [("CO.dat", CO), ("RA.dat", RA), ("EN.dat", EN), ("HS.dat", "x")] {
            let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(body.as_bytes()).unwrap();
            let data = enc.finish().unwrap();
            let offset = z.len() as u32;
            z.extend_from_slice(b"PK\x03\x04");
            z.extend_from_slice(&[20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            z.extend_from_slice(&(data.len() as u32).to_le_bytes());
            z.extend_from_slice(&(body.len() as u32).to_le_bytes());
            z.extend_from_slice(&(name.len() as u16).to_le_bytes());
            z.extend_from_slice(&0u16.to_le_bytes());
            z.extend_from_slice(name.as_bytes());
            z.extend_from_slice(&data);
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(body.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd = z.len() as u32;
        z.extend_from_slice(&central);
        z.extend_from_slice(b"PK\x05\x06\0\0\0\0\x04\0\x04\0");
        z.extend_from_slice(&(central.len() as u32).to_le_bytes());
        z.extend_from_slice(&cd.to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        let files = unzip(&z, |n| n != "HS.dat").unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files["RA.dat"], RA.as_bytes());
        assert_eq!(from_zip(&z).unwrap().len(), 1);
        assert!(unzip(b"not a zip", |_| true).is_err());
    }

    #[test]
    fn points_in_view() {
        let t = parse(CO, RA, EN);
        let p = points(&t, (-90.0, 39.0, -89.0, 40.0));
        assert_eq!(p["features"][0]["properties"]["label"], "35 m");
        assert_eq!(points(&t, (0.0, 0.0, 1.0, 1.0))["features"].as_array().unwrap().len(), 0);
    }
}
