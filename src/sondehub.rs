//! Weather balloons (radiosondes) from [SondeHub](https://sondehub.org), tracked by amateur
//! radio receivers around the world. The data is CC BY-SA 2.0.
//!
//! SondeHub asks clients not to poll its API: query once, then follow the live MQTT stream
//! (over a WebSocket). It has no area filter, and its `batch` topic carries every balloon on
//! Earth, so [`stream`] asks `/sondes` once for the balloons around the view, subscribes to
//! each one's own topic (`sondes/<serial>`) and to the slow feed of newly heard balloons
//! (`sondes-new/#`, kept only when near), and asks again only when the view moves far away.
//! The selected balloon's landing prediction comes from `prediction/<serial>`.
//!
//! [`Balloons`] keeps every balloon's fixes as a [`SampledPosition`], like the aircraft and
//! ship layers.

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::timeseries::{Sample, SampledPosition, Velocity};

/// The API.
pub const API: &str = "https://api.v2.sondehub.org";

/// One telemetry report from a balloon.
#[derive(Clone, Debug, PartialEq)]
pub struct Telemetry {
    /// The sonde's serial number.
    pub serial: String,
    /// Model, such as `RS41-SGP` (subtype, else type).
    pub kind: Option<String>,
    /// Maker, such as Vaisala.
    pub manufacturer: Option<String>,
    /// Degrees.
    pub lat: f64,
    /// Degrees.
    pub lon: f64,
    /// Meters above sea level.
    pub alt: Option<f64>,
    /// Climb (positive) or descent rate, m/s.
    pub vel_v: Option<f64>,
    /// Speed over ground, m/s.
    pub vel_h: Option<f64>,
    /// Direction of travel, degrees.
    pub heading: Option<f64>,
    /// Air temperature, °C.
    pub temp: Option<f64>,
    /// Relative humidity, %.
    pub humidity: Option<f64>,
    /// Battery, volts.
    pub batt: Option<f64>,
    /// Transmit frequency, MHz.
    pub frequency: Option<f64>,
    /// When the sonde measured it (Unix seconds, from its GPS time).
    pub t: f64,
    /// The receiving station's callsign.
    pub uploader: Option<String>,
}

/// Seconds since the Unix epoch for an RFC 3339 UTC time (`2026-11-26T00:57:16.999Z`).
pub fn unix_time(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.split(':');
    let (h, mi, sec) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?, t.next()?.parse::<f64>().ok()?);
    // Days from 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + h * 3600 + mi * 60) as f64 + sec)
}

/// One report from its JSON (the `/sondes` values, and each MQTT telemetry message).
pub fn parse_one(v: &Json) -> Option<Telemetry> {
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64().or_else(|| x.as_str()?.parse().ok())).filter(|n: &f64| n.is_finite());
    let text = |k: &str| v.get(k).and_then(Json::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let (lat, lon) = (num("lat")?, num("lon")?);
    if lat.abs() > 90.0 || lon.abs() > 180.0 || (lat == 0.0 && lon == 0.0) {
        return None;
    }
    Some(Telemetry {
        serial: text("serial")?,
        kind: text("subtype").or_else(|| text("type")),
        manufacturer: text("manufacturer"),
        lat,
        lon,
        alt: num("alt"),
        vel_v: num("vel_v"),
        vel_h: num("vel_h"),
        heading: num("heading"),
        temp: num("temp").filter(|t| *t > -273.0),
        humidity: num("humidity").filter(|h| *h >= 0.0),
        batt: num("batt").filter(|b| *b > 0.0),
        frequency: num("frequency").or_else(|| num("tx_frequency")),
        t: text("datetime").as_deref().and_then(unix_time)?,
        uploader: text("uploader_callsign"),
    })
}

/// The reports in an MQTT message: one object, or a list (the batch topics).
pub fn parse_payload(body: &[u8]) -> Vec<Telemetry> {
    match serde_json::from_slice::<Json>(body) {
        Ok(Json::Array(a)) => a.iter().filter_map(parse_one).collect(),
        Ok(v) => parse_one(&v).into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

/// Where a balloon is expected to go, and land.
#[derive(Clone, Debug, PartialEq)]
pub struct Prediction {
    /// The sonde's serial number.
    pub serial: String,
    /// The predicted path, (Unix seconds, lon, lat, alt), ending at the landing.
    pub path: Vec<(f64, f64, f64, f64)>,
    /// The altitude it's expected to burst at, meters.
    pub burst_alt: Option<f64>,
    /// Whether it's already falling.
    pub descending: bool,
}

impl Prediction {
    /// The predicted landing: (Unix seconds, lon, lat).
    pub fn landing(&self) -> Option<(f64, f64, f64)> {
        self.path.last().map(|&(t, lon, lat, _)| (t, lon, lat))
    }
}

/// A prediction from its JSON (a `/predictions` item, or a `prediction/<serial>` message).
pub fn parse_prediction(v: &Json) -> Option<Prediction> {
    let serial = v.get("vehicle").and_then(Json::as_str)?.to_string();
    // The path is a JSON string inside the JSON.
    let data: Json = match v.get("data")? {
        Json::String(s) => serde_json::from_str(s).ok()?,
        d => d.clone(),
    };
    let path: Vec<(f64, f64, f64, f64)> = data
        .as_array()?
        .iter()
        .filter_map(|p| Some((p.get("time")?.as_f64()?, p.get("lon")?.as_f64()?, p.get("lat")?.as_f64()?, p.get("alt")?.as_f64().unwrap_or(0.0))))
        // The predictor's longitudes can run 0 to 360.
        .map(|(t, lon, lat, alt)| (t, if lon > 180.0 { lon - 360.0 } else { lon }, lat, alt))
        .collect();
    if path.is_empty() {
        return None;
    }
    let flag = |k: &str| v.get(k).is_some_and(|x| x.as_i64() == Some(1) || x.as_bool() == Some(true));
    Some(Prediction { serial, path, burst_alt: v.get("burst_altitude").and_then(Json::as_f64), descending: flag("descending") })
}

/// A response body, gunzipped if it came gzipped (SondeHub sends some that way regardless).
fn body(b: Vec<u8>) -> Vec<u8> {
    if b.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        let mut gz = flate2::read::GzDecoder::new(&b[..]).take(64 << 20);
        if std::io::Read::read_to_end(&mut gz, &mut out).is_ok() {
            return out;
        }
    }
    b
}

use std::io::Read as _;

async fn get(path: &str) -> Result<Vec<u8>> {
    let url = format!("{API}{path}");
    let b = crate::source::get_limited(&crate::source::http_client(), &url, &[], 32 << 20).await.context("SondeHub")?;
    Ok(body(b.context("SondeHub: not found")?))
}

/// The balloons heard within `radius_m` of (`lat`, `lon`) in the last `last_s` seconds.
pub async fn fetch_near(lat: f64, lon: f64, radius_m: f64, last_s: u64) -> Result<Vec<Telemetry>> {
    let b = get(&format!("/sondes?lat={lat:.4}&lon={lon:.4}&distance={:.0}&last={last_s}", radius_m)).await?;
    let v: Json = serde_json::from_slice(&b).context("SondeHub: not JSON")?;
    Ok(v.as_object().map(|o| o.values().filter_map(parse_one).collect()).unwrap_or_default())
}

/// The latest landing prediction for one balloon.
pub async fn fetch_prediction(serial: &str) -> Result<Option<Prediction>> {
    let b = get(&format!("/predictions?vehicles={serial}")).await?;
    let v: Json = serde_json::from_slice(&b).context("SondeHub: not JSON")?;
    Ok(v.as_array().and_then(|a| a.iter().find_map(parse_prediction)))
}

/// Just enough MQTT 3.1.1 to follow topics: connect, (un)subscribe, ping, and read publishes.
pub mod mqtt {
    /// A packet from the server.
    #[derive(Debug, PartialEq)]
    pub enum Packet {
        /// The answer to CONNECT: 0 is accepted.
        ConnAck(u8),
        /// A message on a topic.
        Publish {
            /// Its topic.
            topic: String,
            /// Its body.
            payload: Vec<u8>,
        },
        /// Anything else (SUBACK, UNSUBACK, PINGRESP).
        Other(u8),
    }

    fn remaining(mut n: usize, out: &mut Vec<u8>) {
        loop {
            let mut b = (n % 128) as u8;
            n /= 128;
            if n > 0 {
                b |= 0x80;
            }
            out.push(b);
            if n == 0 {
                break;
            }
        }
    }

    fn string(s: &str, out: &mut Vec<u8>) {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    fn packet(kind: u8, body: Vec<u8>) -> Vec<u8> {
        let mut out = vec![kind];
        remaining(body.len(), &mut out);
        out.extend(body);
        out
    }

    /// CONNECT with a clean session and `keepalive` seconds.
    pub fn connect(client_id: &str, keepalive: u16) -> Vec<u8> {
        let mut b = Vec::new();
        string("MQTT", &mut b);
        b.extend_from_slice(&[4, 0x02]);
        b.extend_from_slice(&keepalive.to_be_bytes());
        string(client_id, &mut b);
        packet(0x10, b)
    }

    /// SUBSCRIBE to `topics` at QoS 0.
    pub fn subscribe(id: u16, topics: &[String]) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        for t in topics {
            string(t, &mut b);
            b.push(0);
        }
        packet(0x82, b)
    }

    /// UNSUBSCRIBE from `topics`.
    pub fn unsubscribe(id: u16, topics: &[String]) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        for t in topics {
            string(t, &mut b);
        }
        packet(0xa2, b)
    }

    /// PINGREQ.
    pub fn ping() -> Vec<u8> {
        vec![0xc0, 0]
    }

    /// Take the next whole packet from the front of `buf` (WebSocket frames may split or join
    /// them), or None until more bytes come.
    pub fn take(buf: &mut Vec<u8>) -> Option<Packet> {
        let (mut len, mut mult, mut i) = (0usize, 1usize, 1);
        loop {
            let b = *buf.get(i)?;
            len += (b & 0x7f) as usize * mult;
            i += 1;
            if b & 0x80 == 0 {
                break;
            }
            mult *= 128;
            if i > 4 {
                buf.clear();
                return None;
            }
        }
        if buf.len() < i + len {
            return None;
        }
        let kind = buf[0];
        let body: Vec<u8> = buf[i..i + len].to_vec();
        buf.drain(..i + len);
        Some(match kind >> 4 {
            2 => Packet::ConnAck(*body.get(1).unwrap_or(&255)),
            3 => {
                let tl = u16::from_be_bytes([*body.first()?, *body.get(1)?]) as usize;
                let topic = String::from_utf8_lossy(body.get(2..2 + tl)?).into_owned();
                // QoS 1 and 2 carry a packet id after the topic.
                let skip = if (kind >> 1) & 3 > 0 { 2 } else { 0 };
                Packet::Publish { topic, payload: body.get(2 + tl + skip..)?.to_vec() }
            }
            k => Packet::Other(k),
        })
    }
}

/// Seconds of fixes kept per balloon: a whole flight (up, burst, down) is about 2 to 3 hours.
const KEEP: f64 = 4.0 * 3600.0;
/// Most seconds a balloon is moved past its last fix.
const EXTRAPOLATE: f64 = 30.0;
/// A balloon unheard for this long is dropped (landed, or out of range of every receiver).
const GONE: f64 = 3600.0;

/// One balloon.
pub struct Balloon {
    /// Its latest report.
    pub latest: Telemetry,
    /// Its fixes over time (altitude in meters).
    pub path: SampledPosition,
    /// When it was last heard (receive time).
    pub seen: f64,
}

impl Balloon {
    /// ▲ climbing, ▼ falling, • still (landed, or no rate).
    pub fn glyph(&self) -> &'static str {
        match self.latest.vel_v {
            Some(v) if v > 0.5 => "▲",
            Some(v) if v < -0.5 => "▼",
            _ => "•",
        }
    }
}

/// Every balloon heard lately, and the latest prediction for the ones asked for.
#[derive(Default)]
pub struct Balloons {
    by_serial: HashMap<String, Balloon>,
    predictions: HashMap<String, Prediction>,
}

impl Balloons {
    /// Take a report received at `now` (Unix seconds). Fixes are timed by the sonde's own GPS
    /// clock, so late or repeated reports fall in place.
    pub fn apply(&mut self, t: Telemetry, now: f64) {
        let b =
            self.by_serial.entry(t.serial.clone()).or_insert_with(|| Balloon { latest: t.clone(), path: SampledPosition::new(KEEP, EXTRAPOLATE), seen: now });
        b.path.push(Sample { t: t.t, lon: t.lon, lat: t.lat, alt: t.alt });
        if t.t >= b.latest.t {
            b.path.set_velocity(match (t.vel_h, t.heading) {
                (Some(s), Some(h)) if s > 0.5 => Some(Velocity { speed: s, bearing: h }),
                _ => None,
            });
            b.latest = t;
        }
        b.seen = now;
    }

    /// Keep a prediction (for the balloon it's about).
    pub fn set_prediction(&mut self, p: Prediction) {
        self.predictions.insert(p.serial.clone(), p);
    }

    /// The prediction for `serial`, if one came.
    pub fn prediction(&self, serial: &str) -> Option<&Prediction> {
        self.predictions.get(serial)
    }

    /// Drop balloons unheard for an hour.
    pub fn prune(&mut self, now: f64) {
        self.by_serial.retain(|_, b| now - b.seen < GONE);
        let keep: HashSet<&String> = self.by_serial.keys().collect();
        self.predictions.retain(|s, _| keep.contains(s));
    }

    /// How many balloons are known.
    pub fn len(&self) -> usize {
        self.by_serial.len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.by_serial.is_empty()
    }

    /// The serials known.
    pub fn serials(&self) -> impl Iterator<Item = &String> {
        self.by_serial.keys()
    }

    /// The balloon `serial`, and where it is at time `t`.
    pub fn get(&self, serial: &str, t: f64) -> Option<(&Balloon, Sample)> {
        let b = self.by_serial.get(serial)?;
        Some((b, b.path.at(t)?))
    }

    /// The balloon drawn nearest the braille dot `dot` at time `t`, within `within` dots.
    pub fn nearest(&self, t: f64, vp: &crate::Viewport, dot: (f64, f64), within: f64) -> Option<String> {
        self.by_serial
            .iter()
            .filter_map(|(s, b)| {
                let p = b.path.at(t)?;
                let (x, y) = vp.dot_ll(p.lon, p.lat);
                let d = ((x - dot.0).powi(2) + ((y - dot.1) / 2.0).powi(2)).sqrt();
                (d <= within).then_some((d, s))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, s)| s.clone())
    }

    /// The balloons by distance from (`lon`, `lat`) at time `t`, nearest first.
    pub fn by_distance(&self, t: f64, lon: f64, lat: f64) -> Vec<String> {
        let mut v: Vec<(f64, &String)> =
            self.by_serial.iter().filter_map(|(s, b)| b.path.at(t).map(|p| (crate::adsb::distance_nm((lon, lat), (p.lon, p.lat)), s))).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(b.1)));
        v.into_iter().map(|(_, s)| s.clone()).collect()
    }

    /// The balloons at time `t` as GeoJSON points (`glyph`, `label`, `alt`, and the report).
    pub fn points(&self, t: f64) -> Json {
        let features: Vec<Json> = self
            .by_serial
            .values()
            .filter_map(|b| {
                let p = b.path.at(t)?;
                let l = &b.latest;
                Some(json!({
                    "type": "Feature",
                    "id": l.serial,
                    "geometry": {"type": "Point", "coordinates": [p.lon, p.lat]},
                    "properties": {
                        "glyph": b.glyph(), "label": l.serial, "alt": p.alt.unwrap_or(0.0),
                        "serial": l.serial, "kind": l.kind, "climb_ms": l.vel_v, "frequency_mhz": l.frequency,
                    }
                }))
            })
            .collect();
        json!({"type": "FeatureCollection", "features": features})
    }

    /// The balloons' trail paths in their altitude colors, for
    /// [`crate::timeseries::render_trails`].
    pub fn trail_paths(&self) -> impl Iterator<Item = (&SampledPosition, (u8, u8, u8))> {
        self.by_serial.values().map(|b| (&b.path, altitude_rgb(b.latest.alt.unwrap_or(0.0))))
    }
}

/// Altitude colors for balloons (meters, up to the ~35 km they burst at).
const STOPS: [(f64, (u8, u8, u8)); 5] =
    [(0.0, (0x9a, 0xa0, 0xa6)), (3000.0, (0x7e, 0xe7, 0x87)), (12000.0, (0xf2, 0xcc, 0x60)), (22000.0, (0xff, 0x9f, 0x43)), (32000.0, (0xff, 0x6b, 0xcb))];

fn altitude_rgb(alt: f64) -> (u8, u8, u8) {
    let i = STOPS.iter().position(|&(a, _)| alt < a).unwrap_or(STOPS.len());
    if i == 0 {
        return STOPS[0].1;
    }
    if i == STOPS.len() {
        return STOPS[STOPS.len() - 1].1;
    }
    let ((a0, c0), (a1, c1)) = (STOPS[i - 1], STOPS[i]);
    let f = (alt - a0) / (a1 - a0);
    let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * f).round() as u8;
    (mix(c0.0, c1.0), mix(c0.1, c1.1), mix(c0.2, c1.2))
}

/// The style layer for balloons from source `source`: ▲ / ▼ colored by altitude, with
/// their serials.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    let mut stops = vec![json!("interpolate"), json!(["linear"]), json!(["get", "alt"])];
    for (a, (r, g, b)) in STOPS {
        stops.push(json!(a));
        stops.push(json!(format!("#{r:02x}{g:02x}{b:02x}")));
    }
    crate::StyleLayer::symbol(id, source).glyph(json!(["get", "glyph"])).text(json!(["get", "label"])).color("#c9d1d9").paint("icon-color", Json::Array(stops))
}

/// The area to follow: a circle, (lat, lon, radius in meters).
pub type Area = (f64, f64, f64);

/// What [`stream`] tells the viewer.
#[derive(Debug)]
pub enum Event {
    /// The balloons the API knew around the area (once per area).
    Snapshot(Vec<Telemetry>),
    /// A live report.
    Update(Telemetry),
    /// A landing prediction.
    Prediction(Prediction),
    /// Connected, following this many balloons.
    Following(usize),
    /// Something failed; trying again after this pause.
    Retrying(String, Duration),
}

/// A client id unique to this run, prefixed as SondeHub asks.
fn client_id() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("mapterm-{:x}{:x}", std::process::id(), t & 0xffff_ffff)
}

/// Follow the balloons around `area` (None: nothing) and the predictions for the serial in
/// `selected`, sending what comes on `tx`; returns when `tx` closes. Errors back off 5 s,
/// doubling to 5 minutes.
pub async fn stream(
    mut area: tokio::sync::watch::Receiver<Option<Area>>,
    mut selected: tokio::sync::watch::Receiver<Option<String>>,
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
) {
    let mut pause = Duration::from_secs(5);
    loop {
        let Some(mut here) = *area.borrow_and_update() else {
            if area.changed().await.is_err() {
                return;
            }
            continue;
        };
        match follow(&mut here, &mut area, &mut selected, &tx).await {
            Ok(()) => return,
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

/// Balloons around `here` worth following: heard in the last hour.
async fn near(here: Area, tx: &tokio::sync::mpsc::UnboundedSender<Event>) -> Result<HashSet<String>> {
    let list = fetch_near(here.0, here.1, here.2, 3 * 3600).await?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let follow = list.iter().filter(|t| now - t.t < 3600.0).map(|t| t.serial.clone()).collect();
    let _ = tx.send(Event::Snapshot(list));
    Ok(follow)
}

async fn follow(
    here: &mut Area,
    area: &mut tokio::sync::watch::Receiver<Option<Area>>,
    selected: &mut tokio::sync::watch::Receiver<Option<String>>,
    tx: &tokio::sync::mpsc::UnboundedSender<Event>,
) -> Result<()> {
    use crate::ws::Message;
    let mut serials = near(*here, tx).await?;
    let url = String::from_utf8(get("/sondes/websocket").await?).context("SondeHub: websocket URL")?;
    // Compression offered, as SondeHub asks (its reader doesn't take it up today).
    let opts = crate::ws::Options { protocol: Some("mqtt"), deflate: true };
    let mut ws = tokio::time::timeout(Duration::from_secs(30), crate::ws::connect(url.trim(), opts)).await.context("connecting timed out")??;
    ws.send_binary(&mqtt::connect(&client_id(), 60)).await?;
    let mut ids = 1u16..;
    let mut next_id = move || ids.next().unwrap_or(1);
    let topics = |set: &HashSet<String>| set.iter().map(|s| format!("sondes/{s}")).collect::<Vec<_>>();
    let mut subscribed = false;
    let mut predicting: Option<String> = None;
    let mut buf = Vec::new();
    let mut ping = tokio::time::interval(Duration::from_secs(45));
    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg?.context("the stream closed")? {
                    Message::Binary(b) => buf.extend_from_slice(&b),
                    Message::Close(_) => bail!("the stream closed"),
                    Message::Text(_) => continue,
                }
                while let Some(p) = mqtt::take(&mut buf) {
                    match p {
                        mqtt::Packet::ConnAck(0) => {
                            let mut t = topics(&serials);
                            t.push("sondes-new/#".into());
                            ws.send_binary(&mqtt::subscribe(next_id(), &t)).await?;
                            subscribed = true;
                            let _ = tx.send(Event::Following(serials.len()));
                            // The selected balloon's prediction, if one was chosen already.
                            selected.mark_changed();
                        }
                        mqtt::Packet::ConnAck(code) => bail!("SondeHub refused the connection (MQTT code {code})"),
                        mqtt::Packet::Publish { topic, payload } => {
                            if let Some(serial) = topic.strip_prefix("prediction/") {
                                let p = serde_json::from_slice::<Json>(&payload).ok().and_then(|v| match v {
                                    Json::Array(a) => a.iter().find_map(parse_prediction),
                                    v => parse_prediction(&v),
                                });
                                if let Some(p) = p.filter(|p| p.serial == serial) {
                                    let _ = tx.send(Event::Prediction(p));
                                }
                            } else if topic.starts_with("sondes-new/") {
                                // A balloon just heard somewhere: follow it if it's near.
                                for t in parse_payload(&payload) {
                                    let d = crate::adsb::distance_nm((here.1, here.0), (t.lon, t.lat)) * 1852.0;
                                    if d <= here.2 && serials.insert(t.serial.clone()) {
                                        ws.send_binary(&mqtt::subscribe(next_id(), &[format!("sondes/{}", t.serial)])).await?;
                                        let _ = tx.send(Event::Update(t));
                                    }
                                }
                            } else {
                                for t in parse_payload(&payload) {
                                    let _ = tx.send(Event::Update(t));
                                }
                            }
                        }
                        mqtt::Packet::Other(_) => {}
                    }
                }
            }
            _ = ping.tick() => {
                ws.send_binary(&mqtt::ping()).await?;
            }
            changed = area.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let Some(new) = *area.borrow_and_update() else { continue };
                *here = new;
                // Ask once for the new area, then follow what's new and drop what's far.
                let now = near(new, tx).await?;
                let add: Vec<String> = now.difference(&serials).cloned().collect();
                let drop: Vec<String> = serials.difference(&now).cloned().collect();
                if subscribed && !add.is_empty() {
                    ws.send_binary(&mqtt::subscribe(next_id(), &topics(&add.iter().cloned().collect()))).await?;
                }
                if subscribed && !drop.is_empty() {
                    ws.send_binary(&mqtt::unsubscribe(next_id(), &topics(&drop.iter().cloned().collect()))).await?;
                }
                serials = now;
                let _ = tx.send(Event::Following(serials.len()));
            }
            changed = selected.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let want = selected.borrow_and_update().clone();
                if !subscribed || want == predicting {
                    continue;
                }
                if let Some(old) = predicting.take() {
                    ws.send_binary(&mqtt::unsubscribe(next_id(), &[format!("prediction/{old}")])).await?;
                }
                if let Some(s) = want {
                    ws.send_binary(&mqtt::subscribe(next_id(), &[format!("prediction/{s}")])).await?;
                    // The latest one now, rather than at the next update (a minute or so).
                    let (tx, s2) = (tx.clone(), s.clone());
                    tokio::spawn(async move {
                        if let Ok(Some(p)) = fetch_prediction(&s2).await {
                            let _ = tx.send(Event::Prediction(p));
                        }
                    });
                    predicting = Some(s);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = r#"{"serial":"Y0922379","type":"RS41","subtype":"RS41-SGP","manufacturer":"Vaisala","lat":43.4855,"lon":-77.2331,"alt":31995.4,
        "vel_v":4.22,"vel_h":22.7,"heading":69.8,"temp":-45.1,"humidity":3.0,"batt":2.6,"frequency":401.5,"datetime":"2026-11-26T00:57:16.999Z","uploader_callsign":"VE3IRR-2"}"#;

    #[test]
    fn parses_times() {
        assert_eq!(unix_time("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(unix_time("2000-03-01T00:00:00Z"), Some(951868800.0));
        assert!((unix_time("2026-11-26T00:57:16.999Z").unwrap() - 1795654636.999).abs() < 1e-6);
        assert_eq!(unix_time("yesterday"), None);
    }

    #[test]
    fn parses_reports_and_predictions() {
        let t = parse_payload(REPORT.as_bytes()).remove(0);
        assert_eq!((t.serial.as_str(), t.kind.as_deref(), t.alt, t.vel_v), ("Y0922379", Some("RS41-SGP"), Some(31995.4), Some(4.22)));
        assert_eq!(parse_payload(format!("[{REPORT},{REPORT}]").as_bytes()).len(), 2);
        assert!(parse_payload(br#"{"serial":"X","lat":0,"lon":0,"datetime":"2026-11-26T00:00:00Z"}"#).is_empty(), "0,0 is no fix");
        let p = parse_prediction(&serde_json::json!({"vehicle":"Y0922379","burst_altitude":33800.0,"descending":0,
            "data":"[{\"time\":1795654740,\"lat\":43.48,\"lon\":-77.23,\"alt\":31995},{\"time\":1795659000,\"lat\":43.2,\"lon\":-76.1,\"alt\":120}]"}))
        .unwrap();
        assert_eq!(p.landing(), Some((1795659000.0, -76.1, 43.2)));
        assert!(!p.descending);
    }

    #[test]
    fn gunzips_what_comes_gzipped() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, b"[1]").unwrap();
        assert_eq!(body(gz.finish().unwrap()), b"[1]");
        assert_eq!(body(b"[2]".to_vec()), b"[2]");
    }

    #[test]
    fn mqtt_round_trip() {
        // A publish split across two WebSocket frames comes out whole, once.
        let mut pkt = vec![0x30];
        let body = [&[0u8, 13][..], b"sondes/Y09223", b"{\"a\":1}"].concat();
        pkt.push(body.len() as u8);
        pkt.extend(&body);
        let mut buf = pkt[..5].to_vec();
        assert_eq!(mqtt::take(&mut buf), None);
        buf.extend(&pkt[5..]);
        buf.extend([0x20, 2, 0, 0]);
        assert_eq!(mqtt::take(&mut buf), Some(mqtt::Packet::Publish { topic: "sondes/Y09223".into(), payload: b"{\"a\":1}".to_vec() }));
        assert_eq!(mqtt::take(&mut buf), Some(mqtt::Packet::ConnAck(0)));
        assert!(buf.is_empty());
        // Lengths over 127 take two bytes.
        let sub = mqtt::subscribe(1, &["x".repeat(200)]);
        assert_eq!((sub[0], sub[1] & 0x80, sub.len()), (0x82, 0x80, 3 + 2 + 2 + 200 + 1));
        assert_eq!(&mqtt::connect("mapterm-1", 60)[..2], &[0x10, 21]);
    }

    #[test]
    fn balloons_follow_gps_time() {
        let mut b = Balloons::default();
        let mut t = parse_payload(REPORT.as_bytes()).remove(0);
        b.apply(t.clone(), t.t + 2.0);
        t.t += 10.0;
        t.lat += 0.01;
        t.alt = Some(32037.6);
        b.apply(t.clone(), t.t + 2.0);
        let (bal, at) = b.get("Y0922379", t.t - 5.0).unwrap();
        assert_eq!(bal.glyph(), "▲");
        assert!((at.lat - 43.4905).abs() < 1e-9 && (at.alt.unwrap() - 32016.5).abs() < 0.01);
        assert_eq!(b.points(t.t)["features"][0]["properties"]["label"], "Y0922379");
        b.prune(t.t + 3700.0);
        assert!(b.is_empty());
    }
}
