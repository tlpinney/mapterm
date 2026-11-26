//! Live ships from [aisstream.io](https://aisstream.io), a free AIS stream over a WebSocket.
//!
//! A client connects to `wss://stream.aisstream.io/v0/stream` and, within 3 s, sends its API
//! key and the bounding boxes it wants; ships in them then stream in as they report (every
//! few seconds under way, every few minutes at anchor). There's no polling and no request
//! quota. [`stream`] keeps one connection for the area in view (reconnecting with a new box
//! when the view moves away, and with a growing pause after errors), and [`Fleet`] keeps
//! every ship's fixes as a [`SampledPosition`], like [`crate::adsb::Tracks`] for aircraft.

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::time::Duration;

use crate::timeseries::{Sample, SampledPosition, Velocity};

/// The stream's address.
pub const STREAM: &str = "wss://stream.aisstream.io/v0/stream";

/// A box of the world: south-west and north-east corners, (lat, lon).
pub type BBox = [(f64, f64); 2];

/// What one message says about a ship.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// Where it is and how it moves.
    Position {
        /// Maritime Mobile Service Identity.
        mmsi: u32,
        /// Degrees.
        lat: f64,
        /// Degrees.
        lon: f64,
        /// Speed over ground, knots.
        sog: Option<f64>,
        /// Course over ground, degrees.
        cog: Option<f64>,
        /// Heading, degrees.
        heading: Option<f64>,
        /// AIS navigational status (0 under way, 1 at anchor, 5 moored, ...).
        status: Option<u8>,
        /// The name the stream knows it by.
        name: Option<String>,
    },
    /// Who it is and where it's going.
    Static(Details),
}

/// A ship's static and voyage data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    /// Maritime Mobile Service Identity.
    pub mmsi: u32,
    /// Ship name.
    pub name: Option<String>,
    /// Radio call sign.
    pub callsign: Option<String>,
    /// IMO number.
    pub imo: Option<u32>,
    /// AIS ship type code (70-79 cargo, 80-89 tanker, ...).
    pub ship_type: Option<u8>,
    /// Destination as the crew typed it.
    pub destination: Option<String>,
    /// Estimated arrival, `MM-DD HH:MM` UTC (AIS has no year).
    pub eta: Option<String>,
    /// Draught, meters.
    pub draught: Option<f64>,
    /// Length and beam, meters.
    pub size: Option<(f64, f64)>,
}

/// Read one stream message. `Ok(None)` for message types that say nothing useful here; an
/// error for the stream's own `{"error": ...}` (a bad key, a bad subscription).
pub fn parse(body: &[u8]) -> Result<Option<Update>> {
    let v: Json = serde_json::from_slice(body).context("AIS stream: not JSON")?;
    if let Some(e) = v.get("error").and_then(Json::as_str) {
        bail!("aisstream.io: {e}");
    }
    let meta = v.get("MetaData").unwrap_or(&Json::Null);
    let kind = v.get("MessageType").and_then(Json::as_str).unwrap_or("");
    let Some(m) = v.get("Message").and_then(|m| m.get(kind)) else { return Ok(None) };
    let num = |o: &Json, k: &str| o.get(k).and_then(Json::as_f64).filter(|n| n.is_finite());
    let text = |o: &Json, k: &str| o.get(k).and_then(Json::as_str).map(|s| s.trim_end_matches(['@', ' ']).trim()).filter(|s| !s.is_empty()).map(str::to_string);
    let mmsi = m.get("UserID").or_else(|| meta.get("MMSI")).and_then(Json::as_u64).context("AIS message without an MMSI")? as u32;
    Ok(match kind {
        "PositionReport" | "StandardClassBPositionReport" | "ExtendedClassBPositionReport" => {
            if m.get("Valid").and_then(Json::as_bool) == Some(false) {
                return Ok(None);
            }
            let (Some(lat), Some(lon)) = (num(m, "Latitude"), num(m, "Longitude")) else { return Ok(None) };
            // AIS marks "not available" with out-of-range values (91, 181, 102.3 kt, 360, 511).
            if lat.abs() > 90.0 || lon.abs() > 180.0 {
                return Ok(None);
            }
            Some(Update::Position {
                mmsi,
                lat,
                lon,
                sog: num(m, "Sog").filter(|s| *s < 102.2),
                cog: num(m, "Cog").filter(|c| *c < 360.0),
                heading: num(m, "TrueHeading").filter(|h| *h < 360.0),
                status: m.get("NavigationalStatus").and_then(Json::as_u64).map(|s| s as u8),
                name: text(meta, "ShipName").or_else(|| text(m, "Name")),
            })
        }
        "ShipStaticData" => {
            let dims = m.get("Dimension").unwrap_or(m);
            let size = match (num(dims, "A"), num(dims, "B"), num(dims, "C"), num(dims, "D")) {
                (Some(a), Some(b), Some(c), Some(d)) if a + b > 0.0 => Some((a + b, c + d)),
                _ => None,
            };
            let eta = m.get("Eta").unwrap_or(m);
            let eta = match (num(eta, "Month"), num(eta, "Day"), num(eta, "Hour"), num(eta, "Minute")) {
                (Some(mo), Some(d), Some(h), Some(mi)) if (1.0..=12.0).contains(&mo) && (1.0..=31.0).contains(&d) && h < 24.0 && mi < 60.0 => {
                    Some(format!("{mo:02}-{d:02} {h:02}:{mi:02}"))
                }
                _ => None,
            };
            Some(Update::Static(Details {
                mmsi,
                name: text(m, "Name").or_else(|| text(meta, "ShipName")),
                callsign: text(m, "CallSign"),
                imo: m.get("ImoNumber").and_then(Json::as_u64).filter(|n| *n > 0).map(|n| n as u32),
                ship_type: m.get("Type").and_then(Json::as_u64).filter(|t| *t > 0).map(|t| t as u8),
                destination: text(m, "Destination"),
                eta,
                draught: num(m, "MaximumStaticDraught").filter(|d| *d > 0.0),
                size,
            }))
        }
        _ => None,
    })
}

/// What kind of ship an AIS type code says, for colors and the panel.
pub fn category(ship_type: Option<u8>) -> &'static str {
    match ship_type {
        Some(30) => "fishing",
        Some(31 | 32 | 52) => "tug",
        Some(35) => "military",
        Some(36 | 37) => "pleasure",
        Some(40..=49) => "high speed",
        Some(50 | 51 | 53..=59) => "special",
        Some(60..=69) => "passenger",
        Some(70..=79) => "cargo",
        Some(80..=89) => "tanker",
        _ => "other",
    }
}

/// The color for a category ([`category`]).
pub fn category_rgb(cat: &str) -> (u8, u8, u8) {
    match cat {
        "cargo" => (0x7e, 0xe7, 0x87),
        "tanker" => (0xff, 0x7b, 0x72),
        "passenger" => (0x79, 0xc0, 0xff),
        "fishing" => (0xf2, 0xcc, 0x60),
        "pleasure" => (0xff, 0x6b, 0xcb),
        "tug" | "special" => (0x56, 0xd4, 0xdd),
        "high speed" => (0xff, 0xa6, 0x57),
        "military" => (0xd2, 0xa8, 0xff),
        _ => (0x9a, 0xa0, 0xa6),
    }
}

/// What a navigational status code means.
pub fn status_text(status: u8) -> &'static str {
    match status {
        0 => "under way (engine)",
        1 => "at anchor",
        2 => "not under command",
        3 => "restricted manoeuvrability",
        4 => "constrained by draught",
        5 => "moored",
        6 => "aground",
        7 => "fishing",
        8 => "under way (sailing)",
        14 => "AIS-SART (distress)",
        _ => "",
    }
}

/// Seconds of fixes kept per ship (the trail's length): ships are slow.
const KEEP: f64 = 600.0;
/// Most seconds a ship is moved past its last fix (Class B boats report every 30 s or so).
const EXTRAPOLATE: f64 = 60.0;
/// A ship unheard for this long is dropped (at anchor they report every few minutes).
const GONE: f64 = 900.0;

/// Every ship heard lately.
#[derive(Default)]
pub struct Fleet {
    ships: HashMap<u32, Ship>,
}

/// One ship: what it said about itself, how it moves, and when it was last heard.
pub struct Ship {
    /// Static and voyage data, once it has sent some.
    pub details: Details,
    /// Its fixes over time.
    pub path: SampledPosition,
    /// Speed over ground, knots.
    pub sog: Option<f64>,
    /// Course over ground, degrees.
    pub cog: Option<f64>,
    /// Heading, degrees.
    pub heading: Option<f64>,
    /// Navigational status.
    pub status: Option<u8>,
    /// When it was last heard (receive time).
    pub seen: f64,
}

impl Ship {
    /// The name to show: its name, else call sign, else MMSI.
    pub fn label(&self) -> String {
        self.details.name.clone().or_else(|| self.details.callsign.clone()).unwrap_or_else(|| self.details.mmsi.to_string())
    }

    /// An arrow along its course when moving, a dot when not.
    pub fn glyph(&self) -> &'static str {
        const ARROWS: [&str; 8] = ["↑", "↗", "→", "↘", "↓", "↙", "←", "↖"];
        match (self.sog, self.cog.or(self.heading)) {
            (Some(s), Some(c)) if s >= 0.5 => ARROWS[((c.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8],
            _ => "•",
        }
    }
}

impl Fleet {
    /// Take one update received at `now` (seconds, the same clock as [`Fleet::points`]).
    pub fn apply(&mut self, u: Update, now: f64) {
        let mmsi = match &u {
            Update::Position { mmsi, .. } => *mmsi,
            Update::Static(d) => d.mmsi,
        };
        let ship = self.ships.entry(mmsi).or_insert_with(|| Ship {
            details: Details { mmsi, ..Details::default() },
            path: SampledPosition::new(KEEP, EXTRAPOLATE),
            sog: None,
            cog: None,
            heading: None,
            status: None,
            seen: now,
        });
        ship.seen = now;
        match u {
            Update::Position { lat, lon, sog, cog, heading, status, name, .. } => {
                ship.path.push(Sample { t: now, lon, lat, alt: None });
                ship.path.set_velocity(match (sog, cog) {
                    (Some(s), Some(c)) if s >= 0.5 => Some(Velocity { speed: s * 1852.0 / 3600.0, bearing: c }),
                    _ => None,
                });
                (ship.sog, ship.cog, ship.heading, ship.status) = (sog, cog, heading, status);
                if ship.details.name.is_none() {
                    ship.details.name = name;
                }
            }
            Update::Static(d) => {
                // Keep what an earlier message said where this one says nothing.
                let old = std::mem::take(&mut ship.details);
                ship.details = Details {
                    mmsi,
                    name: d.name.or(old.name),
                    callsign: d.callsign.or(old.callsign),
                    imo: d.imo.or(old.imo),
                    ship_type: d.ship_type.or(old.ship_type),
                    destination: d.destination.or(old.destination),
                    eta: d.eta.or(old.eta),
                    draught: d.draught.or(old.draught),
                    size: d.size.or(old.size),
                };
            }
        }
    }

    /// Drop ships unheard for 15 minutes, and those outside `bbox` (after the view moved).
    pub fn prune(&mut self, now: f64, bbox: Option<BBox>) {
        self.ships.retain(|_, s| {
            let inside = match (bbox, s.path.last()) {
                (Some([(s_lat, w_lon), (n_lat, e_lon)]), Some(p)) => (s_lat..=n_lat).contains(&p.lat) && (w_lon..=e_lon).contains(&p.lon),
                _ => true,
            };
            now - s.seen < GONE && inside
        });
    }

    /// Every ship known.
    pub fn all(&self) -> impl Iterator<Item = &Ship> {
        self.ships.values()
    }

    /// How many ships are known (with or without a position yet).
    pub fn len(&self) -> usize {
        self.ships.len()
    }

    /// Whether no ships are known.
    pub fn is_empty(&self) -> bool {
        self.ships.is_empty()
    }

    /// The ship `mmsi`, and where it is at time `t`.
    pub fn get(&self, mmsi: u32, t: f64) -> Option<(&Ship, Sample)> {
        let s = self.ships.get(&mmsi)?;
        Some((s, s.path.at(t)?))
    }

    /// The ship drawn nearest the braille dot `dot` at time `t`, within `within` dots.
    pub fn nearest(&self, t: f64, vp: &crate::Viewport, dot: (f64, f64), within: f64) -> Option<(f64, u32)> {
        self.ships
            .iter()
            .filter_map(|(m, s)| {
                let p = s.path.at(t)?;
                let (x, y) = vp.dot_ll(p.lon, p.lat);
                let d = ((x - dot.0).powi(2) + ((y - dot.1) / 2.0).powi(2)).sqrt();
                (d <= within).then_some((d, *m))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
    }

    /// The ships by distance from (`lon`, `lat`) at time `t`, nearest first.
    pub fn by_distance(&self, t: f64, lon: f64, lat: f64) -> Vec<u32> {
        let mut v: Vec<(f64, u32)> =
            self.ships.iter().filter_map(|(m, s)| s.path.at(t).map(|p| (crate::adsb::distance_nm((lon, lat), (p.lon, p.lat)), *m))).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        v.into_iter().map(|(_, m)| m).collect()
    }

    /// The ships at time `t` as GeoJSON points (`glyph`, `label`, `category`, `name`, `mmsi`,
    /// `speed_kt`, `course`, `destination`, ...).
    pub fn points(&self, t: f64) -> Json {
        let features: Vec<Json> = self
            .ships
            .values()
            .filter_map(|s| {
                let p = s.path.at(t)?;
                let d = &s.details;
                Some(json!({
                    "type": "Feature",
                    "id": d.mmsi,
                    "geometry": {"type": "Point", "coordinates": [p.lon, p.lat]},
                    "properties": {
                        "glyph": s.glyph(),
                        "label": d.name.clone().or_else(|| d.callsign.clone()).unwrap_or_default(),
                        "category": category(d.ship_type),
                        "name": d.name, "mmsi": d.mmsi, "callsign": d.callsign, "imo": d.imo,
                        "speed_kt": s.sog, "course": s.cog, "destination": d.destination,
                        "status": s.status.map(status_text),
                    }
                }))
            })
            .collect();
        json!({"type": "FeatureCollection", "features": features})
    }

    /// The moving ships' trail paths in their category colors, for
    /// [`crate::timeseries::render_trails`].
    pub fn trail_paths(&self) -> impl Iterator<Item = (&SampledPosition, (u8, u8, u8))> {
        self.ships.values().filter(|s| s.sog.is_some_and(|v| v >= 0.5)).map(|s| (&s.path, category_rgb(category(s.details.ship_type))))
    }
}

/// The style layer for ships from source `source`: arrows along the course (dots when
/// still), colored by kind, with their names (labels that would collide are left out).
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    let mut colors = vec![json!("match"), json!(["get", "category"])];
    for c in ["cargo", "tanker", "passenger", "fishing", "pleasure", "tug", "special", "high speed", "military"] {
        let (r, g, b) = category_rgb(c);
        colors.push(json!(c));
        colors.push(json!(format!("#{r:02x}{g:02x}{b:02x}")));
    }
    colors.push(json!("#9aa0a6"));
    crate::StyleLayer::symbol(id, source).glyph(json!(["get", "glyph"])).text(json!(["get", "label"])).color("#c9d1d9").paint("icon-color", Json::Array(colors))
}

/// What [`stream`] tells the viewer.
#[derive(Debug)]
pub enum Event {
    /// A ship update.
    Update(Update),
    /// Connected and subscribed to this box.
    Connected(BBox),
    /// The connection dropped or failed; trying again after this pause.
    Retrying(String, Duration),
    /// The stream refused us (a bad key): no more tries.
    Stopped(String),
}

/// Keep a connection to aisstream.io for the box in `area` (None: no connection, as when
/// zoomed out), reconnecting when it changes and after errors (5 s doubling to 5 min).
/// Sends what arrives on `tx`; returns when `tx` closes.
///
/// aisstream.io answers a bad key by dropping the connection right after the subscription,
/// without a word: three of those in a row (within 10 s, before any message) stop it.
pub async fn stream(key: String, mut area: tokio::sync::watch::Receiver<Option<BBox>>, tx: tokio::sync::mpsc::UnboundedSender<Event>) {
    use crate::ws::Message;
    let mut pause = Duration::from_secs(5);
    let mut refused = 0;
    loop {
        let Some(bbox) = *area.borrow_and_update() else {
            if area.changed().await.is_err() {
                return;
            }
            continue;
        };
        let mut heard = false;
        let mut subscribed: Option<std::time::Instant> = None;
        let session = async {
            // The server can take several seconds to accept.
            // Compressed when the server agrees, as aisstream.io asks.
            let opts = crate::ws::Options { protocol: None, deflate: true };
            let mut ws = tokio::time::timeout(Duration::from_secs(30), crate::ws::connect(STREAM, opts)).await.context("connecting timed out")??;
            let [(s, w), (n, e)] = bbox;
            let sub = json!({"APIKey": key, "BoundingBoxes": [[[s, w], [n, e]]],
                             "FilterMessageTypes": ["PositionReport", "StandardClassBPositionReport", "ExtendedClassBPositionReport", "ShipStaticData"]});
            ws.send_text(&sub.to_string()).await?;
            subscribed = Some(std::time::Instant::now());
            let _ = tx.send(Event::Connected(bbox));
            loop {
                tokio::select! {
                    msg = ws.next() => {
                        let body = match msg?.context("the stream closed")? {
                            Message::Binary(b) => b,
                            Message::Text(t) => t.into_bytes(),
                            Message::Close(c) => bail!("the stream closed: {}", c.map(|c| c.1).unwrap_or_default()),
                        };
                        heard = true;
                        match parse(&body) {
                            Ok(Some(u)) => { let _ = tx.send(Event::Update(u)); }
                            Ok(None) => {}
                            // The stream's own error (bad key): stop, don't retry.
                            Err(e) if e.to_string().starts_with("aisstream.io:") => return Ok::<bool, anyhow::Error>(false),
                            Err(_) => {}
                        }
                    }
                    changed = area.changed() => {
                        if changed.is_err() {
                            return Ok(false);
                        }
                        // A new box: reconnect with it.
                        let _ = ws.close().await;
                        return Ok(true);
                    }
                }
            }
        };
        let outcome = session.await;
        // Dropped soon after subscribing, before anything arrived: likely the key.
        let quick_drop = outcome.is_err() && !heard && subscribed.is_some_and(|t| t.elapsed() < Duration::from_secs(10));
        // Only a session that delivered something, or lasted, clears the count: failing to
        // connect at all says nothing about the key.
        if quick_drop {
            refused += 1;
        } else if heard || subscribed.is_some() {
            refused = 0;
        }
        if refused >= 3 {
            let _ = tx.send(Event::Stopped("aisstream.io keeps closing the connection right after subscribing: check the key (AISSTREAM_API_KEY)".into()));
            return;
        }
        match outcome {
            Ok(true) => pause = Duration::from_secs(5),
            Ok(false) => {
                if !tx.is_closed() {
                    let _ = tx.send(Event::Stopped("aisstream.io refused the key (check AISSTREAM_API_KEY)".into()));
                }
                return;
            }
            Err(e) => {
                if tx.send(Event::Retrying(format!("{e:#}"), pause)).is_err() {
                    return;
                }
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_secs(300));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSITION: &str = r#"{"Message":{"PositionReport":{"Cog":215.3,"Latitude":38.85,"Longitude":-77.02,"NavigationalStatus":0,"Sog":11.2,"TrueHeading":214,"UserID":367000001,"Valid":true}},
        "MessageType":"PositionReport","MetaData":{"MMSI":367000001,"ShipName":"SPIRIT OF DC   ","latitude":38.85,"longitude":-77.02,"time_utc":"2026-11-25 23:59:59 +0000 UTC"}}"#;
    const STATIC: &str = r#"{"Message":{"ShipStaticData":{"CallSign":"WDA1234","Destination":"WASHINGTON@@@@","Dimension":{"A":40,"B":10,"C":6,"D":6},
        "Eta":{"Day":26,"Hour":14,"Minute":30,"Month":11},"ImoNumber":0,"MaximumStaticDraught":2.5,"Name":"SPIRIT OF DC","Type":60,"UserID":367000001,"Valid":true}},
        "MessageType":"ShipStaticData","MetaData":{"MMSI":367000001,"ShipName":"SPIRIT OF DC"}}"#;

    #[test]
    fn parses_positions_and_details() {
        let Some(Update::Position { mmsi, lat, sog, cog, name, .. }) = parse(POSITION.as_bytes()).unwrap() else { panic!() };
        assert_eq!((mmsi, lat, sog, cog, name.as_deref()), (367000001, 38.85, Some(11.2), Some(215.3), Some("SPIRIT OF DC")));
        let Some(Update::Static(d)) = parse(STATIC.as_bytes()).unwrap() else { panic!() };
        assert_eq!(d.destination.as_deref(), Some("WASHINGTON"), "AIS @ padding trimmed");
        assert_eq!((d.size, d.eta.as_deref(), d.imo, d.ship_type), (Some((50.0, 12.0)), Some("11-26 14:30"), None, Some(60)));
        assert!(parse(br#"{"error":"Api Key Is Not Valid"}"#).unwrap_err().to_string().starts_with("aisstream.io:"));
        assert_eq!(parse(br#"{"MessageType":"BaseStationReport","Message":{"BaseStationReport":{"UserID":1}},"MetaData":{}}"#).unwrap(), None);
    }

    #[test]
    fn unavailable_values_are_dropped() {
        let raw =
            POSITION.replace("\"Sog\":11.2", "\"Sog\":102.3").replace("\"Cog\":215.3", "\"Cog\":360").replace("\"TrueHeading\":214", "\"TrueHeading\":511");
        let Some(Update::Position { sog, cog, heading, .. }) = parse(raw.as_bytes()).unwrap() else { panic!() };
        assert_eq!((sog, cog, heading), (None, None, None));
        assert_eq!(parse(POSITION.replace("38.85", "91").as_bytes()).unwrap(), None);
    }

    #[test]
    fn fleet_merges_and_moves() {
        let mut f = Fleet::default();
        f.apply(parse(POSITION.as_bytes()).unwrap().unwrap(), 100.0);
        f.apply(parse(STATIC.as_bytes()).unwrap().unwrap(), 101.0);
        let (ship, at) = f.get(367000001, 100.0).unwrap();
        assert_eq!((ship.label(), ship.glyph(), category(ship.details.ship_type)), ("SPIRIT OF DC".to_string(), "↙", "passenger"));
        assert_eq!(at.lat, 38.85);
        // Thirty seconds on at 11.2 kt it has moved, south-west.
        let later = f.get(367000001, 130.0).unwrap().1;
        assert!(later.lat < 38.85 && later.lon < -77.02);
        let doc = f.points(100.0);
        assert_eq!(doc["features"][0]["properties"]["category"], "passenger");
        // Gone after 15 minutes unheard, or once outside the box.
        f.prune(500.0, Some([(38.0, -78.0), (39.0, -76.0)]));
        assert_eq!(f.len(), 1);
        f.prune(500.0, Some([(40.0, -78.0), (41.0, -76.0)]));
        assert!(f.is_empty());
    }

    #[test]
    fn categories_and_status() {
        assert_eq!(category(Some(74)), "cargo");
        assert_eq!(category(Some(84)), "tanker");
        assert_eq!(category(Some(52)), "tug");
        assert_eq!(category(None), "other");
        assert_eq!(status_text(5), "moored");
    }
}
