//! US weather alerts from the National Weather Service (api.weather.gov, public domain, no
//! key): warnings, watches and advisories, each with the area it covers.
//!
//! A few alerts (thunderstorm, tornado and flood warnings) carry their own outline; most
//! name forecast or marine zones instead. Zone outlines come one zone a request, so they're
//! kept on disk for 90 days (zones rarely change) in `~/.cache/mapterm/nws/zones`, and a
//! caller fetches the missing ones slowly, the most severe alerts' first ([`zones_needed`],
//! [`zone_outline`]). The list itself is one request ([`active`]), asked again with its
//! ETag so an unchanged list is a tiny reply.
//!
//! The alert fields and severity order follow skycast (the same author's weather app).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const ACTIVE: &str = "https://api.weather.gov/alerts/active?status=actual";
const ZONES: &str = "https://api.weather.gov/zones/";
/// How far (degrees) an outline may move when simplified: a few hundred meters, finer
/// than a terminal cell shows below zoom 10. Zone outlines come with up to a thousand points
/// along every inlet; simplified they're a fraction of that, to keep and to draw.
pub const SIMPLIFY: f64 = 0.003;
/// How long a zone outline is kept.
const ZONE_FRESH: Duration = Duration::from_secs(90 * 24 * 3600);
/// The credit shown while alerts are on.
pub const CREDIT: &str = "NWS alerts (weather.gov)";

/// How severe an alert is, least first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Not given.
    #[default]
    Unknown,
    /// Minor.
    Minor,
    /// Moderate.
    Moderate,
    /// Severe.
    Severe,
    /// Extreme.
    Extreme,
}

impl Severity {
    fn parse(s: &str) -> Severity {
        match s {
            "Extreme" => Severity::Extreme,
            "Severe" => Severity::Severe,
            "Moderate" => Severity::Moderate,
            "Minor" => Severity::Minor,
            _ => Severity::Unknown,
        }
    }

    /// Its name.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Extreme => "extreme",
            Severity::Severe => "severe",
            Severity::Moderate => "moderate",
            Severity::Minor => "minor",
            Severity::Unknown => "unknown",
        }
    }

    /// Its color on the map.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Severity::Extreme => (0xd2, 0x7a, 0xff),
            Severity::Severe => (0xff, 0x55, 0x55),
            Severity::Moderate => (0xff, 0xa6, 0x4d),
            Severity::Minor => (0xf2, 0xd0, 0x4d),
            Severity::Unknown => (0x9a, 0xa4, 0xae),
        }
    }
}

/// One alert.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Alert {
    /// The NWS id.
    pub id: String,
    /// What it is: "Flood Warning", "Small Craft Advisory".
    pub event: String,
    /// How severe.
    pub severity: Severity,
    /// The one-line summary.
    pub headline: Option<String>,
    /// The full text.
    pub description: String,
    /// What to do.
    pub instruction: Option<String>,
    /// The places it covers, in words.
    pub area: String,
    /// When it ends (or else expires), as the NWS gives it (ISO 8601 with offset).
    pub until: Option<String>,
    /// When it's in force, Unix seconds: from its onset (or else when it took effect or was
    /// sent) to its end (or else expiry); None where the NWS doesn't say. Like a Cesium
    /// entity's `availability`, for showing what was in force at the clock's time.
    pub from: Option<f64>,
    /// See [`Alert::from`].
    pub to: Option<f64>,
    /// Who issued it.
    pub sender: String,
    /// Its own outline (GeoJSON geometry), when it has one.
    pub geometry: Option<Value>,
    /// The zones it covers otherwise, as `forecast/VAZ092`.
    pub zones: Vec<String>,
}

/// Alerts from the NWS GeoJSON, most severe first.
pub fn parse(text: &str) -> Result<Vec<Alert>> {
    let v: Value = serde_json::from_str(text).context("NWS alerts")?;
    let Some(features) = v.get("features").and_then(Value::as_array) else { bail!("NWS alerts: no features") };
    let s = |p: &Value, k: &str| p.get(k).and_then(Value::as_str).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let mut out: Vec<Alert> = features
        .iter()
        .filter_map(|f| {
            let p = f.get("properties")?;
            Some(Alert {
                id: s(p, "id")?,
                event: s(p, "event").unwrap_or_else(|| "Alert".into()),
                severity: Severity::parse(&s(p, "severity").unwrap_or_default()),
                headline: s(p, "headline"),
                description: s(p, "description").unwrap_or_default(),
                instruction: s(p, "instruction"),
                area: s(p, "areaDesc").unwrap_or_default(),
                until: s(p, "ends").or_else(|| s(p, "expires")),
                from: s(p, "onset").or_else(|| s(p, "effective")).or_else(|| s(p, "sent")).and_then(|t| unix_time(&t)),
                to: s(p, "ends").or_else(|| s(p, "expires")).and_then(|t| unix_time(&t)),
                sender: s(p, "senderName").unwrap_or_default(),
                geometry: f.get("geometry").filter(|g| g.is_object()).map(|g| simplify(g, SIMPLIFY)).filter(|g| g.is_object()),
                zones: p.get("affectedZones").and_then(Value::as_array).map(|z| z.iter().filter_map(|u| zone_id(u.as_str()?)).collect()).unwrap_or_default(),
            })
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.severity));
    Ok(out)
}

impl Alert {
    /// Whether it's in force at `t` (Unix seconds); an end or start not given doesn't limit it.
    pub fn in_force(&self, t: f64) -> bool {
        self.from.is_none_or(|f| t >= f) && self.to.is_none_or(|e| t < e)
    }
}

/// `2026-11-26T06:00:00-05:00` (or `Z`) as Unix seconds.
pub fn unix_time(s: &str) -> Option<f64> {
    let (date, rest) = s.trim().split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let (clock, offset) = match rest.find(['Z', '+', '-']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "Z"),
    };
    let mut c = clock.split(':');
    let (h, mi): (i64, i64) = (c.next()?.parse().ok()?, c.next()?.parse().ok()?);
    let sec: f64 = c.next().map_or(Some(0.0), |x| x.parse().ok())?;
    let off = match offset {
        "Z" => 0,
        o => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let (oh, om) = o[1..].split_once(':').unwrap_or((&o[1..], "0"));
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
        }
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + h * 3600 + mi * 60 - off) as f64 + sec)
}

/// `geometry` (a GeoJSON Polygon or MultiPolygon) with points closer than `tolerance`
/// degrees to the line through their neighbors dropped (Douglas-Peucker on each ring); rings
/// that shrink below a triangle go. Anything else is returned as it is.
pub fn simplify(geometry: &Value, tolerance: f64) -> Value {
    let ring = |r: &Value| -> Option<Value> {
        let pts: Vec<(f64, f64)> = r.as_array()?.iter().filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?))).collect();
        let kept = douglas_peucker(&pts, tolerance);
        (kept.len() >= 4).then(|| json!(kept.iter().map(|(x, y)| [*x, *y]).collect::<Vec<_>>()))
    };
    let polygon = |p: &Value| -> Option<Value> {
        let rings: Vec<Value> = p.as_array()?.iter().filter_map(ring).collect();
        (!rings.is_empty()).then_some(Value::Array(rings))
    };
    match geometry.get("type").and_then(Value::as_str) {
        Some("Polygon") => match polygon(&geometry["coordinates"]) {
            Some(c) => json!({"type": "Polygon", "coordinates": c}),
            None => Value::Null,
        },
        Some("MultiPolygon") => {
            let polys: Vec<Value> = geometry["coordinates"].as_array().map(|a| a.iter().filter_map(polygon).collect()).unwrap_or_default();
            if polys.is_empty() { Value::Null } else { json!({"type": "MultiPolygon", "coordinates": polys}) }
        }
        _ => geometry.clone(),
    }
}

fn douglas_peucker(pts: &[(f64, f64)], tol: f64) -> Vec<(f64, f64)> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let ((ax, ay), (bx, by)) = (pts[a], pts[b]);
        let (dx, dy) = (bx - ax, by - ay);
        let len = (dx * dx + dy * dy).sqrt();
        let (mut far, mut at) = (0.0, 0);
        for (i, &(px, py)) in pts.iter().enumerate().take(b).skip(a + 1) {
            // Distance to the segment's line, or to its start when it has no length (a
            // closed ring's first and last points are the same).
            let d = if len > 0.0 { ((px - ax) * dy - (py - ay) * dx).abs() / len } else { ((px - ax).powi(2) + (py - ay).powi(2)).sqrt() };
            if d > far {
                (far, at) = (d, i);
            }
        }
        if far > tol {
            keep[at] = true;
            stack.push((a, at));
            stack.push((at, b));
        }
    }
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// A zone URL's id, `forecast/VAZ092`: only api.weather.gov's zones, in plain letters and
/// digits (it's put into a URL and a file name).
pub fn zone_id(url: &str) -> Option<String> {
    let rest = url.strip_prefix(ZONES)?;
    let (kind, id) = rest.split_once('/')?;
    let plain = |s: &str| !s.is_empty() && s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric());
    (plain(kind) && plain(id)).then(|| format!("{kind}/{id}"))
}

/// The active alerts, unless they're unchanged since `etag` (then None); with the new ETag.
pub async fn active(etag: Option<&str>) -> Result<Option<(Vec<Alert>, Option<String>)>> {
    let mut req = crate::source::http_client().get(ACTIVE).header("Accept", "application/geo+json");
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    let resp = req.send().await.context("NWS")?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if !resp.status().is_success() {
        bail!("NWS answered HTTP {}", resp.status());
    }
    let etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
    let text = resp.text().await.context("NWS")?;
    Ok(Some((parse(&text)?, etag)))
}

fn zone_file(id: &str) -> std::path::PathBuf {
    crate::source::cache_root().join("nws").join("zones").join(format!("{}.json", id.replace('/', "_")))
}

/// A zone's outline from the disk cache, if it's there and under 90 days old.
pub fn cached_zone(id: &str) -> Option<Value> {
    let p = zone_file(id);
    let age = std::time::SystemTime::now().duration_since(std::fs::metadata(&p).ok()?.modified().ok()?).ok()?;
    if age > ZONE_FRESH {
        return None;
    }
    let raw = std::fs::read(&p).ok()?;
    let g = simplify(&serde_json::from_slice(&raw).ok()?, SIMPLIFY);
    // Saved by an older mapterm at full detail: saved again simplified, keeping its date (the
    // 90 days still count from when it was fetched).
    if let Ok(small) = serde_json::to_vec(&g)
        && small.len() * 5 < raw.len() * 4
    {
        let modified = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        let tmp = p.with_extension("part");
        if std::fs::write(&tmp, &small).is_ok()
            && std::fs::rename(&tmp, &p).is_ok()
            && let (Some(t), Ok(f)) = (modified, std::fs::File::options().write(true).open(&p))
        {
            let _ = f.set_modified(t);
        }
    }
    Some(g)
}

/// A zone's outline (GeoJSON geometry): from the disk cache, else one request, kept. A zone
/// without one gives `null`, kept too, so it isn't asked for again.
pub async fn zone_outline(id: &str) -> Result<Value> {
    if let Some(g) = cached_zone(id) {
        return Ok(g);
    }
    if zone_id(&format!("{ZONES}{id}")).as_deref() != Some(id) {
        bail!("not an NWS zone: {id:?}");
    }
    let resp = crate::source::http_client().get(format!("{ZONES}{id}")).header("Accept", "application/geo+json").send().await.context("NWS zone")?;
    let g = match resp.status() {
        s if s.is_success() => serde_json::from_str::<Value>(&resp.text().await?).context("NWS zone")?.get("geometry").cloned().unwrap_or(Value::Null),
        reqwest::StatusCode::NOT_FOUND => Value::Null,
        s => bail!("NWS zone {id}: HTTP {s}"),
    };
    // Kept simplified: a fraction of the size, and all mapterm draws.
    let g = simplify(&g, SIMPLIFY);
    let p = zone_file(id);
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
    let _ = std::fs::write(&p, serde_json::to_vec(&g)?);
    Ok(g)
}

/// The zones `alerts` need that `have` lacks, the most severe alerts' first, each once.
pub fn zones_needed(alerts: &[Alert], have: &HashMap<String, Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in alerts.iter().filter(|a| a.geometry.is_none()) {
        for z in &a.zones {
            if !have.contains_key(z) && !out.contains(z) {
                out.push(z.clone());
            }
        }
    }
    out
}

/// The alerts as GeoJSON for the map, least severe first (so the worst draws on top): each
/// with its own outline or its zones' (those in `zones`), and properties `id`, `event`,
/// `severity` and `color`. Only those in force at `t` (Unix seconds); alerts with no
/// outline yet are left out.
pub fn features(alerts: &[Alert], zones: &HashMap<String, Value>, t: f64) -> Value {
    let mut feats = Vec::new();
    for a in alerts.iter().rev().filter(|a| a.in_force(t)) {
        let geometry = match &a.geometry {
            Some(g) => g.clone(),
            None => {
                let mut polys: Vec<Value> = Vec::new();
                for g in a.zones.iter().filter_map(|z| zones.get(z)) {
                    match g.get("type").and_then(Value::as_str) {
                        Some("Polygon") => polys.push(g["coordinates"].clone()),
                        Some("MultiPolygon") => polys.extend(g["coordinates"].as_array().cloned().unwrap_or_default()),
                        _ => {}
                    }
                }
                if polys.is_empty() {
                    continue;
                }
                json!({"type": "MultiPolygon", "coordinates": polys})
            }
        };
        let (r, g, b) = a.severity.rgb();
        feats.push(json!({
            "type": "Feature",
            "geometry": geometry,
            "properties": {"id": a.id, "event": a.event, "severity": a.severity.label(), "color": format!("#{r:02x}{g:02x}{b:02x}")},
        }));
    }
    json!({"type": "FeatureCollection", "features": feats})
}

/// The alerts in force at `t` drawn for the view `vp` into a `w` x `h` image (RGBA rows,
/// for a kitty image under the text): each area a translucent fill and a smooth outline in
/// its severity's color, the least severe first so the worst is on top. `cell_px` is the
/// cell size in the image's pixels.
pub fn render(alerts: &[Alert], zones: &HashMap<String, Value>, t: f64, vp: &crate::Viewport, w: u32, h: u32, cell_px: (f64, f64)) -> Vec<u8> {
    use tiny_skia::{FillRule, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let Some(mut pm) = Pixmap::new(w.max(1), h.max(1)) else { return Vec::new() };
    let px = |lon: f64, lat: f64| {
        let (dx, dy) = vp.dot_ll(lon, lat);
        ((dx * cell_px.0 / 2.0) as f32, (dy * cell_px.1 / 4.0) as f32)
    };
    let stroke = Stroke { width: (cell_px.0 / 7.0).max(1.2) as f32, line_join: LineJoin::Round, ..Stroke::default() };
    let rings_of = |g: &Value, out: &mut Vec<Value>| match g.get("type").and_then(Value::as_str) {
        Some("Polygon") => out.extend(g["coordinates"].as_array().cloned().unwrap_or_default()),
        Some("MultiPolygon") => {
            for p in g["coordinates"].as_array().into_iter().flatten() {
                out.extend(p.as_array().cloned().unwrap_or_default());
            }
        }
        _ => {}
    };
    for a in alerts.iter().rev().filter(|a| a.in_force(t)) {
        let mut rings = Vec::new();
        match &a.geometry {
            Some(g) => rings_of(g, &mut rings),
            None => a.zones.iter().filter_map(|z| zones.get(z)).for_each(|g| rings_of(g, &mut rings)),
        }
        let mut pb = PathBuilder::new();
        for r in &rings {
            let mut first = true;
            for p in r.as_array().into_iter().flatten() {
                let (Some(lon), Some(lat)) = (p.get(0).and_then(Value::as_f64), p.get(1).and_then(Value::as_f64)) else { continue };
                let (x, y) = px(lon, lat);
                if first {
                    pb.move_to(x, y);
                    first = false;
                } else {
                    pb.line_to(x, y);
                }
            }
            pb.close();
        }
        let Some(path) = pb.finish() else { continue };
        let (r, g, b) = a.severity.rgb();
        let mut paint = Paint { anti_alias: true, ..Paint::default() };
        paint.set_color_rgba8(r, g, b, 38);
        pm.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
        paint.set_color_rgba8(r, g, b, 230);
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
    crate::timeseries::to_rgba(pm)
}

/// The map layers for alerts from GeoJSON source `source`: a faint fill and an outline in
/// each alert's color.
pub fn layers(source: &str) -> Result<Vec<crate::StyleLayer>> {
    let fill = json!({"id": format!("{source}-fill"), "type": "fill", "source": source, "paint": {"fill-color": ["get", "color"], "fill-opacity": 0.12}});
    let line = json!({"id": format!("{source}-line"), "type": "line", "source": source, "paint": {"line-color": ["get", "color"], "line-width": 1}});
    [fill, line].iter().map(|j| Ok(crate::StyleLayer::from_json(j)?.0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"type":"FeatureCollection","features":[
      {"id":"a","type":"Feature","geometry":null,"properties":{"id":"urn:a","event":"Small Craft Advisory","severity":"Minor",
        "areaDesc":"Chesapeake Bay","expires":"2026-11-26T06:00:00-05:00","senderName":"NWS Wakefield VA",
        "affectedZones":["https://api.weather.gov/zones/forecast/ANZ630","https://api.weather.gov/zones/forecast/ANZ631","https://evil.example/zones/forecast/X1"]}},
      {"id":"b","type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-77,38],[-76,38],[-76,39],[-77,38]]]},
        "properties":{"id":"urn:b","event":"Flood Warning","severity":"Severe","headline":"Flood Warning until 6 AM",
        "description":"Minor flooding.","instruction":"Turn around, don't drown.","areaDesc":"Fairfax","ends":"2026-11-26T06:00:00-05:00",
        "affectedZones":["https://api.weather.gov/zones/county/VAC059"]}}
    ]}"#;

    #[test]
    fn reads_alerts_most_severe_first() {
        let a = parse(SAMPLE).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].event.as_str(), a[0].severity), ("Flood Warning", Severity::Severe));
        assert!(a[0].geometry.is_some());
        assert_eq!(a[0].until.as_deref(), Some("2026-11-26T06:00:00-05:00"), "ends");
        assert_eq!(a[1].until.as_deref(), Some("2026-11-26T06:00:00-05:00"), "else expires");
        assert_eq!(a[1].zones, vec!["forecast/ANZ630", "forecast/ANZ631"], "only api.weather.gov's zones");
        // 06:00 at -05:00 is 11:00 UTC.
        assert_eq!(a[0].to, unix_time("2026-11-26T11:00:00Z"));
        assert_eq!(unix_time("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(unix_time("2000-03-01T00:00:00+00:00"), Some(951_868_800.0));
        assert!(a[0].in_force(1_795_660_000.0) && !a[0].in_force(a[0].to.unwrap()), "in force until it ends");
    }

    #[test]
    fn a_click_on_an_area_finds_its_alert() {
        let a = parse(SAMPLE).unwrap();
        let mut map = crate::Map::new();
        map.add_source("alerts", crate::MapSource::geojson(&features(&a, &HashMap::new(), 1_795_660_000.0)).unwrap()).unwrap();
        map.add_layers(layers("alerts").unwrap(), None).unwrap();
        let (cam, area) = (crate::Camera::new(-76.5, 38.5, 7.0), ratatui::layout::Rect::new(0, 0, 80, 24));
        // Inside the flood warning's triangle.
        let dot = crate::Viewport::new(&cam, area).dot_ll(-76.3, 38.2);
        let ids: Vec<String> = map
            .query_features(&cam, area, dot)
            .into_iter()
            .filter(|p| p.source.as_deref() == Some("alerts"))
            .filter_map(|p| p.prop("id")?.as_str().map(str::to_string))
            .collect();
        assert!(ids.contains(&"urn:b".to_string()), "{ids:?}");
        // Outside it: nothing.
        let dot = crate::Viewport::new(&cam, area).dot_ll(-77.4, 38.9);
        assert!(map.query_features(&cam, area, dot).iter().all(|p| p.source.as_deref() != Some("alerts")));
    }

    #[test]
    fn simplifies_outlines() {
        // A square with a wiggle of points along one side, all within the tolerance.
        let mut ring: Vec<[f64; 2]> = vec![[0.0, 0.0]];
        for i in 1..100 {
            ring.push([i as f64 / 100.0, if i % 2 == 0 { 0.0005 } else { -0.0005 }]);
        }
        ring.extend([[1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]]);
        let g = json!({"type": "Polygon", "coordinates": [ring]});
        let s = simplify(&g, SIMPLIFY);
        assert_eq!(s["coordinates"][0].as_array().unwrap().len(), 5, "the square's corners and its closing point: {s}");
        // A sliver smaller than the tolerance goes altogether.
        let tiny = json!({"type": "MultiPolygon", "coordinates": [[[[0.0, 0.0], [0.001, 0.0], [0.001, 0.001], [0.0, 0.0]]]]});
        assert_eq!(simplify(&tiny, SIMPLIFY), Value::Null);
        assert_eq!(simplify(&json!({"type": "Point", "coordinates": [1, 2]}), SIMPLIFY)["type"], "Point");
    }

    #[test]
    fn renders_areas_in_their_colors() {
        let a = parse(SAMPLE).unwrap();
        let (cam, area) = (crate::Camera::new(-76.5, 38.5, 7.0), ratatui::layout::Rect::new(0, 0, 80, 24));
        let vp = crate::Viewport::new(&cam, area);
        let (w, h) = (720, 480);
        let rgba = render(&a, &HashMap::new(), 1_795_660_000.0, &vp, w, h, (9.0, 20.0));
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        // Inside the flood warning's triangle: a faint red; far outside: clear.
        let at = |lon: f64, lat: f64| {
            let (dx, dy) = vp.dot_ll(lon, lat);
            let (x, y) = ((dx * 4.5) as usize, (dy * 5.0) as usize);
            let i = (y * w as usize + x) * 4;
            [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
        };
        let inside = at(-76.3, 38.2);
        assert!(inside[3] > 20 && inside[3] < 80 && inside[0] > inside[2], "{inside:?}");
        assert_eq!(at(-77.4, 38.9)[3], 0);
        // After it ends: nothing drawn.
        assert!(render(&a, &HashMap::new(), 1_900_000_000.0, &vp, w, h, (9.0, 20.0)).chunks(4).all(|p| p[3] == 0));
    }

    #[test]
    fn zone_ids_are_plain() {
        assert_eq!(zone_id("https://api.weather.gov/zones/forecast/VAZ092").as_deref(), Some("forecast/VAZ092"));
        assert_eq!(zone_id("https://api.weather.gov/zones/forecast/../../etc"), None);
        assert_eq!(zone_id("https://api.weather.gov/zones/forecast/VAZ092?x=1"), None);
        assert_eq!(zone_id("https://elsewhere.example/zones/forecast/VAZ092"), None);
    }

    #[test]
    fn outlines_from_polygons_and_zones() {
        let a = parse(SAMPLE).unwrap();
        let mut zones = HashMap::new();
        assert_eq!(zones_needed(&a, &zones), vec!["forecast/ANZ630", "forecast/ANZ631"], "the polygon alert needs none");
        // Before any zone is in: only the alert with its own outline.
        assert_eq!(features(&a, &zones, 1_795_660_000.0)["features"].as_array().unwrap().len(), 1);
        zones.insert("forecast/ANZ630".to_string(), json!({"type":"Polygon","coordinates":[[[-76,37],[-75,37],[-75,38],[-76,37]]]}));
        zones.insert("forecast/ANZ631".to_string(), Value::Null);
        assert!(zones_needed(&a, &zones).is_empty());
        let fc = features(&a, &zones, 1_795_660_000.0);
        let f = fc["features"].as_array().unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0]["properties"]["event"], "Small Craft Advisory", "least severe first, under the worse");
        assert_eq!(f[0]["geometry"]["type"], "MultiPolygon");
        assert_eq!(f[1]["properties"]["color"], "#ff5555");
        assert_eq!(layers("alerts").unwrap().len(), 2);
        // After both end: nothing in force.
        assert!(features(&a, &zones, 1_900_000_000.0)["features"].as_array().unwrap().is_empty());
    }
}
