//! Traffic speeds from Maryland's CHART (MDOT SHA's Coordinated Highways Action Response
//! Team): its roadside and probe speed sensors, each with the speed in each direction, from
//! CHART's public data feeds (JSON, no key, updated about every minute).
//!
//! Each sensor is a dot in the color of its slowest direction (green free-flowing, yellow
//! slow, orange heavy, red stop-and-go), labeled with its speeds once you zoom in. Speeds
//! are mph as CHART reports them; sensors CHART reports offline or failed are left out.

use anyhow::{Context, Result};
use serde_json::{Value as Json, json};

/// CHART's speed sensor feed.
const CHART_TSS: &str = "https://chartexp1.sha.maryland.gov/CHARTExportClientService/getTSSMapDataJSON.do";
/// The credit shown while traffic is on.
pub const CREDIT: &str = "Traffic speeds: MDOT SHA CHART";
/// Seconds between asks (CHART updates about every minute).
pub const REFRESH: u64 = 60;
/// Readings older than this (seconds) are left out.
const STALE: f64 = 30.0 * 60.0;
/// The zoom speed labels show from.
pub const LABEL_ZOOM: f64 = 10.0;

/// How the traffic is moving.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 50 mph and up.
    Free,
    /// 35 to 50.
    Slow,
    /// 20 to 35.
    Heavy,
    /// Under 20.
    Stopped,
}

impl Level {
    /// The level for a speed in mph.
    pub fn of(mph: f64) -> Level {
        if mph >= 50.0 {
            Level::Free
        } else if mph >= 35.0 {
            Level::Slow
        } else if mph >= 20.0 {
            Level::Heavy
        } else {
            Level::Stopped
        }
    }

    /// Its color.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Level::Free => (0x22, 0xc5, 0x5e),
            Level::Slow => (0xea, 0xb3, 0x08),
            Level::Heavy => (0xf9, 0x73, 0x16),
            Level::Stopped => (0xef, 0x44, 0x44),
        }
    }

    /// "free-flowing", "slow", "heavy", "stop and go".
    pub fn label(self) -> &'static str {
        match self {
            Level::Free => "50+ mph",
            Level::Slow => "35–50 mph",
            Level::Heavy => "20–35 mph",
            Level::Stopped => "under 20 mph",
        }
    }
}

/// One direction at a sensor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Zone {
    /// "NORTH", "OUTER_LOOP".
    pub direction: String,
    /// Degrees from north the traffic heads.
    pub bearing: f64,
    /// mph.
    pub mph: f64,
}

/// A speed sensor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sensor {
    /// CHART's id.
    pub id: String,
    /// "I-495 NB/SB @ MD 190".
    pub name: String,
    /// Longitude.
    pub lon: f64,
    /// Latitude.
    pub lat: f64,
    /// Each direction with a reading.
    pub zones: Vec<Zone>,
    /// When it last read (Unix seconds).
    pub updated: f64,
}

impl Sensor {
    /// The slowest direction's speed.
    pub fn slowest(&self) -> f64 {
        self.zones.iter().map(|z| z.mph).fold(f64::INFINITY, f64::min)
    }

    /// The level of its slowest direction.
    pub fn level(&self) -> Level {
        Level::of(self.slowest())
    }

    /// "57 ↑ · 55 ↓": each direction's speed and heading.
    pub fn speeds(&self) -> String {
        self.zones.iter().map(|z| format!("{:.0}{}", z.mph, z.heading().map(|h| format!(" {}", arrow(h))).unwrap_or_default())).collect::<Vec<_>>().join(" · ")
    }
}

impl Zone {
    /// Which way the traffic heads, degrees from north: from the direction's name where it
    /// has one (CHART's bearings don't always match it), else (loops) the bearing.
    pub fn heading(&self) -> Option<f64> {
        match self.direction.as_str() {
            "NORTH" => Some(0.0),
            "EAST" => Some(90.0),
            "SOUTH" => Some(180.0),
            "WEST" => Some(270.0),
            "INNER_LOOP" | "OUTER_LOOP" => Some(self.bearing),
            _ => None,
        }
    }
}

/// An arrow pointing `bearing` degrees from north.
pub fn arrow(bearing: f64) -> char {
    const A: [char; 8] = ['↑', '↗', '→', '↘', '↓', '↙', '←', '↖'];
    A[(((bearing.rem_euclid(360.0) + 22.5) / 45.0) as usize) % 8]
}

/// The working sensors in CHART's TSS feed, read within the past half hour of `now`.
pub fn parse_chart(text: &str, now: f64) -> Result<Vec<Sensor>> {
    let v: Json = serde_json::from_str(text).context("CHART")?;
    let data = v.get("data").and_then(Json::as_array).context("CHART: no data")?;
    let s = |x: &Json, k: &str| x.get(k).and_then(Json::as_str).map(|s| s.trim().chars().take(120).collect::<String>()).unwrap_or_default();
    Ok(data
        .iter()
        .filter(|x| s(x, "opStatus") == "OK" && s(x, "commMode") == "ONLINE")
        .filter_map(|x| {
            let (lon, lat) = (x.get("lon")?.as_f64()?, x.get("lat")?.as_f64()?);
            if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
                return None;
            }
            let updated = x.get("lastUpdateTime")?.as_f64()? / 1000.0;
            if now - updated > STALE {
                return None;
            }
            // Speeds out of range (-1, 65535) are no reading.
            let zones: Vec<Zone> = x
                .get("zones")?
                .as_array()?
                .iter()
                .filter_map(|z| {
                    let mph = z.get("speed")?.as_f64().filter(|m| (0.0..=120.0).contains(m))?;
                    Some(Zone { direction: s(z, "direction"), bearing: z.get("bearing").and_then(Json::as_f64).unwrap_or(0.0), mph })
                })
                .take(4)
                .collect();
            (!zones.is_empty()).then(|| Sensor { id: s(x, "id"), name: s(x, "description"), lon, lat, zones, updated })
        })
        .collect())
}

/// The speed sensors now.
pub async fn sensors() -> Result<Vec<Sensor>> {
    let resp = crate::source::http_client().get(CHART_TSS).send().await.map_err(reqwest::Error::without_url).context("CHART")?;
    let resp = resp.error_for_status().map_err(reqwest::Error::without_url).context("CHART")?;
    let text = resp.text().await.map_err(reqwest::Error::without_url).context("CHART")?;
    parse_chart(&text, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()))
}

/// The sensors as GeoJSON points (`glyph`, `label`, `color`), the slowest drawn last.
pub fn points(sensors: &[Sensor]) -> Json {
    let mut v: Vec<&Sensor> = sensors.iter().collect();
    v.sort_by(|a, b| b.slowest().total_cmp(&a.slowest()));
    let features: Vec<Json> = v
        .into_iter()
        .map(|s| {
            let (r, g, b) = s.level().rgb();
            json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [s.lon, s.lat]},
                "properties": {"id": s.id, "glyph": "●", "label": s.speeds(), "color": format!("#{r:02x}{g:02x}{b:02x}")},
            })
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// The style layer for sensors from source `source`: speeds labeled from [`LABEL_ZOOM`].
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source)
        .glyph(json!(["get", "glyph"]))
        .text(json!(["step", ["zoom"], "", LABEL_ZOOM, ["get", "label"]]))
        .color("#c9d1d9")
        .paint("icon-color", json!(["get", "color"]))
}

/// A legend of the levels, with how many sensors are at each.
pub fn legend(sensors: &[Sensor]) -> crate::legend::Legend {
    use ratatui::style::Color;
    let mut l = crate::legend::Legend::new("traffic speed");
    for lv in [Level::Free, Level::Slow, Level::Heavy, Level::Stopped] {
        let n = sensors.iter().filter(|s| s.level() == lv).count();
        let (r, g, b) = lv.rgb();
        l = l.entry("●", Color::Rgb(r, g, b), lv.label(), Some(n));
    }
    l
}

/// The sensors under 35 mph within about `within_m` meters of a route's line, slowest first.
pub fn slow_along<'a>(sensors: &'a [Sensor], line: &[(f64, f64)], within_m: f64) -> Vec<&'a Sensor> {
    if line.len() < 2 {
        return Vec::new();
    }
    let (mut w, mut s, mut e, mut n) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in line {
        (w, s, e, n) = (w.min(x), s.min(y), e.max(x), n.max(y));
    }
    let pad = within_m / 111_000.0 * 2.0;
    let mut out: Vec<&Sensor> = sensors
        .iter()
        .filter(|x| x.level() >= Level::Heavy)
        .filter(|x| x.lon >= w - pad && x.lon <= e + pad && x.lat >= s - pad && x.lat <= n + pad)
        .filter(|x| {
            let k = x.lat.to_radians().cos();
            line.windows(2).any(|p| {
                // Meters, flat near the sensor.
                let (ax, ay) = ((p[0].0 - x.lon) * k * 111_320.0, (p[0].1 - x.lat) * 110_540.0);
                let (bx, by) = ((p[1].0 - x.lon) * k * 111_320.0, (p[1].1 - x.lat) * 110_540.0);
                let (dx, dy) = (bx - ax, by - ay);
                let t = (-(ax * dx + ay * dy) / (dx * dx + dy * dy).max(1e-9)).clamp(0.0, 1.0);
                (ax + t * dx).hypot(ay + t * dy) <= within_m
            })
        })
        .collect();
    out.sort_by(|a, b| a.slowest().total_cmp(&b.slowest()));
    out
}

/// A route's time with traffic: its free-flow time, except where it passes a sensor going
/// the same way, where the sensor's speed is used for about a mile around it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Estimate {
    /// Seconds with traffic.
    pub seconds: f64,
    /// How many sensors it went by.
    pub sensors: usize,
}

/// Meters along the route a sensor's speed counts for, each way.
const REACH_M: f64 = 800.0;
/// How near the route a sensor must be.
const NEAR_M: f64 = 120.0;

fn bearing(a: (f64, f64), b: (f64, f64)) -> f64 {
    let k = ((a.1 + b.1) / 2.0).to_radians().cos();
    ((b.0 - a.0) * k).atan2(b.1 - a.1).to_degrees().rem_euclid(360.0)
}

fn meters(a: (f64, f64), b: (f64, f64)) -> f64 {
    let k = ((a.1 + b.1) / 2.0).to_radians().cos();
    ((b.0 - a.0) * k * 111_320.0).hypot((b.1 - a.1) * 110_540.0)
}

/// The route's time with the sensors' speeds; `None` when it passes none.
pub fn estimate(route: &crate::route::Route, sensors: &[Sensor]) -> Option<Estimate> {
    let line = &route.line;
    if line.len() < 2 || route.steps.is_empty() {
        return None;
    }
    // Each segment's free-flow speed (m/s), from its step's length and time.
    let mut free = vec![0.0f64; line.len() - 1];
    for (i, st) in route.steps.iter().enumerate() {
        let end = route.steps.get(i + 1).map_or(line.len() - 1, |n| n.at).min(line.len() - 1);
        let v = if st.seconds > 0.0 { st.miles * 1609.344 / st.seconds } else { 0.0 };
        for f in free.iter_mut().take(end).skip(st.at.min(end)) {
            *f = v;
        }
    }
    // Distance along the route at each point.
    let mut along = vec![0.0f64; line.len()];
    for i in 1..line.len() {
        along[i] = along[i - 1] + meters(line[i - 1], line[i]);
    }
    // Each sensor on the route going the same way: where along, and the speed (m/s).
    let mut hits: Vec<(f64, f64)> = Vec::new();
    for s in sensors {
        let best = line
            .windows(2)
            .enumerate()
            .map(|(i, p)| {
                let k = s.lat.to_radians().cos();
                let (ax, ay) = ((p[0].0 - s.lon) * k * 111_320.0, (p[0].1 - s.lat) * 110_540.0);
                let (bx, by) = ((p[1].0 - s.lon) * k * 111_320.0, (p[1].1 - s.lat) * 110_540.0);
                let (dx, dy) = (bx - ax, by - ay);
                let t = (-(ax * dx + ay * dy) / (dx * dx + dy * dy).max(1e-9)).clamp(0.0, 1.0);
                ((ax + t * dx).hypot(ay + t * dy), i, t)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((d, i, t)) = best else { continue };
        if d > NEAR_M {
            continue;
        }
        let heading = bearing(line[i], line[i + 1]);
        let zone = s.zones.iter().filter_map(|z| Some((z, z.heading()?))).find(|(_, h)| {
            let diff = (h - heading).rem_euclid(360.0);
            diff.min(360.0 - diff) <= 60.0
        });
        if let Some((z, _)) = zone {
            hits.push((along[i] + t * (along[i + 1] - along[i]), (z.mph * 0.44704).max(1.5)));
        }
    }
    if hits.is_empty() {
        return None;
    }
    let mut seconds = 0.0;
    for i in 0..free.len() {
        let len = along[i + 1] - along[i];
        let mid = (along[i] + along[i + 1]) / 2.0;
        let near = hits.iter().filter(|h| (h.0 - mid).abs() <= REACH_M).min_by(|a, b| (a.0 - mid).abs().total_cmp(&(b.0 - mid).abs()));
        let v = match near {
            // Not much faster than Valhalla's own speed for the road.
            Some(&(_, v)) if free[i] > 0.0 => v.min(free[i] * 1.2),
            Some(&(_, v)) => v,
            None => free[i],
        };
        if v > 0.0 {
            seconds += len / v;
        }
    }
    // Valhalla's turn and intersection costs, kept as they are.
    let segs: f64 = free.iter().zip(along.windows(2)).filter(|(v, _)| **v > 0.0).map(|(v, a)| (a[1] - a[0]) / v).sum();
    seconds += (route.seconds - segs).max(0.0);
    Some(Estimate { seconds, sensors: hits.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"{"data":[
      {"commMode":"ONLINE","description":"I-495 NB/SB @ MD 190","id":"a","lastUpdateTime":1000000000000,"lat":38.98,"lon":-77.17,"opStatus":"OK",
       "zones":[{"bearing":274,"direction":"OUTER_LOOP","speed":57},{"bearing":94,"direction":"INNER_LOOP","speed":18}]},
      {"commMode":"ONLINE","description":"I-95 PRIOR TO I-295","id":"b","lastUpdateTime":1000000000000,"lat":38.79,"lon":-77.02,"opStatus":"COMM_FAILURE",
       "zones":[{"bearing":41,"direction":"NORTH","speed":-1}]},
      {"commMode":"ONLINE","description":"US 29 SB @ MD 198","id":"c","lastUpdateTime":1000000000000,"lat":39.08,"lon":-76.94,"opStatus":"OK",
       "zones":[{"bearing":180,"direction":"SOUTH","speed":65535}]},
      {"commMode":"ONLINE","description":"old","id":"d","lastUpdateTime":900000000000,"lat":39.0,"lon":-76.9,"opStatus":"OK",
       "zones":[{"bearing":0,"direction":"NORTH","speed":60}]}
    ],"success":true}"#;

    #[test]
    fn reads_working_sensors() {
        let s = parse_chart(FEED, 1_000_000_060.0).unwrap();
        assert_eq!(s.len(), 1, "failed, no-reading and stale sensors are left out: {s:?}");
        assert_eq!((s[0].name.as_str(), s[0].zones.len(), s[0].slowest(), s[0].level()), ("I-495 NB/SB @ MD 190", 2, 18.0, Level::Stopped));
        assert_eq!(s[0].speeds(), "57 ← · 18 →", "loops go by bearing");
        assert_eq!(points(&s)["features"][0]["properties"]["color"], "#ef4444");
        assert_eq!((Level::of(65.0), Level::of(40.0), Level::of(25.0)), (Level::Free, Level::Slow, Level::Heavy));
        assert_eq!((arrow(0.0), arrow(44.0), arrow(359.0), arrow(-90.0)), ('↑', '↗', '↑', '←'));
    }

    #[test]
    fn estimates_a_route_with_traffic() {
        use crate::route::{Route, Step};
        // Two miles due north at 60 mph (120 s), past a sensor at 20 mph northbound.
        let line: Vec<(f64, f64)> = (0..=32).map(|i| (-77.0, 39.0 + i as f64 * 0.0009)).collect();
        let r = Route { miles: 2.0, seconds: 120.0, steps: vec![Step { miles: 2.0, seconds: 120.0, at: 0, ..Step::default() }], line, ..Route::default() };
        let s = |dir: &str, mph| Sensor {
            id: "s".into(),
            lon: -77.0001,
            lat: 39.0145,
            zones: vec![Zone { direction: dir.into(), bearing: 0.0, mph }],
            ..Sensor::default()
        };
        let e = estimate(&r, &[s("NORTH", 20.0)]).unwrap();
        assert_eq!(e.sensors, 1);
        // About 1 mile at 20 mph (180 s) and 1 at 60 (60 s).
        assert!((e.seconds - 240.0).abs() < 25.0, "{}", e.seconds);
        assert!(estimate(&r, &[s("SOUTH", 20.0)]).is_none(), "the other direction doesn't count");
        let fast = estimate(&r, &[s("NORTH", 90.0)]).unwrap();
        assert!(fast.seconds >= 100.0, "not much faster than the road's speed: {}", fast.seconds);
    }

    #[test]
    fn finds_slow_sensors_along_a_route() {
        let s = parse_chart(FEED, 1_000_000_060.0).unwrap();
        // A line passing about 50 m from the sensor, and one 5 km off.
        let near = [(-77.18, 38.9805), (-77.16, 38.9805)];
        let far = [(-77.18, 39.03), (-77.16, 39.03)];
        assert_eq!(slow_along(&s, &near, 150.0).len(), 1);
        assert!(slow_along(&s, &far, 150.0).is_empty());
    }
}
