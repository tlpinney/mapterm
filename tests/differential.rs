//! Differential tests: mapterm's readers against GDAL on a corpus of real files.
//!
//! Each file in `tests/fixtures/corpus/` has GDAL's reading of it beside it
//! (`<file>.gdal.json`, from `make-reference.py`), and mapterm must read the same features:
//!
//! - Shapefile and FlatGeobuf: the same geometry and the same attributes.
//! - GPX and KML: the same geometry and the same feature names (GDAL names the other fields
//!   its own way).
//!
//! Geometry is compared as its 2D parts: points, lines and polygon rings, with Multi* and
//! GeometryCollection taken apart, so a one-part MultiLineString matches a LineString.
//! Features without geometry are left out on both sides, as mapterm can't draw them, and so
//! are GPX lines of fewer than two points. A file GDAL can't read has no reference; mapterm
//! must still read it or refuse it without panicking.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/corpus");

/// Known differences, by file and property, and why they're expected. Anything else that
/// differs fails the test.
const KNOWN: &[(&str, &str, &str)] = &[
    ("fgb/alldatatypes.fgb", "float", "GDAL 3.9's FlatGeobuf driver drops Float (f32) columns"),
    ("fgb/alldatatypes.fgb", "binary", "mapterm shows binary values as <n bytes>"),
];

/// Files mapterm refuses on purpose, and why.
const REFUSED: &[(&str, &str)] = &[
    ("fgb/large_header.fgb", "British National Grid (EPSG:27700): mapterm reads lon/lat only"),
    ("fgb/poly00.fgb", "British National Grid (EPSG:27700): mapterm reads lon/lat only"),
    ("fgb/poly01.fgb", "British National Grid (EPSG:27700): mapterm reads lon/lat only"),
];

#[derive(Debug, PartialEq)]
enum Part {
    Point([f64; 2]),
    Line(Vec<[f64; 2]>),
    Polygon(Vec<Vec<[f64; 2]>>),
}

fn xy(c: &Json) -> [f64; 2] {
    let a = c.as_array().expect("a position");
    [a[0].as_f64().unwrap(), a[1].as_f64().unwrap()]
}

fn line(c: &Json) -> Vec<[f64; 2]> {
    c.as_array().unwrap().iter().map(xy).collect()
}

fn polygon(c: &Json) -> Vec<Vec<[f64; 2]>> {
    c.as_array().unwrap().iter().map(line).collect()
}

/// A geometry as its parts, in order.
fn parts(g: &Json, out: &mut Vec<Part>) {
    let c = &g["coordinates"];
    match g["type"].as_str().unwrap_or("") {
        "Point" => out.push(Part::Point(xy(c))),
        "MultiPoint" => out.extend(c.as_array().unwrap().iter().map(|p| Part::Point(xy(p)))),
        "LineString" | "LinearRing" => out.push(Part::Line(line(c))),
        "MultiLineString" => out.extend(c.as_array().unwrap().iter().map(|l| Part::Line(line(l)))),
        "Polygon" => out.push(Part::Polygon(polygon(c))),
        "MultiPolygon" => out.extend(c.as_array().unwrap().iter().map(|p| Part::Polygon(polygon(p)))),
        "GeometryCollection" => {
            for g in g["geometries"].as_array().unwrap() {
                parts(g, out);
            }
        }
        other => panic!("unexpected geometry type {other:?}"),
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn same_parts(a: &[Part], b: &[Part]) -> bool {
    let pts = |a: &[[f64; 2]], b: &[[f64; 2]]| a.len() == b.len() && a.iter().zip(b).all(|(p, q)| close(p[0], q[0]) && close(p[1], q[1]));
    a.len() == b.len()
        && a.iter().zip(b).all(|(p, q)| match (p, q) {
            (Part::Point(p), Part::Point(q)) => pts(&[*p], &[*q]),
            (Part::Line(p), Part::Line(q)) => pts(p, q),
            (Part::Polygon(p), Part::Polygon(q)) => p.len() == q.len() && p.iter().zip(q).all(|(r, s)| pts(r, s)),
            _ => false,
        })
}

/// Empty strings and nulls are the same missing value.
fn missing(v: Option<&Json>) -> bool {
    matches!(v, None | Some(Json::Null)) || v.and_then(Json::as_str) == Some("")
}

fn same_value(ours: Option<&Json>, gdal: Option<&Json>) -> bool {
    if missing(ours) || missing(gdal) {
        return missing(ours) && missing(gdal);
    }
    let (o, g) = (ours.unwrap(), gdal.unwrap());
    match (o, g) {
        (Json::Number(a), Json::Number(b)) => close(a.as_f64().unwrap(), b.as_f64().unwrap()),
        // A date or time: GDAL writes UTC as +00:00, others as Z.
        (Json::String(a), Json::String(b)) => a == b || a.strip_suffix('Z').is_some_and(|a| b.strip_suffix("+00:00") == Some(a)),
        _ => o == g,
    }
}

struct Feature {
    parts: Vec<Part>,
    props: serde_json::Map<String, Json>,
}

fn features(doc: &Json, gpx: bool) -> Vec<Feature> {
    let mut out = Vec::new();
    for f in doc["features"].as_array().expect("features") {
        if f["geometry"].is_null() {
            continue;
        }
        let mut ps = Vec::new();
        parts(&f["geometry"], &mut ps);
        if gpx {
            ps.retain(|p| !matches!(p, Part::Line(l) if l.len() < 2));
        }
        if ps.is_empty() {
            continue;
        }
        out.push(Feature { parts: ps, props: f["properties"].as_object().cloned().unwrap_or_default() });
    }
    out
}

/// mapterm's reading of a corpus file, as GeoJSON.
fn ours(path: &Path) -> anyhow::Result<Json> {
    let bytes = std::fs::read(path)?;
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("shp")) {
        let side = |ext: &str| std::fs::read(path.with_extension(ext)).ok();
        let (dbf, prj, cpg) = (side("dbf"), side("prj"), side("cpg"));
        let parts = mapterm::shp::Parts { shp: &bytes, dbf: dbf.as_deref(), prj: prj.as_deref(), cpg: cpg.as_deref() };
        return mapterm::shp::to_geojson(&parts);
    }
    mapterm::formats::to_geojson(&path.to_string_lossy(), &bytes)
}

/// The differences between mapterm's and GDAL's reading of one file, if any.
fn compare(path: &Path) -> Option<String> {
    let rel = path.strip_prefix(CORPUS).unwrap().to_string_lossy().replace('\\', "/");
    let ext = path.extension().unwrap().to_string_lossy().to_ascii_lowercase();
    let reference = PathBuf::from(format!("{}.gdal.json", path.display()));
    let result = std::panic::catch_unwind(|| ours(path));
    let Ok(result) = result else { return Some("mapterm panicked".into()) };
    let Ok(reference) = std::fs::read(&reference) else {
        // GDAL couldn't read it: mapterm may read it or refuse it, just not panic.
        return None;
    };
    let reference: Json = serde_json::from_slice(&reference).unwrap();
    let refused = REFUSED.iter().any(|(f, _)| *f == rel);
    let doc = match result {
        Ok(_) if refused => return Some("mapterm now reads a file listed in REFUSED: compare it and drop it from the list".into()),
        Ok(doc) => doc,
        Err(_) if refused => return None,
        Err(e) => return Some(format!("mapterm refused a file GDAL reads: {e:#}")),
    };
    let gpx = ext == "gpx";
    let (ours, gdal) = (features(&doc, gpx), features(&reference, gpx));
    let mut diff = String::new();
    if ours.len() != gdal.len() {
        let _ = writeln!(diff, "  {} features, GDAL {}", ours.len(), gdal.len());
    }
    let all_props = ext == "shp" || ext == "fgb";
    for (i, (o, g)) in ours.iter().zip(&gdal).enumerate() {
        if !same_parts(&o.parts, &g.parts) {
            let _ = writeln!(diff, "  feature {i}: geometry differs\n    mapterm {:?}\n    GDAL    {:?}", brief(&o.parts), brief(&g.parts));
        }
        if all_props {
            let mut keys: Vec<&String> = o.props.keys().chain(g.props.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                if KNOWN.iter().any(|(f, p, _)| *f == rel && p == k) {
                    continue;
                }
                if !same_value(o.props.get(k), g.props.get(k)) {
                    let _ = writeln!(diff, "  feature {i}: {k} = {:?}, GDAL {:?}", o.props.get(k), g.props.get(k));
                }
            }
        } else {
            let name = |p: &serde_json::Map<String, Json>| p.get("name").or_else(|| p.get("Name")).cloned();
            if !same_value(name(&o.props).as_ref(), name(&g.props).as_ref()) {
                let _ = writeln!(diff, "  feature {i}: name {:?}, GDAL {:?}", name(&o.props), name(&g.props));
            }
        }
    }
    (!diff.is_empty()).then_some(diff)
}

/// A short description of geometry parts for failure messages.
fn brief(parts: &[Part]) -> Vec<String> {
    parts
        .iter()
        .map(|p| match p {
            Part::Point(p) => format!("point {p:?}"),
            Part::Line(l) => format!("line of {} from {:?}", l.len(), l.first()),
            Part::Polygon(r) => format!("polygon of {} rings, {:?} points", r.len(), r.iter().map(Vec::len).collect::<Vec<_>>()),
        })
        .collect()
}

fn corpus() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in std::fs::read_dir(CORPUS).unwrap() {
        let dir = dir.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&dir).unwrap() {
            let f = f.unwrap().path();
            let ext = f.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if ["shp", "fgb", "gpx", "kml"].contains(&ext.as_str()) {
                files.push(f);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn readers_match_gdal() {
    let files = corpus();
    assert!(files.len() >= 20, "the corpus is missing: {CORPUS}");
    let mut failures = String::new();
    for f in &files {
        if let Some(d) = compare(f) {
            let _ = writeln!(failures, "{}:\n{d}", f.strip_prefix(CORPUS).unwrap_or(f).display());
        }
    }
    assert!(failures.is_empty(), "mapterm and GDAL disagree:\n{failures}");
}
