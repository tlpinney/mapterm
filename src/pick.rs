//! Feature picking for inspection: which features are under a screen cell, like MapLibre's
//! `queryRenderedFeatures`, with their properties and some measurements for analysis.

use std::{collections::HashSet, sync::Arc};

use ratatui::layout::Rect;

use crate::{
    mvt::{GeomType, Value},
    render::{Camera, Viewport, to_lonlat, to_world},
    store::{TileId, TileStore},
};

/// How close (in braille dots) a line must pass, and a point must be, to count as hit.
const LINE_TOLERANCE_DOTS: f64 = 2.5;
const POINT_TOLERANCE: (f64, f64) = (2.5, 4.5);

/// Measurements of a picked feature.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// Center of the feature's bounding box (lon, lat).
    pub center: (f64, f64),
    /// Total length of a line in km.
    pub length_km: Option<f64>,
    /// Area of a polygon in km² (holes subtracted).
    pub area_km2: Option<f64>,
    pub vertices: usize,
}

/// A feature under the picked spot.
#[derive(Clone, Debug)]
pub struct Pick {
    /// The vector tile the feature came from (None for GeoJSON layers).
    pub tile: Option<TileId>,
    pub layer: Arc<str>,
    pub geom: GeomType,
    pub props: Vec<(String, Value)>,
    /// Geometry in (lon, lat): points, lines, or polygon rings.
    pub lonlat: Vec<Vec<(f64, f64)>>,
    /// Whether the map style draws it (unstyled features are picked too).
    pub drawn: bool,
    pub stats: Stats,
}

impl Pick {
    pub fn prop(&self, key: &str) -> Option<&Value> {
        self.props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// A short title: the name if any, else the class or kind, else the layer.
    pub fn title(&self) -> String {
        for key in ["name", "ref", "class", "kind", "subclass"] {
            if let Some(v) = self.prop(key) {
                return v.to_string();
            }
        }
        self.layer.to_string()
    }

    /// "park · water", "transportation · primary": layer plus class or kind.
    pub fn kind(&self) -> String {
        match self.prop("class").or_else(|| self.prop("kind")) {
            Some(c) => format!("{} · {c}", self.layer),
            None => self.layer.to_string(),
        }
    }

    /// The properties as a JSON object, plus `_layer`, `_geometry`, `_tile` and `_center`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("_layer".into(), self.layer.to_string().into());
        m.insert("_geometry".into(), geom_name(self.geom).into());
        if let Some(t) = self.tile {
            m.insert("_tile".into(), format!("{}/{}/{}", t.0, t.1, t.2).into());
        }
        m.insert("_center".into(), serde_json::json!([self.stats.center.0, self.stats.center.1]));
        for (k, v) in &self.props {
            let jv = match v {
                Value::Str(s) => serde_json::Value::from(s.clone()),
                Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => serde_json::Value::from(*n as i64),
                Value::Num(n) => serde_json::Number::from_f64(*n).map(Into::into).unwrap_or(serde_json::Value::Null),
                Value::Bool(b) => (*b).into(),
            };
            m.insert(k.clone(), jv);
        }
        serde_json::Value::Object(m)
    }
}

pub fn geom_name(g: GeomType) -> &'static str {
    match g {
        GeomType::Point => "point",
        GeomType::Line => "line",
        GeomType::Polygon => "polygon",
        GeomType::Unknown => "unknown",
    }
}

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    ((a.0 + t * dx - p.0).powi(2) + (a.1 + t * dy - p.1).powi(2)).sqrt()
}

/// Even-odd point in polygon over all rings (holes included).
fn inside(p: (f64, f64), rings: &[Vec<(f64, f64)>]) -> bool {
    let mut odd = false;
    for ring in rings {
        let n = ring.len();
        if n < 3 {
            continue;
        }
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (ring[i], ring[j]);
            if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
                odd = !odd;
            }
            j = i;
        }
    }
    odd
}

fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2) = (a.1.to_radians(), b.1.to_radians());
    let dla = la2 - la1;
    let dlo = (b.0 - a.0).to_radians();
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    2.0 * 6371.0088 * h.sqrt().asin()
}

fn stats(geom: GeomType, parts: &[Vec<(f64, f64)>]) -> Stats {
    let all = parts.iter().flatten();
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let mut vertices = 0;
    for &(x, y) in all {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        vertices += 1;
    }
    let center = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let length_km = (geom == GeomType::Line)
        .then(|| parts.iter().map(|p| p.windows(2).map(|w| haversine_km(w[0], w[1])).sum::<f64>()).sum());
    let area_km2 = (geom == GeomType::Polygon).then(|| {
        // Shoelace on a local equirectangular projection around the center. MVT winds outer
        // rings and holes in opposite directions, so the signed sum subtracts the holes.
        let kx = 111.32 * center.1.to_radians().cos();
        let ky = 110.57;
        let signed: f64 = parts
            .iter()
            .map(|r| r.windows(2).map(|w| (w[0].0 * kx) * (w[1].1 * ky) - (w[1].0 * kx) * (w[0].1 * ky)).sum::<f64>() / 2.0)
            .sum();
        signed.abs()
    });
    Stats { center, length_km, area_km2, vertices }
}

/// Features under absolute cell (`col`, `row`) of a map drawn in `area` with `cam`, most
/// specific first: points, then lines, then polygons from smallest to largest.
pub fn pick(store: &TileStore, cam: &Camera, min_zoom: u8, max_zoom: u8, area: Rect, col: u16, row: u16) -> Vec<Pick> {
    if !area.contains(ratatui::layout::Position::new(col, row)) {
        return Vec::new();
    }
    // The cell's center, in dots relative to the map area.
    let target = ((col - area.x) as f64 * 2.0 + 1.0, (row - area.y) as f64 * 4.0 + 2.0);
    pick_dot(store, cam, min_zoom, max_zoom, area, target)
}

/// Like [`pick`], at a lon/lat (e.g. an exact click position).
pub fn pick_lonlat(store: &TileStore, cam: &Camera, min_zoom: u8, max_zoom: u8, area: Rect, lon: f64, lat: f64) -> Vec<Pick> {
    let target = Viewport::new(cam, area).dot_ll(lon, lat);
    pick_dot(store, cam, min_zoom, max_zoom, area, target)
}

/// Like [`pick`], at a braille-dot position relative to the map area (2 dots per column, 4 per
/// row), for pointer positions finer than a cell.
pub fn pick_dot(store: &TileStore, cam: &Camera, min_zoom: u8, max_zoom: u8, area: Rect, target: (f64, f64)) -> Vec<Pick> {
    let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
    if area.width == 0 || target.0 < 0.0 || target.1 < 0.0 || target.0 >= w || target.1 >= h {
        return Vec::new();
    }
    let vp = Viewport::new(cam, area);
    let tz = cam.tile_zoom(min_zoom, max_zoom);
    let n = (1u64 << tz) as f64;
    // The tile under the spot (MVT tiles carry a buffer, so features near an edge are there too).
    let (lon, lat) = vp.dot_to_lonlat(target.0, target.1);
    let (wx, wy) = to_world(lon, lat);
    let id: TileId = (tz, (wx * n).floor().clamp(0.0, n - 1.0) as u32, (wy * n).floor().clamp(0.0, n - 1.0) as u32);
    let Some((tid, tile)) = store.get_or_ancestor(id) else { return Vec::new() };
    let tn = (1u64 << tid.0) as f64;

    let mut hits: Vec<(u8, f64, Pick)> = Vec::new();
    let mut seen = HashSet::new();
    for f in &tile.features {
        let to_world_pt = |p: &[f32; 2]| ((tid.1 as f64 + p[0] as f64) / tn, (tid.2 as f64 + p[1] as f64) / tn);
        let dots: Vec<Vec<(f64, f64)>> =
            f.parts.iter().map(|part| part.iter().map(|p| { let (x, y) = to_world_pt(p); vp.dot(x, y) }).collect()).collect();
        let (hit, rank, score) = match f.geom {
            GeomType::Point => {
                let d = dots.iter().flatten().map(|p| {
                    let (dx, dy) = ((p.0 - target.0).abs(), (p.1 - target.1).abs());
                    if dx <= POINT_TOLERANCE.0 && dy <= POINT_TOLERANCE.1 { dx.hypot(dy) } else { f64::MAX }
                });
                let best = d.fold(f64::MAX, f64::min);
                (best < f64::MAX, 0u8, best)
            }
            GeomType::Line => {
                let best = dots
                    .iter()
                    .flat_map(|p| p.windows(2).map(|w| seg_dist(target, w[0], w[1])))
                    .fold(f64::MAX, f64::min);
                (best <= LINE_TOLERANCE_DOTS, 1, best)
            }
            GeomType::Polygon => (inside(target, &dots), 2, 0.0),
            GeomType::Unknown => (false, 3, 0.0),
        };
        if !hit {
            continue;
        }
        // A feature cut by tile edges can appear more than once; keep one.
        let key = format!("{}|{:?}|{:?}", f.layer, f.geom, f.props);
        if !seen.insert(key) {
            continue;
        }
        let lonlat: Vec<Vec<(f64, f64)>> =
            f.parts.iter().map(|part| part.iter().map(|p| { let (x, y) = to_world_pt(p); to_lonlat(x, y) }).collect()).collect();
        let st = stats(f.geom, &lonlat);
        let score = if f.geom == GeomType::Polygon { st.area_km2.unwrap_or(0.0) } else { score };
        hits.push((
            rank,
            score,
            Pick { tile: Some(tid), layer: f.layer.clone(), geom: f.geom, props: f.props.clone(), lonlat, drawn: f.drawn, stats: st },
        ));
    }
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    hits.into_iter().map(|(_, _, p)| p).collect()
}

/// Features of GeoJSON layers under a braille-dot position (relative to the map area), most
/// specific first, the same way [`pick_dot`] ranks basemap features.
pub fn pick_layers(layers: &[crate::geojson::GeoJson], cam: &Camera, area: Rect, target: (f64, f64)) -> Vec<Pick> {
    let vp = Viewport::new(cam, area);
    let mut hits: Vec<(u8, f64, Pick)> = Vec::new();
    for layer in layers {
        for f in &layer.features {
            // Cheap reject: the target's world position against the feature's padded bbox.
            let dots: Vec<Vec<(f64, f64)>> = f.parts.iter().map(|p| p.iter().map(|&(x, y)| vp.dot(x, y)).collect()).collect();
            let (hit, rank, score) = match f.geom {
                GeomType::Point => {
                    let best = dots
                        .iter()
                        .flatten()
                        .map(|p| {
                            let (dx, dy) = ((p.0 - target.0).abs(), (p.1 - target.1).abs());
                            if dx <= POINT_TOLERANCE.0 && dy <= POINT_TOLERANCE.1 { dx.hypot(dy) } else { f64::MAX }
                        })
                        .fold(f64::MAX, f64::min);
                    (best < f64::MAX, 0u8, best)
                }
                GeomType::Line => {
                    let best = dots.iter().flat_map(|p| p.windows(2).map(|w| seg_dist(target, w[0], w[1]))).fold(f64::MAX, f64::min);
                    (best <= LINE_TOLERANCE_DOTS, 1, best)
                }
                GeomType::Polygon => {
                    let on_edge = dots.iter().flat_map(|p| p.windows(2).map(|w| seg_dist(target, w[0], w[1]))).fold(f64::MAX, f64::min)
                        <= LINE_TOLERANCE_DOTS;
                    (inside(target, &dots) || on_edge, 2, 0.0)
                }
                GeomType::Unknown => (false, 3, 0.0),
            };
            if !hit {
                continue;
            }
            let lonlat: Vec<Vec<(f64, f64)>> = f.parts.iter().map(|p| p.iter().map(|&(x, y)| to_lonlat(x, y)).collect()).collect();
            let st = stats(f.geom, &lonlat);
            let score = if f.geom == GeomType::Polygon { st.area_km2.unwrap_or(0.0) } else { score };
            hits.push((
                rank,
                score,
                Pick { tile: None, layer: Arc::from(layer.name.as_str()), geom: f.geom, props: f.props.clone(), lonlat, drawn: true, stats: st },
            ));
        }
    }
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    hits.into_iter().map(|(_, _, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_in_polygon_with_hole() {
        let outer = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 0.0)];
        let hole = vec![(4.0, 4.0), (6.0, 4.0), (6.0, 6.0), (4.0, 6.0), (4.0, 4.0)];
        let rings = vec![outer, hole];
        assert!(inside((2.0, 2.0), &rings));
        assert!(!inside((5.0, 5.0), &rings), "inside the hole");
        assert!(!inside((12.0, 5.0), &rings));
    }

    #[test]
    fn line_distance() {
        assert!((seg_dist((5.0, 3.0), (0.0, 0.0), (10.0, 0.0)) - 3.0).abs() < 1e-9);
        assert!((seg_dist((-4.0, 3.0), (0.0, 0.0), (10.0, 0.0)) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn picks_geojson_layer_features() {
        let doc = br#"{"type":"FeatureCollection","features":[
          {"type":"Feature","properties":{"name":"box"},"geometry":{"type":"Polygon","coordinates":[[[-77.05,38.88],[-77.02,38.88],[-77.02,38.90],[-77.05,38.90],[-77.05,38.88]]]}},
          {"type":"Feature","properties":{"name":"pin"},"geometry":{"type":"Point","coordinates":[-77.0353,38.8895]}}]}"#;
        let layer = crate::geojson::parse(doc, "mine".into(), crate::geojson::PALETTE[0]).unwrap();
        let cam = Camera::new(-77.0353, 38.8895, 15.0);
        let area = Rect::new(0, 0, 100, 40);
        let center = (100.0, 80.0);
        let picks = pick_layers(std::slice::from_ref(&layer), &cam, area, center);
        let names: Vec<String> = picks.iter().map(|p| p.title()).collect();
        assert_eq!(names, vec!["pin", "box"], "point first, then the polygon around it");
        assert!(picks.iter().all(|p| p.tile.is_none() && &*p.layer == "mine"));
        assert!(pick_layers(std::slice::from_ref(&layer), &cam, area, (2.0, 2.0)).iter().all(|p| p.title() == "box"));
    }

    #[test]
    fn measurements() {
        // About 0.01° of longitude at the equator: ~1.11 km.
        let line = stats(GeomType::Line, &[vec![(0.0, 0.0), (0.01, 0.0)]]);
        assert!((line.length_km.unwrap() - 1.112).abs() < 0.01, "{:?}", line.length_km);
        // A 0.01° square at the equator: ~1.23 km².
        let sq = vec![(0.0, 0.0), (0.01, 0.0), (0.01, 0.01), (0.0, 0.01), (0.0, 0.0)];
        let poly = stats(GeomType::Polygon, &[sq]);
        assert!((poly.area_km2.unwrap() - 1.231).abs() < 0.02, "{:?}", poly.area_km2);
        assert_eq!(poly.center, (0.005, 0.005));
    }
}
