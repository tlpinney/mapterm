//! Power outages in Pennsylvania, from the state's outage map, run by PEMA (the Pennsylvania
//! Emergency Management Agency) and PennDOT: customers without power and customers served,
//! by county and by municipality, from the utilities' reports, updated about hourly. A
//! public ArcGIS service, no key.
//!
//! The outlines don't change, so each level's are fetched once and kept on disk for 30 days
//! in `~/.cache/mapterm/outages` ([`outlines`]); the counts are asked for without them
//! ([`counts`]), with the ETag, so an unchanged answer is a tiny reply. Only counts, no
//! outage areas or restoration times: the service has none.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const SERVICE: &str = "https://gis.penndot.pa.gov/gis/rest/services/poweroutages/papoweroutages/FeatureServer";
/// The credit shown while outages are on.
pub const CREDIT: &str = "Power outages: PEMA / PennDOT";
/// From this zoom the map shows municipalities, below it counties.
pub const MUNICIPAL_ZOOM: f64 = 9.0;
/// How long outlines are kept.
const OUTLINES_FRESH: Duration = Duration::from_secs(30 * 24 * 3600);
/// At most this many pages of features (the service gives up to 5000 a page; there are
/// 2,572 municipalities).
const MAX_PAGES: usize = 5;

/// Counties or municipalities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// Pennsylvania's 67 counties.
    County,
    /// Its 2,572 municipalities (cities, boroughs, townships).
    Municipal,
}

impl Level {
    /// The level shown at map zoom `zoom`.
    pub fn at(zoom: f64) -> Level {
        if zoom >= MUNICIPAL_ZOOM { Level::Municipal } else { Level::County }
    }

    /// The service's polygon layer for it.
    fn layer(self) -> u8 {
        match self {
            Level::County => 1,
            Level::Municipal => 4,
        }
    }

    /// The fields that identify an area.
    fn key_fields(self) -> &'static str {
        match self {
            Level::County => "FIPS",
            Level::Municipal => "FIPS,FIPS_MUNICIPAL_CODE",
        }
    }

    /// The outline cache's file name.
    fn file(self) -> &'static str {
        match self {
            Level::County => "counties.geojson",
            Level::Municipal => "municipalities.geojson",
        }
    }

    /// How far (degrees) the service may move an outline's points when simplifying it: about
    /// 200 m for counties, 50 m for municipalities, finer than a cell where each shows.
    fn simplify(self) -> f64 {
        match self {
            Level::County => 0.002,
            Level::Municipal => 0.0005,
        }
    }

    /// "county" or "municipality".
    pub fn label(self) -> &'static str {
        match self {
            Level::County => "county",
            Level::Municipal => "municipality",
        }
    }
}

/// One county's or municipality's outage.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    /// The county's FIPS code, or the county's and the municipality's: `42029-83080`.
    pub key: String,
    /// "Chester", or "West Goshen Twp".
    pub name: String,
    /// The county's name (for a municipality; a county's own otherwise).
    pub county: String,
    /// Customers without power.
    pub out: u64,
    /// Customers served (an estimate).
    pub served: u64,
    /// For a municipality, its county's customers without power.
    pub county_out: Option<u64>,
    /// When the utilities' figures were gathered (Unix seconds).
    pub updated: Option<f64>,
}

impl Area {
    /// The share of customers without power, 0 to 1.
    pub fn share(&self) -> f64 {
        if self.served == 0 { if self.out > 0 { 1.0 } else { 0.0 } } else { (self.out as f64 / self.served as f64).min(1.0) }
    }
}

/// The color bands, by the share of customers out: (from, label, color).
pub const BANDS: [(f64, &str, (u8, u8, u8)); 5] = [
    (0.0, "under 1%", (0xff, 0xe0, 0x78)),
    (0.01, "1–5%", (0xff, 0xb4, 0x46)),
    (0.05, "5–15%", (0xff, 0x7a, 0x32)),
    (0.15, "15–30%", (0xe8, 0x40, 0x3a)),
    (0.30, "30% or more", (0xc8, 0x2a, 0x8c)),
];

/// The band an area falls in, None with nobody out.
pub fn band(a: &Area) -> Option<usize> {
    if a.out == 0 {
        return None;
    }
    let s = a.share();
    Some(BANDS.iter().rposition(|b| s >= b.0).unwrap_or(0))
}

/// An area's color: its band's, or grey with nobody out.
pub fn rgb(a: &Area) -> (u8, u8, u8) {
    band(a).map_or((0x80, 0x80, 0x80), |i| BANDS[i].2)
}

/// An area's key from its attributes (or GeoJSON properties).
fn key(level: Level, p: &Value) -> Option<String> {
    let text = |k: &str| -> Option<String> {
        let v = p.get(k)?;
        let s = v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|n| n.to_string()))?;
        (!s.is_empty() && s.len() <= 8 && s.chars().all(|c| c.is_ascii_digit())).then_some(s)
    };
    match level {
        Level::County => text("FIPS"),
        Level::Municipal => Some(format!("{}-{}", text("FIPS")?, text("FIPS_MUNICIPAL_CODE")?)),
    }
}

/// "WEST GOSHEN TWP" as "West Goshen Twp".
fn title(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The features of an ArcGIS query answer (`f=json`), and whether there are more pages.
fn esri_features(text: &str) -> Result<(Vec<Value>, bool)> {
    let v: Value = serde_json::from_str(text).context("outages")?;
    if let Some(e) = v.get("error") {
        bail!("outages: {}", e.get("message").and_then(Value::as_str).unwrap_or("the service answered with an error"));
    }
    let feats = v.get("features").and_then(Value::as_array).cloned().context("outages: no features")?;
    Ok((feats, v.get("exceededTransferLimit").and_then(Value::as_bool).unwrap_or(false)))
}

/// The areas in an ArcGIS query answer of `level`'s counts.
pub fn parse_counts(level: Level, text: &str) -> Result<(Vec<Area>, bool)> {
    let (feats, more) = esri_features(text)?;
    let num = |p: &Value, k: &str| p.get(k).and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0).map(|n| n.round() as u64);
    let areas = feats
        .iter()
        .filter_map(|f| {
            let p = f.get("attributes")?;
            let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
            let (name, county, out, county_out) = match level {
                Level::County => (s("NAME"), s("NAME"), num(p, "CUSTOMERS_OUT"), None),
                Level::Municipal => (title(&s("FIPS_MUNICIPAL_NAME")), s("COUNTY_NAME"), num(p, "MUNICIPAL_OUT"), num(p, "COUNTY_OUT")),
            };
            Some(Area {
                key: key(level, p)?,
                name,
                county,
                out: out.unwrap_or(0),
                served: num(p, "EST_CUST_SERVED").unwrap_or(0),
                county_out,
                updated: p.get("UPDATE_DATE").and_then(Value::as_f64).map(|ms| ms / 1000.0),
            })
        })
        .collect();
    Ok((areas, more))
}

fn query_url(level: Level, offset: usize) -> String {
    let (filter, fields) = match level {
        Level::County => ("1%3D1", "FIPS,NAME,CUSTOMERS_OUT,EST_CUST_SERVED,UPDATE_DATE"),
        // Only those with customers out: a fraction of the 2,572.
        Level::Municipal => {
            ("MUNICIPAL_OUT%3E0", "FIPS,FIPS_MUNICIPAL_CODE,FIPS_MUNICIPAL_NAME,COUNTY_NAME,COUNTY_OUT,MUNICIPAL_OUT,EST_CUST_SERVED,UPDATE_DATE")
        }
    };
    format!("{SERVICE}/{}/query?where={filter}&outFields={fields}&returnGeometry=false&orderByFields=OBJECTID&resultOffset={offset}&f=json", level.layer())
}

/// `level`'s counts, unless they're unchanged since `etag` (then None); with the new ETag.
/// Municipalities: only those with customers out.
pub async fn counts(level: Level, etag: Option<&str>) -> Result<Option<(Vec<Area>, Option<String>)>> {
    let mut all = Vec::new();
    let mut tag = None;
    for page in 0..MAX_PAGES {
        let mut req = crate::source::http_client().get(query_url(level, all.len()));
        if page == 0
            && let Some(e) = etag
        {
            req = req.header("If-None-Match", e);
        }
        let resp = req.send().await.context("outages")?;
        if page == 0 && resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("outages: HTTP {}", resp.status());
        }
        if page == 0 {
            tag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
        }
        let (areas, more) = parse_counts(level, &resp.text().await.context("outages")?)?;
        let got = areas.len();
        all.extend(areas);
        if !more || got == 0 {
            break;
        }
    }
    Ok(Some((all, tag)))
}

/// The outlines in a GeoJSON FeatureCollection of `level`, by key.
pub fn parse_outlines(level: Level, text: &str) -> Result<HashMap<String, Value>> {
    let v: Value = serde_json::from_str(text).context("outage outlines")?;
    if let Some(e) = v.get("error") {
        bail!("outage outlines: {}", e.get("message").and_then(Value::as_str).unwrap_or("the service answered with an error"));
    }
    Ok(v.get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| Some((key(level, f.get("properties")?)?, f.get("geometry").filter(|g| !g.is_null())?.clone())))
        .collect())
}

fn outline_file(level: Level) -> std::path::PathBuf {
    crate::source::cache_root().join("outages").join(level.file())
}

/// `level`'s outlines (GeoJSON geometries, lon/lat) by key: from the disk cache when under 30
/// days old, else from the service (a page or two), kept. If the service can't be reached,
/// an older copy will do.
pub async fn outlines(level: Level) -> Result<HashMap<String, Value>> {
    let p = outline_file(level);
    let read = || -> Option<HashMap<String, Value>> { parse_outlines(level, &std::fs::read_to_string(&p).ok()?).ok().filter(|m| !m.is_empty()) };
    let age = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok());
    if age.is_some_and(|a| a < OUTLINES_FRESH)
        && let Some(m) = read()
    {
        return Ok(m);
    }
    let fetched = async {
        let mut feats: Vec<Value> = Vec::new();
        for _ in 0..MAX_PAGES {
            let url = format!(
                "{SERVICE}/{}/query?where=1%3D1&outFields={}&outSR=4326&maxAllowableOffset={}&geometryPrecision=4&orderByFields=OBJECTID&resultOffset={}&f=geojson",
                level.layer(),
                level.key_fields(),
                level.simplify(),
                feats.len()
            );
            let resp = crate::source::http_client().get(url).send().await.context("outage outlines")?;
            if !resp.status().is_success() {
                bail!("outage outlines: HTTP {}", resp.status());
            }
            let v: Value = serde_json::from_str(&resp.text().await?).context("outage outlines")?;
            if let Some(e) = v.get("error") {
                bail!("outage outlines: {}", e.get("message").and_then(Value::as_str).unwrap_or("the service answered with an error"));
            }
            let page = v.get("features").and_then(Value::as_array).cloned().unwrap_or_default();
            let more = v.get("exceededTransferLimit").and_then(Value::as_bool).unwrap_or(false)
                || v.pointer("/properties/exceededTransferLimit").and_then(Value::as_bool).unwrap_or(false);
            let got = page.len();
            feats.extend(page);
            if !more || got == 0 {
                break;
            }
        }
        let text = serde_json::to_string(&json!({"type": "FeatureCollection", "features": feats}))?;
        let m = parse_outlines(level, &text)?;
        if m.is_empty() {
            bail!("outage outlines: none in the answer");
        }
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        let tmp = p.with_extension("part");
        if std::fs::write(&tmp, &text).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
        Ok::<_, anyhow::Error>(m)
    }
    .await;
    match fetched {
        Ok(m) => Ok(m),
        Err(e) => read().ok_or(e),
    }
}

/// The order areas draw in: nobody out first, then by share, the selected one last.
fn draw_order<'a>(areas: &'a [Area], level: Level, selected: Option<&str>) -> Vec<&'a Area> {
    let mut v: Vec<&Area> = areas.iter().filter(|a| level == Level::County || a.out > 0).collect();
    v.sort_by(|a, b| (Some(a.key.as_str()) == selected).cmp(&(Some(b.key.as_str()) == selected)).then(a.share().total_cmp(&b.share())));
    v
}

/// `level`'s areas as tiles to draw, with their outlines from `outlines`: shaded by the share
/// of customers out; counties with nobody out a faint outline, municipalities with nobody
/// out not at all; the one with key `selected` highlighted.
pub fn shapes(areas: &[Area], outlines: &HashMap<String, Value>, level: Level, selected: Option<&str>) -> Vec<crate::areas::Shape> {
    draw_order(areas, level, selected)
        .into_iter()
        .filter_map(|a| {
            let g = outlines.get(&a.key)?;
            let mut s = crate::areas::Shape::new(rgb(a), Some(a.key.as_str()) == selected);
            (s.fill, s.line) = if a.out > 0 { (110, 200) } else { (0, 70) };
            s.add(g);
            Some(s)
        })
        .collect()
}

/// `level`'s areas as GeoJSON for the map (braille, and clicks): properties `key`, `color`,
/// `out` and `selected`, in drawing order.
pub fn features(areas: &[Area], outlines: &HashMap<String, Value>, level: Level, selected: Option<&str>) -> Value {
    let feats: Vec<Value> = draw_order(areas, level, selected)
        .into_iter()
        .filter_map(|a| {
            let (r, g, b) = rgb(a);
            Some(json!({
                "type": "Feature",
                "geometry": outlines.get(&a.key)?,
                "properties": {"key": a.key, "color": format!("#{r:02x}{g:02x}{b:02x}"), "out": a.out, "selected": Some(a.key.as_str()) == selected},
            }))
        })
        .collect();
    json!({"type": "FeatureCollection", "features": feats})
}

/// The map layers for outages from GeoJSON source `source`: areas with customers out filled
/// in their band's color, every area outlined, and the selected one bright.
pub fn layers(source: &str) -> Result<Vec<crate::StyleLayer>> {
    let fill = json!({"id": format!("{source}-fill"), "type": "fill", "source": source, "filter": [">", ["get", "out"], 0],
                      "paint": {"fill-color": ["get", "color"], "fill-opacity": 0.35}});
    let line = json!({"id": format!("{source}-line"), "type": "line", "source": source, "paint": {"line-color": ["get", "color"], "line-width": 1}});
    let selected = json!({"id": format!("{source}-selected"), "type": "line", "source": source, "filter": ["==", ["get", "selected"], true],
                          "paint": {"line-color": "#ffffff", "line-width": 2}});
    [fill, line, selected].iter().map(|j| Ok(crate::StyleLayer::from_json(j)?.0)).collect()
}

/// A legend of the bands, with how many of `level`'s areas are in each.
pub fn legend(areas: &[Area], level: Level) -> crate::legend::Legend {
    use ratatui::style::Color;
    let mut n = [0usize; BANDS.len()];
    for i in areas.iter().filter_map(band) {
        n[i] += 1;
    }
    let title = match level {
        Level::County => "counties by share out",
        Level::Municipal => "towns by share out",
    };
    BANDS
        .iter()
        .zip(n)
        .rev()
        .fold(crate::legend::Legend::new(title), |l, ((_, label, (r, g, b)), count)| l.entry("■", Color::Rgb(*r, *g, *b), *label, Some(count)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNTIES: &str = r#"{"features":[
      {"attributes":{"FIPS":"42029","NAME":"Chester","CUSTOMERS_OUT":39962,"EST_CUST_SERVED":208818,"UPDATE_DATE":1795726920000}},
      {"attributes":{"FIPS":"42001","NAME":"Adams","CUSTOMERS_OUT":0,"EST_CUST_SERVED":41124,"UPDATE_DATE":1795726920000}},
      {"attributes":{"FIPS":"../x","NAME":"Bad","CUSTOMERS_OUT":1,"EST_CUST_SERVED":2}}
    ],"exceededTransferLimit":false}"#;
    const TOWNS: &str = r#"{"features":[
      {"attributes":{"FIPS":"42029","FIPS_MUNICIPAL_CODE":"83080","FIPS_MUNICIPAL_NAME":"WEST GOSHEN TWP","COUNTY_NAME":"Chester",
        "COUNTY_OUT":39962,"MUNICIPAL_OUT":4111,"EST_CUST_SERVED":9733.0,"UPDATE_DATE":1795726920000}}
    ],"exceededTransferLimit":true}"#;
    const OUTLINES: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-76,39.7],[-75.5,39.7],[-75.5,40.2],[-76,39.7]]]},"properties":{"FIPS":"42029"}},
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-77.5,39.7],[-77,39.7],[-77,40.1],[-77.5,39.7]]]},"properties":{"FIPS":"42001"}},
      {"type":"Feature","geometry":null,"properties":{"FIPS":"42003"}}
    ]}"#;

    #[test]
    fn reads_counts() {
        let (c, more) = parse_counts(Level::County, COUNTIES).unwrap();
        assert!(!more);
        assert_eq!(c.len(), 2, "a bad key is left out");
        assert_eq!((c[0].key.as_str(), c[0].name.as_str(), c[0].out, c[0].served), ("42029", "Chester", 39962, 208818));
        assert_eq!(c[0].updated, Some(1_795_726_920.0));
        assert!((c[0].share() - 0.1914).abs() < 0.001);
        let (t, more) = parse_counts(Level::Municipal, TOWNS).unwrap();
        assert!(more, "more pages");
        assert_eq!((t[0].key.as_str(), t[0].name.as_str(), t[0].county.as_str()), ("42029-83080", "West Goshen Twp", "Chester"));
        assert_eq!((t[0].out, t[0].county_out), (4111, Some(39962)));
        assert!(parse_counts(Level::County, r#"{"error":{"code":400,"message":"Invalid query"}}"#).unwrap_err().to_string().contains("Invalid query"));
    }

    #[test]
    fn bands_by_share() {
        let a = |out, served| Area { key: "1".into(), name: String::new(), county: String::new(), out, served, county_out: None, updated: None };
        assert_eq!(band(&a(0, 100)), None);
        assert_eq!(band(&a(1, 1000)), Some(0));
        assert_eq!(band(&a(1, 100)), Some(1));
        assert_eq!(band(&a(10, 100)), Some(2));
        assert_eq!(band(&a(20, 100)), Some(3));
        assert_eq!(band(&a(4111, 9733)), Some(4));
        assert_eq!(band(&a(5, 0)), Some(4), "out with no customer count: the top band");
        assert_eq!(Level::at(8.9), Level::County);
        assert_eq!(Level::at(9.0), Level::Municipal);
    }

    #[test]
    fn outlines_features_and_tiles() {
        let o = parse_outlines(Level::County, OUTLINES).unwrap();
        assert_eq!(o.len(), 2, "no geometry, no outline");
        let (c, _) = parse_counts(Level::County, COUNTIES).unwrap();
        let fc = features(&c, &o, Level::County, Some("42029"));
        let f = fc["features"].as_array().unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!((f[0]["properties"]["key"].as_str(), f[0]["properties"]["color"].as_str()), (Some("42001"), Some("#808080")), "nobody out: grey, first");
        assert_eq!((f[1]["properties"]["key"].as_str(), f[1]["properties"]["selected"].as_bool()), (Some("42029"), Some(true)));
        assert_eq!(layers("outages").unwrap().len(), 3);
        // Municipalities with nobody out aren't drawn.
        assert!(features(&c, &o, Level::Municipal, None)["features"].as_array().unwrap().len() == 1);
        // Tiles: Chester's triangle is filled; Adams's only outlined.
        let tiles = crate::areas::AreaTiles::default();
        tiles.set(shapes(&c, &o, Level::County, None));
        let (z, n) = (8u8, 256.0);
        let (wx, wy) = crate::areas::world(-75.6, 39.8);
        let (x, y) = ((wx * n) as u32, (wy * n) as u32);
        let rgba = tiles.draw(z, x, y, 512).expect("a tile");
        let i = ((((wy * n - y as f64) * 512.0) as usize) * (512 + crate::areas::BLEED as usize) + ((wx * n - x as f64) * 512.0) as usize) * 4;
        assert!(rgba[i + 3] > 80, "filled: {}", rgba[i + 3]);
        let (wx, wy) = crate::areas::world(-77.1, 39.75);
        let (x, y) = ((wx * n) as u32, (wy * n) as u32);
        let rgba = tiles.draw(z, x, y, 512).expect("a tile");
        let i = ((((wy * n - y as f64) * 512.0) as usize) * (512 + crate::areas::BLEED as usize) + ((wx * n - x as f64) * 512.0) as usize) * 4;
        assert_eq!(rgba[i + 3], 0, "not filled");
        let l = legend(&c, Level::County);
        assert_eq!(l.entries.len(), BANDS.len());
    }

    #[test]
    fn a_click_finds_the_area() {
        let o = parse_outlines(Level::County, OUTLINES).unwrap();
        let (c, _) = parse_counts(Level::County, COUNTIES).unwrap();
        let mut map = crate::Map::new();
        map.add_source("outages", crate::MapSource::geojson(&features(&c, &o, Level::County, None)).unwrap()).unwrap();
        map.add_layers(layers("outages").unwrap(), None).unwrap();
        let (cam, area) = (crate::Camera::new(-76.2, 39.9, 7.0), ratatui::layout::Rect::new(0, 0, 80, 24));
        let keys = |lon: f64, lat: f64| -> Vec<String> {
            let dot = crate::Viewport::new(&cam, area).dot_ll(lon, lat);
            map.query_features(&cam, area, dot)
                .into_iter()
                .filter(|p| p.source.as_deref() == Some("outages"))
                .filter_map(|p| p.prop("key")?.as_str().map(str::to_string))
                .collect()
        };
        // Inside Chester's triangle (customers out): found.
        assert!(keys(-75.6, 39.8).contains(&"42029".to_string()));
        // Outside both: nothing.
        assert!(keys(-76.5, 40.15).is_empty());
    }

    #[test]
    fn query_urls_are_the_services() {
        assert!(query_url(Level::County, 0).starts_with("https://gis.penndot.pa.gov/gis/rest/services/poweroutages/papoweroutages/FeatureServer/1/query?"));
        assert!(
            query_url(Level::Municipal, 5000).contains("/4/query?where=MUNICIPAL_OUT%3E0") && query_url(Level::Municipal, 5000).contains("resultOffset=5000")
        );
    }
}
