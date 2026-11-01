//! Feature picking for inspection: which features are under a screen cell, like MapLibre's
//! `queryRenderedFeatures`, with their properties and some measurements for analysis.

use std::sync::Arc;

use crate::{
    mvt::{GeomType, Value},
    store::TileId,
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
    /// The [`crate::Map`] source it came from.
    pub source: Option<Arc<str>>,
    /// The style (or custom) layer that drew it, for [`crate::Map::query_rendered_features`].
    pub style_layer: Option<Arc<str>>,
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
        if let Some(l) = &self.style_layer {
            m.insert("_style_layer".into(), l.to_string().into());
        }
        if let Some(src) = &self.source {
            m.insert("_source".into(), src.to_string().into());
        }
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

pub(crate) fn stats(geom: GeomType, parts: &[Vec<(f64, f64)>]) -> Stats {
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

/// Whether a feature (parts in dots) is under `target`: its rank (points 0, lines 1,
/// polygons 2) and a score to sort by within the rank (distance; polygons score 0).
/// `edges` also counts a polygon as hit near its outline.
pub(crate) fn hit(geom: GeomType, dots: &[Vec<(f64, f64)>], target: (f64, f64), edges: bool) -> Option<(u8, f64)> {
    let near_line = || dots.iter().flat_map(|p| p.windows(2).map(|w| seg_dist(target, w[0], w[1]))).fold(f64::MAX, f64::min);
    match geom {
        GeomType::Point => {
            let best = dots
                .iter()
                .flatten()
                .map(|p| {
                    let (dx, dy) = ((p.0 - target.0).abs(), (p.1 - target.1).abs());
                    if dx <= POINT_TOLERANCE.0 && dy <= POINT_TOLERANCE.1 { dx.hypot(dy) } else { f64::MAX }
                })
                .fold(f64::MAX, f64::min);
            (best < f64::MAX).then_some((0, best))
        }
        GeomType::Line => {
            let best = near_line();
            (best <= LINE_TOLERANCE_DOTS).then_some((1, best))
        }
        GeomType::Polygon => (inside(target, dots) || (edges && near_line() <= LINE_TOLERANCE_DOTS)).then_some((2, 0.0)),
        GeomType::Unknown => None,
    }
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
