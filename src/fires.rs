//! Wildfires in the US from the National Interagency Fire Center's WFIGS services
//! (Wildland Fire Interagency Geospatial Services; public ArcGIS layers, no key): the
//! current incidents, wildfires and prescribed burns, as points, and the perimeters of those
//! still burning. NIFC caches them for 5 minutes; mapterm asks every 5 minutes, with each
//! layer's ETag.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};

const INCIDENTS: &str = "https://services3.arcgis.com/T4QMspbfLg3qTGWY/arcgis/rest/services/WFIGS_Incident_Locations_Current/FeatureServer/0";
const PERIMETERS: &str = "https://services3.arcgis.com/T4QMspbfLg3qTGWY/arcgis/rest/services/WFIGS_Interagency_Perimeters_Current/FeatureServer/0";
/// The credit shown while fires are on.
pub const CREDIT: &str = "Wildfires: NIFC (WFIGS)";
/// At most this many pages of 2,000.
const MAX_PAGES: usize = 5;

/// One fire incident.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fire {
    /// Its IRWIN id, upper case without braces; perimeters carry the same.
    pub id: String,
    /// "Aspen Acres" (as given; some are in capitals).
    pub name: String,
    /// "WF" wildfire, "RX" prescribed fire, "CX" complex.
    pub kind: String,
    /// Acres.
    pub acres: Option<f64>,
    /// Percent contained.
    pub contained: Option<f64>,
    /// When it was discovered (Unix seconds).
    pub discovered: Option<f64>,
    /// When the record was last changed (Unix seconds).
    pub modified: Option<f64>,
    /// "CO" (from "US-CO").
    pub state: String,
    /// "Custer".
    pub county: String,
    /// "Human", "Natural", "Undetermined".
    pub cause: String,
    /// "Minimal", "Moderate", "Active", "Extreme".
    pub behavior: String,
    /// People assigned.
    pub personnel: Option<u64>,
    /// "2026-COCUX-001160".
    pub unique: String,
    /// Longitude.
    pub lon: f64,
    /// Latitude.
    pub lat: f64,
}

impl Fire {
    /// "wildfire", "prescribed fire", "fire complex".
    pub fn kind_name(&self) -> &'static str {
        match self.kind.as_str() {
            "RX" => "prescribed fire",
            "CX" => "fire complex",
            _ => "wildfire",
        }
    }

    /// The name in title case when it came in capitals: "GRASSHOPPER" as "Grasshopper".
    pub fn title(&self) -> String {
        if self.name.chars().any(char::is_lowercase) {
            return self.name.clone();
        }
        self.name
            .split_whitespace()
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// An IRWIN id as a key: upper case, without braces; letters, digits and dashes only.
fn irwin(v: Option<&Json>) -> Option<String> {
    let s = v?.as_str()?.trim().trim_matches(|c| c == '{' || c == '}').to_uppercase();
    (!s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(s)
}

fn text(p: &Json, k: &str) -> String {
    p.get(k).and_then(Json::as_str).map(|s| s.trim().chars().take(120).collect()).unwrap_or_default()
}

fn num(p: &Json, k: &str) -> Option<f64> {
    p.get(k).and_then(Json::as_f64).filter(|n| n.is_finite() && *n >= 0.0)
}

/// The incidents in an ArcGIS query answer (`f=json`, `outSR=4326`), and whether there are
/// more pages.
pub fn parse_incidents(text_: &str) -> Result<(Vec<Fire>, bool)> {
    let v: Json = serde_json::from_str(text_).context("NIFC")?;
    if let Some(e) = v.get("error") {
        bail!("NIFC: {}", e.get("message").and_then(Json::as_str).unwrap_or("the service answered with an error"));
    }
    let feats = v.get("features").and_then(Json::as_array).context("NIFC: no features")?;
    let more = v.get("exceededTransferLimit").and_then(Json::as_bool).unwrap_or(false);
    let fires = feats
        .iter()
        .filter_map(|f| {
            let p = f.get("attributes")?;
            let g = f.get("geometry")?;
            let (lon, lat) = (g.get("x")?.as_f64()?, g.get("y")?.as_f64()?);
            if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
                return None;
            }
            Some(Fire {
                id: irwin(p.get("IrwinID"))?,
                name: text(p, "IncidentName"),
                kind: text(p, "IncidentTypeCategory"),
                acres: num(p, "IncidentSize"),
                contained: num(p, "PercentContained").map(|c| c.min(100.0)),
                discovered: num(p, "FireDiscoveryDateTime").map(|ms| ms / 1000.0),
                modified: num(p, "ModifiedOnDateTime_dt").map(|ms| ms / 1000.0),
                state: text(p, "POOState").trim_start_matches("US-").to_string(),
                county: text(p, "POOCounty"),
                cause: text(p, "FireCauseGeneral"),
                behavior: text(p, "FireBehaviorGeneral"),
                personnel: num(p, "TotalIncidentPersonnel").map(|n| n.round() as u64),
                unique: text(p, "UniqueFireIdentifier"),
                lon,
                lat,
            })
        })
        .collect();
    Ok((fires, more))
}

/// The perimeters in a GeoJSON answer, by the IRWIN id of their incident.
pub fn parse_perimeters(text_: &str) -> Result<(HashMap<String, Json>, bool)> {
    let v: Json = serde_json::from_str(text_).context("NIFC perimeters")?;
    if let Some(e) = v.get("error") {
        bail!("NIFC perimeters: {}", e.get("message").and_then(Json::as_str).unwrap_or("the service answered with an error"));
    }
    let more = v.get("exceededTransferLimit").and_then(Json::as_bool).unwrap_or(false)
        || v.pointer("/properties/exceededTransferLimit").and_then(Json::as_bool).unwrap_or(false);
    let m = v
        .get("features")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let g = f.get("geometry").filter(|g| matches!(g.get("type").and_then(Json::as_str), Some("Polygon" | "MultiPolygon")))?;
            Some((irwin(f.pointer("/properties/attr_IrwinID"))?, g.clone()))
        })
        .collect();
    Ok((m, more))
}

/// A layer's pages: `query` (no offset) asked from 0 until it says there are no more,
/// unless the first page is unchanged since `etag` (then None); with the new ETag.
async fn pages<T, F>(query: &str, etag: Option<&str>, parse: F) -> Result<Option<(Vec<T>, Option<String>)>>
where
    F: Fn(&str) -> Result<(Vec<T>, bool)>,
{
    let mut all = Vec::new();
    let mut tag = None;
    for page in 0..MAX_PAGES {
        let mut req = crate::source::http_client().get(format!("{query}&resultOffset={}", all.len()));
        if page == 0
            && let Some(e) = etag
        {
            req = req.header("If-None-Match", e);
        }
        let resp = req.send().await.context("NIFC")?;
        if page == 0 && resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("NIFC: HTTP {}", resp.status());
        }
        if page == 0 {
            tag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
        }
        let (items, more) = parse(&resp.text().await.context("NIFC")?)?;
        let got = items.len();
        all.extend(items);
        if !more || got == 0 {
            break;
        }
    }
    Ok(Some((all, tag)))
}

/// The current incidents, unless unchanged since `etag` (then None); with the new ETag.
pub async fn incidents(etag: Option<&str>) -> Result<Option<(Vec<Fire>, Option<String>)>> {
    let fields = "IrwinID,IncidentName,IncidentTypeCategory,IncidentSize,PercentContained,FireDiscoveryDateTime,ModifiedOnDateTime_dt,\
                  POOState,POOCounty,FireCauseGeneral,FireBehaviorGeneral,TotalIncidentPersonnel,UniqueFireIdentifier";
    pages(&format!("{INCIDENTS}/query?where=1%3D1&outFields={fields}&outSR=4326&orderByFields=OBJECTID&f=json"), etag, parse_incidents).await
}

/// The current perimeters by incident, unless unchanged since `etag` (then None); with the
/// new ETag.
pub async fn perimeters(etag: Option<&str>) -> Result<Option<(HashMap<String, Json>, Option<String>)>> {
    let q = format!(
        "{PERIMETERS}/query?where=1%3D1&outFields=attr_IrwinID&outSR=4326&maxAllowableOffset=0.0005&geometryPrecision=5&orderByFields=OBJECTID&f=geojson"
    );
    Ok(pages(&q, etag, |t| parse_perimeters(t).map(|(m, more)| (m.into_iter().collect::<Vec<_>>(), more))).await?.map(|(v, t)| (v.into_iter().collect(), t)))
}

/// The containment bands: (from percent, label, color); prescribed fires apart.
pub const BANDS: [(f64, &str, (u8, u8, u8)); 3] =
    [(0.0, "under 50% contained", (0xff, 0x45, 0x3a)), (50.0, "50–99% contained", (0xff, 0x9f, 0x0a)), (100.0, "contained", (0x9a, 0xa5, 0xb1))];
/// Prescribed fires' color.
pub const PRESCRIBED: (u8, u8, u8) = (0x7e, 0xb8, 0x8a);

/// A fire's color: by containment, or the prescribed-fire green.
pub fn rgb(f: &Fire) -> (u8, u8, u8) {
    if f.kind == "RX" {
        return PRESCRIBED;
    }
    let c = f.contained.unwrap_or(0.0);
    BANDS.iter().rev().find(|b| c >= b.0).map_or(BANDS[0].2, |b| b.2)
}

/// The fires as GeoJSON points (`id`, `glyph`, `label`, `color`): ✹ for wildfires, ◇ for
/// prescribed fires, names for those of 1,000 acres or more and the selected one; the
/// biggest drawn last.
pub fn points(fires: &[Fire], selected: Option<&str>) -> Json {
    let mut v: Vec<&Fire> = fires.iter().collect();
    v.sort_by(|a, b| {
        let sel = |f: &Fire| Some(f.id.as_str()) == selected;
        sel(a).cmp(&sel(b)).then((a.kind != "RX").cmp(&(b.kind != "RX"))).then(a.acres.unwrap_or(0.0).total_cmp(&b.acres.unwrap_or(0.0)))
    });
    let features: Vec<Json> = v
        .into_iter()
        .map(|f| {
            let (r, g, b) = rgb(f);
            let sel = Some(f.id.as_str()) == selected;
            let big = f.kind != "RX" && f.acres.unwrap_or(0.0) >= 1000.0;
            json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [f.lon, f.lat]},
                "properties": {"id": f.id, "glyph": if sel { "◎" } else if f.kind == "RX" { "◇" } else { "✹" },
                               "label": if sel || big { f.title() } else { String::new() }, "color": format!("#{r:02x}{g:02x}{b:02x}")},
            })
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// The perimeters as areas to draw (a translucent fill and outline in the fire's color),
/// the selected one highlighted.
pub fn shapes(fires: &[Fire], perimeters: &HashMap<String, Json>, selected: Option<&str>) -> Vec<crate::areas::Shape> {
    let by_id: HashMap<&str, &Fire> = fires.iter().map(|f| (f.id.as_str(), f)).collect();
    let mut ids: Vec<&String> = perimeters.keys().collect();
    ids.sort_by_key(|id| Some(id.as_str()) == selected);
    ids.into_iter()
        .map(|id| {
            let rgb = by_id.get(id.as_str()).map_or(BANDS[0].2, |f| rgb(f));
            let mut s = crate::areas::Shape::new(rgb, Some(id.as_str()) == selected);
            (s.fill, s.line) = (70, 220);
            s.add(&perimeters[id]);
            s
        })
        .collect()
}

/// The perimeters as GeoJSON for the map (braille, and clicks): properties `id`, `color`.
pub fn perimeter_features(fires: &[Fire], perimeters: &HashMap<String, Json>, selected: Option<&str>) -> Json {
    let by_id: HashMap<&str, &Fire> = fires.iter().map(|f| (f.id.as_str(), f)).collect();
    let feats: Vec<Json> = perimeters
        .iter()
        .map(|(id, g)| {
            let (r, gr, b) = by_id.get(id.as_str()).map_or(BANDS[0].2, |f| rgb(f));
            json!({"type": "Feature", "geometry": g,
                   "properties": {"id": id, "color": format!("#{r:02x}{gr:02x}{b:02x}"), "selected": Some(id.as_str()) == selected}})
        })
        .collect();
    json!({"type": "FeatureCollection", "features": feats})
}

/// The map layers for perimeters from GeoJSON source `source`: a fill and outline in each
/// fire's color, and the selected one bright.
pub fn perimeter_layers(source: &str) -> Result<Vec<crate::StyleLayer>> {
    let fill = json!({"id": format!("{source}-fill"), "type": "fill", "source": source, "paint": {"fill-color": ["get", "color"], "fill-opacity": 0.3}});
    let line = json!({"id": format!("{source}-line"), "type": "line", "source": source, "paint": {"line-color": ["get", "color"], "line-width": 1}});
    let selected = json!({"id": format!("{source}-selected"), "type": "line", "source": source, "filter": ["==", ["get", "selected"], true],
                          "paint": {"line-color": "#ffffff", "line-width": 2}});
    [fill, line, selected].iter().map(|j| Ok(crate::StyleLayer::from_json(j)?.0)).collect()
}

/// The style layer for incidents from source `source`.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source)
        .glyph(json!(["get", "glyph"]))
        .text(json!(["get", "label"]))
        .color("#c9d1d9")
        .paint("icon-color", json!(["get", "color"]))
}

/// The incident nearest `dot` (braille dots), within `within` cells; the larger first when
/// two are about as near.
pub fn nearest(fires: &[Fire], vp: &crate::Viewport, dot: (f64, f64), within: f64) -> Option<String> {
    fires
        .iter()
        .filter_map(|f| {
            let (x, y) = vp.dot_ll(f.lon, f.lat);
            let d = ((x - dot.0).powi(2) + ((y - dot.1) / 2.0).powi(2)).sqrt();
            (d <= within).then_some((d - f.acres.unwrap_or(0.0).max(1.0).log10() * 0.3, f))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, f)| f.id.clone())
}

/// A legend: wildfires by containment, and prescribed fires, with how many of each.
pub fn legend(fires: &[Fire]) -> crate::legend::Legend {
    use ratatui::style::Color;
    let mut n = [0usize; BANDS.len()];
    let mut rx = 0;
    for f in fires {
        if f.kind == "RX" {
            rx += 1;
        } else {
            let c = f.contained.unwrap_or(0.0);
            n[BANDS.iter().rposition(|b| c >= b.0).unwrap_or(0)] += 1;
        }
    }
    let mut l = crate::legend::Legend::new("wildfires");
    for ((_, label, (r, g, b)), count) in BANDS.iter().zip(n) {
        l = l.entry("✹", Color::Rgb(*r, *g, *b), *label, Some(count));
    }
    let (r, g, b) = PRESCRIBED;
    l.entry("◇", Color::Rgb(r, g, b), "prescribed fire", Some(rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INC: &str = r#"{"features":[
      {"attributes":{"IrwinID":"{1cdf5e5a-f22e-4352-a582-c2a47663b93d}","IncidentName":"Aspen Acres","IncidentTypeCategory":"WF","IncidentSize":102007,
        "PercentContained":96,"FireDiscoveryDateTime":1782734686000,"POOState":"US-CO","POOCounty":"Custer","FireBehaviorGeneral":"Moderate",
        "TotalIncidentPersonnel":1,"UniqueFireIdentifier":"2026-COCUX-001160"},"geometry":{"x":-105.07,"y":38.02}},
      {"attributes":{"IrwinID":"{AA}","IncidentName":"PILE BURN 4","IncidentTypeCategory":"RX","IncidentSize":12,"PercentContained":null},
        "geometry":{"x":-120.5,"y":44.1}},
      {"attributes":{"IrwinID":"{BB}","IncidentName":"NEW ONE","IncidentTypeCategory":"WF","IncidentSize":300,"PercentContained":10},
        "geometry":{"x":-118.0,"y":34.0}},
      {"attributes":{"IrwinID":"../x","IncidentName":"bad"},"geometry":{"x":0,"y":0}}
    ],"exceededTransferLimit":false}"#;
    const PER: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-105.2,37.9],[-104.9,37.9],[-104.9,38.1],[-105.2,37.9]]]},
       "properties":{"attr_IrwinID":"{1CDF5E5A-F22E-4352-A582-C2A47663B93D}"}},
      {"type":"Feature","geometry":null,"properties":{"attr_IrwinID":"{CC}"}}
    ]}"#;

    #[test]
    fn reads_incidents_and_perimeters() {
        let (f, more) = parse_incidents(INC).unwrap();
        assert!(!more);
        assert_eq!(f.len(), 3, "a bad id is left out");
        assert_eq!(
            (f[0].id.as_str(), f[0].state.as_str(), f[0].acres, f[0].contained),
            ("1CDF5E5A-F22E-4352-A582-C2A47663B93D", "CO", Some(102007.0), Some(96.0))
        );
        assert_eq!((f[1].kind_name(), f[1].title()), ("prescribed fire", "Pile Burn 4".to_string()));
        let (p, _) = parse_perimeters(PER).unwrap();
        assert_eq!(p.len(), 1);
        assert!(p.contains_key(&f[0].id), "perimeters join their incidents by IRWIN id");
    }

    #[test]
    fn colors_points_areas_and_legend() {
        let (f, _) = parse_incidents(INC).unwrap();
        let (p, _) = parse_perimeters(PER).unwrap();
        assert_eq!(rgb(&f[0]), BANDS[1].2, "96%: orange");
        assert_eq!(rgb(&f[1]), PRESCRIBED);
        assert_eq!(rgb(&f[2]), BANDS[0].2, "10%: red");
        let fc = points(&f, None);
        let feats = fc["features"].as_array().unwrap();
        assert_eq!(feats[0]["properties"]["glyph"], "◇", "prescribed under wildfires");
        assert_eq!(feats.last().unwrap()["properties"]["label"], "Aspen Acres", "the biggest last, named");
        assert_eq!(feats[1]["properties"]["label"], "", "small fires unnamed");
        let sel = points(&f, Some("BB"));
        assert_eq!(sel["features"].as_array().unwrap().last().unwrap()["properties"]["glyph"], "◎");
        let tiles = crate::areas::AreaTiles::default();
        tiles.set(shapes(&f, &p, None));
        let (wx, wy) = crate::areas::world(-104.95, 37.95);
        let n = 256.0;
        let (x, y) = ((wx * n) as u32, (wy * n) as u32);
        assert!(tiles.draw(8, x, y, 512).is_some(), "the perimeter is drawn");
        assert_eq!(perimeter_features(&f, &p, None)["features"].as_array().unwrap().len(), 1);
        assert_eq!(perimeter_layers("fire-areas").unwrap().len(), 3);
        let l = legend(&f);
        assert_eq!(l.entries.len(), 4);
        let cam = crate::Camera::new(-105.0, 38.0, 8.0);
        let vp = crate::Viewport::new(&cam, ratatui::layout::Rect::new(0, 0, 80, 24));
        assert_eq!(nearest(&f, &vp, vp.dot_ll(-105.07, 38.02), 4.0).as_deref(), Some("1CDF5E5A-F22E-4352-A582-C2A47663B93D"));
    }
}
