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
/// OpenMapTiles and Protomaps both: `landuse`/`landcover` by `class` or `kind`, `building`
/// or `buildings`.
fn area_class(p: &crate::pick::Pick) -> Option<char> {
    let class = feature_class(p).unwrap_or_default();
    match (&*p.layer, class.as_str()) {
        ("water" | "waterway", _) | (_, "water" | "river" | "lake" | "ocean" | "sea" | "canal" | "reservoir") => Some('W'),
        ("building" | "buildings", _) => Some('B'),
        ("park", _)
        | ("landcover", "grass" | "grassland" | "wood" | "forest" | "farmland" | "wetland" | "scrub")
        | (
            "landuse",
            "park" | "cemetery" | "grass" | "pitch" | "garden" | "playground" | "golf_course" | "forest" | "wood" | "nature_reserve" | "national_park"
            | "meadow" | "recreation_ground" | "village_green",
        ) => Some('P'),
        _ => None,
    }
}

/// A basemap feature's class: OpenMapTiles' `class`, else Protomaps' `kind`. For Protomaps
/// roads, the OSM road type in `kind_detail` (`primary`), not the coarse kind (`major_road`).
fn feature_class(p: &crate::pick::Pick) -> Option<String> {
    let get = |k: &str| p.props.iter().find(|(key, _)| key == k).map(|(_, v)| v.to_string()).filter(|v| !v.is_empty());
    get("class").or_else(|| if &*p.layer == "roads" { get("kind_detail").or_else(|| get("kind")) } else { get("kind") })
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
            // Most votes; a tie goes by the same order as within a point (B, W, P, then .),
            // so the grid is the same every run.
            let order = |c: char| ['.', 'P', 'W', 'B'].iter().position(|x| *x == c).unwrap_or(0);
            *cell = votes.into_iter().max_by_key(|(c, n)| (*n, order(*c))).map(|(c, _)| c).unwrap_or('.');
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

/// Roads listed, most important first.
const MAX_ROADS: usize = 30;

/// A road feature's class and its rank (0 most important), from OpenMapTiles
/// (`transportation`, `transportation_name`: `class`) or Protomaps (`roads`: `kind`). None
/// for anything else, and for service roads, tracks and paths.
fn road_class(layer: &str, get: &dyn Fn(&str) -> Option<String>) -> Option<(u8, String)> {
    const CLASSES: [&str; 6] = ["motorway", "trunk", "primary", "secondary", "tertiary", "minor"];
    let class = match layer {
        "transportation" | "transportation_name" => get("class")?,
        // The OSM road type when it's one of the classes, else the coarse kind.
        "roads" => {
            let detail = get("kind_detail").map(|d| d.trim_end_matches("_link").to_string());
            match (detail.as_deref(), get("kind")?.as_str()) {
                (Some(d @ ("motorway" | "trunk" | "primary" | "secondary" | "tertiary")), _) => d.into(),
                (Some("residential" | "unclassified" | "living_street"), _) => "minor".into(),
                (Some("service" | "track" | "pedestrian" | "footway" | "cycleway" | "path"), _) => return None,
                (_, "highway") => "motorway".into(),
                (_, "major_road") => "primary".into(),
                (_, "medium_road") => "secondary".into(),
                (_, "minor_road") => "minor".into(),
                _ => return None,
            }
        }
        _ => return None,
    };
    let rank = CLASSES.iter().position(|c| *c == class)?;
    Some((rank as u8, class))
}

/// Which way a road runs and where, from its points in view (in dots): its two
/// ends along its main direction as cells, and the compass direction between them.
fn road_course(pts: &[(f64, f64)]) -> (String, (i64, i64), (i64, i64)) {
    // The main direction: whichever of the four axes spreads the points most.
    let axes: [(f64, f64); 4] = [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, -1.0)];
    let spread = |a: (f64, f64)| {
        let v = pts.iter().map(|p| p.0 * a.0 + p.1 * a.1);
        let (lo, hi) = v.fold((f64::MAX, f64::MIN), |m, x| (m.0.min(x), m.1.max(x)));
        (hi - lo) / a.0.hypot(a.1)
    };
    let axis = axes.into_iter().max_by(|a, b| spread(*a).total_cmp(&spread(*b))).unwrap_or((1.0, 0.0));
    let along = |p: &&(f64, f64)| p.0 * axis.0 + p.1 * axis.1;
    let a = *pts.iter().min_by(|p, q| along(p).total_cmp(&along(q))).unwrap_or(&(0.0, 0.0));
    let b = *pts.iter().max_by(|p, q| along(p).total_cmp(&along(q))).unwrap_or(&(0.0, 0.0));
    // Dots are about square on screen; north is up (y grows down).
    let (dx, dy) = (b.0 - a.0, a.1 - b.1);
    let bearing = dx.atan2(dy).to_degrees().rem_euclid(180.0);
    let dir = match ((bearing + 22.5) / 45.0) as usize % 4 {
        0 => "north-south",
        1 => "north-east to south-west",
        2 => "east-west",
        _ => "north-west to south-east",
    };
    let cell = |p: (f64, f64)| ((p.0 / 2.0) as i64, (p.1 / 4.0) as i64);
    (dir.to_string(), cell(a), cell(b))
}

/// The part of segment `a`-`b` inside the `w` x `h` view (Liang-Barsky), or None.
fn clip(a: (f64, f64), b: (f64, f64), w: f64, h: f64) -> Option<((f64, f64), (f64, f64))> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, a.0), (dx, w - a.0), (-dy, a.1), (dy, h - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 <= t1).then_some(((a.0 + t0 * dx, a.1 + t0 * dy), (a.0 + t1 * dx, a.1 + t1 * dy)))
}

/// Named roads in the view, most important first: name, class, which way they run, their
/// ends in view and length there. Names come from the road lines or, in OpenMapTiles, the
/// `transportation_name` lines drawn as their labels (read even when labels are hidden).
fn roads(map: &Map, cam: &Camera, area: Rect, vp: &Viewport, out: &mut String) {
    let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
    // Road -> (rank, class, ref, points in view in dots).
    type Road = (u8, String, Option<String>, Vec<(f64, f64)>);
    let mut found: std::collections::BTreeMap<String, Road> = Default::default();
    let mut unnamed = 0usize;
    let mut y = 2.0;
    while y < h {
        let mut x = 2.0;
        while x < w {
            for p in map.query_features_with(cam, area, (x, y), None, true) {
                if p.geom != crate::mvt::GeomType::Line {
                    continue;
                }
                let get = |k: &str| p.props.iter().find(|(key, _)| key == k).map(|(_, v)| v.to_string()).filter(|v| !v.is_empty());
                let Some((rank, class)) = road_class(&p.layer, &get) else { continue };
                let reference = get("ref");
                let Some(name) = get("name").or_else(|| get("name:en")).or_else(|| get("name_en")).or_else(|| reference.clone()) else {
                    unnamed += 1;
                    continue;
                };
                let e = found.entry(name).or_insert_with(|| (rank, class.clone(), None, Vec::new()));
                if rank < e.0 {
                    (e.0, e.1) = (rank, class);
                }
                if e.2.is_none() {
                    e.2 = reference;
                }
                for part in &p.lonlat {
                    let dots: Vec<(f64, f64)> = part.iter().map(|&(lon, lat)| vp.dot_ll(lon, lat)).collect();
                    for seg in dots.windows(2) {
                        // Just inside the last column and row, so ends name real cells.
                        if let Some((a, b)) = clip(seg[0], seg[1], w - 0.01, h - 0.01) {
                            e.3.extend([a, b]);
                        }
                    }
                }
            }
            x += 4.0;
        }
        y += 8.0;
    }
    // Segments known only by a route number ("29") join the named road with that number.
    let numbered: Vec<(String, String)> = found.iter().filter_map(|(n, r)| r.2.clone().filter(|x| x != n).map(|x| (x, n.clone()))).collect();
    for (number, name) in numbered {
        if let Some(r) = found.remove(&number) {
            let e = found.get_mut(&name).expect("listed above");
            e.0 = e.0.min(r.0);
            e.3.extend(r.3);
        }
    }
    // Long enough to say which way it runs: 3 cells end to end.
    let long = |r: &Road| {
        let (_, a, b) = road_course(&r.3);
        (a.0 - b.0).abs().max((a.1 - b.1).abs()) >= 3
    };
    let mut roads: Vec<(String, Road)> = found.into_iter().filter(|(_, r)| r.3.len() >= 2 && long(r)).collect();
    let length = |r: &Road| {
        let (_, a, b) = road_course(&r.3);
        let (p, q) = (vp.dot_to_lonlat(a.0 as f64 * 2.0, a.1 as f64 * 4.0), vp.dot_to_lonlat(b.0 as f64 * 2.0, b.1 as f64 * 4.0));
        dist(p.0, p.1, q.0, q.1)
    };
    roads.sort_by(|a, b| a.1.0.cmp(&b.1.0).then(length(&b.1).total_cmp(&length(&a.1))));
    let n = roads.len();
    let _ = writeln!(out, "\n# Roads ({n} named{})", if n > MAX_ROADS { format!(", the {MAX_ROADS} most important listed") } else { String::new() });
    for (name, r) in roads.iter().take(MAX_ROADS) {
        let (dir, a, b) = road_course(&r.3);
        let reference = r.2.as_ref().filter(|x| *x != name).map(|x| format!(" ({x})")).unwrap_or_default();
        let m = (length(r) / 10.0).round() * 10.0;
        let m = if m >= 1000.0 { format!("{:.1} km", m / 1000.0) } else { format!("{m:.0} m") };
        let _ = writeln!(out, "- {name}{reference} [{}]: runs {dir}, from cell ({}, {}) to ({}, {}), about {m} in view", r.1, a.0, a.1, b.0, b.1);
    }
    if unnamed > 0 && n == 0 {
        let _ = writeln!(out, "(roads are drawn but carry no names)");
    }
}

/// What a label labels: the layer and class of a feature under it that has that name.
fn kind_of(map: &Map, cam: &Camera, area: Rect, vp: &Viewport, lon: f64, lat: f64, text: &str) -> Option<String> {
    let dot = vp.dot_ll(lon, lat);
    let picks = map.query_rendered_features(cam, area, dot, None);
    let named = |p: &&crate::pick::Pick| p.props.iter().any(|(k, v)| (k == "name" || k.starts_with("name:") || k == "ref") && v.to_string() == text);
    let p = picks.iter().find(named).or_else(|| picks.first())?;
    let class = feature_class(p);
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
    fn road_classes_and_courses() {
        let props = |k: &'static str, v: &'static str| move |q: &str| (q == k).then(|| v.to_string());
        assert_eq!(road_class("transportation_name", &props("class", "tertiary")), Some((4, "tertiary".into())));
        assert_eq!(road_class("roads", &props("kind", "highway")), Some((0, "motorway".into())));
        let pm = |kind: &'static str, detail: &'static str| {
            move |q: &str| match q {
                "kind" => Some(kind.to_string()),
                "kind_detail" => Some(detail.to_string()),
                _ => None,
            }
        };
        assert_eq!(road_class("roads", &pm("major_road", "secondary")), Some((3, "secondary".into())), "Protomaps' OSM type wins");
        assert_eq!(road_class("roads", &pm("highway", "motorway_link")), Some((0, "motorway".into())));
        assert_eq!(road_class("roads", &pm("minor_road", "residential")), Some((5, "minor".into())));
        assert_eq!(road_class("roads", &pm("minor_road", "service")), None);
        assert_eq!(road_class("roads", &pm("path", "sidewalk")), None);
        assert_eq!(road_class("transportation", &props("class", "service")), None);
        assert_eq!(road_class("waterway", &props("class", "primary")), None, "not a road layer");
        // In a 120 x 160 dot view (60 x 40 cells).
        let line =
            |a: (f64, f64), b: (f64, f64)| (0..=10).map(|i| (a.0 + (b.0 - a.0) * i as f64 / 10.0, a.1 + (b.1 - a.1) * i as f64 / 10.0)).collect::<Vec<_>>();
        assert_eq!(road_course(&line((60.0, 0.0), (60.0, 159.0))), ("north-south".into(), (30, 0), (30, 39)));
        assert_eq!(road_course(&line((0.0, 80.0), (119.0, 80.0))).0, "east-west");
        assert_eq!(road_course(&line((0.0, 159.0), (119.0, 40.0))).0, "north-east to south-west");
        assert_eq!(road_course(&line((0.0, 0.0), (100.0, 100.0))).0, "north-west to south-east");
        // A straight street with both ends outside the view still crosses it.
        assert_eq!(clip((-50.0, 80.0), (500.0, 80.0), 120.0, 160.0), Some(((0.0, 80.0), (120.0, 80.0))));
        assert_eq!(clip((-50.0, -1.0), (500.0, -1.0), 120.0, 160.0), None);
        assert_eq!(clip((10.0, 10.0), (20.0, 20.0), 120.0, 160.0), Some(((10.0, 10.0), (20.0, 20.0))));
    }

    #[test]
    fn roads_named_from_hidden_label_layers() {
        // A street's name on a label layer that's switched off (--no-labels), its vertices
        // far outside the view: listed with its class, direction and both ends at the edges.
        let mut map = Map::new();
        let doc = serde_json::json!({"type": "Feature", "properties": {"class": "primary", "name": "Main Street"},
            "geometry": {"type": "LineString", "coordinates": [[-1.0, 0.0], [1.0, 0.0]]}});
        map.add_source("transportation_name", MapSource::geojson(&doc).unwrap()).unwrap();
        map.add_layer(crate::StyleLayer::line("road-label", "transportation_name").color("#000000")).unwrap();
        map.set_visibility("road-label", false).unwrap();
        let (cam, area) = (Camera::new(0.0, 0.0, 15.0), Rect::new(0, 0, 60, 20));
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(map.view(&cam), area, &mut buf);
        let d = describe(&map, &cam, area);
        assert!(d.contains("# Roads (1 named)"), "{d}");
        assert!(d.contains("- Main Street [primary]: runs east-west, from cell (0, 10) to (59, 10)"), "{d}");
        assert!(map.query_rendered_features(&cam, area, (60.0, 40.0), None).is_empty(), "still hidden from picking");
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
    fn protomaps_areas() {
        // Protomaps names green by `kind` in `landuse`, and its buildings layer `buildings`.
        let mut map = Map::new();
        let sq = |x0: f64, x1: f64, props: serde_json::Value| {
            serde_json::json!({"type": "Feature", "properties": props,
            "geometry": {"type": "Polygon", "coordinates": [[[x0, -0.01], [x1, -0.01], [x1, 0.01], [x0, 0.01], [x0, -0.01]]]}})
        };
        map.add_source("landuse", MapSource::geojson(&sq(-0.01, 0.0, serde_json::json!({"kind": "park"}))).unwrap()).unwrap();
        map.add_source("buildings", MapSource::geojson(&sq(0.0, 0.01, serde_json::json!({"kind": "building"}))).unwrap()).unwrap();
        map.add_layer(crate::StyleLayer::fill("park", "landuse").color("#00ff00")).unwrap();
        map.add_layer(crate::StyleLayer::fill("bldg", "buildings").color("#888888")).unwrap();
        let (cam, area) = (Camera::new(0.0, 0.0, 15.0), Rect::new(0, 0, 60, 20));
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(map.view(&cam), area, &mut buf);
        let d = describe(&map, &cam, area);
        let row = d.lines().skip_while(|l| !l.starts_with("grid")).nth(5).unwrap_or_default().to_string();
        assert!(row.starts_with("PPPPPPPPPPPP") && row.ends_with("BBBBBBBBBBBB"), "{row}\n{d}");
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
