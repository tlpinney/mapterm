//! Earthquakes worldwide from the USGS real-time GeoJSON feeds (public domain, no key):
//! the past hour or day of all earthquakes, the past week from M2.5 and the past month from
//! M4.5. USGS updates them every minute and says they're meant for applications; mapterm
//! asks once a minute, with `If-Modified-Since`.
//!
//! Each quake is drawn the way USGS's own map does: the symbol grows with the magnitude,
//! the color goes by age (red the past hour, orange the past day, yellow the past week).

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};

const FEED: &str = "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/";
/// The credit shown while quakes are on.
pub const CREDIT: &str = "Earthquakes: USGS";

/// Which feed: how far back, and from what magnitude.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Period {
    /// The past hour, all magnitudes.
    Hour,
    /// The past day, all magnitudes.
    #[default]
    Day,
    /// The past week, M2.5 and up.
    Week,
    /// The past 30 days, M4.5 and up.
    Month,
}

impl Period {
    /// `hour`, `day`, `week` or `month`.
    pub fn parse(s: &str) -> Option<Period> {
        Some(match s {
            "hour" => Period::Hour,
            "day" => Period::Day,
            "week" => Period::Week,
            "month" => Period::Month,
            _ => return None,
        })
    }

    fn file(self) -> &'static str {
        match self {
            Period::Hour => "all_hour",
            Period::Day => "all_day",
            Period::Week => "2.5_week",
            Period::Month => "4.5_month",
        }
    }

    /// "past day", "past week, M2.5+", for the status line.
    pub fn label(self) -> &'static str {
        match self {
            Period::Hour => "past hour",
            Period::Day => "past day",
            Period::Week => "past week, M2.5+",
            Period::Month => "past 30 days, M4.5+",
        }
    }
}

/// One earthquake (or other seismic event).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quake {
    /// USGS's event id: `ci41580648`.
    pub id: String,
    /// Magnitude, where given.
    pub mag: Option<f64>,
    /// The magnitude type: `ml`, `mb`, `mww`.
    pub mag_type: String,
    /// "3 km SE of Lake Henshaw, CA".
    pub place: String,
    /// When it happened (Unix seconds).
    pub time: f64,
    /// Longitude.
    pub lon: f64,
    /// Latitude.
    pub lat: f64,
    /// Depth, km.
    pub depth: f64,
    /// USGS's event page.
    pub url: String,
    /// "Did You Feel It?" reports.
    pub felt: Option<u64>,
    /// USGS PAGER alert level: green, yellow, orange, red.
    pub alert: Option<String>,
    /// "automatic" or "reviewed".
    pub status: String,
    /// The tsunami flag: a large event in an oceanic region (not a warning).
    pub tsunami: bool,
    /// "earthquake", "quarry blast", "ice quake"...
    pub kind: String,
}

/// The quakes in a USGS GeoJSON feed, newest first. Events without a place or time are left
/// out; the event page must be USGS's.
pub fn parse(text: &str) -> Result<Vec<Quake>> {
    let v: Json = serde_json::from_str(text).context("USGS")?;
    let feats = v.get("features").and_then(Json::as_array).context("USGS: no features")?;
    let s = |p: &Json, k: &str| p.get(k).and_then(Json::as_str).map(|s| s.chars().take(200).collect::<String>()).unwrap_or_default();
    let mut q: Vec<Quake> = feats
        .iter()
        .filter_map(|f| {
            let p = f.get("properties")?;
            let c = f.pointer("/geometry/coordinates")?.as_array()?;
            let (lon, lat) = (c.first()?.as_f64()?, c.get(1)?.as_f64()?);
            if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
                return None;
            }
            let url = s(p, "url");
            Some(Quake {
                id: f.get("id")?.as_str()?.chars().take(40).collect(),
                mag: p.get("mag").and_then(Json::as_f64).filter(|m| m.is_finite()),
                mag_type: s(p, "magType"),
                place: s(p, "place"),
                time: p.get("time")?.as_f64()? / 1000.0,
                lon,
                lat,
                depth: c.get(2).and_then(Json::as_f64).unwrap_or(0.0),
                url: if url.starts_with("https://earthquake.usgs.gov/") { url } else { String::new() },
                felt: p.get("felt").and_then(Json::as_u64),
                alert: p.get("alert").and_then(Json::as_str).filter(|a| ["green", "yellow", "orange", "red"].contains(a)).map(str::to_string),
                status: s(p, "status"),
                tsunami: p.get("tsunami").and_then(Json::as_i64) == Some(1),
                kind: s(p, "type"),
            })
        })
        .collect();
    q.sort_by(|a, b| b.time.total_cmp(&a.time));
    Ok(q)
}

/// The feed for `period`, unless unchanged since `since` (a `Last-Modified` value; then
/// None); with the new `Last-Modified`.
pub async fn fetch(period: Period, since: Option<&str>) -> Result<Option<(Vec<Quake>, Option<String>)>> {
    let mut req = crate::source::http_client().get(format!("{FEED}{}.geojson", period.file()));
    if let Some(s) = since {
        req = req.header("If-Modified-Since", s);
    }
    let resp = req.send().await.context("USGS")?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if !resp.status().is_success() {
        bail!("USGS: HTTP {}", resp.status());
    }
    let modified = resp.headers().get("last-modified").and_then(|v| v.to_str().ok()).map(str::to_string);
    Ok(Some((parse(&resp.text().await.context("USGS")?)?, modified)))
}

/// The age bands: (up to this many seconds old, label, color).
pub const AGES: [(f64, &str, (u8, u8, u8)); 4] = [
    (3600.0, "past hour", (0xff, 0x45, 0x3a)),
    (86400.0, "past day", (0xff, 0x9f, 0x0a)),
    (7.0 * 86400.0, "past week", (0xff, 0xd6, 0x0a)),
    (f64::MAX, "older", (0x9a, 0xa5, 0xb1)),
];

/// The band of a quake at `now`.
pub fn age_band(q: &Quake, now: f64) -> usize {
    let age = (now - q.time).max(0.0);
    AGES.iter().position(|a| age <= a.0).unwrap_or(AGES.len() - 1)
}

/// The symbol for a magnitude: bigger with it.
pub fn glyph(mag: Option<f64>) -> &'static str {
    match mag.unwrap_or(0.0) {
        m if m >= 6.0 => "⬤",
        m if m >= 4.5 => "●",
        m if m >= 2.5 => "•",
        _ => "·",
    }
}

/// "M5.7", or "M?" without one.
pub fn mag_text(q: &Quake) -> String {
    q.mag.map_or_else(|| "M?".to_string(), |m| format!("M{m:.1}"))
}

/// The quakes happened by `now` as GeoJSON points (`glyph`, `label`, `color`, `id`), oldest
/// and smallest first so the newest and biggest draw on top. Labels for M4.5 and up, and
/// for the selected one.
pub fn points(quakes: &[Quake], now: f64, selected: Option<&str>) -> Json {
    let mut v: Vec<&Quake> = quakes.iter().filter(|q| q.time <= now).collect();
    v.sort_by(|a, b| {
        let sel = |q: &Quake| Some(q.id.as_str()) == selected;
        sel(a).cmp(&sel(b)).then(a.mag.unwrap_or(0.0).total_cmp(&b.mag.unwrap_or(0.0))).then(a.time.total_cmp(&b.time))
    });
    let features: Vec<Json> = v
        .into_iter()
        .map(|q| {
            let (r, g, b) = AGES[age_band(q, now)].2;
            let selected = Some(q.id.as_str()) == selected;
            let label = if selected || q.mag.unwrap_or(0.0) >= 4.5 { mag_text(q) } else { String::new() };
            json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [q.lon, q.lat]},
                "properties": {"id": q.id, "glyph": if selected { "◎" } else { glyph(q.mag) }, "label": label,
                               "color": format!("#{r:02x}{g:02x}{b:02x}")},
            })
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// The style layer for quakes from source `source`: each symbol in its age's color, with
/// its magnitude for the larger ones.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source)
        .glyph(json!(["get", "glyph"]))
        .text(json!(["get", "label"]))
        .color("#c9d1d9")
        .paint("icon-color", json!(["get", "color"]))
}

/// The quake nearest `dot` (braille dots) in view, within `within` cells; the larger first
/// when two are about as near.
pub fn nearest(quakes: &[Quake], now: f64, vp: &crate::Viewport, dot: (f64, f64), within: f64) -> Option<String> {
    quakes
        .iter()
        .filter(|q| q.time <= now)
        .filter_map(|q| {
            let (x, y) = vp.dot_ll(q.lon, q.lat);
            let d = ((x - dot.0).powi(2) + ((y - dot.1) / 2.0).powi(2)).sqrt();
            (d <= within).then_some((d - q.mag.unwrap_or(0.0) * 0.5, q))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, q)| q.id.clone())
}

/// A legend of the age bands, with how many in each, and the symbols' magnitudes.
pub fn legend(quakes: &[Quake], now: f64) -> crate::legend::Legend {
    use ratatui::style::Color;
    let mut n = [0usize; AGES.len()];
    for q in quakes.iter().filter(|q| q.time <= now) {
        n[age_band(q, now)] += 1;
    }
    let mut l = crate::legend::Legend::new("earthquakes by age");
    for ((_, label, (r, g, b)), count) in AGES.iter().zip(n) {
        if count > 0 {
            l = l.entry("●", Color::Rgb(*r, *g, *b), *label, Some(count));
        }
    }
    l.entry("·•●⬤", Color::Gray, "M<2.5 · 2.5 · 4.5 · 6+", None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED_JSON: &str = r#"{"type":"FeatureCollection","metadata":{"count":3},"features":[
      {"type":"Feature","id":"ci41580648","properties":{"mag":1.47,"place":"3 km SE of Lake Henshaw, CA","time":1795737492150,
        "url":"https://earthquake.usgs.gov/earthquakes/eventpage/ci41580648","felt":null,"alert":null,"status":"automatic","tsunami":0,
        "magType":"ml","type":"earthquake"},"geometry":{"type":"Point","coordinates":[-116.74184,33.218,9.45]}},
      {"type":"Feature","id":"us7000big","properties":{"mag":5.7,"place":"120 km S of Somewhere","time":1795700000000,
        "url":"https://evil.example/x","felt":12,"alert":"yellow","status":"reviewed","tsunami":1,"magType":"mww","type":"earthquake"},
        "geometry":{"type":"Point","coordinates":[140.1,-5.2,35.0]}},
      {"type":"Feature","id":"bad","properties":{"mag":2.0,"time":1795700000000},"geometry":{"type":"Point","coordinates":[999,0,0]}}
    ]}"#;

    #[test]
    fn reads_the_feed() {
        let q = parse(FEED_JSON).unwrap();
        assert_eq!(q.len(), 2, "a point off the globe is left out");
        assert_eq!((q[0].id.as_str(), q[0].mag, q[0].depth), ("ci41580648", Some(1.47), 9.45), "newest first");
        assert_eq!(q[0].time, 1_795_737_492.15);
        assert_eq!((q[1].alert.as_deref(), q[1].tsunami, q[1].felt), (Some("yellow"), true, Some(12)));
        assert!(q[1].url.is_empty(), "only USGS's event pages");
        assert_eq!(Period::parse("week"), Some(Period::Week));
        assert_eq!(Period::parse("year"), None);
    }

    #[test]
    fn points_age_size_and_pick() {
        let q = parse(FEED_JSON).unwrap();
        let now = 1_795_737_600.0;
        assert_eq!(age_band(&q[0], now), 0, "two minutes old: past hour");
        assert_eq!(age_band(&q[1], now), 1, "ten hours old: past day");
        assert_eq!((glyph(Some(1.4)), glyph(Some(2.5)), glyph(Some(5.7)), glyph(Some(7.0)), glyph(None)), ("·", "•", "●", "⬤", "·"));
        let fc = points(&q, now, None);
        let f = fc["features"].as_array().unwrap();
        assert_eq!(f[1]["properties"]["id"], "us7000big", "the biggest on top");
        assert_eq!((f[1]["properties"]["label"].as_str(), f[0]["properties"]["label"].as_str()), (Some("M5.7"), Some("")));
        assert_eq!(f[0]["properties"]["color"], "#ff453a");
        let sel = points(&q, now, Some("ci41580648"));
        let last = sel["features"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(
            (last["properties"]["id"].as_str(), last["properties"]["glyph"].as_str(), last["properties"]["label"].as_str()),
            (Some("ci41580648"), Some("◎"), Some("M1.5"))
        );
        // Before a quake happened (a clock in the past), it isn't there.
        assert_eq!(points(&q, 1_795_710_000.0, None)["features"].as_array().unwrap().len(), 1);
        let cam = crate::Camera::new(-116.7, 33.2, 8.0);
        let vp = crate::Viewport::new(&cam, ratatui::layout::Rect::new(0, 0, 80, 24));
        let dot = vp.dot_ll(-116.74, 33.22);
        assert_eq!(nearest(&q, now, &vp, dot, 6.0).as_deref(), Some("ci41580648"));
        let l = legend(&q, now);
        assert_eq!(l.entries.len(), 3, "the two bands with quakes, and the sizes");
    }
}
