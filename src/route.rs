//! Directions from Valhalla, the open routing engine over OpenStreetMap: by default the
//! public server FOSSGIS runs (valhalla1.openstreetmap.de), or your own (`--router`,
//! `MAPTERM_ROUTER`).
//!
//! The public server is fair-use, like OpenStreetMap's other demo services: mapterm asks only
//! when you ask for a route, at most one request a second, identified with an `X-Client-Id`
//! header and its User-Agent, as Valhalla's maintainers ask of apps.
//!
//! A route can avoid points (Valhalla's `exclude_locations`: the road at each point is left
//! out), which is how `--closures` keeps routes off closed roads. Estimates, without traffic.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};

/// FOSSGIS's public Valhalla server.
pub const PUBLIC: &str = "https://valhalla1.openstreetmap.de";
/// The credit shown with a route.
pub const CREDIT: &str = "Routing: Valhalla (FOSSGIS), © OpenStreetMap contributors";
/// How many other routes to ask for, beside the best.
pub const ALTERNATES: usize = 2;
/// How many points a route may be asked to avoid.
pub const MAX_AVOID: usize = 50;
/// The least time between requests to the public server.
const MIN_GAP: Duration = Duration::from_millis(1100);

/// How you're getting there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Driving.
    #[default]
    Car,
    /// Cycling.
    Bike,
    /// Walking.
    Walk,
}

impl Mode {
    /// `car`, `bike`, `walk` (and `drive`, `auto`, `bicycle`, `foot`, `pedestrian`).
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "car" | "drive" | "auto" => Mode::Car,
            "bike" | "bicycle" | "cycle" => Mode::Bike,
            "walk" | "foot" | "pedestrian" => Mode::Walk,
            _ => return None,
        })
    }

    fn costing(self) -> &'static str {
        match self {
            Mode::Car => "auto",
            Mode::Bike => "bicycle",
            Mode::Walk => "pedestrian",
        }
    }

    /// "drive", "ride", "walk".
    pub fn verb(self) -> &'static str {
        match self {
            Mode::Car => "drive",
            Mode::Bike => "ride",
            Mode::Walk => "walk",
        }
    }
}

/// One step of the directions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Step {
    /// "Turn right onto King Street/VA 7."
    pub instruction: String,
    /// Miles.
    pub miles: f64,
    /// Seconds.
    pub seconds: f64,
    /// Where it starts along the route's line (an index into [`Route::line`]).
    pub at: usize,
    /// The roads it's on.
    pub streets: Vec<String>,
}

/// A route.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Route {
    /// Miles in all.
    pub miles: f64,
    /// Seconds in all, without traffic.
    pub seconds: f64,
    /// The way, lon/lat, start to end.
    pub line: Vec<(f64, f64)>,
    /// The directions.
    pub steps: Vec<Step>,
    /// How you're going.
    pub mode: Mode,
    /// How many points it was asked to avoid.
    pub avoided: usize,
}

impl Route {
    /// The route as a GeoJSON LineString.
    pub fn geometry(&self) -> Json {
        json!({"type": "LineString", "coordinates": self.line.iter().map(|(x, y)| [*x, *y]).collect::<Vec<_>>()})
    }

    /// The road it spends the most miles on ("I 70"), to tell routes apart.
    pub fn via(&self) -> Option<String> {
        let mut miles: Vec<(String, f64)> = Vec::new();
        for s in &self.steps {
            let Some(name) = s.streets.first() else { continue };
            match miles.iter_mut().find(|(n, _)| n == name) {
                Some((_, m)) => *m += s.miles,
                None => miles.push((name.clone(), s.miles)),
            }
        }
        miles.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(n, _)| n)
    }

    /// Its extent: (west, south, east, north).
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y) in &self.line {
            b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
        }
        (b.0 <= b.2).then_some(b)
    }
}

/// An encoded polyline (Google's format) at precision `digits` (Valhalla's is 6) as lon/lat.
pub fn decode_polyline(s: &str, digits: u32) -> Vec<(f64, f64)> {
    let scale = 10f64.powi(digits as i32);
    let bytes = s.as_bytes();
    let (mut i, mut lat, mut lon) = (0usize, 0i64, 0i64);
    let mut out = Vec::new();
    let next = |i: &mut usize| -> Option<i64> {
        let (mut result, mut shift) = (0i64, 0u32);
        loop {
            let b = (*bytes.get(*i)? as i64) - 63;
            *i += 1;
            result |= (b & 0x1f) << shift;
            shift += 5;
            if b < 0x20 || shift > 60 {
                break;
            }
        }
        Some(if result & 1 != 0 { !(result >> 1) } else { result >> 1 })
    };
    while i < bytes.len() {
        let (Some(dlat), Some(dlon)) = (next(&mut i), next(&mut i)) else { break };
        lat += dlat;
        lon += dlon;
        out.push((lon as f64 / scale, lat as f64 / scale));
    }
    out
}

/// The routes in Valhalla's `/route` answer: the best, then the alternates.
pub fn parse(text: &str, mode: Mode, avoided: usize) -> Result<Vec<Route>> {
    let v: Json = serde_json::from_str(text).context("Valhalla")?;
    if let Some(e) = v.get("error") {
        bail!("{}", e.as_str().unwrap_or("no route"));
    }
    let trip = v.get("trip").context("Valhalla: no trip in the answer")?;
    let mut routes = vec![parse_trip(trip, mode, avoided)?];
    for a in v["alternates"].as_array().into_iter().flatten() {
        if let Ok(r) = parse_trip(&a["trip"], mode, avoided) {
            routes.push(r);
        }
    }
    Ok(routes)
}

fn parse_trip(trip: &Json, mode: Mode, avoided: usize) -> Result<Route> {
    let mut line: Vec<(f64, f64)> = Vec::new();
    let mut steps = Vec::new();
    for leg in trip["legs"].as_array().into_iter().flatten() {
        let base = line.len();
        let shape = decode_polyline(leg["shape"].as_str().unwrap_or(""), 6);
        for m in leg["maneuvers"].as_array().into_iter().flatten() {
            steps.push(Step {
                instruction: m["instruction"].as_str().unwrap_or("").chars().take(200).collect(),
                miles: m["length"].as_f64().unwrap_or(0.0),
                seconds: m["time"].as_f64().unwrap_or(0.0),
                at: base + m["begin_shape_index"].as_u64().unwrap_or(0) as usize,
                streets: m["street_names"].as_array().into_iter().flatten().filter_map(|n| n.as_str()).take(4).map(|n| n.chars().take(80).collect()).collect(),
            });
        }
        line.extend(shape);
    }
    if line.len() < 2 {
        bail!("Valhalla: the route has no line");
    }
    Ok(Route { miles: trip["summary"]["length"].as_f64().unwrap_or(0.0), seconds: trip["summary"]["time"].as_f64().unwrap_or(0.0), line, steps, mode, avoided })
}

/// The `/route` request: from, to (lon/lat), how, avoiding `avoid` (at most [`MAX_AVOID`]).
pub fn request(from: (f64, f64), to: (f64, f64), mode: Mode, avoid: &[(f64, f64)]) -> Json {
    let loc = |(lon, lat): (f64, f64)| json!({"lat": lat, "lon": lon});
    let mut r = json!({
        "locations": [loc(from), loc(to)],
        "costing": mode.costing(),
        "alternates": ALTERNATES,
        "directions_options": {"units": "miles"},
    });
    if !avoid.is_empty() {
        r["exclude_locations"] = Json::Array(avoid.iter().take(MAX_AVOID).map(|p| loc(*p)).collect());
    }
    r
}

/// When the last request went to the public server.
static LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// Routes from Valhalla at `server` (the public one, else yours): the best, then up to
/// [`ALTERNATES`] others.
pub async fn route(server: &str, from: (f64, f64), to: (f64, f64), mode: Mode, avoid: &[(f64, f64)]) -> Result<Vec<Route>> {
    // At most one request a second to the public server.
    if server == PUBLIC {
        let wait = {
            let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
            let wait = last.map_or(Duration::ZERO, |t| MIN_GAP.saturating_sub(t.elapsed()));
            *last = Some(Instant::now() + wait);
            wait
        };
        tokio::time::sleep(wait).await;
    }
    let body = request(from, to, mode, avoid);
    let resp = crate::source::http_client()
        .post(format!("{}/route", server.trim_end_matches('/')))
        .header("X-Client-Id", "mapterm")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .context("Valhalla")?;
    let status = resp.status();
    let text = resp.text().await.context("Valhalla")?;
    if !status.is_success() {
        // Valhalla explains a route it can't find (400, with "error").
        if let Ok(r) = parse(&text, mode, 0) {
            return Ok(r);
        }
        let why = serde_json::from_str::<Json>(&text).ok().and_then(|v| v["error"].as_str().map(str::to_string));
        bail!("Valhalla: {}", why.unwrap_or_else(|| format!("HTTP {status}")));
    }
    parse(&text, mode, avoid.len().min(MAX_AVOID))
}

/// Points on the closed roads near a trip from `from` to `to`, for a route to avoid: each
/// closed line's middle (and both ends), each closed point; those inside the trip's box and a
/// margin, nearest the straight line first, at most [`MAX_AVOID`].
pub fn avoid_closures(closures: &[crate::closures::Closure], now: f64, from: (f64, f64), to: (f64, f64)) -> Vec<(f64, f64)> {
    let (w, e) = (from.0.min(to.0), from.0.max(to.0));
    let (s, n) = (from.1.min(to.1), from.1.max(to.1));
    let (mx, my) = (((e - w) * 0.3).max(0.05), ((n - s) * 0.3).max(0.05));
    let inside = |p: &(f64, f64)| p.0 >= w - mx && p.0 <= e + mx && p.1 >= s - my && p.1 <= n + my;
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for c in closures.iter().filter(|c| c.impact == crate::closures::Impact::Closed && c.shown(now) && !c.planned(now)) {
        let g = &c.geometry;
        let coords = |a: &Json| -> Vec<(f64, f64)> { a.as_array().into_iter().flatten().filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?))).collect() };
        let lines: Vec<Vec<(f64, f64)>> = match g["type"].as_str() {
            Some("LineString") => vec![coords(&g["coordinates"])],
            Some("MultiLineString") => g["coordinates"].as_array().into_iter().flatten().map(coords).collect(),
            Some("Point") => vec![coords(&json!([g["coordinates"]]))],
            Some("MultiPoint") => vec![coords(&g["coordinates"])],
            _ => Vec::new(),
        };
        for l in lines.into_iter().filter(|l| !l.is_empty()) {
            pts.push(l[l.len() / 2]);
            if l.len() > 2 {
                pts.push(l[0]);
                pts.push(l[l.len() - 1]);
            }
        }
    }
    pts.retain(inside);
    // Nearest the straight line first.
    let d = |p: &(f64, f64)| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let len2 = (dx * dx + dy * dy).max(1e-12);
        let t = (((p.0 - from.0) * dx + (p.1 - from.1) * dy) / len2).clamp(0.0, 1.0);
        (p.0 - (from.0 + t * dx)).powi(2) + (p.1 - (from.1 + t * dy)).powi(2)
    };
    pts.sort_by(|a, b| d(a).total_cmp(&d(b)));
    pts.dedup();
    pts.truncate(MAX_AVOID);
    pts
}

/// "41.7 mi", "800 ft".
pub fn distance(miles: f64) -> String {
    if miles < 0.2 {
        format!("{:.0} ft", miles * 5280.0)
    } else if miles < 10.0 {
        format!("{miles:.1} mi")
    } else {
        format!("{miles:.0} mi")
    }
}

/// "47 min", "1 h 05 min".
pub fn duration(seconds: f64) -> String {
    let m = (seconds / 60.0).round() as i64;
    if m < 60 { format!("{m} min") } else { format!("{} h {:02} min", m / 60, m % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_polylines() {
        // Google's example at precision 5: (38.5, -120.2), (40.7, -120.95), (43.252, -126.453).
        let p = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@", 5);
        assert_eq!(p.len(), 3);
        assert!((p[0].0 + 120.2).abs() < 1e-9 && (p[0].1 - 38.5).abs() < 1e-9);
        assert!((p[2].0 + 126.453).abs() < 1e-9 && (p[2].1 - 43.252).abs() < 1e-9);
        assert!(decode_polyline("", 6).is_empty());
    }

    #[test]
    fn reads_a_route_and_its_steps() {
        let text = r#"{"trip":{"status":0,"units":"miles","summary":{"length":41.69,"time":2795.3},
          "legs":[{"shape":"_p~iF~ps|U_ulLnnqC_mqNvxq`@","maneuvers":[
            {"instruction":"Drive north on South Washington Street/VA 400.","length":0.0965,"time":13.7,"begin_shape_index":0},
            {"instruction":"Turn right onto King Street/VA 7.","street_names":["King Street","VA 7"],"length":0.2367,"time":43.8,"begin_shape_index":1}]}]},
          "alternates":[{"trip":{"summary":{"length":50.0,"time":3600},"legs":[{"shape":"_p~iF~ps|U_ulLnnqC","maneuvers":[]}]}}]}"#;
        assert_eq!(parse(text, Mode::Car, 0).unwrap().len(), 2, "the best and an alternate");
        let r = parse(text, Mode::Car, 3).unwrap().remove(0);
        assert_eq!((r.miles, r.steps.len(), r.line.len(), r.avoided), (41.69, 2, 3, 3));
        assert_eq!(r.steps[1].instruction, "Turn right onto King Street/VA 7.");
        assert!((r.line[0].0 + 12.02).abs() < 1e-9 && (r.line[0].1 - 3.85).abs() < 1e-9, "read at precision 6: {:?}", r.line[0]);
        assert_eq!(r.geometry()["type"], "LineString");
        assert!(r.bounds().is_some());
        assert_eq!(r.via().as_deref(), Some("King Street"));
        assert!(parse(r#"{"error_code":442,"error":"No path could be found for input"}"#, Mode::Car, 0).unwrap_err().to_string().contains("No path"));
        assert_eq!((distance(0.1), distance(3.25), distance(41.7)), ("528 ft".to_string(), "3.2 mi".to_string(), "42 mi".to_string()));
        assert_eq!((duration(2795.0), duration(3900.0)), ("47 min".to_string(), "1 h 05 min".to_string()));
        assert_eq!((Mode::parse("bike"), Mode::parse("drive"), Mode::parse("boat")), (Some(Mode::Bike), Some(Mode::Car), None));
    }

    #[test]
    fn asks_to_avoid_closed_roads_near_the_trip() {
        use crate::closures::{Closure, Impact};
        let c =
            |impact, coords: Json| Closure { key: "k".into(), impact, geometry: json!({"type": "LineString", "coordinates": coords}), ..Closure::default() };
        let closures = vec![
            c(Impact::Closed, json!([[-77.0, 38.9], [-76.9, 38.9], [-76.8, 38.9]])),
            c(Impact::Lanes, json!([[-77.0, 38.95], [-76.9, 38.95]])),
            c(Impact::Closed, json!([[-80.0, 40.0], [-80.1, 40.0]])),
        ];
        let a = avoid_closures(&closures, 1.0e9, (-77.05, 38.8), (-76.5, 39.0));
        assert_eq!(a.len(), 3, "the closed line near the trip (middle and ends): {a:?}");
        assert!(a.contains(&(-76.9, 38.9)));
        let r = request((-77.05, 38.8), (-76.5, 39.0), Mode::Car, &a);
        assert_eq!((r["costing"].as_str(), r["alternates"].as_u64()), (Some("auto"), Some(2)));
        assert_eq!(r["exclude_locations"].as_array().unwrap().len(), 3);
        assert_eq!(r["locations"][0]["lat"], 38.8);
        assert!(request((0.0, 0.0), (1.0, 1.0), Mode::Walk, &[]).get("exclude_locations").is_none());
    }
}
