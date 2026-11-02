//! Esri Shapefiles as GeoJSON, read with the [`shapefile`] crate: the geometry from `.shp`,
//! attributes from the `.dbf` beside it (text decoded with the `.cpg` code page when there is
//! one), and the coordinate system from `.prj`. Coordinates must be lon/lat: a `.prj` that
//! names a projected system (`PROJCS`) is refused, as is a file without one whose
//! coordinates fall outside lon/lat range. Reprojection is for later.
//!
//! Polygons follow the Shapefile ring rule: outer rings run clockwise and each hole follows
//! its outer ring, so every outer ring starts a new polygon of a MultiPolygon. Z and M values
//! are dropped; multipatches (3D surfaces) are skipped.

use std::io::Cursor;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value as Json, json};
use shapefile::dbase::{self, FieldValue};
use shapefile::{PolygonRing, Shape};

/// The magic number a `.shp` (and `.shx`) file starts with: 9994, big-endian.
const MAGIC: [u8; 4] = [0x00, 0x00, 0x27, 0x0a];

/// Whether the bytes start like a `.shp` file.
pub fn is_shp(bytes: &[u8]) -> bool {
    bytes.len() >= 100 && bytes[..4] == MAGIC
}

/// The files of one shapefile, as bytes: `.shp` is required, the rest optional.
#[derive(Default)]
pub struct Parts<'a> {
    /// The geometry.
    pub shp: &'a [u8],
    /// The attribute table.
    pub dbf: Option<&'a [u8]>,
    /// The coordinate system, as WKT.
    pub prj: Option<&'a [u8]>,
    /// The code page of the `.dbf`'s text, e.g. `UTF-8` or `1252`.
    pub cpg: Option<&'a [u8]>,
}

/// A shapefile as a GeoJSON FeatureCollection.
pub fn to_geojson(parts: &Parts) -> Result<Json> {
    if !is_shp(parts.shp) {
        bail!("not a shapefile (no .shp magic)");
    }
    if let Some(prj) = parts.prj {
        check_prj(&String::from_utf8_lossy(prj))?;
    }
    check_records(parts.shp)?;
    let shapes = shapefile::ShapeReader::new(Cursor::new(parts.shp)).context("reading .shp")?;
    if parts.prj.is_none() {
        let b = &shapes.header().bbox;
        let lonlat = |x: f64, y: f64| (-180.0..=180.0).contains(&x) && (-90.0..=90.0).contains(&y);
        if !(lonlat(b.min.x, b.min.y) && lonlat(b.max.x, b.max.y)) {
            bail!(
                "coordinates are not lon/lat (bounds {:.0},{:.0} to {:.0},{:.0}) and there is no .prj; \
                 mapterm reads lon/lat shapefiles only",
                b.min.x,
                b.min.y,
                b.max.x,
                b.max.y
            );
        }
    }
    let features = match parts.dbf {
        Some(dbf) => {
            let source = Cursor::new(dbf);
            let encoding = parts.cpg.and_then(|c| {
                let label = String::from_utf8_lossy(&c[..c.len().min(1024)]).into_owned();
                dbase::encoding::DynEncoding::from_name(label.trim().trim_start_matches('\u{feff}'))
            });
            let table = match encoding {
                Some(e) => dbase::ReaderBuilder::new().with_encoding(e).build(source),
                None => dbase::Reader::new(source),
            }
            .context("reading .dbf")?;
            let mut reader = shapefile::Reader::new(shapes, table);
            let mut out = Vec::new();
            for item in reader.iter_shapes_and_records() {
                let (shape, record) = item.context("reading a shapefile record")?;
                let props: Map<String, Json> = record.into_iter().map(|(k, v)| (k, value(v))).collect();
                if let Some(f) = feature(shape, props) {
                    out.push(f);
                }
            }
            out
        }
        None => {
            let mut shapes = shapes;
            let mut out = Vec::new();
            for shape in shapes.iter_shapes() {
                if let Some(f) = feature(shape.context("reading a shape")?, Map::new()) {
                    out.push(f);
                }
            }
            out
        }
    };
    Ok(json!({"type": "FeatureCollection", "features": features}))
}

/// Check that every record's part and point counts fit in the record, before the
/// `shapefile` crate reads them: it sizes its buffers from those counts first and checks
/// them against the record length after, so one record claiming two billion parts made it
/// reserve 7 GB (found by fuzzing).
fn check_records(shp: &[u8]) -> Result<()> {
    let be = |p: usize| shp.get(p..p + 4).map(|b| i32::from_be_bytes(b.try_into().unwrap()));
    let le = |p: usize| shp.get(p..p + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap()));
    let mut pos = 100;
    while pos + 8 <= shp.len() {
        // Record header: number, then the content length in 16-bit words, both big-endian.
        let len = be(pos + 4).unwrap_or(0).max(0) as usize * 2;
        let start = pos + 8;
        let end = start.saturating_add(len).min(shp.len());
        let count = |at: usize| le(start + at).filter(|&n| n >= 0).map(|n| n as usize);
        // Bytes the counts need: parts (4 bytes each), points (16), and per point 8 more
        // each for Z and M, which may be present.
        let needed = match le(start) {
            // Polyline and Polygon, plain, Z and M; Multipatch also has a part type per part.
            Some(3 | 5 | 13 | 15 | 23 | 25 | 31) => {
                let (parts, points) = (count(36), count(40));
                let (Some(parts), Some(points)) = (parts, points) else { bail!("shapefile: bad part or point count in a record") };
                let per_part = if le(start) == Some(31) { 8 } else { 4 };
                let needed = 44usize.saturating_add(parts.saturating_mul(per_part)).saturating_add(points.saturating_mul(16));
                // The crate sizes each part from its start index, not from the point count:
                // starts must run from 0 up, within the points (one part claiming 200
                // million points in a record of none reserved 3.5 GB).
                if needed <= end - start {
                    let mut last = 0usize;
                    for i in 0..parts {
                        let p = le(start + 44 + 4 * i).unwrap_or(-1);
                        if p < 0 || (p as usize) < last || p as usize > points || (i == 0 && p != 0) {
                            bail!("shapefile: a record's part {i} starts at point {p} of {points}");
                        }
                        last = p as usize;
                    }
                }
                needed
            }
            // Multipoint, plain, Z and M.
            Some(8 | 18 | 28) => {
                let Some(points) = count(36) else { bail!("shapefile: bad point count in a record") };
                40usize.saturating_add(points.saturating_mul(16))
            }
            _ => 0,
        };
        if needed > end - start {
            bail!("shapefile: a record claims more parts or points ({needed} bytes) than it holds ({} bytes)", end - start);
        }
        pos = start.saturating_add(len);
    }
    Ok(())
}

/// Refuse a projected coordinate system: only lon/lat (`GEOGCS`) draws in the right place.
fn check_prj(wkt: &str) -> Result<()> {
    let t = wkt.trim_start_matches('\u{feff}').trim_start();
    let upper = t.to_ascii_uppercase();
    if upper.starts_with("PROJCS") || upper.starts_with("PROJCRS") || upper.starts_with("PROJECTEDCRS") {
        // PROJCS["NAD83 / UTM zone 18N",...]
        let name = t.split('"').nth(1).unwrap_or("a projected system");
        bail!("the shapefile is in {name} (projected); mapterm reads lon/lat shapefiles only for now");
    }
    Ok(())
}

/// A shape and its attributes as a GeoJSON Feature, or None for a null shape or multipatch.
fn feature(shape: Shape, properties: Map<String, Json>) -> Option<Json> {
    let geometry = geometry(shape)?;
    Some(json!({"type": "Feature", "properties": properties, "geometry": geometry}))
}

fn xy(x: f64, y: f64) -> Json {
    json!([x, y])
}

fn geometry(shape: Shape) -> Option<Json> {
    macro_rules! line {
        ($pl:expr) => {{
            let parts: Vec<Vec<Json>> = $pl.into_inner().into_iter().map(|p| p.into_iter().map(|q| xy(q.x, q.y)).collect()).collect();
            if parts.len() == 1 {
                json!({"type": "LineString", "coordinates": parts[0]})
            } else {
                json!({"type": "MultiLineString", "coordinates": parts})
            }
        }};
    }
    macro_rules! multipoint {
        ($mp:expr) => {{
            let pts: Vec<Json> = $mp.into_inner().into_iter().map(|q| xy(q.x, q.y)).collect();
            json!({"type": "MultiPoint", "coordinates": pts})
        }};
    }
    macro_rules! polygon {
        ($pg:expr) => {{
            // Each outer ring starts a polygon; the holes after it belong to it.
            let mut polys: Vec<Vec<Vec<Json>>> = Vec::new();
            for ring in $pg.into_inner() {
                match ring {
                    PolygonRing::Outer(r) => polys.push(vec![r.into_iter().map(|q| xy(q.x, q.y)).collect()]),
                    PolygonRing::Inner(r) => {
                        let r: Vec<Json> = r.into_iter().map(|q| xy(q.x, q.y)).collect();
                        match polys.last_mut() {
                            Some(p) => p.push(r),
                            // A hole before any outer ring: draw it as a polygon of its own.
                            None => polys.push(vec![r]),
                        }
                    }
                }
            }
            if polys.len() == 1 {
                json!({"type": "Polygon", "coordinates": polys[0]})
            } else {
                json!({"type": "MultiPolygon", "coordinates": polys})
            }
        }};
    }
    Some(match shape {
        Shape::NullShape | Shape::Multipatch(_) => return None,
        Shape::Point(p) => json!({"type": "Point", "coordinates": xy(p.x, p.y)}),
        Shape::PointM(p) => json!({"type": "Point", "coordinates": xy(p.x, p.y)}),
        Shape::PointZ(p) => json!({"type": "Point", "coordinates": xy(p.x, p.y)}),
        Shape::Polyline(l) => line!(l),
        Shape::PolylineM(l) => line!(l),
        Shape::PolylineZ(l) => line!(l),
        Shape::Polygon(p) => polygon!(p),
        Shape::PolygonM(p) => polygon!(p),
        Shape::PolygonZ(p) => polygon!(p),
        Shape::Multipoint(m) => multipoint!(m),
        Shape::MultipointM(m) => multipoint!(m),
        Shape::MultipointZ(m) => multipoint!(m),
    })
}

/// A `.dbf` value as JSON: numbers as numbers, dates as `YYYY-MM-DD`, empty fields as null.
fn value(v: FieldValue) -> Json {
    let num = |f: f64| serde_json::Number::from_f64(f).map_or(Json::Null, Json::Number);
    match v {
        FieldValue::Character(s) => s.map_or(Json::Null, |s| Json::String(s.trim_end().to_string())),
        FieldValue::Memo(s) => Json::String(s),
        FieldValue::Numeric(n) => n.map_or(Json::Null, |n| if n.fract() == 0.0 && n.abs() < 9.0e15 { json!(n as i64) } else { num(n) }),
        FieldValue::Float(n) => n.map_or(Json::Null, |n| num(n as f64)),
        FieldValue::Double(n) | FieldValue::Currency(n) => num(n),
        FieldValue::Integer(n) => json!(n),
        FieldValue::Logical(b) => b.map_or(Json::Null, Json::Bool),
        FieldValue::Date(d) => d.map_or(Json::Null, |d| Json::String(format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day()))),
        FieldValue::DateTime(dt) => {
            let (d, t) = (dt.date(), dt.time());
            Json::String(format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", d.year(), d.month(), d.day(), t.hours(), t.minutes(), t.seconds()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_projected_prj() {
        let e = check_prj(r#"PROJCS["NAD83 / UTM zone 18N",GEOGCS["NAD83"]]"#).unwrap_err();
        assert!(e.to_string().contains("NAD83 / UTM zone 18N"), "{e}");
        check_prj(r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984"]]"#).unwrap();
    }

    #[test]
    fn refuses_records_claiming_more_than_they_hold() {
        // A header, then one 52-byte polyline record that claims two billion parts.
        let mut shp = vec![0u8; 100];
        shp[..4].copy_from_slice(&MAGIC);
        shp[32..36].copy_from_slice(&3i32.to_le_bytes());
        shp.extend_from_slice(&1i32.to_be_bytes());
        shp.extend_from_slice(&26i32.to_be_bytes()); // 26 words = 52 bytes
        let mut rec = vec![0u8; 52];
        rec[..4].copy_from_slice(&3i32.to_le_bytes());
        rec[36..40].copy_from_slice(&2_000_000_000i32.to_le_bytes());
        rec[40..44].copy_from_slice(&1i32.to_le_bytes());
        shp.extend_from_slice(&rec);
        let e = to_geojson(&Parts { shp: &shp, ..Default::default() }).unwrap_err();
        assert!(e.to_string().contains("claims more"), "{e}");
        // Two parts and no points, the second part starting at point 200 million.
        let rec = &mut shp[108..];
        rec[36..40].copy_from_slice(&2i32.to_le_bytes());
        rec[40..44].copy_from_slice(&0i32.to_le_bytes());
        rec[44..48].copy_from_slice(&0i32.to_le_bytes());
        rec[48..52].copy_from_slice(&200_000_000i32.to_le_bytes());
        let e = to_geojson(&Parts { shp: &shp, ..Default::default() }).unwrap_err();
        assert!(e.to_string().contains("part 1 starts at point 200000000"), "{e}");
    }

    #[test]
    fn rejects_non_shapefiles() {
        assert!(!is_shp(b"{\"type\":\"FeatureCollection\"}"));
        assert!(to_geojson(&Parts { shp: &[0u8; 100], ..Default::default() }).is_err());
    }
}
