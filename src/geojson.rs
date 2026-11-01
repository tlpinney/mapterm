//! GeoJSON overlay layers: a file or URL becomes a layer of points, lines and polygons drawn
//! over the map, with each feature's properties kept for inspection.
//! Spec: <https://datatracker.ietf.org/doc/html/rfc7946>

use anyhow::{Context, Result, bail};
use ratatui::style::Color;
use serde_json::Value as Json;


use crate::{
    mvt::{GeomType, Value},
    render::to_world,
};

/// Largest GeoJSON document read (files and downloads).
const MAX_BYTES: usize = 512 << 20;

/// Layer colors, assigned in order: distinct hues that read on dark and light maps.
pub const PALETTE: [Color; 8] = [
    Color::Rgb(255, 99, 132),
    Color::Rgb(54, 200, 235),
    Color::Rgb(255, 196, 61),
    Color::Rgb(124, 220, 120),
    Color::Rgb(190, 130, 255),
    Color::Rgb(255, 150, 70),
    Color::Rgb(80, 230, 200),
    Color::Rgb(240, 120, 220),
];

#[derive(Clone, Debug)]
pub struct Feature {
    /// The feature's `id` (also kept in `props` for inspecting).
    pub id: Option<Value>,
    pub geom: GeomType,
    /// Parts in Web Mercator world coordinates 0..1 (points, lines, or polygon rings).
    pub parts: Vec<Vec<(f64, f64)>>,
    /// [min_x, min_y, max_x, max_y] in world coordinates.
    pub bbox: [f64; 4],
    pub props: Vec<(String, Value)>,
    /// Text for an on-map label (the `name` property, or `title`/`label`).
    pub label: Option<String>,
}

pub struct GeoJson {
    pub name: String,
    pub color: Color,
    pub features: Vec<Feature>,
    /// Tint polygon interiors (turn off over a raster layer to keep it visible).
    pub fill: bool,
    /// Label features that have a name.
    pub labels: bool,
    /// [min_x, min_y, max_x, max_y] in world coordinates over all features.
    pub bbox: [f64; 4],
}

impl GeoJson {
    /// Lon/lat bounds (west, south, east, north).
    pub fn bounds_lonlat(&self) -> Option<(f64, f64, f64, f64)> {
        if self.features.is_empty() {
            return None;
        }
        let [x0, y0, x1, y1] = self.bbox;
        let (w, n) = crate::render::to_lonlat(x0, y0);
        let (e, s) = crate::render::to_lonlat(x1, y1);
        Some((w, s, e, n))
    }
}

/// Style layers that draw a GeoJSON source in one color, the way `--geojson` does: tinted
/// polygon fills (when `fill`), outlines and lines, `●` points with their names, names at the
/// middle of lines and polygons, and `◉` clusters with their point counts (for a clustered
/// source). Ids are `{source}-fill`, `-line`, `-point`, `-cluster`, `-label`.
pub fn overlay_layers(source: &str, color: Color, fill: bool) -> Vec<crate::spec::StyleLayer> {
    use crate::spec::StyleLayer;
    use serde_json::json;
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (255, 255, 255),
    };
    let hex = format!("#{r:02x}{g:02x}{b:02x}");
    let t = |v: u8| (v as f32 * 0.32) as u8;
    let tint = format!("#{:02x}{:02x}{:02x}", t(r), t(g), t(b));
    let name = json!(["coalesce", ["get", "name"], ["get", "title"], ["get", "label"], ["get", "NAME"], ["get", "Name"]]);
    let bold = json!(["Bold"]);
    let mut out = Vec::new();
    if fill {
        out.push(StyleLayer::fill(format!("{source}-fill"), source).color(tint.as_str()));
    }
    out.push(StyleLayer::line(format!("{source}-line"), source).color(hex.as_str()));
    out.push(
        StyleLayer::symbol(format!("{source}-point"), source)
            .filter(json!(["all", ["==", ["geometry-type"], "Point"], ["!", ["has", "point_count"]]]))
            .glyph("●")
            .text(name.clone())
            .layout("text-font", bold.clone())
            .color(hex.as_str()),
    );
    out.push(
        StyleLayer::symbol(format!("{source}-cluster"), source)
            .filter(json!(["has", "point_count"]))
            .glyph("◉")
            .text(json!(["get", "point_count_abbreviated"]))
            .layout("text-font", bold.clone())
            .color(hex.as_str()),
    );
    out.push(
        StyleLayer::symbol(format!("{source}-label"), source)
            .filter(json!(["!=", ["geometry-type"], "Point"]))
            .text(name)
            .layout("text-font", bold)
            .color(hex.as_str()),
    );
    out
}

/// Load a layer from a path or http(s) URL: GeoJSON, or CSV, GPX or KML (by extension, else
/// by content; see [`crate::formats`]).
pub async fn load(spec: &str, color: Color) -> Result<GeoJson> {
    let bytes = if spec.starts_with("http://") || spec.starts_with("https://") {
        let http = crate::source::http_client();
        crate::source::get_with(&http, spec, None, &[]).await?.with_context(|| format!("{spec}: not found"))?
    } else {
        let meta = std::fs::metadata(spec).with_context(|| format!("reading {spec}"))?;
        if meta.len() as usize > MAX_BYTES {
            bail!("{spec}: larger than {} MB", MAX_BYTES >> 20);
        }
        std::fs::read(spec).with_context(|| format!("reading {spec}"))?
    };
    // The layer's name: the file name without its extension.
    let file = spec.rsplit('/').next().unwrap_or(spec).split(['?', '#']).next().unwrap_or(spec);
    let name = match file.rsplit_once('.') {
        Some((stem, ext)) if ["geojson", "json", "csv", "tsv", "txt", "gpx", "kml"].contains(&ext.to_ascii_lowercase().as_str()) => stem.to_string(),
        _ => file.to_string(),
    };
    let spec = spec.to_string();
    tokio::task::spawn_blocking(move || {
        // CSV, GPX and KML become GeoJSON first.
        let doc = crate::formats::to_geojson(&spec, &bytes).with_context(|| format!("{spec}: not GeoJSON, CSV, GPX or KML"))?;
        let (features, bbox) = features(&doc)?;
        Ok(GeoJson { name, color, features, bbox, fill: true, labels: true })
    })
    .await?
}

/// Parse a GeoJSON document (FeatureCollection, Feature, or bare geometry).
pub fn parse(bytes: &[u8], name: String, color: Color) -> Result<GeoJson> {
    let doc: Json = serde_json::from_slice(bytes).context("not valid JSON")?;
    let (features, bbox) = features(&doc)?;
    Ok(GeoJson { name, color, features, bbox, fill: true, labels: true })
}

/// The features of a parsed GeoJSON document and their bounding box in world coordinates.
pub fn features(doc: &Json) -> Result<(Vec<Feature>, [f64; 4])> {
    let mut features = Vec::new();
    match doc["type"].as_str() {
        Some("FeatureCollection") => {
            for f in doc["features"].as_array().into_iter().flatten() {
                add_feature(f, &mut features);
            }
        }
        Some("Feature") => add_feature(doc, &mut features),
        Some(_) => add_geometry(doc, None, Vec::new(), None, &mut features),
        None => bail!("not GeoJSON: no \"type\""),
    }
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for f in &features {
        bbox = [bbox[0].min(f.bbox[0]), bbox[1].min(f.bbox[1]), bbox[2].max(f.bbox[2]), bbox[3].max(f.bbox[3])];
    }
    Ok((features, bbox))
}

fn to_value(v: &Json) -> Option<Value> {
    match v {
        Json::Null => None,
        Json::Bool(b) => Some(Value::Bool(*b)),
        Json::Number(n) => n.as_f64().map(Value::Num),
        Json::String(s) => Some(Value::Str(s.clone())),
        other => Some(Value::Str(other.to_string())),
    }
}

fn add_feature(f: &Json, out: &mut Vec<Feature>) {
    let mut props: Vec<(String, Value)> = f["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| to_value(v).map(|v| (k.clone(), v)))
        .collect();
    if let Some(id) = f.get("id").and_then(to_value) {
        props.insert(0, ("id".into(), id));
    }
    let label = ["name", "title", "label", "NAME", "Name"]
        .iter()
        .find_map(|k| f["properties"][*k].as_str().filter(|s| !s.is_empty()).map(str::to_string));
    let id = f.get("id").and_then(to_value);
    add_geometry(&f["geometry"], id, props, label, out);
}

/// One position -> world coordinates, rejecting anything that isn't a finite lon/lat.
fn pos(p: &Json) -> Option<(f64, f64)> {
    let (lon, lat) = (p.get(0)?.as_f64()?, p.get(1)?.as_f64()?);
    (lon.is_finite() && lat.is_finite() && lon.abs() <= 540.0 && lat.abs() <= 90.0).then(|| to_world(lon, lat))
}

fn line(v: &Json) -> Vec<(f64, f64)> {
    v.as_array().into_iter().flatten().filter_map(pos).collect()
}

fn add_geometry(g: &Json, id: Option<Value>, props: Vec<(String, Value)>, label: Option<String>, out: &mut Vec<Feature>) {
    let c = &g["coordinates"];
    let (geom, parts): (GeomType, Vec<Vec<(f64, f64)>>) = match g["type"].as_str() {
        Some("Point") => (GeomType::Point, pos(c).map(|p| vec![vec![p]]).unwrap_or_default()),
        Some("MultiPoint") => (GeomType::Point, line(c).into_iter().map(|p| vec![p]).collect()),
        Some("LineString") => (GeomType::Line, vec![line(c)]),
        Some("MultiLineString") => (GeomType::Line, c.as_array().into_iter().flatten().map(line).collect()),
        Some("Polygon") => (GeomType::Polygon, c.as_array().into_iter().flatten().map(line).collect()),
        Some("MultiPolygon") => {
            (GeomType::Polygon, c.as_array().into_iter().flatten().flat_map(|poly| poly.as_array().into_iter().flatten().map(line)).collect())
        }
        Some("GeometryCollection") => {
            for sub in g["geometries"].as_array().into_iter().flatten() {
                add_geometry(sub, id.clone(), props.clone(), label.clone(), out);
            }
            return;
        }
        _ => return,
    };
    let parts: Vec<Vec<(f64, f64)>> = parts.into_iter().filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return;
    }
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for &(x, y) in parts.iter().flatten() {
        bbox = [bbox[0].min(x), bbox[1].min(y), bbox[2].max(x), bbox[3].max(y)];
    }
    out.push(Feature { id, geom, parts, bbox, props, label });
}

/// The camera (lon, lat, zoom) that fits lon/lat bounds into a map of `cols` x `rows` cells.
pub fn fit(bounds: (f64, f64, f64, f64), cols: u16, rows: u16, max_zoom: f64) -> (f64, f64, f64) {
    let (w, s, e, n) = bounds;
    let (x0, y0) = to_world(w, n);
    let (x1, y1) = to_world(e, s);
    let (bw, bh) = ((x1 - x0).abs().max(1e-9), (y1 - y0).abs().max(1e-9));
    // 2 dots per column, 4 per row; leave a 20% margin so edge features keep room for labels.
    let scale = (cols as f64 * 2.0 / bw).min(rows as f64 * 4.0 / bh) * 0.8;
    let zoom = (scale / 256.0).log2().clamp(0.0, max_zoom);
    let (lon, lat) = crate::render::to_lonlat((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    (lon, lat, zoom)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type": "Feature", "id": 7, "properties": {"name": "Monument", "height_m": 169.3, "open": true, "tags": ["a", "b"], "gone": null},
         "geometry": {"type": "Point", "coordinates": [-77.0353, 38.8895]}},
        {"type": "Feature", "properties": {"kind": "route"},
         "geometry": {"type": "LineString", "coordinates": [[-77.05, 38.889], [-77.0091, 38.8899]]}},
        {"type": "Feature", "properties": {"name": "Mall"},
         "geometry": {"type": "MultiPolygon", "coordinates": [[[[-77.05, 38.887], [-77.01, 38.887], [-77.01, 38.892], [-77.05, 38.892], [-77.05, 38.887]]]]}},
        {"type": "Feature", "properties": {}, "geometry": {"type": "GeometryCollection", "geometries": [
          {"type": "Point", "coordinates": [-77.02, 38.88]}, {"type": "LineString", "coordinates": [[-77.03, 38.88], [-77.02, 38.881]]}]}},
        {"type": "Feature", "properties": {}, "geometry": null},
        {"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": ["bad", 1]}}
      ]
    }"#;

    #[test]
    fn parses_features_properties_and_geometry_types() {
        let l = parse(DOC.as_bytes(), "test".into(), PALETTE[0]).unwrap();
        let kinds: Vec<GeomType> = l.features.iter().map(|f| f.geom).collect();
        assert_eq!(kinds, vec![GeomType::Point, GeomType::Line, GeomType::Polygon, GeomType::Point, GeomType::Line]);
        let m = &l.features[0];
        assert_eq!(m.label.as_deref(), Some("Monument"));
        assert_eq!(m.props[0], ("id".to_string(), Value::Num(7.0)));
        assert!(m.props.iter().any(|(k, v)| k == "tags" && *v == Value::Str("[\"a\",\"b\"]".into())));
        assert!(!m.props.iter().any(|(k, _)| k == "gone"), "nulls are dropped");
        let (w, s, e, n) = l.bounds_lonlat().unwrap();
        assert!((w - -77.05).abs() < 1e-9 && (e - -77.0091).abs() < 1e-9 && s < 38.881 && n > 38.891);
    }

    #[test]
    fn bare_geometry_and_bad_input() {
        let l = parse(br#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}"#, "g".into(), PALETTE[1]).unwrap();
        assert_eq!(l.features.len(), 1);
        assert!(parse(b"[1,2]", "x".into(), PALETTE[0]).is_err());
        assert!(parse(b"not json", "x".into(), PALETTE[0]).is_err());
    }

    #[test]
    fn fit_contains_the_bounds() {
        let (lon, lat, z) = fit((-77.05, 38.88, -77.0, 38.9), 100, 40, 20.0);
        assert!((lon - -77.025).abs() < 1e-6 && (lat - 38.89).abs() < 0.001);
        assert!(z > 12.0 && z < 16.0, "{z}");
    }
}
