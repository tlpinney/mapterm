//! A map view described as text for a language model (`mapterm --snapshot ... --format describe`):
//! what's shown and where, as facts rather than a picture. Models read shapes poorly from
//! characters laid out in two dimensions, but read lists well, so this lists the view, every
//! visible label with what it is and where it sits (cell, lon/lat, direction and distance from
//! the center), and the data layers' features nearest the center with their properties.

use std::fmt::Write;

use ratatui::layout::Rect;

use crate::{Camera, Map, MapSource, Viewport};

/// Data-layer features listed per layer (nearest the center first).
const PER_LAYER: usize = 25;

/// Describe the view `cam` x `area` of `map`, which must just have been drawn there (the
/// labels are the ones placed in that frame).
pub fn describe(map: &Map, cam: &Camera, area: Rect) -> String {
    let vp = Viewport::new(cam, area);
    let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
    let (lon0, lat0) = (cam.lon, cam.lat);
    let (nw, se) = (vp.dot_to_lonlat(0.0, 0.0), vp.dot_to_lonlat(w, h));
    let (width_m, height_m) =
        (meters(lon0, lat0, se.0, lat0).0 - meters(lon0, lat0, nw.0, lat0).0, meters(lon0, lat0, lon0, nw.1).1 - meters(lon0, lat0, lon0, se.1).1);
    let mut out = String::new();
    let _ = writeln!(out, "# Map view");
    let _ = writeln!(out, "center: {lat0:.5}, {lon0:.5} (lat, lon); zoom {:.1}", cam.zoom);
    let _ = writeln!(
        out,
        "extent: {:.0} m wide x {:.0} m tall; north is up; {} columns x {} rows ({:.0} m per column)",
        width_m.abs(),
        height_m.abs(),
        area.width,
        area.height,
        width_m.abs() / area.width as f64
    );
    let _ = writeln!(out, "bounds: west {:.5}, south {:.5}, east {:.5}, north {:.5}", nw.0, se.1, se.0, nw.1);
    let _ = writeln!(out, "positions: (column, row) from the top-left; direction and distance are from the center.");

    // Labels, nearest the center first, each with what it labels.
    let mut labels: Vec<(String, String, f64, f64)> = map
        .placed_labels()
        .into_iter()
        .map(|(key, lon, lat)| {
            let text = key.rsplit('\u{1}').next().unwrap_or(&key).trim().to_string();
            let glyph = key.strip_prefix('\u{1}').and_then(|k| k.split('\u{1}').next()).map(str::to_string);
            let kind = kind_of(map, cam, area, &vp, lon, lat, &text).or_else(|| glyph.map(|g| format!("marker {g}"))).unwrap_or_default();
            (text, kind, lon, lat)
        })
        .filter(|(t, ..)| !t.is_empty())
        .collect();
    labels.sort_by(|a, b| dist(lon0, lat0, a.2, a.3).total_cmp(&dist(lon0, lat0, b.2, b.3)));
    labels.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let _ = writeln!(out, "\n# Labels ({})", labels.len());
    for (text, kind, lon, lat) in &labels {
        let (x, y) = vp.dot_ll(*lon, *lat);
        let kind = if kind.is_empty() { String::new() } else { format!(" [{kind}]") };
        let _ = writeln!(out, "- {text}{kind}: cell ({}, {}), {lat:.5}, {lon:.5}, {}", (x / 2.0) as i64, (y / 4.0) as i64, heading(lon0, lat0, *lon, *lat));
    }

    areas(map, cam, area, &mut out);
    roads(map, cam, area, &vp, &mut out);

    // Data layers (GeoJSON, GeoParquet, ...): how many in view, and the nearest ones.
    for (id, src) in map.sources() {
        let MapSource::GeoJson(_) = src else { continue };
        let Some(features) = map.features_in_view_list(id, cam, area) else { continue };
        if features.is_empty() {
            continue;
        }
        let n = features.len();
        let _ = writeln!(out, "\n# Layer {id}: {n} features in view (nearest {} listed)", n.min(PER_LAYER));
        let mut near: Vec<(f64, (f64, f64), &crate::geojson::Feature)> = features
            .into_iter()
            .map(|f| {
                let c = centroid(f);
                (if c.0.is_nan() { f64::INFINITY } else { dist(lon0, lat0, c.0, c.1) }, c, f)
            })
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, (lon, lat), p) in near.into_iter().take(PER_LAYER) {
            let props: Vec<String> = p.props.iter().filter(|(k, _)| !k.starts_with('_')).map(|(k, v)| format!("{k}={v}")).collect();
            let (x, y) = vp.dot_ll(lon, lat);
            let _ = writeln!(
                out,
                "- {:?} at cell ({}, {}), {lat:.5}, {lon:.5}, {}{}",
                p.geom,
                (x / 2.0) as i64,
                (y / 4.0) as i64,
                heading(lon0, lat0, lon, lat),
                if props.is_empty() { String::new() } else { format!("; {}", props.join(", ")) }
            );
        }
    }
    out
}

/// Grid squares across and down in the area grid.
const GRID: (usize, usize) = (25, 10);

/// What a basemap feature is, for the area grid: water, park (green space) or building.
fn area_class(p: &crate::pick::Pick) -> Option<char> {
    let class = p.props.iter().find(|(k, _)| k == "class").map(|(_, v)| v.to_string()).unwrap_or_default();
    match (&*p.layer, class.as_str()) {
        ("water" | "waterway", _) | (_, "water" | "river" | "lake" | "ocean" | "sea" | "canal" | "reservoir") => Some('W'),
        ("building", _) => Some('B'),
        ("park", _)
        | ("landcover", "grass" | "wood" | "farmland" | "wetland" | "scrub")
        | ("landuse", "park" | "cemetery" | "grass" | "pitch" | "garden" | "playground") => Some('P'),
        _ => None,
    }
}

/// A grid of what covers each part of the view (W water, P park or green, B buildings,
/// . none of them), and each one's share, from what's under points in each square.
fn areas(map: &Map, cam: &Camera, area: Rect, out: &mut String) {
    let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
    let (gw, gh) = GRID;
    let mut grid = vec![vec!['.'; gw]; gh];
    let mut count = std::collections::HashMap::new();
    for (gy, row) in grid.iter_mut().enumerate() {
        for (gx, cell) in row.iter_mut().enumerate() {
            let mut votes = std::collections::HashMap::new();
            for (fx, fy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let dot = ((gx as f64 + fx) * w / gw as f64, (gy as f64 + fy) * h / gh as f64);
                let picks = map.query_rendered_features(cam, area, dot, None);
                // Buildings stand on everything; then water, then green.
                let c = ['B', 'W', 'P'].into_iter().find(|c| picks.iter().any(|p| area_class(p) == Some(*c))).unwrap_or('.');
                *votes.entry(c).or_insert(0) += 1;
            }
            *cell = votes.into_iter().max_by_key(|(c, n)| (*n, *c != '.')).map(|(c, _)| c).unwrap_or('.');
            *count.entry(*cell).or_insert(0) += 1;
        }
    }
    let share = |c: char| 100 * count.get(&c).copied().unwrap_or(0) / (gw * gh);
    let _ = writeln!(out, "\n# Areas");
    let _ = writeln!(out, "water {}%, parks and green {}%, buildings {}% of the view", share('W'), share('P'), share('B'));
    let _ = writeln!(out, "grid ({gw} x {gh}, north up; W water, P park or green, B buildings, . other):");
    for row in grid {
        let _ = writeln!(out, "{}", row.into_iter().collect::<String>());
    }
}

/// Major roads in the view: name, class, which way they run and where.
fn roads(map: &Map, cam: &Camera, area: Rect, vp: &Viewport, out: &mut String) {
    const MAJOR: [&str; 4] = ["motorway", "trunk", "primary", "secondary"];
    let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
    // Road -> (class, points in view in dots).
    let mut found: std::collections::BTreeMap<String, (String, Vec<(f64, f64)>)> = Default::default();
    let mut y = 2.0;
    while y < h {
        let mut x = 2.0;
        while x < w {
            for p in map.query_rendered_features(cam, area, (x, y), None) {
                let get = |k: &str| p.props.iter().find(|(key, _)| key == k).map(|(_, v)| v.to_string());
                let Some(class) = get("class").filter(|c| MAJOR.contains(&c.as_str())) else { continue };
                if p.geom != crate::mvt::GeomType::Line {
                    continue;
                }
                let name = get("name").or_else(|| get("ref")).unwrap_or_else(|| format!("unnamed {class} road"));
                let e = found.entry(name).or_insert_with(|| (class.clone(), Vec::new()));
                for &(lon, lat) in p.lonlat.iter().flatten() {
                    let d = vp.dot_ll(lon, lat);
                    if d.0 >= 0.0 && d.1 >= 0.0 && d.0 < w && d.1 < h {
                        e.1.push(d);
                    }
                }
            }
            x += 4.0;
        }
        y += 8.0;
    }
    let _ = writeln!(out, "\n# Major roads ({})", found.values().filter(|(_, pts)| pts.len() >= 2).count());
    for (name, (class, pts)) in found {
        if pts.len() < 2 {
            continue;
        }
        let (x0, x1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
        let (y0, y1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
        let (sx, sy) = ((x1 - x0) / w, (y1 - y0) / h);
        let third = |v: f64, n: f64, names: [&'static str; 3]| names[((v / n * 3.0) as usize).min(2)];
        let (dir, at) = if sx > 2.0 * sy {
            ("east-west", format!("in the {} third", third((y0 + y1) / 2.0, h, ["top", "middle", "bottom"])))
        } else if sy > 2.0 * sx {
            ("north-south", format!("in the {} third", third((x0 + x1) / 2.0, w, ["left", "center", "right"])))
        } else {
            ("diagonally", String::new())
        };
        let span = sx.max(sy);
        let how = if span > 0.7 { "across the whole view" } else { "partway" };
        let _ = writeln!(out, "- {name} [{class}]: runs {dir} {how}{}", if at.is_empty() { String::new() } else { format!(", {at}") });
    }
}

/// What a label labels: the layer and class of a feature under it that has that name.
fn kind_of(map: &Map, cam: &Camera, area: Rect, vp: &Viewport, lon: f64, lat: f64, text: &str) -> Option<String> {
    let dot = vp.dot_ll(lon, lat);
    let picks = map.query_rendered_features(cam, area, dot, None);
    let named = |p: &&crate::pick::Pick| p.props.iter().any(|(k, v)| (k == "name" || k.starts_with("name:") || k == "ref") && v.to_string() == text);
    let p = picks.iter().find(named).or_else(|| picks.first())?;
    let class = p.props.iter().find(|(k, _)| k == "class" || k == "subclass").map(|(_, v)| v.to_string());
    Some(match class {
        Some(c) => format!("{}/{c}", p.layer),
        None => p.layer.to_string(),
    })
}

/// East and north offsets in meters from (lon0, lat0) to (lon, lat), on a local flat plane.
fn meters(lon0: f64, lat0: f64, lon: f64, lat: f64) -> (f64, f64) {
    ((lon - lon0) * 111_320.0 * lat0.to_radians().cos(), (lat - lat0) * 110_540.0)
}

fn dist(lon0: f64, lat0: f64, lon: f64, lat: f64) -> f64 {
    let (e, n) = meters(lon0, lat0, lon, lat);
    e.hypot(n)
}

/// "320 m north-west", or "at the center".
fn heading(lon0: f64, lat0: f64, lon: f64, lat: f64) -> String {
    let (e, n) = meters(lon0, lat0, lon, lat);
    let d = e.hypot(n);
    if d < 15.0 {
        return "at the center".into();
    }
    const DIRS: [&str; 8] = ["north", "north-east", "east", "south-east", "south", "south-west", "west", "north-west"];
    let bearing = e.atan2(n).to_degrees().rem_euclid(360.0);
    let dir = DIRS[((bearing + 22.5) / 45.0) as usize % 8];
    let d = if d >= 1000.0 { format!("{:.1} km", d / 1000.0) } else { format!("{:.0} m", (d / 10.0).round() * 10.0) };
    format!("{d} {dir}")
}

/// A feature's lon/lat center (the middle of its box).
fn centroid(f: &crate::geojson::Feature) -> (f64, f64) {
    if f.parts.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    crate::render::to_lonlat((f.bbox[0] + f.bbox[2]) / 2.0, (f.bbox[1] + f.bbox[3]) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings() {
        assert_eq!(heading(0.0, 0.0, 0.0, 0.0), "at the center");
        assert_eq!(heading(0.0, 0.0, 0.0, 0.003), "330 m north");
        assert_eq!(heading(0.0, 0.0, -0.002, -0.002), "310 m south-west");
        assert_eq!(heading(0.0, 0.0, 0.02, 0.0), "2.2 km east");
    }

    #[test]
    fn area_grid() {
        // Water over the west half of the view: W on the left of the grid, . on the right.
        let mut map = Map::new();
        let doc = serde_json::json!({"type": "Polygon", "coordinates": [[[-0.01, -0.01], [0.0, -0.01], [0.0, 0.01], [-0.01, 0.01], [-0.01, -0.01]]]});
        map.add_source("water", MapSource::geojson(&doc).unwrap()).unwrap();
        map.add_layer(crate::StyleLayer::fill("water", "water").color("#0000ff")).unwrap();
        let (cam, area) = (Camera::new(0.0, 0.0, 15.0), Rect::new(0, 0, 60, 20));
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(map.view(&cam), area, &mut buf);
        let d = describe(&map, &cam, area);
        let rows: Vec<&str> = d.lines().skip_while(|l| !l.starts_with("grid")).skip(1).take(GRID.1).collect();
        assert_eq!(rows.len(), GRID.1, "{d}");
        for r in rows {
            assert!(r.starts_with("WWWWWWWWWWWW") && r.ends_with("............"), "{r}");
        }
        assert!(d.contains("water 4") || d.contains("water 5"), "about half: {d}");
    }

    #[test]
    fn describes_labels_and_layers() {
        let mut map = Map::new();
        let doc = serde_json::json!({"type": "FeatureCollection", "features": [
            {"type": "Feature", "properties": {"name": "Alpha", "height": 12}, "geometry": {"type": "Point", "coordinates": [0.001, 0.001]}},
            {"type": "Feature", "properties": {"name": "Beta"}, "geometry": {"type": "Point", "coordinates": [-0.002, 0.0]}}]});
        let g = crate::geojson::parse(&serde_json::to_vec(&doc).unwrap(), "sites".into(), ratatui::style::Color::Red).unwrap();
        map.add_source("sites", MapSource::from(g)).unwrap();
        map.add_layers(crate::geojson::overlay_layers("sites", ratatui::style::Color::Red, true), None).unwrap();
        let (cam, area) = (Camera::new(0.0, 0.0, 15.0), Rect::new(0, 0, 60, 20));
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(map.view(&cam), area, &mut buf);
        let d = describe(&map, &cam, area);
        assert!(d.contains("center: 0.00000, 0.00000"), "{d}");
        assert!(d.contains("- Alpha") && d.contains("north-east"), "{d}");
        assert!(d.contains("- Beta") && d.contains("west"), "{d}");
        assert!(d.contains("# Layer sites: 2 features in view"), "{d}");
        assert!(d.contains("height=12"), "{d}");
    }
}
