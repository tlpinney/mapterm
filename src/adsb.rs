//! Live aircraft from free ADS-B feeds, as a GeoJSON layer that moves.
//!
//! [adsb.lol](https://api.adsb.lol) and [adsb.fi](https://github.com/adsbfi/opendata) answer
//! "aircraft within N nautical miles of a point" without a key, in the readsb JSON format
//! (`{"ac": [{"hex", "flight", "lat", "lon", "alt_baro", "gs", "track", ...}]}`). The viewer
//! polls the feed for the area in view and keeps each aircraft's recent fixes as a
//! [`SampledPosition`] ([`Tracks`]); it draws them a few seconds in the past, interpolating
//! between real fixes, so planes glide and follow their turns, and draws the fixes behind
//! them as trails.

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};
use std::collections::HashMap;

use crate::timeseries::{Sample, SampledPosition, Velocity};

/// A feed: a URL template with `{lat}`, `{lon}` and `{nm}` (radius in nautical miles), and
/// the credit to show.
#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    /// Short name, for the status line.
    pub name: String,
    /// The request URL, with `{lat}`, `{lon}` and `{nm}` to fill in.
    pub url: String,
    /// The attribution the feed asks for.
    pub attribution: String,
    /// The largest radius the feed answers, in nautical miles.
    pub max_nm: f64,
    /// An API key, sent as `x-api-key` (adsb.lol's free keys raise its daily quota).
    pub key: Option<String>,
}

impl Feed {
    /// A feed by name (`lol` / `adsb.lol`, `fi` / `adsb.fi`), or a URL template.
    pub fn named(spec: &str) -> Result<Feed> {
        let spec = spec.trim();
        Ok(match spec {
            "" | "lol" | "adsb.lol" => Feed {
                name: "adsb.lol".into(),
                url: "https://api.adsb.lol/v2/point/{lat}/{lon}/{nm}".into(),
                attribution: "adsb.lol (ODbL)".into(),
                max_nm: 250.0,
                key: None,
            },
            "fi" | "adsb.fi" => Feed {
                name: "adsb.fi".into(),
                url: "https://opendata.adsb.fi/api/v2/lat/{lat}/lon/{lon}/dist/{nm}".into(),
                attribution: "adsb.fi".into(),
                max_nm: 250.0,
                key: None,
            },
            url if url.starts_with("https://") || url.starts_with("http://") => {
                if !["{lat}", "{lon}", "{nm}"].iter().all(|k| url.contains(k)) {
                    bail!("an ADS-B feed URL needs {{lat}}, {{lon}} and {{nm}}: {url}");
                }
                let host = url.split('/').nth(2).unwrap_or("ADS-B").to_string();
                Feed { name: host.clone(), url: url.into(), attribution: host, max_nm: 250.0, key: None }
            }
            other => bail!("unknown ADS-B feed {other:?}: use lol, fi, or a URL with {{lat}}, {{lon}} and {{nm}}"),
        })
    }

    /// How often to ask when the view stays put: adsb.lol allows 500 requests a day per
    /// address without a key, so every 30 s (about 4 hours of watching); with a key (7,200 a
    /// day for feeders) every 12 s; other feeds every 10 s. [`Quota`] slows this further
    /// when the day's requests run low.
    pub fn every(&self) -> std::time::Duration {
        use std::time::Duration;
        match (self.name.as_str(), &self.key) {
            ("adsb.lol", None) => Duration::from_secs(30),
            ("adsb.lol", Some(_)) => Duration::from_secs(12),
            _ => Duration::from_secs(10),
        }
    }

    /// The request for a circle of `nm` around (`lon`, `lat`), clamped to what the feed takes.
    pub fn url_for(&self, lon: f64, lat: f64, nm: f64) -> String {
        let nm = nm.clamp(1.0, self.max_nm).ceil();
        self.url.replace("{lat}", &format!("{lat:.4}")).replace("{lon}", &format!("{lon:.4}")).replace("{nm}", &format!("{nm:.0}"))
    }
}

/// One aircraft as the feed last saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Aircraft {
    /// ICAO 24-bit address, hex.
    pub hex: String,
    /// Callsign, trimmed (often the flight number).
    pub flight: Option<String>,
    /// Registration (tail number).
    pub registration: Option<String>,
    /// ICAO type code, such as `B738`.
    pub type_code: Option<String>,
    /// Latitude and longitude, degrees, at the time of the fetch minus `seen_pos`.
    pub lat: f64,
    /// See `lat`.
    pub lon: f64,
    /// Barometric altitude in feet; `None` on the ground or unknown.
    pub alt_ft: Option<f64>,
    /// Whether the feed reports it on the ground.
    pub on_ground: bool,
    /// Ground speed, knots.
    pub gs_kt: Option<f64>,
    /// Track over the ground, degrees from true north.
    pub track: Option<f64>,
    /// How old the position was when fetched, seconds.
    pub seen_pos: f64,
    /// The aircraft type in words, such as `BOEING 737-800`, when the feed has it.
    pub desc: Option<String>,
    /// The registered owner or operator, when the feed has it.
    pub operator: Option<String>,
    /// Transponder code.
    pub squawk: Option<String>,
    /// Climb (positive) or descent rate, feet per minute.
    pub vert_rate: Option<f64>,
    /// An emergency the aircraft declares (`general`, `lifeguard`, `nordo`, ...).
    pub emergency: Option<String>,
    /// Its ADS-B emitter category (`A3` a large airplane, `B2` lighter than air, ...).
    pub category: Option<String>,
}

impl Aircraft {
    /// Lighter than air: a balloon or an airship. Its ADS-B category (B2) says so; the
    /// type from the registry (`BALL`) only counts when it sends no category, as registries
    /// have planes typed `BALL` by mistake (a CargoJet 767, category A5, was).
    pub fn is_balloon(&self) -> bool {
        match self.category.as_deref() {
            Some(c) => c == "B2",
            None => self.type_code.as_deref() == Some("BALL"),
        }
    }
}

/// An ADS-B emitter category in words (DO-260B), for the aircraft panel.
pub fn category_name(c: &str) -> Option<&'static str> {
    Some(match c {
        "A1" => "light airplane (under 7 t)",
        "A2" => "small airplane (7 to 34 t)",
        "A3" => "large airplane (34 to 136 t)",
        "A4" => "high-vortex large (a 757)",
        "A5" => "heavy (over 136 t)",
        "A6" => "high performance (fast, agile)",
        "A7" => "rotorcraft",
        "B1" => "glider or sailplane",
        "B2" => "lighter than air (balloon, airship)",
        "B3" => "parachutist or skydiver",
        "B4" => "ultralight, hang glider, paraglider",
        "B6" => "unmanned aircraft (drone)",
        "B7" => "space or trans-atmospheric vehicle",
        "C1" => "emergency vehicle (on the ground)",
        "C2" => "service vehicle (on the ground)",
        "C3" => "obstacle",
        _ => return None,
    })
}

/// Read the feed's answer: the aircraft that have a position.
pub fn parse(body: &[u8]) -> Result<Vec<Aircraft>> {
    let v: Json = serde_json::from_slice(body).context("ADS-B feed: not JSON")?;
    let list = v.get("ac").or_else(|| v.get("aircraft")).and_then(Json::as_array).context("ADS-B feed: no aircraft list")?;
    let text = |a: &Json, k: &str| a.get(k).and_then(Json::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let num = |a: &Json, k: &str| a.get(k).and_then(Json::as_f64).filter(|n| n.is_finite());
    Ok(list
        .iter()
        .filter_map(|a| {
            let (lat, lon) = (num(a, "lat")?, num(a, "lon")?);
            let on_ground = a.get("alt_baro").and_then(Json::as_str) == Some("ground");
            Some(Aircraft {
                hex: text(a, "hex")?,
                flight: text(a, "flight"),
                registration: text(a, "r"),
                type_code: text(a, "t"),
                lat,
                lon,
                alt_ft: if on_ground { None } else { num(a, "alt_baro").or_else(|| num(a, "alt_geom")) },
                on_ground,
                gs_kt: num(a, "gs"),
                track: num(a, "track").or_else(|| num(a, "true_heading")),
                seen_pos: num(a, "seen_pos").unwrap_or(0.0).max(0.0),
                desc: text(a, "desc"),
                operator: text(a, "ownOp"),
                squawk: text(a, "squawk"),
                vert_rate: num(a, "baro_rate").or_else(|| num(a, "geom_rate")),
                emergency: text(a, "emergency").filter(|e| e != "none"),
                category: text(a, "category"),
            })
        })
        .collect())
}

/// The feed asked us to slow down (HTTP 429 or 503), with how long to wait if it said
/// (`Retry-After`, in seconds).
#[derive(Debug)]
pub struct Throttled(pub Option<std::time::Duration>);

impl std::fmt::Display for Throttled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(d) => write!(f, "the feed asked to slow down (retry after {} s)", d.as_secs()),
            None => write!(f, "the feed asked to slow down"),
        }
    }
}

impl std::error::Error for Throttled {}

/// What a feed says about its request quota (`x-ratelimit-*` headers).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quota {
    /// Requests left in the current window.
    pub remaining: u64,
    /// Requests the window allows.
    pub limit: Option<u64>,
    /// Seconds until the window resets.
    pub reset_s: Option<u64>,
}

impl Quota {
    /// Read `x-ratelimit-remaining`, `-limit` (`500, 500;w=86400`: the first number) and
    /// `-reset` (seconds).
    pub fn from_headers(h: &reqwest::header::HeaderMap) -> Option<Quota> {
        let num = |k: &str| h.get(k)?.to_str().ok()?.split([',', ';']).next()?.trim().parse::<u64>().ok();
        Some(Quota { remaining: num("x-ratelimit-remaining")?, limit: num("x-ratelimit-limit"), reset_s: num("x-ratelimit-reset") })
    }

    /// The pause that spreads the requests left over the time until the reset, at least
    /// `base`: with plenty left it's `base`; as they run out, polling slows instead of
    /// stopping at the limit.
    pub fn pace(&self, base: std::time::Duration) -> std::time::Duration {
        match self.reset_s {
            Some(reset) => base.max(std::time::Duration::from_secs_f64(reset as f64 / self.remaining.max(1) as f64)),
            None => base,
        }
    }
}

/// A feed's answer: the aircraft, and its quota if it said.
#[derive(Clone, Debug)]
pub struct Answer {
    /// The aircraft with positions.
    pub aircraft: Vec<Aircraft>,
    /// The request quota left, if the feed reports it.
    pub quota: Option<Quota>,
}

/// Largest answer read (a 250 nm circle over a busy region is a few MB).
const MAX_BODY: usize = 16 << 20;

/// Fetch the aircraft within `nm` of (`lon`, `lat`). A feed that says "too many requests"
/// gives a [`Throttled`] error (find it with `downcast_ref`), so the caller can back off.
pub async fn fetch(feed: &Feed, lon: f64, lat: f64, nm: f64) -> Result<Answer> {
    let url = feed.url_for(lon, lat, nm);
    let what = || format!("ADS-B feed {}", feed.name);
    // A key only goes with the client that won't follow a redirect to another host.
    let req = match &feed.key {
        Some(k) => crate::source::keyed_client().get(&url).header("x-api-key", k),
        None => crate::source::http_client().get(&url),
    };
    let resp = req.send().await.with_context(what)?;
    let quota = Quota::from_headers(resp.headers());
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        let wait = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
        return Err(anyhow::Error::new(Throttled(wait.map(std::time::Duration::from_secs))).context(what()));
    }
    if !status.is_success() {
        bail!("{}: HTTP {status}", what());
    }
    if resp.content_length().is_some_and(|n| n as usize > MAX_BODY) {
        bail!("{}: answer larger than {} MB", what(), MAX_BODY >> 20);
    }
    let body = resp.bytes().await.with_context(what)?;
    if body.len() > MAX_BODY {
        bail!("{}: answer larger than {} MB", what(), MAX_BODY >> 20);
    }
    Ok(Answer { aircraft: parse(&body)?, quota })
}

/// How long to wait after a failed request: the feed's `Retry-After` if it gave one, else
/// 30 s doubling with each failure in a row, at most 5 minutes (and never less than 30 s).
pub fn backoff(err: &anyhow::Error, failures_in_a_row: u32) -> std::time::Duration {
    use std::time::Duration;
    let doubling = Duration::from_secs(30).saturating_mul(1 << failures_in_a_row.saturating_sub(1).min(4)).min(Duration::from_secs(300));
    match err.downcast_ref::<Throttled>() {
        Some(Throttled(Some(d))) => (*d).clamp(Duration::from_secs(30), Duration::from_secs(3600)),
        _ => doubling,
    }
}

/// An arrow for a track, in eight directions; a dot on the ground or without a track.
pub fn arrow(a: &Aircraft) -> &'static str {
    const ARROWS: [&str; 8] = ["↑", "↗", "→", "↘", "↓", "↙", "←", "↖"];
    // A balloon drifts with the wind: no heading to point.
    if a.is_balloon() {
        return "◓";
    }
    match a.track {
        Some(t) if !a.on_ground => ARROWS[((t.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8],
        _ => "•",
    }
}

/// Seconds of fixes kept per aircraft (the trail's length).
const KEEP: f64 = 120.0;
/// Most seconds an aircraft is moved past its last fix along its track.
const EXTRAPOLATE: f64 = 30.0;
/// An aircraft the feed hasn't reported for this long is dropped.
const GONE: f64 = 60.0;

/// Every aircraft seen lately: its latest report and its fixes over time.
#[derive(Default)]
pub struct Tracks {
    by_hex: HashMap<String, Track>,
}

struct Track {
    latest: Aircraft,
    path: SampledPosition,
    /// When the feed last listed it (receive time).
    seen: f64,
}

impl Tracks {
    /// Take a feed answer received at `now` (seconds, the same clock as [`Tracks::points`]):
    /// each position becomes a fix at `now - seen_pos`. Aircraft not heard from for a minute
    /// are dropped.
    pub fn update(&mut self, list: Vec<Aircraft>, now: f64) {
        for a in list {
            let t = now - a.seen_pos;
            let track =
                self.by_hex.entry(a.hex.clone()).or_insert_with(|| Track { latest: a.clone(), path: SampledPosition::new(KEEP, EXTRAPOLATE), seen: now });
            track.path.push(Sample { t, lon: a.lon, lat: a.lat, alt: a.alt_ft });
            // Extrapolate only what's moving, along the reported track.
            let v = match (a.gs_kt, a.track) {
                (Some(gs), Some(bearing)) if !a.on_ground && gs >= 30.0 => Some(Velocity { speed: gs * 1852.0 / 3600.0, bearing }),
                _ => None,
            };
            track.path.set_velocity(v);
            track.latest = a;
            track.seen = now;
        }
        self.by_hex.retain(|_, tr| now - tr.seen < GONE);
    }

    /// The aircraft `hex` at time `t`: its latest report, where it is at `t`, and when the
    /// feed last listed it (receive time).
    pub fn get(&self, hex: &str, t: f64) -> Option<(&Aircraft, Sample, f64)> {
        let tr = self.by_hex.get(hex)?;
        Some((&tr.latest, tr.path.at(t)?, tr.seen))
    }

    /// The aircraft drawn nearest the braille dot `dot` of the map `vp` at time `t`, if one is
    /// within `within` dots (a click on or next to a plane).
    pub fn nearest(&self, t: f64, vp: &crate::Viewport, dot: (f64, f64), within: f64) -> Option<String> {
        self.by_hex
            .iter()
            .filter_map(|(hex, tr)| {
                let p = tr.path.at(t)?;
                let (x, y) = vp.dot_ll(p.lon, p.lat);
                // Cells are about twice as tall as wide, and so are 2 x 4 dots: weigh rows half.
                let d = ((x - dot.0).powi(2) + ((y - dot.1) / 2.0).powi(2)).sqrt();
                (d <= within).then_some((d, hex))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, hex)| hex.clone())
    }

    /// The aircraft by distance from (`lon`, `lat`) at time `t`, nearest first (their hexes),
    /// for picking one with the keyboard.
    pub fn by_distance(&self, t: f64, lon: f64, lat: f64) -> Vec<String> {
        let mut v: Vec<(f64, &String)> =
            self.by_hex.iter().filter_map(|(hex, tr)| tr.path.at(t).map(|p| (distance_nm((lon, lat), (p.lon, p.lat)), hex))).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        v.into_iter().map(|(_, h)| h.clone()).collect()
    }

    /// Every aircraft's latest report.
    pub fn all(&self) -> impl Iterator<Item = &Aircraft> {
        self.by_hex.values().map(|t| &t.latest)
    }

    /// How many aircraft are tracked.
    pub fn len(&self) -> usize {
        self.by_hex.len()
    }

    /// Whether no aircraft are tracked.
    pub fn is_empty(&self) -> bool {
        self.by_hex.is_empty()
    }

    /// The aircraft at time `t` as GeoJSON points, with properties for styling and inspect
    /// (`glyph`, `label`, `alt`, `altitude`, `flight`, `hex`, `type`, `registration`,
    /// `speed_kt`, `track`).
    pub fn points(&self, t: f64) -> Json {
        let features: Vec<Json> = self
            .by_hex
            .values()
            .filter_map(|tr| {
                let p = tr.path.at(t)?;
                let a = &tr.latest;
                Some(json!({
                    "type": "Feature",
                    "id": a.hex,
                    "geometry": {"type": "Point", "coordinates": [p.lon, p.lat]},
                    "properties": {
                        "glyph": arrow(a),
                        "label": a.flight.clone().or_else(|| a.registration.clone()).unwrap_or_default(),
                        "alt": if a.on_ground { 0.0 } else { p.alt.unwrap_or(0.0) },
                        "altitude": if a.on_ground { json!("ground") } else { json!(p.alt.map(f64::round)) },
                        "flight": a.flight,
                        "hex": a.hex,
                        "type": a.type_code,
                        "registration": a.registration,
                        "speed_kt": a.gs_kt,
                        "track": a.track,
                        "category": a.category.as_deref().and_then(category_name).or(a.category.as_deref()),
                        "balloon": a.is_balloon(),
                    }
                }))
            })
            .collect();
        json!({"type": "FeatureCollection", "features": features})
    }

    /// Where each airborne aircraft has been over the `seconds` before time `t`, as GeoJSON
    /// lines (with `alt` for color). Aircraft with fewer than two points have none.
    pub fn trails(&self, t: f64, seconds: f64) -> Json {
        let features: Vec<Json> = self
            .by_hex
            .values()
            .filter(|tr| !tr.latest.on_ground)
            .filter_map(|tr| {
                let pts = tr.path.trail(t, seconds);
                (pts.len() >= 2).then(|| {
                    json!({
                        "type": "Feature",
                        "geometry": {"type": "LineString", "coordinates": pts.iter().map(|&(x, y)| [x, y]).collect::<Vec<_>>()},
                        "properties": {"alt": tr.latest.alt_ft.unwrap_or(0.0), "hex": tr.latest.hex},
                    })
                })
            })
            .collect();
        json!({"type": "FeatureCollection", "features": features})
    }
}

/// The airborne aircraft's trail paths at their altitude colors, for
/// [`crate::timeseries::render_trails`].
pub fn trail_paths(tracks: &Tracks) -> impl Iterator<Item = (&SampledPosition, (u8, u8, u8))> {
    tracks.by_hex.values().filter(|tr| !tr.latest.on_ground).map(|tr| (&tr.path, altitude_rgb(tr.latest.alt_ft.unwrap_or(0.0))))
}

/// What the aircraft colors mean, with how many of `tracks` are in each band.
pub fn legend(tracks: &Tracks) -> crate::legend::Legend {
    use ratatui::style::Color;
    let bands: [(&str, f64, f64); 5] = [
        ("on the ground", f64::NEG_INFINITY, 1.0),
        ("under 10,000 ft", 1.0, 10_000.0),
        ("10,000 to 25,000 ft", 10_000.0, 25_000.0),
        ("25,000 to 40,000 ft", 25_000.0, 40_000.0),
        ("over 40,000 ft", 40_000.0, f64::INFINITY),
    ];
    let mut l = crate::legend::Legend::new("aircraft by altitude");
    for (label, lo, hi) in bands {
        let n = tracks.all().filter(|a| {
            let alt = if a.on_ground { 0.0 } else { a.alt_ft.unwrap_or(0.0) };
            alt >= lo && alt < hi
        });
        let count = n.count();
        // The band's color where most of it is: its floor, or the ground's gray.
        let (r, g, b) = altitude_rgb(if lo.is_finite() { lo.max(1000.0) } else { 0.0 });
        if count > 0 {
            l = l.entry("●", Color::Rgb(r, g, b), label, Some(count));
        }
    }
    l
}

/// The altitude colors of [`by_altitude`], as RGB for images.
fn altitude_rgb(alt: f64) -> (u8, u8, u8) {
    const STOPS: [(f64, (u8, u8, u8)); 5] =
        [(0.0, (0x9a, 0xa0, 0xa6)), (1000.0, (0x7e, 0xe7, 0x87)), (10000.0, (0xf2, 0xcc, 0x60)), (25000.0, (0xff, 0x9f, 0x43)), (40000.0, (0xff, 0x6b, 0xcb))];
    let i = STOPS.iter().position(|&(a, _)| alt < a).unwrap_or(STOPS.len());
    if i == 0 {
        return STOPS[0].1;
    }
    if i == STOPS.len() {
        return STOPS[STOPS.len() - 1].1;
    }
    let ((a0, c0), (a1, c1)) = (STOPS[i - 1], STOPS[i]);
    let f = (alt - a0) / (a1 - a0);
    let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * f).round() as u8;
    (mix(c0.0, c1.0), mix(c0.1, c1.1), mix(c0.2, c1.2))
}

/// Altitude colors: gray on the ground, green low, through yellow and orange, to pink at
/// cruise.
fn by_altitude() -> Json {
    json!(["interpolate", ["linear"], ["get", "alt"], 0, "#9aa0a6", 1000, "#7ee787", 10000, "#f2cc60", 25000, "#ff9f43", 40000, "#ff6bcb"])
}

/// The style layer for trails from source `source`: thin lines in the altitude colors, dimmed.
pub fn trail_layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::line(id, source).color(by_altitude()).opacity(0.5)
}

/// The style layer for aircraft from source `source`: an arrow along the track, colored by
/// altitude (gray on the ground, green low, through yellow and orange, to pink at cruise),
/// with the callsign beside it.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source)
        .glyph(json!(["get", "glyph"]))
        .text(json!(["get", "label"]))
        .color("#c9d1d9")
        .paint("icon-color", by_altitude())
        .layout("symbol-sort-key", json!(["get", "alt"]))
}

/// Great-circle distance in nautical miles.
pub fn distance_nm((lon1, lat1): (f64, f64), (lon2, lat2): (f64, f64)) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = (p2 - p1, (lon2 - lon1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 3440.065 * h.sqrt().min(1.0).asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"ac":[
        {"hex":"a1b2c3","flight":"UAL123  ","r":"N12345","t":"B738","lat":38.9,"lon":-77.0,"alt_baro":35000,"gs":450,"track":90,"seen_pos":1.5},
        {"hex":"abcdef","lat":38.85,"lon":-77.04,"alt_baro":"ground","gs":12,"track":180},
        {"hex":"000001","flight":"NOPOS"}
    ],"now":1764100000000,"total":3}"#;

    #[test]
    fn parses_readsb_json() {
        let ac = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(ac.len(), 2, "the one without a position is left out");
        assert_eq!(ac[0].flight.as_deref(), Some("UAL123"));
        assert_eq!((ac[0].alt_ft, ac[0].on_ground, ac[0].seen_pos), (Some(35000.0), false, 1.5));
        assert_eq!((ac[1].alt_ft, ac[1].on_ground), (None, true));
        assert!(parse(b"{}").is_err());
    }

    #[test]
    fn tracks_interpolate_between_fixes() {
        let mut tracks = Tracks::default();
        let mut ac = parse(SAMPLE.as_bytes()).unwrap();
        tracks.update(ac.clone(), 100.0);
        // Five seconds later the same plane, 0.1 degrees further east, seen just now.
        ac[0].lon = -76.9;
        ac[0].seen_pos = 0.0;
        tracks.update(ac.clone(), 105.0);
        // Its fixes are at 98.5 and 105: halfway between them, halfway along.
        let doc = tracks.points(101.75);
        let f = doc["features"].as_array().unwrap().iter().find(|f| f["id"] == "a1b2c3").unwrap();
        assert!((f["geometry"]["coordinates"][0].as_f64().unwrap() - -76.95).abs() < 1e-9, "{f}");
        assert_eq!(f["properties"]["glyph"], "→");
        assert_eq!(f["properties"]["label"], "UAL123");
        assert_eq!(tracks.trails(105.0, 60.0)["features"].as_array().unwrap().len(), 1, "only the airborne one");
    }

    #[test]
    fn tracks_drop_aircraft_no_longer_reported() {
        let mut tracks = Tracks::default();
        tracks.update(parse(SAMPLE.as_bytes()).unwrap(), 0.0);
        assert_eq!(tracks.len(), 2);
        tracks.update(Vec::new(), 61.0);
        assert!(tracks.is_empty());
    }

    #[test]
    fn arrows_follow_the_track() {
        let mut a = parse(SAMPLE.as_bytes()).unwrap().remove(0);
        for (t, g) in [(0.0, "↑"), (44.0, "↗"), (90.0, "→"), (200.0, "↓"), (350.0, "↑"), (-90.0, "←")] {
            a.track = Some(t);
            assert_eq!(arrow(&a), g, "{t}");
        }
        a.on_ground = true;
        assert_eq!(arrow(&a), "•");
        a.on_ground = false;
        a.category = Some("B2".into());
        assert!(a.is_balloon());
        assert_eq!(arrow(&a), "◓", "a balloon, whatever its track");
        (a.category, a.type_code) = (None, Some("BALL".into()));
        assert!(a.is_balloon(), "ICAO type BALL, no category");
        a.category = Some("A5".into());
        assert!(!a.is_balloon(), "a heavy jet typed BALL by mistake");
        assert_eq!(category_name("B6"), Some("unmanned aircraft (drone)"));
        assert_eq!(category_name("Z9"), None);
    }

    #[test]
    fn feeds_and_urls() {
        let lol = Feed::named("lol").unwrap();
        assert_eq!(lol.url_for(-77.03, 38.89, 12.2), "https://api.adsb.lol/v2/point/38.8900/-77.0300/13");
        assert_eq!(Feed::named("fi").unwrap().url_for(0.0, 0.0, 900.0), "https://opendata.adsb.fi/api/v2/lat/0.0000/lon/0.0000/dist/250");
        assert!(Feed::named("https://example.com/{lat}/{lon}").is_err());
        assert!(Feed::named("nope").is_err());
    }

    #[test]
    fn trails_render_as_an_image() {
        let mut tracks = Tracks::default();
        let mut ac = parse(SAMPLE.as_bytes()).unwrap();
        tracks.update(ac.clone(), 100.0);
        ac[0].lon = -76.95;
        ac[0].seen_pos = 0.0;
        tracks.update(ac, 105.0);
        let vp = crate::Viewport::new(&crate::Camera::new(-76.97, 38.9, 11.0), ratatui::layout::Rect::new(0, 0, 40, 10));
        let (w, h) = (40 * 8, 10 * 16);
        let rgba = crate::timeseries::render_trails(trail_paths(&tracks), 105.0, 90.0, &vp, w, h, (8.0, 16.0));
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        let drawn = rgba.chunks(4).filter(|p| p[3] > 0).count();
        assert!(drawn > 50, "the trail is drawn: {drawn} pixels");
        // Altitude 35000 is between orange and pink.
        assert_eq!(altitude_rgb(0.0), (0x9a, 0xa0, 0xa6));
        assert_eq!(altitude_rgb(99999.0), (0xff, 0x6b, 0xcb));
    }

    #[test]
    fn finds_the_aircraft_under_a_click() {
        let mut tracks = Tracks::default();
        tracks.update(parse(SAMPLE.as_bytes()).unwrap(), 100.0);
        let vp = crate::Viewport::new(&crate::Camera::new(-77.0, 38.9, 12.0), ratatui::layout::Rect::new(0, 0, 80, 24));
        // Where it's drawn at t = 100: 1.5 s past its fix, a little east.
        let (a, here, seen) = tracks.get("a1b2c3", 100.0).unwrap();
        assert_eq!((a.flight.as_deref(), seen), (Some("UAL123"), 100.0));
        assert!(here.lon > -77.0 && (here.lat - 38.9).abs() < 0.01);
        let at = vp.dot_ll(here.lon, here.lat);
        assert_eq!(tracks.nearest(100.0, &vp, (at.0 + 2.0, at.1 + 1.0), 6.0).as_deref(), Some("a1b2c3"));
        assert_eq!(tracks.nearest(100.0, &vp, (at.0 + 40.0, at.1), 6.0), None);
    }

    #[test]
    fn backs_off_after_failures() {
        use std::time::Duration;
        let plain = anyhow::anyhow!("connection refused");
        assert_eq!(backoff(&plain, 1), Duration::from_secs(30));
        assert_eq!(backoff(&plain, 2), Duration::from_secs(60));
        assert_eq!(backoff(&plain, 9), Duration::from_secs(300));
        let told = anyhow::Error::new(Throttled(Some(Duration::from_secs(120)))).context("feed");
        assert_eq!(backoff(&told, 1), Duration::from_secs(120));
        let soon = anyhow::Error::new(Throttled(Some(Duration::from_secs(1))));
        assert_eq!(backoff(&soon, 1), Duration::from_secs(30), "never sooner than 30 s");
    }

    #[test]
    fn quota_from_headers_paces_polling() {
        use std::time::Duration;
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-ratelimit-limit", "500, 500;w=86400".parse().unwrap());
        h.insert("x-ratelimit-remaining", "312".parse().unwrap());
        h.insert("x-ratelimit-reset", "550".parse().unwrap());
        let q = Quota::from_headers(&h).unwrap();
        assert_eq!(q, Quota { remaining: 312, limit: Some(500), reset_s: Some(550) });
        // Plenty left before the reset: the base pace.
        assert_eq!(q.pace(Duration::from_secs(30)), Duration::from_secs(30));
        // 100 left for 20,000 s: one every 200 s.
        let low = Quota { remaining: 100, limit: Some(500), reset_s: Some(20_000) };
        assert_eq!(low.pace(Duration::from_secs(30)), Duration::from_secs(200));
        assert_eq!(Quota::from_headers(&reqwest::header::HeaderMap::new()), None);
    }

    #[test]
    fn polling_follows_the_feed_and_key() {
        use std::time::Duration;
        let mut lol = Feed::named("lol").unwrap();
        assert_eq!(lol.every(), Duration::from_secs(30));
        lol.key = Some("k".into());
        assert_eq!(lol.every(), Duration::from_secs(12));
        assert_eq!(Feed::named("fi").unwrap().every(), Duration::from_secs(10));
    }
}
