//! Other vector data as GeoJSON: CSV / TSV with latitude and longitude columns (or a WKT
//! column), GPX (waypoints, tracks, routes), KML (placemarks) and FlatGeobuf
//! ([`crate::fgb`]). Each is turned into a
//! GeoJSON FeatureCollection, so it draws, inspects and clusters like GeoJSON.

use anyhow::{Result, bail};
use serde_json::{Map, Value as Json, json};

/// A data format mapterm reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    GeoJson,
    Csv,
    Gpx,
    Kml,
    FlatGeobuf,
}

impl Format {
    /// From a file name or URL's extension, else by looking at the content.
    pub fn detect(name: &str, bytes: &[u8]) -> Format {
        let lower = name.to_ascii_lowercase();
        let path = lower.split(['?', '#']).next().unwrap_or("");
        match path.rsplit('.').next() {
            Some("csv" | "tsv" | "txt") => return Format::Csv,
            Some("gpx") => return Format::Gpx,
            Some("kml") => return Format::Kml,
            Some("fgb") => return Format::FlatGeobuf,
            Some("geojson" | "json") => return Format::GeoJson,
            _ => {}
        }
        if crate::fgb::is_fgb(bytes) {
            return Format::FlatGeobuf;
        }
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_ascii_lowercase();
        let start = head.trim_start_matches('\u{feff}').trim_start();
        if start.starts_with('{') || start.starts_with('[') {
            Format::GeoJson
        } else if head.contains("<gpx") {
            Format::Gpx
        } else if head.contains("<kml") {
            Format::Kml
        } else {
            Format::Csv
        }
    }
}

/// Read `bytes` (named `name`, for the format) as a GeoJSON document.
pub fn to_geojson(name: &str, bytes: &[u8]) -> Result<Json> {
    match Format::detect(name, bytes) {
        Format::GeoJson => Ok(serde_json::from_slice(bytes)?),
        Format::Csv => csv(&String::from_utf8_lossy(bytes)),
        Format::Gpx => gpx(&String::from_utf8_lossy(bytes)),
        Format::Kml => kml(&String::from_utf8_lossy(bytes)),
        Format::FlatGeobuf => crate::fgb::to_geojson(bytes),
    }
}

fn collection(features: Vec<Json>) -> Json {
    json!({"type": "FeatureCollection", "features": features})
}

// ---- CSV ----

/// Split CSV text into rows (RFC 4180: quoted fields with "" escapes and newlines; `delim`
/// is comma or tab).
fn csv_rows(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let (mut row, mut field, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', true) => quoted = false,
            ('"', false) if field.is_empty() => quoted = true,
            (c, false) if c == delim => row.push(std::mem::take(&mut field)),
            ('\r', false) => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            (c, _) => field.push(c),
        }
    }
    row.push(field);
    if row.iter().any(|f| !f.is_empty()) {
        rows.push(row);
    }
    rows
}

/// A cell as a JSON value: numbers and true/false as such, else text.
fn cell(s: &str) -> Json {
    let t = s.trim();
    if let Ok(n) = t.parse::<f64>()
        && n.is_finite()
        && !t.is_empty()
    {
        return json!(n);
    }
    match t {
        "true" | "TRUE" => json!(true),
        "false" | "FALSE" => json!(false),
        _ => json!(t),
    }
}

/// CSV or TSV: points from latitude and longitude columns (lat/latitude/y and
/// lon/lng/long/longitude/x, any case), or geometry from a WKT column (wkt/geometry/geom).
/// The other columns become properties.
pub fn csv(text: &str) -> Result<Json> {
    let first = text.lines().next().unwrap_or("");
    let delim = if first.matches('\t').count() > first.matches(',').count() { '\t' } else { ',' };
    let rows = csv_rows(text, delim);
    let Some((header, body)) = rows.split_first() else { bail!("empty CSV") };
    let find = |names: &[&str]| header.iter().position(|h| names.contains(&h.trim().to_ascii_lowercase().as_str()));
    let lat = find(&["lat", "latitude", "y"]);
    let lon = find(&["lon", "lng", "long", "longitude", "x"]);
    let wkt = find(&["wkt", "geometry", "geom", "the_geom"]);
    if (lat.is_none() || lon.is_none()) && wkt.is_none() {
        bail!("CSV needs latitude and longitude columns (lat/latitude, lon/lng/longitude) or a WKT column; found: {}", header.join(", "));
    }
    let mut features = Vec::new();
    for row in body {
        let get = |i: Option<usize>| i.and_then(|i| row.get(i)).map(|s| s.trim());
        let geometry = match (get(lat).and_then(|s| s.parse::<f64>().ok()), get(lon).and_then(|s| s.parse::<f64>().ok())) {
            (Some(la), Some(lo)) => json!({"type": "Point", "coordinates": [lo, la]}),
            _ => match get(wkt).and_then(wkt_geometry) {
                Some(g) => g,
                None => continue,
            },
        };
        let mut props = Map::new();
        for (i, h) in header.iter().enumerate() {
            if Some(i) != lat && Some(i) != lon && Some(i) != wkt
                && let Some(v) = row.get(i)
                && !v.trim().is_empty()
            {
                props.insert(h.trim().to_string(), cell(v));
            }
        }
        features.push(json!({"type": "Feature", "properties": props, "geometry": geometry}));
    }
    Ok(collection(features))
}

/// A WKT geometry (POINT, LINESTRING, POLYGON and their MULTI forms) as GeoJSON.
pub fn wkt_geometry(s: &str) -> Option<Json> {
    let s = s.trim();
    let open = s.find('(')?;
    let kind = s[..open].trim().to_ascii_uppercase();
    let kind = kind.trim_end_matches(" Z").trim_end_matches(" M").trim_end_matches(" ZM").trim();
    let body = &s[open..];
    // Nested lists of coordinates, by parenthesis depth.
    fn parse(b: &str) -> Option<(Json, &str)> {
        let b = b.trim_start();
        if let Some(rest) = b.strip_prefix('(') {
            let mut items = Vec::new();
            let mut rest = rest;
            loop {
                let r = rest.trim_start();
                if let Some(after) = r.strip_prefix(')') {
                    return Some((Json::Array(items), after));
                }
                let r = r.strip_prefix(',').unwrap_or(r).trim_start();
                if r.starts_with('(') {
                    let (item, after) = parse(r)?;
                    items.push(item);
                    rest = after;
                } else {
                    let end = r.find([',', ')'])?;
                    let nums: Vec<f64> = r[..end].split_whitespace().filter_map(|n| n.parse().ok()).collect();
                    if nums.len() < 2 {
                        return None;
                    }
                    items.push(json!([nums[0], nums[1]]));
                    rest = &r[end..];
                }
            }
        }
        None
    }
    let (coords, _) = parse(body)?;
    let unwrap1 = |c: &Json| c.as_array().and_then(|a| a.first()).cloned();
    Some(match kind {
        "POINT" => json!({"type": "Point", "coordinates": unwrap1(&coords)?}),
        "LINESTRING" => json!({"type": "LineString", "coordinates": coords}),
        "POLYGON" => json!({"type": "Polygon", "coordinates": coords}),
        // MULTIPOINT((1 2),(3 4)) or MULTIPOINT(1 2, 3 4)
        "MULTIPOINT" => {
            let pts: Vec<Json> = coords.as_array()?.iter().map(|p| if p[0].is_array() { p[0].clone() } else { p.clone() }).collect();
            json!({"type": "MultiPoint", "coordinates": pts})
        }
        "MULTILINESTRING" => json!({"type": "MultiLineString", "coordinates": coords}),
        "MULTIPOLYGON" => json!({"type": "MultiPolygon", "coordinates": coords}),
        _ => return None,
    })
}

// ---- XML (just enough for GPX and KML) ----

#[derive(Debug, Default)]
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Element>,
    text: String,
}

impl Element {
    /// The local name (without a namespace prefix).
    fn is(&self, name: &str) -> bool {
        self.name.rsplit(':').next() == Some(name)
    }

    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.is(name))
    }

    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.is(name))
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn text_of(&self, name: &str) -> Option<String> {
        self.child(name).map(|c| c.text.trim().to_string()).filter(|t| !t.is_empty())
    }

    /// Every descendant (depth first) with this local name.
    fn descendants<'a>(&'a self, name: &'a str, out: &mut Vec<&'a Element>) {
        for c in &self.children {
            if c.is(name) {
                out.push(c);
            }
            c.descendants(name, out);
        }
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let Some(end) = rest[i..].find(';') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let ent = &rest[i + 1..i + end];
        match ent {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if ent.starts_with("#x") => out.extend(u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)),
            _ if ent.starts_with('#') => out.extend(ent[1..].parse().ok().and_then(char::from_u32)),
            _ => out.push_str(&rest[i..=i + end]),
        }
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    out
}

/// Parse XML into a tree: elements, attributes, text and CDATA; comments, declarations and
/// processing instructions are skipped.
fn parse_xml(s: &str) -> Result<Element> {
    let mut stack = vec![Element::default()];
    let mut rest = s;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&unescape(rest));
            }
            break;
        };
        if lt > 0
            && let Some(top) = stack.last_mut()
        {
            top.text.push_str(&unescape(&rest[..lt]));
        }
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map_or("", |e| &r[e + 3..]);
        } else if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let e = r.find("]]>").unwrap_or(r.len());
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&r[..e]);
            }
            rest = r.get(e + 3..).unwrap_or("");
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            rest = rest.find('>').map_or("", |e| &rest[e + 1..]);
        } else if let Some(r) = rest.strip_prefix("</") {
            let e = r.find('>').unwrap_or(r.len());
            if stack.len() > 1 {
                let done = stack.pop().unwrap_or_default();
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(done);
                }
            }
            rest = r.get(e + 1..).unwrap_or("");
        } else {
            let e = rest.find('>').ok_or_else(|| anyhow::anyhow!("unclosed tag"))?;
            let tag = &rest[1..e];
            let self_closing = tag.ends_with('/');
            let tag = tag.trim_end_matches('/');
            let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
            let mut el = Element { name: tag[..name_end].to_string(), ..Element::default() };
            let mut a = &tag[name_end..];
            while let Some(eq) = a.find('=') {
                let key = a[..eq].trim().to_string();
                let v = a[eq + 1..].trim_start();
                let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
                let Some(close) = v[1..].find(q) else { break };
                el.attrs.push((key, unescape(&v[1..1 + close])));
                a = &v[close + 2..];
            }
            rest = &rest[e + 1..];
            if self_closing {
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(el);
                }
            } else {
                stack.push(el);
            }
        }
    }
    while stack.len() > 1 {
        let done = stack.pop().unwrap_or_default();
        if let Some(parent) = stack.last_mut() {
            parent.children.push(done);
        }
    }
    Ok(stack.pop().unwrap_or_default())
}

// ---- GPX ----

/// GPX: waypoints as points (with name, elevation, time...), each track segment and route as
/// a line named after its track or route.
pub fn gpx(text: &str) -> Result<Json> {
    let root = parse_xml(text)?;
    let Some(gpx) = root.child("gpx") else { bail!("not GPX: no <gpx>") };
    let pt = |e: &Element| Some(json!([e.attr("lon")?.trim().parse::<f64>().ok()?, e.attr("lat")?.trim().parse::<f64>().ok()?]));
    let props = |e: &Element, skip: &[&str]| {
        let mut m = Map::new();
        for c in &e.children {
            let k = c.name.rsplit(':').next().unwrap_or(&c.name);
            if !skip.contains(&k) && c.children.is_empty() && !c.text.trim().is_empty() {
                m.insert(k.to_string(), cell(&c.text));
            }
        }
        m
    };
    let mut features = Vec::new();
    for w in gpx.all("wpt") {
        if let Some(p) = pt(w) {
            features.push(json!({"type": "Feature", "properties": props(w, &[]), "geometry": {"type": "Point", "coordinates": p}}));
        }
    }
    for t in gpx.all("trk") {
        let lines: Vec<Json> = t.all("trkseg").map(|s| Json::Array(s.all("trkpt").filter_map(pt).collect())).filter(|l| l.as_array().is_some_and(|a| a.len() >= 2)).collect();
        if !lines.is_empty() {
            features.push(json!({"type": "Feature", "properties": props(t, &["trkseg"]), "geometry": {"type": "MultiLineString", "coordinates": lines}}));
        }
    }
    for r in gpx.all("rte") {
        let line: Vec<Json> = r.all("rtept").filter_map(pt).collect();
        if line.len() >= 2 {
            features.push(json!({"type": "Feature", "properties": props(r, &["rtept"]), "geometry": {"type": "LineString", "coordinates": line}}));
        }
    }
    Ok(collection(features))
}

// ---- KML ----

/// KML `coordinates`: "lon,lat[,alt]" tuples separated by whitespace.
fn kml_coords(e: Option<&Element>) -> Vec<Json> {
    e.map(|c| {
        c.text
            .split_whitespace()
            .filter_map(|t| {
                let mut it = t.split(',').map(|n| n.trim().parse::<f64>());
                Some(json!([it.next()?.ok()?, it.next()?.ok()?]))
            })
            .collect()
    })
    .unwrap_or_default()
}

fn kml_geometry(e: &Element) -> Option<Json> {
    let name = e.name.rsplit(':').next().unwrap_or(&e.name);
    Some(match name {
        "Point" => json!({"type": "Point", "coordinates": kml_coords(e.child("coordinates")).first()?.clone()}),
        "LineString" | "LinearRing" => json!({"type": "LineString", "coordinates": kml_coords(e.child("coordinates"))}),
        "Polygon" => {
            let ring = |b: &Element| kml_coords(b.child("LinearRing").and_then(|r| r.child("coordinates")));
            let mut rings: Vec<Json> = e.all("outerBoundaryIs").map(|b| Json::Array(ring(b))).collect();
            rings.extend(e.all("innerBoundaryIs").map(|b| Json::Array(ring(b))));
            json!({"type": "Polygon", "coordinates": rings})
        }
        "MultiGeometry" => {
            let parts: Vec<Json> = e.children.iter().filter_map(kml_geometry).collect();
            json!({"type": "GeometryCollection", "geometries": parts})
        }
        _ => return None,
    })
}

/// KML: each Placemark with its name, description and ExtendedData as properties, and its
/// Point, LineString, Polygon or MultiGeometry. (Not KMZ: unzip it first.)
pub fn kml(text: &str) -> Result<Json> {
    let root = parse_xml(text)?;
    if root.child("kml").is_none() {
        bail!("not KML: no <kml>");
    }
    let mut marks = Vec::new();
    root.descendants("Placemark", &mut marks);
    let mut features = Vec::new();
    for p in marks {
        let Some(geometry) = p.children.iter().find_map(kml_geometry) else { continue };
        let mut props = Map::new();
        for k in ["name", "description"] {
            if let Some(v) = p.text_of(k) {
                props.insert(k.into(), json!(v));
            }
        }
        if let Some(ext) = p.child("ExtendedData") {
            for d in ext.all("Data") {
                if let (Some(k), Some(v)) = (d.attr("name"), d.text_of("value")) {
                    props.insert(k.to_string(), cell(&v));
                }
            }
            let mut simple = Vec::new();
            ext.descendants("SimpleData", &mut simple);
            for d in simple {
                if let Some(k) = d.attr("name") {
                    props.insert(k.to_string(), cell(&d.text));
                }
            }
        }
        features.push(json!({"type": "Feature", "properties": props, "geometry": geometry}));
    }
    Ok(collection(features))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats() {
        assert_eq!(Format::detect("sites.csv", b""), Format::Csv);
        assert_eq!(Format::detect("https://x/trip.GPX?x=1", b""), Format::Gpx);
        assert_eq!(Format::detect("data", b"\xef\xbb\xbf{\"type\":"), Format::GeoJson);
        assert_eq!(Format::detect("data", b"<?xml version=\"1.0\"?><kml xmlns="), Format::Kml);
        assert_eq!(Format::detect("data", b"<?xml?>\n<gpx version="), Format::Gpx);
        assert_eq!(Format::detect("data", b"name,lat,lon\n"), Format::Csv);
        assert_eq!(Format::detect("data", b"fgb\x03fgb\x00\x10\0\0\0"), Format::FlatGeobuf);
        assert_eq!(Format::detect("x.FGB", b""), Format::FlatGeobuf);
    }

    #[test]
    fn csv_points_and_wkt() {
        let doc = csv("Name,Latitude,Longitude,visits,open\nMonument,38.8895,-77.0353,12,true\n\"Lincoln, Memorial\",38.8893,-77.0502,\"3\",false\nbad,,\n").unwrap();
        let f = doc["features"].as_array().unwrap();
        assert_eq!(f.len(), 2, "rows without coordinates are skipped");
        assert_eq!(f[0]["geometry"]["coordinates"], json!([-77.0353, 38.8895]));
        assert_eq!(f[1]["properties"]["Name"], json!("Lincoln, Memorial"));
        assert_eq!(f[0]["properties"]["visits"], json!(12.0));
        assert_eq!(f[0]["properties"]["open"], json!(true));
        let tsv = csv("name\tlng\tlat\nA\t-77\t38.9\n").unwrap();
        assert_eq!(tsv["features"][0]["geometry"]["coordinates"], json!([-77.0, 38.9]));
        let w = csv("id,WKT\n1,\"LINESTRING (-77 38.9, -76.9 38.95)\"\n2,POINT(1 2)\n3,\"POLYGON ((0 0, 1 0, 1 1, 0 0))\"\n").unwrap();
        let g: Vec<&str> = w["features"].as_array().unwrap().iter().map(|f| f["geometry"]["type"].as_str().unwrap()).collect();
        assert_eq!(g, vec!["LineString", "Point", "Polygon"]);
        assert_eq!(wkt_geometry("MULTIPOINT ((1 2), (3 4))").unwrap()["coordinates"], json!([[1.0, 2.0], [3.0, 4.0]]));
        assert!(csv("a,b\n1,2\n").unwrap_err().to_string().contains("latitude"));
    }

    #[test]
    fn gpx_waypoints_tracks_routes() {
        let doc = gpx(r#"<?xml version="1.0"?><gpx version="1.1" xmlns="http://www.topografix.com/GPX/1/1">
          <wpt lat="38.8895" lon="-77.0353"><name>Monument &amp; Mall</name><ele>12.5</ele></wpt>
          <trk><name>Morning run</name><trkseg><trkpt lat="38.88" lon="-77.05"/><trkpt lat="38.89" lon="-77.04"><ele>3</ele></trkpt></trkseg></trk>
          <rte><name>Plan</name><rtept lat="38.9" lon="-77.0"/><rtept lat="38.91" lon="-77.01"/></rte>
        </gpx>"#).unwrap();
        let f = doc["features"].as_array().unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0]["properties"]["name"], json!("Monument & Mall"));
        assert_eq!(f[0]["properties"]["ele"], json!(12.5));
        assert_eq!(f[1]["geometry"]["type"], json!("MultiLineString"));
        assert_eq!(f[1]["properties"]["name"], json!("Morning run"));
        assert_eq!(f[2]["geometry"]["coordinates"][1], json!([-77.01, 38.91]));
    }

    #[test]
    fn kml_placemarks() {
        let doc = kml(r#"<?xml version="1.0" encoding="UTF-8"?>
        <kml xmlns="http://www.opengis.net/kml/2.2"><Document><Folder>
          <Placemark><name>Pin</name><description><![CDATA[<b>bold</b> note]]></description>
            <ExtendedData><Data name="rating"><value>4.5</value></Data></ExtendedData>
            <Point><coordinates>-77.0353,38.8895,0</coordinates></Point></Placemark>
          <Placemark><name>Area</name><Polygon>
            <outerBoundaryIs><LinearRing><coordinates>-77.05,38.88 -77.02,38.88 -77.02,38.9 -77.05,38.88</coordinates></LinearRing></outerBoundaryIs>
            <innerBoundaryIs><LinearRing><coordinates>-77.04,38.885 -77.03,38.885 -77.03,38.89 -77.04,38.885</coordinates></LinearRing></innerBoundaryIs>
          </Polygon></Placemark>
          <Placemark><name>Both</name><MultiGeometry><Point><coordinates>1,2</coordinates></Point>
            <LineString><coordinates>1,2 3,4</coordinates></LineString></MultiGeometry></Placemark>
          <!-- a comment --><Placemark><name>No geometry</name></Placemark>
        </Folder></Document></kml>"#).unwrap();
        let f = doc["features"].as_array().unwrap();
        assert_eq!(f.len(), 3, "placemarks without geometry are skipped");
        assert_eq!(f[0]["properties"]["description"], json!("<b>bold</b> note"));
        assert_eq!(f[0]["properties"]["rating"], json!(4.5));
        assert_eq!(f[0]["geometry"]["coordinates"], json!([-77.0353, 38.8895]));
        assert_eq!(f[1]["geometry"]["coordinates"].as_array().unwrap().len(), 2, "outer and inner rings");
        assert_eq!(f[2]["geometry"]["type"], json!("GeometryCollection"));
        // It all reads as GeoJSON.
        let (features, _) = crate::geojson::features(&doc).unwrap();
        assert_eq!(features.len(), 4, "the multi-geometry splits into its parts");
    }
}
