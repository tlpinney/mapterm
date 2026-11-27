//! Weather balloon launch sites: the upper-air stations that launch radiosondes, from NOAA's
//! [Integrated Global Radiosonde Archive](https://www.ncei.noaa.gov/products/weather-balloon/integrated-global-radiosonde-archive)
//! (IGRA) station list (public domain): every station that has reported soundings, where it
//! is, and the years it did. Those still reporting are today's launch sites (about 900, 92 in
//! the US). Most launch at 00 and 12 UTC, some more often.
//!
//! The list (~260 KB) is kept on disk and read again after 30 days.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The station list.
pub const LIST: &str = "https://www.ncei.noaa.gov/data/integrated-global-radiosonde-archive/doc/igra2-station-list.txt";
/// The credit.
pub const CREDIT: &str = "Launch sites: NOAA NCEI IGRA";
/// A station that reported in this year or later is launching now (the list's last year
/// lags a little).
const ACTIVE_SINCE: i32 = 2025;
/// How long the list is kept before it's read again.
const KEEP: std::time::Duration = std::time::Duration::from_secs(30 * 86400);
/// A balloon first heard within this distance (km) of a site, low (under [`LOW`] m), was
/// launched from it.
pub const NEAR_KM: f64 = 50.0;
/// See [`NEAR_KM`].
pub const LOW: f64 = 3000.0;

/// A launch site.
#[derive(Clone, Debug, PartialEq)]
pub struct Site {
    /// IGRA's id, such as `USM00072403`.
    pub id: String,
    /// Its WMO number (the id's last five digits), such as `72403`.
    pub wmo: String,
    /// Its name, as IGRA has it (`STERLING`).
    pub name: String,
    /// A US state, when it's in one.
    pub state: Option<String>,
    /// Degrees.
    pub lat: f64,
    /// Degrees.
    pub lon: f64,
    /// Meters.
    pub elevation: f64,
    /// The first and last years it reported.
    pub years: (i32, i32),
    /// How many soundings IGRA has from it.
    pub soundings: u32,
}

/// The sites in IGRA's station list (fixed columns), those still launching.
pub fn parse(text: &str) -> Vec<Site> {
    text.lines()
        .filter_map(|l| {
            let col = |a: usize, b: usize| l.get(a..b.min(l.len())).map(str::trim).filter(|s| !s.is_empty());
            let id = col(0, 11)?.to_string();
            let lat: f64 = col(12, 20)?.parse().ok()?;
            let lon: f64 = col(21, 30)?.parse().ok()?;
            let elevation: f64 = col(31, 37)?.parse().ok()?;
            let years = (col(72, 76)?.parse().ok()?, col(77, 81)?.parse().ok()?);
            let ok = years.1 >= ACTIVE_SINCE && lat.abs() <= 90.0 && lon.abs() <= 180.0 && elevation > -999.0;
            let state = col(38, 40).map(str::to_string);
            ok.then(|| Site {
                wmo: id.get(id.len().saturating_sub(5)..).unwrap_or("").to_string(),
                name: clean_name(col(41, 71).unwrap_or(""), state.as_deref()),
                state,
                id,
                lat,
                lon,
                elevation,
                years,
                soundings: col(82, 88).and_then(|n| n.parse().ok()).unwrap_or(0),
            })
        })
        .collect()
}

/// A station's name without the state it ends with (`DULLES INT.; VA.` is `DULLES INT`) or
/// trailing punctuation.
fn clean_name(name: &str, state: Option<&str>) -> String {
    let mut n = name.trim().trim_end_matches(['.', ';', ',', ' ']);
    if let Some(st) = state
        && let Some(rest) = n.strip_suffix(st)
        && rest.trim_end().ends_with([';', ','])
    {
        n = rest.trim_end().trim_end_matches([';', ',', '.', ' ']);
    }
    n.trim().to_string()
}

/// Where the list is kept.
fn cache_file() -> PathBuf {
    crate::source::cache_root().join("igra").join("igra2-station-list.txt")
}

/// The launch sites: from the copy on disk while it's under 30 days old, else from NOAA
/// (kept for next time; the old copy if NOAA can't be reached).
pub async fn load() -> Result<Vec<Site>> {
    let path = cache_file();
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < KEEP);
    if fresh && let Ok(text) = std::fs::read_to_string(&path) {
        return Ok(parse(&text));
    }
    match crate::source::get_limited(&crate::source::http_client(), LIST, &[], 8 << 20).await {
        Ok(Some(b)) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, &b);
            Ok(parse(&String::from_utf8_lossy(&b)))
        }
        other => match std::fs::read_to_string(&path) {
            Ok(text) => Ok(parse(&text)),
            Err(_) => Err(other.err().unwrap_or_else(|| anyhow::anyhow!("not found"))).context("NOAA's launch site list"),
        },
    }
}

/// The site a balloon first heard at (`lon`, `lat`), `alt` meters up, was launched from: the
/// nearest within [`NEAR_KM`], when it was heard low (under [`LOW`]).
pub fn launched_from(sites: &[Site], lon: f64, lat: f64, alt: f64) -> Option<&Site> {
    if alt > LOW {
        return None;
    }
    sites
        .iter()
        .map(|s| (crate::adsb::distance_nm((lon, lat), (s.lon, s.lat)) * 1.852, s))
        .filter(|(km, _)| *km <= NEAR_KM)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, s)| s)
}

/// The site drawn nearest the braille dot `dot` in `vp`, within `within` dots.
pub fn nearest<'a>(sites: &'a [Site], vp: &crate::Viewport, dot: (f64, f64), within: f64) -> Option<&'a Site> {
    sites
        .iter()
        .map(|s| {
            let (x, y) = vp.dot_ll(s.lon, s.lat);
            (((x - dot.0).powi(2) + ((y - dot.1) / 2.0).powi(2)).sqrt(), s)
        })
        .filter(|(d, _)| *d <= within)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, s)| s)
}

/// The next of the usual launches (00 and 12 UTC; balloons go up about an hour before) after
/// `now`: Unix seconds of the synoptic hour.
pub fn next_launch(now: f64) -> f64 {
    let half_day = 43_200.0;
    ((now / half_day).floor() + 1.0) * half_day
}

/// The sites as GeoJSON points: ⊙ with the name.
pub fn points(sites: &[Site]) -> Value {
    let features: Vec<Value> = sites
        .iter()
        .map(|s| {
            json!({
                "type": "Feature",
                "id": format!("site:{}", s.id),
                "geometry": {"type": "Point", "coordinates": [s.lon, s.lat]},
                "properties": {"glyph": "⊙", "label": s.name, "id": s.id, "wmo": s.wmo, "kind": "weather balloon launch site"},
            })
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// The style layer: ⊙ in the balloons' grey-green, names from zoom 6.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source)
        .glyph(json!(["get", "glyph"]))
        .text(json!(["step", ["zoom"], "", 6, ["get", "label"]]))
        .color("#9aa0a6")
        .paint("icon-color", json!("#7ee787"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "\
USM00072403  38.9767  -77.4867   88.0 VA STERLING/WASHINGTON DULLES INT 1960 2026  71012
USM00072528  42.9411  -78.7189  218.4 NY BUFFALO/GREATER BUFFALO INT.;  1929 2026  80584
USW00013815  31.6000  -84.0833   68.0 GA ALBANY TURNER FIELD            1941 1942    372
ASM00094802 -34.9414  117.8022   71.0    ALBANY AIRPORT                 1965 2026  61035
";

    #[test]
    fn reads_the_active_sites() {
        let s = parse(LIST);
        assert_eq!(s.len(), 3, "the 1942 station isn't launching");
        assert_eq!((s[0].wmo.as_str(), s[0].state.as_deref(), s[0].years), ("72403", Some("VA"), (1960, 2026)));
        assert_eq!(s[0].name, "STERLING/WASHINGTON DULLES INT");
        assert_eq!(s[1].name, "BUFFALO/GREATER BUFFALO INT", "trailing punctuation dropped");
        assert_eq!(clean_name("WASHINGTON/DULLES INT.; VA.", Some("VA")), "WASHINGTON/DULLES INT");
        assert_eq!(clean_name("WALLOPS ISLAND; VA.", Some("VA")), "WALLOPS ISLAND");
        assert_eq!(clean_name("NORMAN VA", Some("VA")), "NORMAN VA", "only a separated state goes");
        assert_eq!((s[2].state.as_deref(), s[2].lat, s[2].soundings), (None, -34.9414, 61035));
    }

    #[test]
    fn balloons_are_matched_to_where_they_went_up() {
        let s = parse(LIST);
        assert_eq!(launched_from(&s, -77.40, 38.95, 900.0).map(|x| x.wmo.as_str()), Some("72403"));
        assert!(launched_from(&s, -77.40, 38.95, 20_000.0).is_none(), "first heard high: not known");
        assert!(launched_from(&s, -75.0, 37.0, 500.0).is_none(), "nowhere near a site");
    }

    #[test]
    fn launches_are_at_00_and_12_utc() {
        let t = crate::sondehub::unix_time("2026-11-27T14:05:00Z").unwrap();
        assert_eq!(next_launch(t), crate::sondehub::unix_time("2026-11-28T00:00:00Z").unwrap());
        assert_eq!(next_launch(crate::sondehub::unix_time("2026-11-27T00:00:00Z").unwrap()), crate::sondehub::unix_time("2026-11-27T12:00:00Z").unwrap());
    }
}
