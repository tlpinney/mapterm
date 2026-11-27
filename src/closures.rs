//! Road closures and work zones from county, city and state feeds (public, no key):
//!
//! - Maryland's road closure system (MD iMAP / MDOT SHA), where counties and cities enter
//!   their closures (construction, accidents, parades, special events): active, and planned
//!   for the next week. Montgomery County DOT keeps its own in the same form.
//! - Prince William County, VA: its road closures.
//! - Fairfax County, VA: its emergency management office's road hazards and restrictions
//!   (the service has been failing its queries; it's tried each time all the same).
//! - Work zones in the national WZDx format from Maryland (MDOT SHA), Delaware (DelDOT) and
//!   New Jersey (NJDOT / TRANSCOM); and Virginia's (VDOT) with your own SmarterRoads token
//!   (free registration at smarterroads.vdot.virginia.gov; VDOT's agreement doesn't allow
//!   sharing one), from `--vdot-key-cmd` or `VDOT_SMARTERROADS_TOKEN`.
//!
//! Each source is read into [`Closure`]s: a road, what's happening, how much of it is closed,
//! when, and where (a line along the road, or points). Every 5 minutes.

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};

/// How long ahead planned closures are shown, seconds.
pub const PLANNED_AHEAD: f64 = 7.0 * 86400.0;

/// How much of the road is closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Impact {
    /// Not said.
    #[default]
    Unknown,
    /// Lanes closed, the road open.
    Lanes,
    /// Restricted: emergency vehicles or local access only.
    Restricted,
    /// Closed.
    Closed,
}

impl Impact {
    /// "road closed", "lanes closed"...
    pub fn label(self) -> &'static str {
        match self {
            Impact::Closed => "road closed",
            Impact::Restricted => "restricted access",
            Impact::Lanes => "lanes closed",
            Impact::Unknown => "work zone / other",
        }
    }

    /// Its color.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Impact::Closed => (0xff, 0x45, 0x3a),
            Impact::Restricted => (0xff, 0x7a, 0x32),
            Impact::Lanes => (0xff, 0xb4, 0x46),
            Impact::Unknown => (0xff, 0xd6, 0x0a),
        }
    }
}

/// Planned closures' color.
pub const PLANNED: (u8, u8, u8) = (0x9a, 0x7c, 0xff);

/// One closure or work zone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Closure {
    /// The source's id and the closure's own.
    pub key: String,
    /// Who it's from: "Maryland road closures", "Prince William County"...
    pub source: &'static str,
    /// The road: "STREET RD", "I-280".
    pub road: String,
    /// Between or at: "MD 165 (PYLESVILLE RD) to HIGHLAND RD".
    pub at: String,
    /// What: "Parade", "Construction", "Accident", "work zone".
    pub event: String,
    /// How much is closed.
    pub impact: Impact,
    /// The direction, where given.
    pub direction: String,
    /// The text given: comments, description.
    pub text: String,
    /// The county or city.
    pub jurisdiction: String,
    /// From when (Unix seconds).
    pub start: Option<f64>,
    /// Until when (Unix seconds).
    pub end: Option<f64>,
    /// GeoJSON geometry, lon/lat: a line along the road, or points.
    pub geometry: Json,
}

impl Closure {
    /// Whether it starts after `now`.
    pub fn planned(&self, now: f64) -> bool {
        self.start.is_some_and(|s| s > now)
    }

    /// Whether to show it at `now`: not ended, and if planned, within a week.
    pub fn shown(&self, now: f64) -> bool {
        self.end.is_none_or(|e| e > now) && self.start.is_none_or(|s| s <= now + PLANNED_AHEAD)
    }
}

/// How much is closed, from a closure type and the text: "Closed" or "all lanes closed" is
/// closed; emergency-only or limited access is restricted; a lane or shoulder is lanes.
pub fn impact_of(kind: &str, text: &str) -> Impact {
    let k = kind.to_lowercase();
    let t = text.to_lowercase();
    if k == "closed" || k.contains("road closed") || t.contains("all lanes closed") || t.contains("road closed") || t.contains("to through traffic") {
        Impact::Closed
    } else if k.contains("emergency") || k.contains("limited") || k.contains("local") || t.contains("local traffic only") {
        Impact::Restricted
    } else if k.contains("lane") || t.contains("lane closed") || t.contains("lanes closed") || t.contains("shoulder closed") {
        Impact::Lanes
    } else {
        Impact::Unknown
    }
}

fn s(p: &Json, k: &str) -> String {
    p.get(k).and_then(Json::as_str).map(|s| s.trim().chars().take(600).collect()).unwrap_or_default()
}

fn ms(p: &Json, k: &str) -> Option<f64> {
    p.get(k).and_then(Json::as_f64).filter(|n| *n > 0.0).map(|n| n / 1000.0)
}

/// A GeoJSON geometry worth drawing: points or lines, lon/lat in range.
fn usable(g: &Json) -> bool {
    matches!(g["type"].as_str(), Some("Point" | "MultiPoint" | "LineString" | "MultiLineString")) && !g["coordinates"].is_null()
}

/// An ArcGIS JSON geometry (`outSR=4326`) as GeoJSON: a point, or polyline paths.
fn esri_geometry(g: &Json) -> Option<Json> {
    if let (Some(x), Some(y)) = (g.get("x").and_then(Json::as_f64), g.get("y").and_then(Json::as_f64)) {
        return Some(json!({"type": "Point", "coordinates": [x, y]}));
    }
    let paths = g.get("paths")?.as_array()?;
    Some(json!({"type": "MultiLineString", "coordinates": paths}))
}

/// The closures in Maryland's road closure schema (GeoJSON from the SHA's and Montgomery
/// County's layers): `RoadName`, `CrossStreet1/2`, `ClosureType`, `typeSummary`,
/// `closureSummary`, `Comments`, `Jurisdiction`, `ClosureStart/End`.
pub fn parse_maryland(text: &str, source: &'static str, id: &str) -> Result<Vec<Closure>> {
    let v: Json = serde_json::from_str(text).context("road closures")?;
    if let Some(e) = v.get("error") {
        bail!("{source}: {}", e["message"].as_str().unwrap_or("the service answered with an error"));
    }
    Ok(v["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let p = &f["properties"];
            let g = f.get("geometry").filter(|g| usable(g))?;
            let kind = s(p, "ClosureType");
            let summary = s(p, "closureSummary");
            let cross = [s(p, "CrossStreet1"), s(p, "CrossStreet2")].into_iter().filter(|c| !c.is_empty()).collect::<Vec<_>>();
            Some(Closure {
                key: format!("{id}:{}", p.get("RC_GUID").and_then(Json::as_str).map(str::to_string).or_else(|| p.get("OBJECTID").map(|o| o.to_string()))?),
                source,
                road: s(p, "RoadName"),
                at: match cross.len() {
                    2 => format!("{} to {}", cross[0], cross[1]),
                    1 => format!("at {}", cross[0]),
                    _ => String::new(),
                },
                event: s(p, "typeSummary"),
                impact: impact_of(&kind, &summary),
                direction: String::new(),
                text: [summary, s(p, "Comments")].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" "),
                jurisdiction: s(p, "Jurisdiction"),
                start: ms(p, "ClosureStart"),
                end: ms(p, "ClosureEnd"),
                geometry: g.clone(),
            })
        })
        .collect())
}

/// Prince William County's closures (ArcGIS JSON): incident points (`incidenttypename`,
/// `location`, `routeclosedtime`, `allcomments`) and road segments (`eventtype`,
/// `description`, `startdate`, `enddate`).
pub fn parse_prince_william(text: &str, segments: bool) -> Result<Vec<Closure>> {
    let v: Json = serde_json::from_str(text).context("Prince William County")?;
    if let Some(e) = v.get("error") {
        bail!("Prince William County: {}", e["message"].as_str().unwrap_or("the service answered with an error"));
    }
    Ok(v["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let p = &f["attributes"];
            let g = esri_geometry(&f["geometry"])?;
            let id = p.get("GlobalID").and_then(Json::as_str).map(str::to_string).or_else(|| p.get("OBJECTID").map(|o| o.to_string()))?;
            Some(if segments {
                let text = s(p, "description");
                Closure {
                    key: format!("pwc-s:{id}"),
                    source: "Prince William County",
                    event: s(p, "eventtype"),
                    impact: impact_of("", &text),
                    text,
                    jurisdiction: "Prince William".into(),
                    start: ms(p, "startdate"),
                    end: ms(p, "enddate"),
                    geometry: g,
                    ..Closure::default()
                }
            } else {
                let text = s(p, "allcomments");
                Closure {
                    key: format!("pwc-p:{id}"),
                    source: "Prince William County",
                    road: s(p, "location"),
                    event: s(p, "incidenttypename"),
                    impact: if p.get("routeclosedtime").is_some_and(|t| !t.is_null()) { Impact::Closed } else { impact_of("", &text) },
                    text,
                    jurisdiction: "Prince William".into(),
                    start: ms(p, "routeclosedtime").or_else(|| ms(p, "incidentdate")),
                    geometry: g,
                    ..Closure::default()
                }
            })
        })
        .collect())
}

/// Fairfax County's road hazards (`event_type`, `status_desc`, street, `location_comments`,
/// `IS_OPEN`, `event_close_date`) or restrictions (`restrictionname`, `restrictiontype`,
/// `restrictionremarks`, `street`, `direction`), ArcGIS JSON points.
pub fn parse_fairfax(text: &str, restrictions: bool) -> Result<Vec<Closure>> {
    let v: Json = serde_json::from_str(text).context("Fairfax County")?;
    if let Some(e) = v.get("error") {
        bail!("Fairfax County: {}", e["message"].as_str().unwrap_or("the service answered with an error"));
    }
    Ok(v["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let p = &f["attributes"];
            let g = esri_geometry(&f["geometry"])?;
            let id = p.get("OBJECTID").or_else(|| p.get("objectid")).map(|o| o.to_string())?;
            Some(if restrictions {
                let kind = s(p, "restrictiontype");
                let text = [s(p, "restrictionname"), s(p, "restrictionremarks")].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(": ");
                Closure {
                    key: format!("ffx-r:{id}"),
                    source: "Fairfax County",
                    road: s(p, "street"),
                    event: if kind.is_empty() { "restriction".into() } else { kind.clone() },
                    impact: impact_of(&kind, &text),
                    direction: s(p, "direction"),
                    text,
                    jurisdiction: "Fairfax".into(),
                    geometry: g,
                    ..Closure::default()
                }
            } else {
                let street = ["street_prefix", "street_name", "street_type", "street_suffix"]
                    .iter()
                    .map(|k| s(p, k))
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                let cross = [s(p, "xstreet1"), s(p, "xstreet2")].into_iter().filter(|c| !c.is_empty()).collect::<Vec<_>>();
                let open = p.get("IS_OPEN").and_then(|o| o.as_str().map(|s| s.eq_ignore_ascii_case("y") || s == "1").or(o.as_i64().map(|n| n == 1)));
                let text = [s(p, "status_desc"), s(p, "location_comments")].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(": ");
                Closure {
                    key: format!("ffx-h:{id}"),
                    source: "Fairfax County",
                    road: street,
                    at: cross.join(" and "),
                    event: s(p, "event_type"),
                    impact: if open == Some(false) { Impact::Closed } else { impact_of("", &text) },
                    text,
                    jurisdiction: "Fairfax".into(),
                    end: ms(p, "event_close_date"),
                    geometry: g,
                    ..Closure::default()
                }
            })
        })
        .collect())
}

/// The road events in a WZDx (or CWZ) feed: `core_details` (event type, road names,
/// direction, description), start and end dates, `vehicle_impact` (or the description when
/// that's unknown or contradicts it), cross streets.
pub fn parse_wzdx(text: &str, source: &'static str, id: &str) -> Result<Vec<Closure>> {
    let v: Json = serde_json::from_str(text).context("WZDx")?;
    let feats = v["features"].as_array().context("WZDx: no features")?;
    Ok(feats
        .iter()
        .filter_map(|f| {
            let p = &f["properties"];
            let c = &p["core_details"];
            let g = f.get("geometry").filter(|g| usable(g))?;
            let roads: Vec<String> = c["road_names"].as_array().into_iter().flatten().filter_map(Json::as_str).map(str::to_string).collect();
            let desc = s(c, "description");
            let from_text = impact_of("", &desc);
            let impact = match p["vehicle_impact"].as_str().unwrap_or("") {
                "all-lanes-closed" => Impact::Closed,
                "some-lanes-closed" | "some-lanes-closed-merge-left" | "some-lanes-closed-merge-right" | "alternating-one-way" => Impact::Lanes,
                // "all-lanes-open" and "unknown": what the description says, if anything.
                _ => from_text,
            };
            let cross = [s(p, "beginning_cross_street"), s(p, "ending_cross_street")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>();
            Some(Closure {
                key: format!("{id}:{}", f.get("id").and_then(Json::as_str).map(str::to_string).unwrap_or_else(|| desc.chars().take(40).collect())),
                source,
                road: roads.first().cloned().unwrap_or_default(),
                at: if cross.is_empty() { roads.get(1..).map(|r| r.join(", ")).unwrap_or_default() } else { cross.join(" to ") },
                event: s(c, "event_type").replace('-', " "),
                impact,
                direction: s(c, "direction"),
                text: desc,
                start: p["start_date"].as_str().and_then(crate::nws::unix_time),
                end: p["end_date"].as_str().and_then(crate::nws::unix_time),
                geometry: g.clone(),
                ..Closure::default()
            })
        })
        .collect())
}

/// A GML position, "lat lon", as [lon, lat].
fn gml_pos(v: &Json) -> Option<[f64; 2]> {
    let pos = v.pointer("/gml:Point/gml:pos")?.as_str()?;
    let mut it = pos.split_whitespace().filter_map(|n| n.parse::<f64>().ok());
    let (lat, lon) = (it.next()?, it.next()?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some([lon, lat])
}

/// Virginia's road closures (VDOT's VaTraffic events from SmarterRoads, JSON keyed by event
/// id): type, category, location, direction, jurisdiction, `road_closure`, lane conditions,
/// times, the 511 text, and start, mid and end points (a line from start to end).
pub fn parse_vdot(text: &str) -> Result<Vec<Closure>> {
    let v: Json = serde_json::from_str(text).context("VDOT road closures")?;
    let events = v.as_object().context("VDOT road closures: not an object of events")?;
    let o = |e: &Json, k: &str| s(e, &format!("orci:{k}"));
    let t = |e: &Json, k: &str| e.get(format!("orci:{k}")).and_then(Json::as_str).and_then(crate::nws::unix_time);
    Ok(events
        .iter()
        .filter_map(|(id, e)| {
            let pts: Vec<[f64; 2]> = ["start_point", "end_point"].iter().filter_map(|k| gml_pos(e.get(format!("orci:{k}"))?)).collect();
            let geometry = match pts.as_slice() {
                [a, b] if a != b => json!({"type": "LineString", "coordinates": [a, b]}),
                [a, ..] => json!({"type": "Point", "coordinates": a}),
                [] => json!({"type": "Point", "coordinates": gml_pos(e.get("orci:mid_point")?)?}),
            };
            if o(e, "status").eq_ignore_ascii_case("closed") {
                // The event itself is over.
                return None;
            }
            let lanes = o(e, "lanes_condition");
            let impact = if e.get("road_closure").and_then(Json::as_bool) == Some(true) {
                Impact::Closed
            } else if lanes.split('|').any(|c| c == "closed") {
                Impact::Lanes
            } else {
                impact_of("", &o(e, "template_511_text"))
            };
            let sub = o(e, "event_subcategory");
            Some(Closure {
                key: format!("va:{}", id.chars().take(60).collect::<String>()),
                source: "VDOT road closures (SmarterRoads)",
                road: o(e, "special_facility"),
                event: [o(e, "type_event"), sub].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "),
                impact,
                direction: o(e, "travel_direction"),
                text: o(e, "template_511_text"),
                jurisdiction: o(e, "containing_jurisdiction"),
                start: t(e, "segment_start_time").or_else(|| t(e, "scheduled_start_time")),
                end: t(e, "segment_stop_time").or_else(|| t(e, "scheduled_stop_time")),
                geometry,
                ..Closure::default()
            })
        })
        .collect())
}

/// Where a source is and how to read it.
#[derive(Clone, Copy, Debug)]
pub enum Reader {
    /// Maryland's schema, GeoJSON.
    Maryland,
    /// Prince William County, ArcGIS JSON (`true`: segments).
    PrinceWilliam(bool),
    /// Fairfax County, ArcGIS JSON (`true`: restrictions).
    Fairfax(bool),
    /// A WZDx or CWZ feed.
    Wzdx,
    /// VDOT's road closures.
    Vdot,
}

/// One feed.
pub struct Feed {
    /// A short id, the start of its closures' keys.
    pub id: &'static str,
    /// Who it's from, for the panel.
    pub source: &'static str,
    /// The URL to ask.
    pub url: &'static str,
    /// How to read it.
    pub reader: Reader,
    /// It needs a VDOT SmarterRoads token (sent as the `token` query parameter).
    pub vdot: bool,
}

/// The feeds mapterm reads.
pub const FEEDS: [Feed; 12] = [
    Feed {
        id: "md",
        source: "Maryland road closures (MD iMAP / MDOT SHA)",
        url: "https://mdgeodata.md.gov/appdata/rest/services/SHA_RoadClosure/RoadClosureActive/MapServer/1/query?where=1%3D1&outFields=*&outSR=4326&f=geojson",
        reader: Reader::Maryland,
        vdot: false,
    },
    Feed {
        id: "md-p",
        source: "Maryland road closures (MD iMAP / MDOT SHA)",
        url: "https://mdgeodata.md.gov/appdata/rest/services/SHA_RoadClosure/RoadClosurePlanned/MapServer/1/query?where=1%3D1&outFields=*&outSR=4326&f=geojson",
        reader: Reader::Maryland,
        vdot: false,
    },
    Feed {
        id: "moco",
        source: "Montgomery County DOT",
        url: "https://www7.montgomerycountymd.gov/arcgis/rest/services/DOT/Road_Closure_lines/MapServer/0/query?where=1%3D1&outFields=*&outSR=4326&f=geojson",
        reader: Reader::Maryland,
        vdot: false,
    },
    Feed {
        id: "pwc-p",
        source: "Prince William County",
        url: "https://gisweb.pwcva.gov/arcgis/rest/services/RoadClosures/RoadClosures/FeatureServer/0/query?where=1%3D1&outFields=*&outSR=4326&f=json",
        reader: Reader::PrinceWilliam(false),
        vdot: false,
    },
    Feed {
        id: "pwc-s",
        source: "Prince William County",
        url: "https://gisweb.pwcva.gov/arcgis/rest/services/RoadClosures/RoadClosures/FeatureServer/2/query?where=1%3D1&outFields=*&outSR=4326&f=json",
        reader: Reader::PrinceWilliam(true),
        vdot: false,
    },
    Feed {
        id: "ffx-h",
        source: "Fairfax County",
        url: "https://www.fairfaxcounty.gov/gispubsf1/rest/services/DEMS/RoadHazardsPublic/MapServer/0/query?where=1%3D1&outFields=*&outSR=4326&f=json",
        reader: Reader::Fairfax(false),
        vdot: false,
    },
    Feed {
        id: "ffx-r",
        source: "Fairfax County",
        url: "https://www.fairfaxcounty.gov/gispubsf1/rest/services/DEMS/RoadHazardsPublic/MapServer/1/query?where=1%3D1&outFields=*&outSR=4326&f=json",
        reader: Reader::Fairfax(true),
        vdot: false,
    },
    Feed { id: "wz-md", source: "MDOT SHA work zones (WZDx)", url: "https://filter.ritis.org/wzdx_v4.1/mdot.geojson", reader: Reader::Wzdx, vdot: false },
    Feed { id: "wz-de", source: "DelDOT work zones (WZDx)", url: "https://wzdx.e-dot.com/del_dot_feed_wzdx_v4.1.geojson", reader: Reader::Wzdx, vdot: false },
    Feed { id: "wz-nj", source: "NJDOT / TRANSCOM work zones (WZDx)", url: "https://smartworkzones.njit.edu/nj/wzdx", reader: Reader::Wzdx, vdot: false },
    Feed {
        id: "wz-va",
        source: "VDOT work zones (WZDx, SmarterRoads)",
        url: "https://data.511-atis-ttrip-prod.iteriscloud.com/smarterRoads/workzone/workZoneJSON/current/workZone.json",
        reader: Reader::Wzdx,
        vdot: true,
    },
    Feed {
        id: "va",
        source: "VDOT road closures (SmarterRoads)",
        url: "https://data.511-atis-ttrip-prod.iteriscloud.com/smarterRoads/other/vDOTRoadClosures/current/RoadClosures_current.json",
        reader: Reader::Vdot,
        vdot: true,
    },
];

/// The credit shown while closures are on.
pub const CREDIT: &str = "Road closures: MD iMAP / MDOT SHA, Montgomery Co., Prince William Co., Fairfax Co., DelDOT, NJDOT / TRANSCOM, VDOT";

/// Read a feed's answer.
pub fn parse(feed: &Feed, text: &str) -> Result<Vec<Closure>> {
    match feed.reader {
        Reader::Maryland => parse_maryland(text, feed.source, feed.id),
        Reader::PrinceWilliam(seg) => parse_prince_william(text, seg),
        Reader::Fairfax(r) => parse_fairfax(text, r),
        Reader::Wzdx => parse_wzdx(text, feed.source, feed.id),
        Reader::Vdot => parse_vdot(text),
    }
}

/// Ask a feed; a VDOT one with `vdot_token` (None: it's skipped, with nothing). The token
/// never appears in an error: reqwest's errors carry the URL, so it's taken off.
pub async fn fetch(feed: &Feed, vdot_token: Option<&str>) -> Result<Vec<Closure>> {
    let url = if feed.vdot {
        let Some(t) = vdot_token else { return Ok(Vec::new()) };
        // Encoded: the token has characters such as $ and /.
        reqwest::Url::parse_with_params(feed.url, &[("token", t)]).context("the VDOT feed URL")?
    } else {
        reqwest::Url::parse(feed.url).context("a feed URL")?
    };
    let resp = crate::source::http_client().get(url).send().await.map_err(reqwest::Error::without_url).with_context(|| feed.source.to_string())?;
    if !resp.status().is_success() {
        bail!("{}: HTTP {}", feed.source, resp.status());
    }
    parse(feed, &resp.text().await.map_err(reqwest::Error::without_url).with_context(|| feed.source.to_string())?)
}

/// The color of a closure at `now`: planned ones apart.
pub fn rgb(c: &Closure, now: f64) -> (u8, u8, u8) {
    if c.planned(now) { PLANNED } else { c.impact.rgb() }
}

/// The closures shown at `now` as GeoJSON (`key`, `color`, `glyph`, `selected`): the worst
/// drawn last, the selected one last of all; lines as lines, points with a glyph.
pub fn features(all: &[Closure], now: f64, selected: Option<&str>) -> Json {
    let mut v: Vec<&Closure> = all.iter().filter(|c| c.shown(now)).collect();
    v.sort_by_key(|c| (Some(c.key.as_str()) == selected, !c.planned(now), c.impact));
    let feats: Vec<Json> = v
        .into_iter()
        .map(|c| {
            let (r, g, b) = if Some(c.key.as_str()) == selected { (0xff, 0xff, 0xff) } else { rgb(c, now) };
            let glyph = match c.impact {
                Impact::Closed => "⛔",
                _ if c.planned(now) => "◇",
                _ => "▲",
            };
            json!({"type": "Feature", "geometry": c.geometry,
                   "properties": {"key": c.key, "color": format!("#{r:02x}{g:02x}{b:02x}"), "glyph": glyph}})
        })
        .collect();
    json!({"type": "FeatureCollection", "features": feats})
}

/// The closures shown at `now` drawn along their roads as image tiles: lines in their colors
/// (points stay symbols), the worse on top, the selected one highlighted.
pub fn shapes(all: &[Closure], now: f64, selected: Option<&str>) -> Vec<crate::areas::Shape> {
    let mut v: Vec<&Closure> = all.iter().filter(|c| c.shown(now) && matches!(c.geometry["type"].as_str(), Some("LineString" | "MultiLineString"))).collect();
    v.sort_by_key(|c| (Some(c.key.as_str()) == selected, !c.planned(now), c.impact));
    v.into_iter()
        .map(|c| {
            let mut s = crate::areas::Shape::lines(rgb(c, now), if c.impact == Impact::Closed { 4.0 } else { 3.0 }, Some(c.key.as_str()) == selected);
            s.add(&c.geometry);
            s
        })
        .collect()
}

/// The map layers from GeoJSON source `source`: lines along the roads, and a glyph at points.
pub fn layers(source: &str) -> Result<Vec<crate::StyleLayer>> {
    let line = json!({"id": format!("{source}-line"), "type": "line", "source": source,
                      "filter": ["!=", ["geometry-type"], "Point"],
                      "paint": {"line-color": ["get", "color"], "line-width": 2}});
    let mut out: Vec<crate::StyleLayer> = vec![crate::StyleLayer::from_json(&line)?.0];
    out.push(
        crate::StyleLayer::symbol(format!("{source}-point"), source)
            .filter(json!(["==", ["geometry-type"], "Point"]))
            .glyph(json!(["get", "glyph"]))
            .paint("icon-color", json!(["get", "color"])),
    );
    Ok(out)
}

/// A legend: closures in effect by impact, and the planned ones, with how many of each.
pub fn legend(all: &[Closure], now: f64) -> crate::legend::Legend {
    use ratatui::style::Color;
    let shown: Vec<&Closure> = all.iter().filter(|c| c.shown(now)).collect();
    let mut l = crate::legend::Legend::new("road closures");
    for i in [Impact::Closed, Impact::Restricted, Impact::Lanes, Impact::Unknown] {
        let n = shown.iter().filter(|c| !c.planned(now) && c.impact == i).count();
        let (r, g, b) = i.rgb();
        l = l.entry("━", Color::Rgb(r, g, b), i.label(), Some(n));
    }
    let (r, g, b) = PLANNED;
    l.entry("━", Color::Rgb(r, g, b), "planned (next 7 days)", Some(shown.iter().filter(|c| c.planned(now)).count()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","geometry":{"type":"LineString","coordinates":[[-76.38,39.68],[-76.37,39.69]]},
       "properties":{"OBJECTID":1,"RC_GUID":"{155C}","ClosureStart":1796504414000,"ClosureEnd":1796515214000,"ClosureType":"Closed",
        "Comments":"Closed 4 to 7 p.m. for the Mason Dixon Country Tractor Parade.","RoadName":"STREET RD","CrossStreet1":"MD 165","CrossStreet2":"HIGHLAND RD",
        "Jurisdiction":"Harford","closureSummary":"STREET RD: Closed, all directions.  All lanes closed.","typeSummary":"Parade"}},
      {"type":"Feature","geometry":{"type":"LineString","coordinates":[[-76.6,39.3],[-76.59,39.31]]},
       "properties":{"OBJECTID":2,"ClosureStart":1795000000000,"ClosureEnd":1797000000000,"ClosureType":"Emergency Vehicle Only Access",
        "RoadName":"YORK RD","Jurisdiction":"Baltimore County","closureSummary":"YORK RD: emergency vehicles only","typeSummary":"Accident"}},
      {"type":"Feature","geometry":null,"properties":{"OBJECTID":3}}
    ]}"#;
    const WZDX: &str = r#"{"feed_info":{},"features":[
      {"id":"a1","type":"Feature","geometry":{"type":"MultiPoint","coordinates":[[-74.4,40.86],[-74.3,40.85]]},
       "properties":{"core_details":{"event_type":"work-zone","road_names":["I-280","West of Exit 1W"],"direction":"eastbound",
         "description":"Roadwork on I-280 Eastbound, left lane closed"},
         "start_date":"2026-11-25T04:09:09-05:00","end_date":"2026-12-01T05:00:00-05:00","vehicle_impact":"all-lanes-open"}},
      {"id":"a2","type":"Feature","geometry":{"type":"LineString","coordinates":[[-75.75,39.68],[-75.75,39.69]]},
       "properties":{"core_details":{"event_type":"work-zone","road_names":["SR 896NB"],"direction":"northbound"},
         "start_date":"2026-11-27T00:32:00Z","end_date":"2026-11-27T02:32:00Z","vehicle_impact":"all-lanes-closed"}}
    ]}"#;

    #[test]
    fn reads_maryland_and_wzdx() {
        let m = parse_maryland(MD, "Maryland road closures", "md").unwrap();
        assert_eq!(m.len(), 2, "no geometry, left out");
        assert_eq!(
            (m[0].key.as_str(), m[0].road.as_str(), m[0].at.as_str(), m[0].event.as_str(), m[0].impact),
            ("md:{155C}", "STREET RD", "MD 165 to HIGHLAND RD", "Parade", Impact::Closed)
        );
        assert!(m[0].text.contains("Tractor Parade"));
        assert_eq!((m[1].impact, m[1].key.as_str()), (Impact::Restricted, "md:2"), "no GUID: the object id");
        let w = parse_wzdx(WZDX, "NJ", "wz-nj").unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(
            (w[0].road.as_str(), w[0].at.as_str(), w[0].impact, w[0].event.as_str()),
            ("I-280", "West of Exit 1W", Impact::Lanes, "work zone"),
            "'all lanes open' but the text says a lane is closed"
        );
        assert_eq!(w[1].impact, Impact::Closed);
        assert_eq!(w[0].end, crate::nws::unix_time("2026-12-01T10:00:00Z"));
    }

    #[test]
    fn reads_the_virginia_counties() {
        let pwc = r#"{"features":[{"attributes":{"OBJECTID":1,"GlobalID":"{G1}","incidenttypename":"Water Main Break","location":"DALE BLVD / MINNIEVILLE RD",
            "routeclosedtime":1795740000000,"allcomments":"Road closed both ways"},"geometry":{"x":-77.33,"y":38.64}}]}"#;
        let p = parse_prince_william(pwc, false).unwrap();
        assert_eq!(
            (p[0].road.as_str(), p[0].event.as_str(), p[0].impact, p[0].geometry["type"].as_str()),
            ("DALE BLVD / MINNIEVILLE RD", "Water Main Break", Impact::Closed, Some("Point"))
        );
        let seg = r#"{"features":[{"attributes":{"OBJECTID":2,"eventtype":"Construction","description":"VDOT will close Catharpin Road to through traffic",
            "startdate":1470117600000,"enddate":1470398400000},"geometry":{"paths":[[[-77.6,38.8],[-77.59,38.81]]]}}]}"#;
        let s = parse_prince_william(seg, true).unwrap();
        assert_eq!((s[0].impact, s[0].geometry["type"].as_str()), (Impact::Closed, Some("MultiLineString")));
        assert!(!s[0].shown(1_795_700_000.0), "ended in 2016: not shown");
        let ffx = r#"{"features":[{"attributes":{"OBJECTID":7,"event_type":"FLOODING","street_name":"OX","street_type":"RD","xstreet1":"POPES HEAD RD",
            "status_desc":"Road closed","IS_OPEN":"N"},"geometry":{"x":-77.33,"y":38.83}}]}"#;
        let f = parse_fairfax(ffx, false).unwrap();
        assert_eq!((f[0].road.as_str(), f[0].event.as_str(), f[0].impact), ("OX RD", "FLOODING", Impact::Closed));
        assert!(
            parse_fairfax(r#"{"error":{"code":400,"message":"Failed to execute query."}}"#, false).unwrap_err().to_string().contains("Failed to execute query")
        );
    }

    #[test]
    fn reads_virginia() {
        let va = r#"{"WZNO4295656-10052023-2": {"gml:id": "vat_event_line.VATRAFFIC-WZNO4295656-10052023-2",
            "orci:type_event": "Road Widening Project Activities", "orci:status": "Open", "orci:special_facility": "On I-66W on the ramp to VA-286S",
            "orci:travel_direction": "West", "orci:containing_jurisdiction": "Fairfax (County)", "orci:event_subcategory": "WZ - Construction",
            "orci:lanes_condition": "open|open|open|closed|closed|closed|", "orci:template_511_text": "On I-66W on the ramp to VA-286S, motorists can expect delays.",
            "road_closure": true, "orci:scheduled_start_time": "2023-10-06T02:00:00Z", "orci:scheduled_stop_time": "2023-10-06T09:00:00Z",
            "orci:segment_start_time": "2023-10-06T04:00:00Z", "orci:segment_stop_time": "2023-10-06T08:00:00Z",
            "orci:start_point": {"gml:Point": {"srsName": "urn:x-ogc:def:crs:EPSG:4326", "gml:pos": "38.854679 -77.389861"}},
            "orci:end_point": {"gml:Point": {"gml:pos": "38.855366 -77.38909"}}},
          "OLD-1": {"orci:status": "Closed", "orci:start_point": {"gml:Point": {"gml:pos": "38 -77"}}},
          "LANES-1": {"orci:status": "Open", "road_closure": false, "orci:lanes_condition": "open|closed|",
            "orci:mid_point": {"gml:Point": {"gml:pos": "37.5 -77.4"}}}}"#;
        let c = parse_vdot(va).unwrap();
        assert_eq!(c.len(), 2, "a closed event is left out");
        let a = c.iter().find(|c| c.key == "va:WZNO4295656-10052023-2").unwrap();
        assert_eq!(
            (a.impact, a.road.as_str(), a.jurisdiction.as_str(), a.event.as_str()),
            (Impact::Closed, "On I-66W on the ramp to VA-286S", "Fairfax (County)", "Road Widening Project Activities · WZ - Construction")
        );
        assert_eq!(a.geometry["coordinates"][0][0].as_f64(), Some(-77.389861), "lon first");
        assert_eq!(a.start, crate::nws::unix_time("2023-10-06T04:00:00Z"), "the segment's time first");
        let b = c.iter().find(|c| c.key == "va:LANES-1").unwrap();
        assert_eq!((b.impact, b.geometry["type"].as_str()), (Impact::Lanes, Some("Point")));
    }

    #[test]
    fn shows_what_matters_now() {
        let m = parse_maryland(MD, "Maryland road closures", "md").unwrap();
        let now = 1_796_100_000.0;
        assert!(m[0].planned(now) && m[0].shown(now), "the parade, in a few days");
        assert!(!m[0].shown(now - 30.0 * 86400.0), "a month before: not yet");
        assert_eq!(rgb(&m[0], now), PLANNED);
        let fc = features(&m, now, Some("md:2"));
        let f = fc["features"].as_array().unwrap();
        assert_eq!(f.last().unwrap()["properties"]["key"], "md:2", "the selected one last");
        assert_eq!(f.last().unwrap()["properties"]["color"], "#ffffff");
        assert_eq!(layers("closures").unwrap().len(), 2);
        // As image tiles: the lines, drawn along the road.
        let tiles = crate::areas::AreaTiles::default();
        tiles.set(shapes(&m, now, None));
        let (wx, wy) = crate::areas::world(-76.6, 39.3);
        let n = 4096.0;
        let (x, y) = ((wx * n) as u32, (wy * n) as u32);
        assert!(tiles.draw(12, x, y, 512).is_some(), "York Rd's line is drawn");
        let l = legend(&m, now);
        assert_eq!(l.entries.len(), 5);
        assert_eq!(impact_of("Curb Lane Closure", ""), Impact::Lanes);
        assert_eq!(FEEDS.iter().filter(|f| matches!(f.reader, Reader::Wzdx)).count(), 4);
        assert_eq!(FEEDS.iter().filter(|f| f.vdot).map(|f| f.id).collect::<Vec<_>>(), vec!["wz-va", "va"]);
        let u = reqwest::Url::parse_with_params(FEEDS[10].url, &[("token", "a$b/c+d")]).unwrap();
        assert!(u.as_str().ends_with("workZone.json?token=a%24b%2Fc%2Bd"), "{u}");
    }
}
