//! Weather radar from [RainViewer](https://www.rainviewer.com): precipitation worldwide, the
//! last two hours in 10-minute scans, free and without a key for personal, educational and
//! small community projects. Its terms ask for the credit "Weather data by RainViewer" with
//! a link, and allow 100 requests a minute per address: callers should stay well under that
//! (the viewer allows itself 60), and cache.
//!
//! [`frames`] reads the list of scans; [`frame_source`] makes a scan an overlay
//! [`RasterSource`] for a [`TimeRaster`](crate::timelayer::TimeRaster). Since 2026 RainViewer
//! serves zoom 7 at most, one color scheme (Universal Blue), and past scans only (no
//! forecast).
//!
//! The index checks come from skycast (the same author's terminal weather app): only
//! RainViewer's own tile host is used whatever the index says, and only plain
//! `/v2/radar/<id>` paths, since they're put into tile URLs.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::raster::RasterSource;

const INDEX: &str = "https://api.rainviewer.com/public/weather-maps.json";
/// The only tile host used.
pub const TILE_HOST: &str = "https://tilecache.rainviewer.com";
/// RainViewer's credit, as its terms ask.
pub const CREDIT: &str = "Weather data by RainViewer (rainviewer.com)";
/// Tiles above this zoom are a "Zoom Level Not Supported" image.
pub const MAX_ZOOM: u8 = 7;
/// Seconds between scans.
pub const SCAN_SECS: f64 = 600.0;

/// One radar scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan {
    /// When, Unix seconds.
    pub time: i64,
    /// Its tile path, `/v2/radar/<id>`.
    pub path: String,
}

/// The scans RainViewer has now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scans {
    /// When RainViewer made this list, Unix seconds: a new value means new scans.
    pub generated: i64,
    /// Oldest first; the last is the latest.
    pub past: Vec<Scan>,
}

/// The scans from RainViewer's index JSON, checked: the tile host must be RainViewer's own
/// and paths plain `/v2/radar/<id>` (others are left out).
pub fn parse(text: &str) -> Result<Scans> {
    let v: Value = serde_json::from_str(text).context("RainViewer index")?;
    let host = v.get("host").and_then(Value::as_str).unwrap_or_default();
    if host != TILE_HOST {
        bail!("RainViewer index: unexpected tile host {host:?}");
    }
    let mut past: Vec<Scan> = v
        .pointer("/radar/past")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|f| Some(Scan { time: f.get("time")?.as_i64()?, path: f.get("path")?.as_str()?.to_string() }))
                .filter(|s| valid_path(&s.path))
                .collect()
        })
        .unwrap_or_default();
    past.sort_by_key(|s| s.time);
    if past.is_empty() {
        bail!("RainViewer index: no radar scans");
    }
    Ok(Scans { generated: v.get("generated").and_then(Value::as_i64).unwrap_or(0), past })
}

fn valid_path(path: &str) -> bool {
    path.strip_prefix("/v2/radar/").is_some_and(|id| !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// RainViewer's scans now (one small request).
pub async fn frames() -> Result<Scans> {
    let resp = crate::source::http_client().get(INDEX).send().await.context("RainViewer")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("RainViewer answered HTTP {status}");
    }
    parse(&resp.text().await.context("RainViewer")?)
}

/// A scan as an overlay raster source: 512-pixel tiles (a quarter of the requests 256 would
/// take), Universal Blue, smoothed, no snow colors; to zoom 7; kept in memory only (a scan is
/// two hours old by the time it's dropped). Tiles that aren't transparent are RainViewer's
/// error image and count as empty.
pub fn frame_source(scan: &Scan) -> Result<Arc<RasterSource>> {
    if !valid_path(&scan.path) {
        bail!("not a RainViewer scan path: {:?}", scan.path);
    }
    let mut s = RasterSource::new(&format!("{TILE_HOST}{}/512/{{z}}/{{x}}/{{y}}/2/1_0.png", scan.path), false, MAX_ZOOM)?;
    s.tile_px = 512;
    s.overlay = true;
    s.require_alpha = true;
    s.cache = false;
    s.name = "radar".into();
    s.attribution = CREDIT.into();
    Ok(Arc::new(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX_JSON: &str = r#"{"version":"2.0","generated":1764100205,"host":"https://tilecache.rainviewer.com",
      "radar":{"past":[{"time":1764093000,"path":"/v2/radar/6e6c1c4e2f1a"},{"time":1764092400,"path":"/v2/radar/5d5b0b3d1e09"},
      {"time":1764093600,"path":"/v2/radar/../../evil"}],"nowcast":[]},"satellite":{"infrared":[]}}"#;

    #[test]
    fn reads_and_checks_the_index() {
        let s = parse(INDEX_JSON).unwrap();
        assert_eq!(s.generated, 1764100205);
        assert_eq!(s.past.len(), 2, "the path that isn't plain is left out");
        assert!(s.past[0].time < s.past[1].time, "oldest first");
        let src = frame_source(&s.past[1]).unwrap();
        assert!(src.overlay && src.require_alpha && !src.cache);
        assert_eq!((src.tile_px, src.max_zoom), (512, 7));
        assert_eq!(src.attribution, CREDIT);
    }

    #[test]
    fn only_rainviewers_tile_host() {
        let other = INDEX_JSON.replace("https://tilecache.rainviewer.com", "https://evil.example");
        assert!(parse(&other).is_err());
        assert!(parse(r#"{"host":"https://tilecache.rainviewer.com","radar":{"past":[]}}"#).is_err(), "no scans");
        assert!(frame_source(&Scan { time: 0, path: "/v2/radar/a?b=c".into() }).is_err());
    }
}
