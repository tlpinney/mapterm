//! Flight restrictions from the FAA (public, no key): the temporary flight restrictions in
//! force or coming (tfr.faa.gov), and standing restricted airspace from the FAA's ArcGIS
//! layers: the Washington DC SFRA and FRZ, prohibited areas (P-56 over the Mall and White
//! House, P-40 at Camp David...), and the stadiums under the blanket sporting-event
//! restriction (NOTAM 0/0367: 3 NM, up to 3,000 ft, an hour before to an hour after events
//! at venues of 30,000 seats or more; event times aren't published).
//!
//! TFR outlines come from the TFR site's map service, kinds from its list; a TFR's areas,
//! floors and ceilings, times and text from its NOTAM, fetched when it's selected. The
//! standing airspace changes on the 56-day chart cycle, so it's kept on disk for a week.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};

const LIST: &str = "https://tfr.faa.gov/tfrapi/getTfrList";
const SHAPES: &str =
    "https://tfr.faa.gov/geoserver/TFR/ows?service=WFS&version=1.1.0&request=GetFeature&typeName=TFR:V_TFR_LOC&outputFormat=application/json&srsname=EPSG:4326";
const DETAIL: &str = "https://tfr.faa.gov/download/detail_";
const FAA: &str = "https://services6.arcgis.com/ssFJjBXIUyZDrSYZ/arcgis/rest/services";
/// The credit shown while restrictions are on.
pub const CREDIT: &str = "Flight restrictions: FAA";
/// How long the standing airspace is kept.
const STANDING_FRESH: Duration = Duration::from_secs(7 * 24 * 3600);
/// The blanket stadium restriction's radius, nautical miles.
pub const STADIUM_NM: f64 = 3.0;

/// What a restriction is, for its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A stadium under the blanket sporting-event restriction.
    Stadium,
    /// Standing special flight rules (the DC SFRA, FRZ).
    Rules,
    /// A prohibited area (P-56...).
    Prohibited,
    /// A TFR: hazards (wildfires, disasters; 91.137).
    Hazards,
    /// Space launches and reentries (91.143).
    Space,
    /// Air shows and sporting events (91.145, 99.7 stadiums).
    Event,
    /// Other special TFRs.
    Special,
    /// Security (99.7).
    Security,
    /// VIP movements (91.141).
    Vip,
}

impl Kind {
    /// From the TFR list's `type` (or the map service's `LEGAL`).
    pub fn from_tfr(s: &str) -> Kind {
        match s.trim().to_uppercase().as_str() {
            "VIP" => Kind::Vip,
            "SECURITY" => Kind::Security,
            "HAZARDS" => Kind::Hazards,
            "SPACE OPERATIONS" => Kind::Space,
            "AIR SHOWS/SPORTS" => Kind::Event,
            _ => Kind::Special,
        }
    }

    /// "VIP", "security"...
    pub fn label(self) -> &'static str {
        match self {
            Kind::Vip => "VIP movement",
            Kind::Security => "security",
            Kind::Hazards => "hazards (fires, disasters)",
            Kind::Space => "space operations",
            Kind::Event => "air show / sports",
            Kind::Special => "special",
            Kind::Prohibited => "prohibited area",
            Kind::Rules => "special flight rules (SFRA, FRZ)",
            Kind::Stadium => "stadium (events only)",
        }
    }

    /// Its color.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Kind::Vip => (0xc8, 0x4d, 0xe8),
            Kind::Security => (0xff, 0x45, 0x3a),
            Kind::Hazards => (0xff, 0x9f, 0x0a),
            Kind::Space => (0x4f, 0xa3, 0xff),
            Kind::Event => (0xff, 0xd6, 0x0a),
            Kind::Special => (0x40, 0xc8, 0xc8),
            Kind::Prohibited => (0xff, 0x2d, 0x55),
            Kind::Rules => (0x9a, 0xa5, 0xb1),
            Kind::Stadium => (0xb8, 0xa0, 0x40),
        }
    }

    /// Whether it's a TFR (rather than standing airspace).
    pub fn is_tfr(self) -> bool {
        self >= Kind::Hazards
    }
}

/// One restricted area on the map.
#[derive(Clone, Debug, PartialEq)]
pub struct Restriction {
    /// `tfr:6/9862`, `p:P-56`, `sfra:...`, `stadium:PNC Park`.
    pub key: String,
    /// What it is.
    pub kind: Kind,
    /// "Cape Canaveral, FL, Sunday, November 29, 2026 Local", "P-56", "PNC Park".
    pub title: String,
    /// The NOTAM, for a TFR: "6/9862".
    pub notam: Option<String>,
    /// Floor and ceiling as given, for standing airspace: "surface to FL180".
    pub vertical: Option<String>,
    /// GeoJSON Polygon or MultiPolygon, lon/lat.
    pub geometry: Json,
}

/// A NOTAM number as the TFR site writes it: "6/9862".
fn notam(s: &str) -> Option<String> {
    let s = s.trim();
    let (a, b) = s.split_once('/')?;
    (a.len() <= 2 && b.len() <= 5 && a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit())).then(|| s.to_string())
}

/// The TFR list's kinds and titles, by NOTAM.
pub fn parse_list(text: &str) -> Result<HashMap<String, (Kind, String)>> {
    let v: Json = serde_json::from_str(text).context("TFR list")?;
    let list = v.as_array().context("TFR list: not a list")?;
    Ok(list
        .iter()
        .filter_map(|t| {
            let id = notam(t.get("notam_id")?.as_str()?)?;
            let kind = Kind::from_tfr(t.get("type").and_then(Json::as_str).unwrap_or(""));
            Some((id, (kind, t.get("description").and_then(Json::as_str).unwrap_or("").chars().take(160).collect())))
        })
        .collect())
}

/// The TFR outlines in the map service's GeoJSON, as restrictions: kinds and titles from
/// `list` where it has them (it's newer), else the service's own.
pub fn parse_shapes(text: &str, list: &HashMap<String, (Kind, String)>) -> Result<Vec<Restriction>> {
    let v: Json = serde_json::from_str(text).context("TFR shapes")?;
    let feats = v.get("features").and_then(Json::as_array).context("TFR shapes: no features")?;
    let mut out: Vec<Restriction> = Vec::new();
    for f in feats {
        let p = &f["properties"];
        let Some(id) = p["NOTAM_KEY"].as_str().and_then(|k| notam(k.split('-').next()?)) else { continue };
        let Some(g) = f.get("geometry").filter(|g| matches!(g["type"].as_str(), Some("Polygon" | "MultiPolygon"))) else { continue };
        let (kind, title) =
            list.get(&id).cloned().unwrap_or_else(|| (Kind::from_tfr(p["LEGAL"].as_str().unwrap_or("")), p["TITLE"].as_str().unwrap_or("").to_string()));
        // One NOTAM's several areas: kept as parts of one restriction.
        match out.iter_mut().find(|r| r.notam.as_deref() == Some(id.as_str())) {
            Some(r) => {
                let mut polys: Vec<Json> = Vec::new();
                for g in [&r.geometry, g] {
                    match g["type"].as_str() {
                        Some("Polygon") => polys.push(g["coordinates"].clone()),
                        _ => polys.extend(g["coordinates"].as_array().cloned().unwrap_or_default()),
                    }
                }
                r.geometry = json!({"type": "MultiPolygon", "coordinates": polys});
            }
            None => out.push(Restriction { key: format!("tfr:{id}"), kind, title, notam: Some(id), vertical: None, geometry: g.clone() }),
        }
    }
    Ok(out)
}

/// One area of a TFR, from its NOTAM.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Area {
    /// "Area A".
    pub name: String,
    /// The rule: "91.143", "99.7".
    pub rule: String,
    /// "surface to FL180", "1,000 ft AGL to 17,999 ft MSL".
    pub vertical: String,
    /// From when (Unix seconds).
    pub from: Option<f64>,
    /// Until when (Unix seconds).
    pub to: Option<f64>,
}

/// A TFR's NOTAM: its areas, place and text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notam {
    /// Its areas.
    pub areas: Vec<Area>,
    /// "Cape Canaveral, FLORIDA".
    pub place: String,
    /// The operator's name for it: "SpaceX Cygnus NG-25".
    pub name: String,
    /// The NOTAM's text, in the FAA's plain form.
    pub text: String,
}

/// "0 FT HEI" as "surface" or "1,000 ft AGL"; "180 FL ALT" as "FL180".
fn height(val: &str, uom: &str, code: &str) -> String {
    let n: f64 = val.trim().parse().unwrap_or(0.0);
    match (uom.trim(), code.trim()) {
        ("FL", _) => format!("FL{n:.0}"),
        (_, "HEI") if n == 0.0 => "surface".into(),
        (u, c) => {
            let ft = n.round() as i64;
            let s = ft.to_string();
            let grouped = s.as_bytes().rchunks(3).rev().map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(",");
            let unit = if u.is_empty() { "ft".to_string() } else { u.to_lowercase() };
            format!("{grouped} {unit} {}", if c == "HEI" { "AGL" } else { "MSL" })
        }
    }
}

/// A TFR's NOTAM detail XML.
pub fn parse_notam(xml: &str) -> Result<Notam> {
    let root = crate::formats::parse_xml(xml).context("TFR detail")?;
    let mut groups = Vec::new();
    root.descendants("TFRAreaGroup", &mut groups);
    let text_in = |name: &str| {
        let mut v = Vec::new();
        root.descendants(name, &mut v);
        v.first().map(|e| e.text.trim().to_string()).unwrap_or_default()
    };
    let areas = groups
        .iter()
        .filter_map(|g| {
            let a = g.child("aseTFRArea")?;
            let t = |n: &str| a.text_of(n).unwrap_or_default();
            let time = |n: &str| a.text_of(n).and_then(|s| crate::nws::unix_time(&format!("{}Z", s.trim_end_matches('Z'))));
            let mut sched = Vec::new();
            g.descendants("dateEffective", &mut sched);
            let from = time("dateEffective").or_else(|| sched.first().and_then(|e| crate::nws::unix_time(&format!("{}Z", e.text.trim()))));
            let mut exp = Vec::new();
            g.descendants("dateExpire", &mut exp);
            let to = time("dateExpire").or_else(|| exp.first().and_then(|e| crate::nws::unix_time(&format!("{}Z", e.text.trim()))));
            Some(Area {
                name: t("txtName"),
                rule: a.child("codeType").map(|c| c.text.trim().to_string()).unwrap_or_default(),
                vertical: format!(
                    "{} to {}",
                    height(&t("valDistVerLower"), &t("uomDistVerLower"), &t("codeDistVerLower")),
                    height(&t("valDistVerUpper"), &t("uomDistVerUpper"), &t("codeDistVerUpper"))
                ),
                from,
                to,
            })
        })
        .collect();
    let city = text_in("txtNameCity");
    let state = text_in("txtNameUSState");
    let place = [city, state].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
    let text: String = text_in("txtDescrUSNS").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(1200).collect();
    Ok(Notam { areas, place, name: text_in("txtLocalName"), text })
}

async fn get(url: &str) -> Result<String> {
    let resp = crate::source::http_client().get(url).send().await.with_context(|| format!("FAA: {url}"))?;
    if !resp.status().is_success() {
        bail!("FAA: HTTP {}", resp.status());
    }
    resp.text().await.context("FAA")
}

/// The current TFRs: the list, then their outlines.
pub async fn tfrs() -> Result<Vec<Restriction>> {
    let list = parse_list(&get(LIST).await?)?;
    parse_shapes(&get(SHAPES).await?, &list)
}

/// A TFR's NOTAM detail.
pub async fn notam_detail(id: &str) -> Result<Notam> {
    let id = notam(id).context("not a NOTAM number")?;
    parse_notam(&get(&format!("{DETAIL}{}.xml", id.replace('/', "_"))).await?)
}

/// A circle of `nm` nautical miles around (lon, lat), as a GeoJSON Polygon.
pub fn circle(lon: f64, lat: f64, nm: f64) -> Json {
    let r_lat = nm / 60.0;
    let r_lon = r_lat / lat.to_radians().cos().max(0.01);
    let ring: Vec<Json> = (0..=48)
        .map(|i| {
            let a = i as f64 / 48.0 * std::f64::consts::TAU;
            json!([lon + r_lon * a.cos(), lat + r_lat * a.sin()])
        })
        .collect();
    json!({"type": "Polygon", "coordinates": [ring]})
}

/// The standing airspace in the FAA layers' GeoJSON answers: the special flight rules areas
/// (`rules`), prohibited areas (`prohibited`) and stadiums (`stadiums`, points, drawn as the
/// blanket restriction's circles).
pub fn parse_standing(rules: &str, prohibited: &str, stadiums: &str) -> Result<Vec<Restriction>> {
    let feats = |t: &str| -> Result<Vec<Json>> {
        let v: Json = serde_json::from_str(t).context("FAA airspace")?;
        Ok(v.get("features").and_then(Json::as_array).cloned().unwrap_or_default())
    };
    let vertical = |p: &Json| {
        let lo = p["LOWER_VAL"].as_f64().filter(|v| *v > -9000.0)?;
        let hi = p["UPPER_VAL"].as_f64().filter(|v| *v > -9000.0)?;
        let code = |c: &Json| if c.as_str() == Some("SFC") { "HEI" } else { "ALT" };
        Some(format!(
            "{} to {}",
            height(&lo.to_string(), p["LOWER_UOM"].as_str().unwrap_or("FT"), if lo == 0.0 { "HEI" } else { code(&p["LOWER_CODE"]) }),
            height(&hi.to_string(), p["UPPER_UOM"].as_str().unwrap_or("FT"), code(&p["UPPER_CODE"]))
        ))
    };
    let mut out = Vec::new();
    for (i, f) in feats(rules)?.iter().enumerate() {
        let p = &f["properties"];
        let name = p["NAME"].as_str().unwrap_or("");
        if !(name.contains("SPECIAL FLIGHT RULES") || name.starts_with("FLIGHT RESTRICTIONS IN THE VICINITY")) {
            continue;
        }
        let Some(g) = f.get("geometry").filter(|g| g.is_object()) else { continue };
        out.push(Restriction {
            key: format!("rules:{i}"),
            kind: Kind::Rules,
            title: name.to_string(),
            notam: None,
            vertical: vertical(p),
            geometry: g.clone(),
        });
    }
    for f in feats(prohibited)? {
        let p = &f["properties"];
        let Some(g) = f.get("geometry").filter(|g| g.is_object()) else { continue };
        let name = p["NAME"].as_str().unwrap_or("prohibited area");
        let place =
            [p["CITY"].as_str().unwrap_or(""), p["STATE"].as_str().unwrap_or("")].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(", ");
        let title = if place.is_empty() { name.to_string() } else { format!("{name} · {place}") };
        let key = format!("p:{name}");
        match out.iter_mut().find(|r: &&mut Restriction| r.key == key) {
            // P-56 comes in two parts (A and B): one restriction.
            Some(r) => {
                let polys: Vec<Json> = [&r.geometry, g]
                    .iter()
                    .flat_map(|g| match g["type"].as_str() {
                        Some("Polygon") => vec![g["coordinates"].clone()],
                        _ => g["coordinates"].as_array().cloned().unwrap_or_default(),
                    })
                    .collect();
                r.geometry = json!({"type": "MultiPolygon", "coordinates": polys});
            }
            None => out.push(Restriction { key, kind: Kind::Prohibited, title, notam: None, vertical: vertical(p), geometry: g.clone() }),
        }
    }
    for f in feats(stadiums)? {
        let p = &f["properties"];
        let c = &f["geometry"]["coordinates"];
        let (Some(lon), Some(lat)) = (c[0].as_f64(), c[1].as_f64()) else { continue };
        if p["STATUS_CODE"].as_str().is_some_and(|s| s != "Open") {
            continue;
        }
        let name = p["NAME"].as_str().unwrap_or("stadium");
        let place =
            [p["CITY"].as_str().unwrap_or(""), p["STATE"].as_str().unwrap_or("")].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(", ");
        out.push(Restriction {
            key: format!("stadium:{name}"),
            kind: Kind::Stadium,
            title: if place.is_empty() { name.to_string() } else { format!("{name} · {place}") },
            notam: Some("0/0367".into()),
            vertical: Some("surface to 3,000 ft AGL, 3 NM, an hour before to an hour after events".into()),
            geometry: circle(lon, lat, STADIUM_NM),
        });
    }
    Ok(out)
}

fn standing_file() -> std::path::PathBuf {
    crate::source::cache_root().join("tfrs").join("standing.json")
}

/// The standing airspace: from the disk cache when under a week old, else from the FAA's
/// layers (three requests), kept. An older copy will do if the FAA can't be reached.
pub async fn standing() -> Result<Vec<Restriction>> {
    let p = standing_file();
    let read = || -> Option<Vec<Restriction>> {
        let v: Json = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
        parse_standing(&v["rules"].to_string(), &v["prohibited"].to_string(), &v["stadiums"].to_string()).ok().filter(|r| !r.is_empty())
    };
    if std::fs::metadata(&p).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|a| a < STANDING_FRESH)
        && let Some(r) = read()
    {
        return Ok(r);
    }
    let q = |layer: &str, filter: &str, fields: &str| {
        format!("{FAA}/{layer}/FeatureServer/0/query?where={filter}&outFields={fields}&outSR=4326&geometryPrecision=5&f=geojson")
    };
    let fetched = async {
        let rules = get(&q("Boundary_Airspace", "TYPE_CODE%3D%27SATA%27", "NAME,TYPE_CODE,LOWER_VAL,LOWER_UOM,LOWER_CODE,UPPER_VAL,UPPER_UOM,UPPER_CODE")).await?;
        let prohibited = get(&q("Prohibited_Areas", "1%3D1", "NAME,CITY,STATE,LOWER_VAL,LOWER_UOM,LOWER_CODE,UPPER_VAL,UPPER_UOM,UPPER_CODE")).await?;
        let stadiums = get(&q("Stadiums", "1%3D1", "NAME,CITY,STATE,STATUS_CODE")).await?;
        let r = parse_standing(&rules, &prohibited, &stadiums)?;
        if r.is_empty() {
            bail!("FAA airspace: nothing in the answers");
        }
        let doc = json!({"rules": serde_json::from_str::<Json>(&rules)?, "prohibited": serde_json::from_str::<Json>(&prohibited)?, "stadiums": serde_json::from_str::<Json>(&stadiums)?});
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        let tmp = p.with_extension("part");
        if std::fs::write(&tmp, doc.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
        Ok::<_, anyhow::Error>(r)
    }
    .await;
    match fetched {
        Ok(r) => Ok(r),
        Err(e) => read().ok_or(e),
    }
}

/// Whether (lon, lat) is inside a GeoJSON Polygon or MultiPolygon (even-odd over all rings).
pub fn contains(g: &Json, lon: f64, lat: f64) -> bool {
    let polys: Vec<&Json> = match g["type"].as_str() {
        Some("Polygon") => vec![&g["coordinates"]],
        Some("MultiPolygon") => g["coordinates"].as_array().map(|a| a.iter().collect()).unwrap_or_default(),
        _ => Vec::new(),
    };
    polys.into_iter().any(|p| {
        let mut inside = false;
        for ring in p.as_array().into_iter().flatten() {
            let pts: Vec<(f64, f64)> = ring.as_array().into_iter().flatten().filter_map(|c| Some((c[0].as_f64()?, c[1].as_f64()?))).collect();
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                if (a.1 > lat) != (b.1 > lat) && lon < a.0 + (lat - a.1) / (b.1 - a.1) * (b.0 - a.0) {
                    inside = !inside;
                }
            }
        }
        inside
    })
}

/// A rough size of a geometry: its bounding box's area in square degrees.
pub fn extent(g: &Json) -> f64 {
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let mut walk = |c: &Json| {
        fn go(c: &Json, b: &mut (f64, f64, f64, f64)) {
            match c.as_array() {
                Some(a) if a.first().is_some_and(Json::is_number) => {
                    if let (Some(x), Some(y)) = (a[0].as_f64(), a.get(1).and_then(Json::as_f64)) {
                        *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
                    }
                }
                Some(a) => a.iter().for_each(|x| go(x, b)),
                None => {}
            }
        }
        go(c, &mut b);
    };
    walk(&g["coordinates"]);
    if b.0 > b.2 { 0.0 } else { (b.2 - b.0) * (b.3 - b.1) }
}

/// The order restrictions draw in: stadiums and standing airspace under TFRs, the selected
/// one last.
fn order<'a>(all: &'a [Restriction], selected: Option<&str>) -> Vec<&'a Restriction> {
    let mut v: Vec<&Restriction> = all.iter().collect();
    v.sort_by_key(|r| (Some(r.key.as_str()) == selected, r.kind));
    v
}

/// The restrictions as areas to draw: TFRs filled, standing airspace fainter, stadiums an
/// outline.
pub fn shapes(all: &[Restriction], selected: Option<&str>) -> Vec<crate::areas::Shape> {
    order(all, selected)
        .into_iter()
        .map(|r| {
            let mut s = crate::areas::Shape::new(r.kind.rgb(), Some(r.key.as_str()) == selected);
            (s.fill, s.line) = match r.kind {
                Kind::Stadium => (0, 120),
                Kind::Rules => (14, 160),
                _ => (60, 230),
            };
            s.add(&r.geometry);
            s
        })
        .collect()
}

/// The restrictions as GeoJSON for the map (braille, and clicks): `key`, `color`,
/// `selected`.
pub fn features(all: &[Restriction], selected: Option<&str>) -> Json {
    let feats: Vec<Json> = order(all, selected)
        .into_iter()
        .map(|r| {
            let (red, g, b) = r.kind.rgb();
            json!({"type": "Feature", "geometry": r.geometry,
                   "properties": {"key": r.key, "color": format!("#{red:02x}{g:02x}{b:02x}"), "selected": Some(r.key.as_str()) == selected}})
        })
        .collect();
    json!({"type": "FeatureCollection", "features": feats})
}

/// The map layers from GeoJSON source `source`: outlines in each kind's color, and the
/// selected one bright. (No fill: the SFRA covers the whole region.)
pub fn layers(source: &str) -> Result<Vec<crate::StyleLayer>> {
    let line = json!({"id": format!("{source}-line"), "type": "line", "source": source, "paint": {"line-color": ["get", "color"], "line-width": 1}});
    let selected = json!({"id": format!("{source}-selected"), "type": "line", "source": source, "filter": ["==", ["get", "selected"], true],
                          "paint": {"line-color": "#ffffff", "line-width": 2}});
    [line, selected].iter().map(|j| Ok(crate::StyleLayer::from_json(j)?.0)).collect()
}

/// A legend of the kinds on the map, with how many of each.
pub fn legend(all: &[Restriction]) -> crate::legend::Legend {
    use ratatui::style::Color;
    let mut counts: Vec<(Kind, usize)> = Vec::new();
    for r in all {
        match counts.iter_mut().find(|(k, _)| *k == r.kind) {
            Some(c) => c.1 += 1,
            None => counts.push((r.kind, 1)),
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.0));
    counts.into_iter().fold(crate::legend::Legend::new("flight restrictions"), |l, (k, n)| {
        let (r, g, b) = k.rgb();
        l.entry(if k.is_tfr() { "■" } else { "□" }, Color::Rgb(r, g, b), k.label(), Some(n))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_JSON: &str = r#"[
      {"notam_id":"6/9862","state":"FL","type":"SPACE OPERATIONS","description":"Cape Canaveral, FL, Sunday, November 29, 2026 Local"},
      {"notam_id":"6/9661","state":"OH","type":"VIP","description":"Cincinnati, OH"},
      {"notam_id":"bad","type":"VIP"}
    ]"#;
    const SHAPES_JSON: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-80.7,28.8],[-80.3,29.1],[-80.2,28.9],[-80.7,28.8]]]},
       "properties":{"NOTAM_KEY":"6/9862-1-FDC-F","TITLE":"old title","LEGAL":"SPACE OPERATIONS"}},
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-84.6,39.0],[-84.4,39.0],[-84.4,39.2],[-84.6,39.0]]]},
       "properties":{"NOTAM_KEY":"6/9661-1-FDC-F","LEGAL":"VIP"}},
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-84.0,39.0],[-83.8,39.0],[-83.8,39.2],[-84.0,39.0]]]},
       "properties":{"NOTAM_KEY":"6/9661-2-FDC-F","LEGAL":"VIP"}},
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-97.2,26.0],[-97.1,26.0],[-97.1,26.1],[-97.2,26.0]]]},
       "properties":{"NOTAM_KEY":"5/6914-1-FDC-F","TITLE":"South Padre Island, TX","LEGAL":"SECURITY"}}
    ]}"#;
    const NOTAM_XML: &str = r#"<XNOTAM-Update><Group><Add><Not><txtLocalName>261129 FCS SpaceX Cygnus NG-25</txtLocalName>
      <TFRNot><TFRAreaGroup><aseTFRArea><codeType>91.143</codeType><txtName>Area A</txtName>
        <valDistVerLower>0</valDistVerLower><uomDistVerLower>FT</uomDistVerLower><codeDistVerLower>HEI</codeDistVerLower>
        <valDistVerUpper>180</valDistVerUpper><uomDistVerUpper>FL</uomDistVerUpper><codeDistVerUpper>ALT</codeDistVerUpper>
        <dateEffective>2026-11-29T15:30:00</dateEffective><dateExpire>2026-11-29T16:43:00</dateExpire></aseTFRArea></TFRAreaGroup>
      <txtNameCity>Cape Canaveral</txtNameCity><txtNameUSState>FLORIDA</txtNameUSState>
      <txtDescrUSNS>!FDC 6/9862 ZJX FL..AIRSPACE   CAPE CANAVERAL, FL..TEMPORARY FLIGHT RESTRICTIONS.</txtDescrUSNS></TFRNot></Not></Add></Group></XNOTAM-Update>"#;

    #[test]
    fn reads_tfrs_and_their_notams() {
        let list = parse_list(LIST_JSON).unwrap();
        assert_eq!(list.len(), 2, "a bad NOTAM number is left out");
        let r = parse_shapes(SHAPES_JSON, &list).unwrap();
        assert_eq!(r.len(), 3, "one NOTAM's two areas are one restriction");
        assert_eq!(
            (r[0].key.as_str(), r[0].kind, r[0].title.as_str()),
            ("tfr:6/9862", Kind::Space, "Cape Canaveral, FL, Sunday, November 29, 2026 Local"),
            "the list's title wins"
        );
        assert_eq!(r[1].geometry["type"], "MultiPolygon");
        assert_eq!((r[2].kind, r[2].title.as_str()), (Kind::Security, "South Padre Island, TX"), "not in the list: the service's own");
        let n = parse_notam(NOTAM_XML).unwrap();
        assert_eq!(n.areas.len(), 1);
        assert_eq!((n.areas[0].name.as_str(), n.areas[0].rule.as_str(), n.areas[0].vertical.as_str()), ("Area A", "91.143", "surface to FL180"));
        assert_eq!(n.areas[0].from, crate::nws::unix_time("2026-11-29T15:30:00Z"));
        assert_eq!((n.place.as_str(), n.name.as_str()), ("Cape Canaveral, FLORIDA", "261129 FCS SpaceX Cygnus NG-25"));
        assert!(n.text.starts_with("!FDC 6/9862 ZJX FL..AIRSPACE CAPE CANAVERAL"), "spaces collapsed: {}", n.text);
        assert_eq!(height("1000", "FT", "HEI"), "1,000 ft AGL");
        assert_eq!(height("17999", "FT", "ALT"), "17,999 ft MSL");
    }

    #[test]
    fn reads_standing_airspace_and_draws() {
        let rules = r#"{"features":[
          {"geometry":{"type":"Polygon","coordinates":[[[-77.5,38.5],[-76.5,38.5],[-76.5,39.3],[-77.5,38.5]]]},
           "properties":{"NAME":"WASHINGTON, DC METROPOLITAN AREA SPECIAL FLIGHT RULES AREA","LOWER_VAL":0,"LOWER_UOM":"FT","LOWER_CODE":"SFC","UPPER_VAL":180,"UPPER_UOM":"FL"}},
          {"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]},"properties":{"NAME":"WASHINGTON","LOWER_VAL":-9998}}]}"#;
        let prohibited = r#"{"features":[
          {"geometry":{"type":"Polygon","coordinates":[[[-77.04,38.89],[-77.02,38.89],[-77.02,38.90],[-77.04,38.89]]]},"properties":{"NAME":"P-56","CITY":"WASHINGTON","STATE":"DC","LOWER_VAL":0,"UPPER_VAL":18000,"UPPER_UOM":"FT","UPPER_CODE":"MSL"}},
          {"geometry":{"type":"Polygon","coordinates":[[[-77.05,38.89],[-77.04,38.89],[-77.04,38.90],[-77.05,38.89]]]},"properties":{"NAME":"P-56","CITY":"WASHINGTON","STATE":"DC"}}]}"#;
        let stadiums = r#"{"features":[{"geometry":{"type":"Point","coordinates":[-80.006,40.447]},"properties":{"NAME":"PNC Park","CITY":"Pittsburgh","STATE":"PA","STATUS_CODE":"Open"}},
          {"geometry":{"type":"Point","coordinates":[-80.0,40.0]},"properties":{"NAME":"Closed One","STATUS_CODE":"Closed"}}]}"#;
        let s = parse_standing(rules, prohibited, stadiums).unwrap();
        assert_eq!(s.len(), 3, "the SFRA, P-56 (two parts, one area), PNC Park: {s:?}");
        assert_eq!((s[0].kind, s[0].vertical.as_deref()), (Kind::Rules, Some("surface to FL180")));
        assert_eq!((s[1].kind, s[1].title.as_str(), s[1].geometry["type"].as_str()), (Kind::Prohibited, "P-56 · WASHINGTON, DC", Some("MultiPolygon")));
        assert_eq!((s[2].kind, s[2].notam.as_deref()), (Kind::Stadium, Some("0/0367")));
        let ring = s[2].geometry["coordinates"][0].as_array().unwrap();
        assert_eq!(ring.len(), 49);
        assert!((ring[0][0].as_f64().unwrap() - (-80.006 + 3.0 / 60.0 / 40.447_f64.to_radians().cos())).abs() < 1e-6, "3 NM east");
        let mut all = s.clone();
        all.extend(parse_shapes(SHAPES_JSON, &parse_list(LIST_JSON).unwrap()).unwrap());
        let fc = features(&all, Some("p:P-56"));
        let f = fc["features"].as_array().unwrap();
        assert_eq!(f[0]["properties"]["key"], "stadium:PNC Park", "stadiums under everything");
        assert_eq!(f.last().unwrap()["properties"]["key"], "p:P-56", "the selected one last");
        assert_eq!(shapes(&all, None).len(), all.len());
        assert_eq!(layers("tfrs").unwrap().len(), 2);
        assert_eq!(legend(&all).entries.len(), 6);
        assert!(Kind::Vip.is_tfr() && !Kind::Prohibited.is_tfr());
        assert!(contains(&s[0].geometry, -77.0, 38.7) && !contains(&s[0].geometry, -78.0, 38.7));
        assert!(contains(&s[1].geometry, -77.045, 38.893), "the second part of P-56");
        assert!(extent(&s[1].geometry) < extent(&s[0].geometry));
    }
}
