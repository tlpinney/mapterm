//! NASA's scientific balloons, from the [Columbia Scientific Balloon Facility](https://www.csbf.nasa.gov)
//! (CSBF), which flies them from Palestine (Texas), Fort Sumner (New Mexico), McMurdo
//! (Antarctica), Wanaka (New Zealand) and Kiruna (Sweden).
//!
//! CSBF publishes each flight's latest payload position as a small web page ("Near Real Time
//! Payload Data": time, latitude and longitude in degrees and minutes, altitude, speed and
//! heading), linked from each campaign's page, which the home page links. There's no feed: the
//! viewer finds the flight pages from the campaigns about once an hour, and reads the pages of
//! the flights heard lately every couple of minutes. Public domain (a NASA work).

use anyhow::{Context, Result};
use std::collections::HashSet;

/// The facility's site.
pub const HOME: &str = "https://www.csbf.nasa.gov/";
/// The credit.
pub const CREDIT: &str = "Scientific balloons: NASA CSBF";
/// A flight whose position is newer than this (seconds) is flying, or just landed: it's
/// shown, and its page read often.
pub const RECENT: f64 = 3.0 * 86400.0;
/// How often a recent flight's page is read again, seconds.
pub const REFRESH: u64 = 120;
/// How often the campaigns are looked through for flights, seconds.
pub const DISCOVER: u64 = 3600;

/// A flight's latest reported payload position.
#[derive(Clone, Debug, PartialEq)]
pub struct Flight {
    /// Its page.
    pub url: String,
    /// The payload (the science group's name), such as `PUEO`.
    pub name: String,
    /// The flight number, such as `760N`.
    pub id: String,
    /// When the position was taken (Unix seconds).
    pub t: f64,
    /// Degrees.
    pub lat: f64,
    /// Degrees.
    pub lon: f64,
    /// Feet.
    pub alt_ft: f64,
    /// Knots.
    pub speed_kt: f64,
    /// Degrees from north.
    pub heading: f64,
    /// Its whole track so far (`TrackNNN.xml`, beside the page), when the page links one.
    pub track_url: Option<String>,
}

/// One point of a flight's track: (Unix seconds, lon, lat, feet).
pub type TrackPoint = (f64, f64, f64, f64);

/// A page's characters: UTF-8 when it is (some pages are), else ISO-8859-1 (the ° is 0xB0).
fn decode(page: &[u8]) -> String {
    match std::str::from_utf8(page) {
        Ok(s) => s.to_string(),
        Err(_) => page.iter().map(|&b| b as char).collect(),
    }
}

/// A page's text, without tags.
fn text(page: &[u8]) -> String {
    let s = decode(page);
    let mut out = String::with_capacity(s.len());
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => {
                tag = false;
                out.push(' ');
            }
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    // The entities these pages use (older ones write the degree sign as one).
    let out = out.replace("&deg;", "°").replace("&#176;", "°").replace("&nbsp;", " ").replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Degrees from `87°22.16 S` (degrees, minutes, hemisphere).
fn degrees(s: &str) -> Option<f64> {
    let s = s.trim();
    let (deg, rest) = s.split_once('°')?;
    let mut parts = rest.split_whitespace();
    let min: f64 = parts.next()?.parse().ok()?;
    let hemi = parts.next()?;
    let v = deg.trim().parse::<f64>().ok()? + min / 60.0;
    match hemi {
        "N" | "E" => Some(v),
        "S" | "W" => Some(-v),
        _ => None,
    }
}

/// The text after `label` up to the next `stop` (or the end).
fn after<'a>(t: &'a str, label: &str, stop: &str) -> Option<&'a str> {
    let rest = &t[t.find(label)? + label.len()..];
    Some(rest.split(stop).next().unwrap_or(rest).trim())
}

/// Unix seconds for `16:14:57Z 01/20/26` (UTC, month/day/two-digit year).
fn when(s: &str) -> Option<f64> {
    let mut p = s.split_whitespace();
    let (time, date) = (p.next()?.trim_end_matches('Z'), p.next()?);
    let mut d = date.split('/').map(|x| x.parse::<u32>());
    let (m, day, y) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    crate::sondehub::unix_time(&format!("{:04}-{m:02}-{day:02}T{time}Z", 2000 + y))
}

/// A flight from its page (at `url`).
pub fn parse_flight(url: &str, page: &[u8]) -> Option<Flight> {
    let t = text(page);
    // "PUEO - Flight 760N - LDB Relayed Real Time GPS Data"
    let head = &t[..t.find(" - Flight ")?];
    let name = head.rsplit("Facility").next().unwrap_or(head).trim().to_string();
    let id = after(&t, " - Flight ", " ")?.to_string();
    let lat = degrees(after(&t, "Latitude:", "Longitude:")?)?;
    let lon = degrees(after(&t, "Longitude:", "Altitude:")?)?;
    let alt_ft: f64 = after(&t, "Altitude:", "Feet")?.parse().ok()?;
    let motion = after(&t, "Feet", "°")?; // "5.33 Knots @ 165.00"
    let (kt, hd) = motion.split_once("Knots @")?;
    let t = when(after(&t, "Payload position as of:", "Latitude:")?)?;
    // The map page beside it (`Google760N.htm`) draws `Track760N.xml`.
    let track_url = links(url, page).into_iter().find_map(|u| {
        let (dir, file) = u.rsplit_once('/')?;
        let rest = file.strip_prefix("Google").or_else(|| file.strip_prefix("google"))?;
        Some(format!("{dir}/Track{}.xml", rest.strip_suffix(".htm").or_else(|| rest.strip_suffix(".html"))?))
    });
    let ok = lat.abs() <= 90.0 && lon.abs() <= 180.0 && !(lat == 0.0 && lon == 0.0);
    ok.then(|| Flight {
        url: url.to_string(),
        name,
        id,
        t,
        lat,
        lon,
        alt_ft,
        speed_kt: kt.trim().parse().unwrap_or(0.0),
        heading: hd.trim().parse().unwrap_or(0.0),
        track_url,
    })
}

/// A track's points from its XML (`<trkpt lat= lon= alt= time="12/01/25 01:35:07"/>`, UTC,
/// feet), in time order.
pub fn parse_track(xml: &[u8]) -> Vec<TrackPoint> {
    let s = String::from_utf8_lossy(xml);
    let attr = |tag: &str, name: &str| -> Option<String> {
        let at = tag.find(&format!("{name}=\""))? + name.len() + 2;
        Some(tag[at..].split('"').next()?.to_string())
    };
    let mut v: Vec<TrackPoint> = s
        .split("<trkpt")
        .skip(1)
        .filter_map(|tag| {
            let tag = tag.split("/>").next()?;
            let lat: f64 = attr(tag, "lat")?.parse().ok()?;
            let lon: f64 = attr(tag, "lon")?.parse().ok()?;
            let alt: f64 = attr(tag, "alt").and_then(|a| a.parse().ok()).unwrap_or(0.0);
            let time = attr(tag, "time")?;
            let (date, clock) = time.split_once(' ')?;
            let t = when(&format!("{clock} {date}"))?;
            (lat.abs() <= 90.0 && lon.abs() <= 180.0 && !(lat == 0.0 && lon == 0.0)).then_some((t, lon, lat, alt))
        })
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

/// A flight's track.
pub async fn track(url: &str) -> Result<Vec<TrackPoint>> {
    Ok(parse_track(&get(url).await?))
}

/// The links in a page, made absolute against `base`.
fn links(base: &str, page: &[u8]) -> Vec<String> {
    let s = decode(page);
    let lower = s.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("href=") {
        let start = at + i + 5;
        let rest = &s[start..];
        let (quote, rest) = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => (Some(q), &rest[1..]),
            _ => (None, rest),
        };
        let end = rest.find(|c: char| Some(c) == quote || (quote.is_none() && (c.is_whitespace() || c == '>'))).unwrap_or(rest.len());
        if let Some(u) = absolute(base, rest[..end].trim()) {
            out.push(u);
        }
        at = start + 1;
    }
    out
}

/// `href` against the page at `base`.
fn absolute(base: &str, href: &str) -> Option<String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }
    if href.is_empty() || href.starts_with('#') || href.starts_with("mailto:") || href.starts_with("javascript:") {
        return None;
    }
    let origin = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    let dir = &base[..base.rfind('/')? + 1];
    // Resolve ../ against the folder.
    let mut parts: Vec<&str> = dir.trim_start_matches(&origin).split('/').filter(|p| !p.is_empty()).collect();
    let mut h = href;
    while let Some(r) = h.strip_prefix("../") {
        parts.pop();
        h = r;
    }
    let h = h.trim_start_matches("./");
    let path = if parts.is_empty() { String::new() } else { format!("{}/", parts.join("/")) };
    Some(format!("{origin}/{path}{h}"))
}

/// Whether `url` is a CSBF flight page (`.../map/balloonN/flightNNNx.htm`).
fn is_flight(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains("csbf.nasa.gov/map/balloon") && u.rsplit('/').next().is_some_and(|f| f.starts_with("flight") && f.ends_with(".htm"))
}

/// Whether `url` is a campaign page on the site (one folder deep, `.htm`).
fn is_campaign(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.starts_with("https://www.csbf.nasa.gov/") && u.ends_with(".htm") && !u.contains("/map/") && u.matches('/').count() == 4
}

async fn get(url: &str) -> Result<Vec<u8>> {
    crate::source::get_limited(&crate::source::http_client(), url, &[], 1 << 20).await?.with_context(|| format!("{url}: not found"))
}

/// The flight pages the campaigns link to, from the home page.
pub async fn discover() -> Result<Vec<String>> {
    let home = get(HOME).await.context("CSBF")?;
    let campaigns: HashSet<String> = links(HOME, &home).into_iter().map(|u| u.replace("http://", "https://")).filter(|u| is_campaign(u)).collect();
    let mut flights: Vec<String> = links(HOME, &home).into_iter().filter(|u| is_flight(u)).collect();
    for c in campaigns {
        // One campaign's page failing leaves the others.
        if let Ok(page) = get(&c).await {
            flights.extend(links(&c, &page).into_iter().filter(|u| is_flight(u)));
        }
    }
    let mut seen = HashSet::new();
    flights.retain(|u| seen.insert(u.to_ascii_lowercase().replace("http://", "https://")));
    Ok(flights)
}

/// A flight's latest position, from its page.
pub async fn flight(url: &str) -> Result<Option<Flight>> {
    Ok(parse_flight(url, &get(url).await?))
}

/// The flights as GeoJSON points: ◓ with the payload's name, and what's known; the recent
/// ones, or `all` (past flights where they came down, dimmed).
pub fn points(flights: &[Flight], now: f64, all: bool) -> serde_json::Value {
    let features: Vec<serde_json::Value> = flights
        .iter()
        .filter(|f| all || now - f.t <= RECENT)
        .map(|f| {
            serde_json::json!({
                "type": "Feature",
                "id": format!("csbf:{}", f.id),
                "geometry": {"type": "Point", "coordinates": [f.lon, f.lat]},
                "properties": {
                    "glyph": "◓", "label": f.name, "name": f.name, "flight": f.id, "alt_ft": f.alt_ft,
                    "speed_kt": f.speed_kt, "heading": f.heading, "minutes_ago": ((now - f.t) / 60.0).round(),
                    "page": f.url, "kind": "NASA scientific balloon", "recent": now - f.t <= RECENT,
                    "last_position": crate::clock::Zone::Utc.hhmm_zone(f.t),
                }
            })
        })
        .collect();
    serde_json::json!({"type": "FeatureCollection", "features": features})
}

/// The flights' tracks as GeoJSON lines (`tracks`: flight id and its points).
pub fn track_lines<'a>(tracks: impl Iterator<Item = (&'a String, &'a Vec<TrackPoint>)>) -> serde_json::Value {
    let features: Vec<serde_json::Value> = tracks
        .filter(|(_, pts)| pts.len() >= 2)
        .map(|(id, pts)| {
            let coords: Vec<[f64; 2]> = pts.iter().map(|&(_, lon, lat, _)| [lon, lat]).collect();
            serde_json::json!({"type": "Feature", "geometry": {"type": "LineString", "coordinates": coords}, "properties": {"flight": id}})
        })
        .collect();
    serde_json::json!({"type": "FeatureCollection", "features": features})
}

/// The style layer: ◓ in NASA's red (dim for a past flight), with the payload's name.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source)
        .glyph(serde_json::json!(["get", "glyph"]))
        .text(serde_json::json!(["get", "label"]))
        .color("#ff6b6b")
        .paint("icon-color", serde_json::json!(["case", ["get", "recent"], "#ff6b6b", "#8b5a5a"]))
}

/// The style layer for the tracks: thin red lines.
pub fn track_layer(id: &str, source: &str) -> crate::StyleLayer {
    let v = serde_json::json!({"id": id, "type": "line", "source": source, "paint": {"line-color": "#ff6b6b", "line-width": 1, "line-opacity": 0.7}});
    crate::StyleLayer::from_json(&v).expect("a valid line layer").0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flight page as CSBF serves it (ISO-8859-1: the ° is byte 0xB0).
    fn page() -> Vec<u8> {
        let html = "<HTML><HEAD><TITLE>Real Time LDB Data</TITLE></HEAD><BODY>\r\n<CAPTION><CENTER>\r\n<H2>Columbia Scientific Balloon Facility</CENTER></H2>\r\n<CENTER><H2><FONT SIZE=+1>PUEO - Flight 760N - LDB Relayed Real Time GPS \r\nData</FONT></CENTER></H2></CAPTION><TR><TD>\r\n<CENTER>Payload position as of: </CENTER>\r\n<CENTER>16:14:57Z  01/20/26 </CENTER>\r\n<CENTER>Latitude:  87\u{b0}22.16  S</CENTER>\r\n<CENTER>Longitude:  17\u{b0}3.32  E</CENTER>\r\n<CENTER>Altitude:  8739  Feet</CENTER>\r\n<CENTER>5.33 Knots @ 165.00\u{b0} </CENTER>\r\n<TR><TD><A HREF=\"/map/balloon9/balloon9.png\">Full Size Payload Track Image</A></TD>\r\n<TD><A HREF=\"Google760N.htm\">Google Maps</A></TD></BODY></HTML>";
        // As Latin-1 bytes, the way the server sends it.
        html.chars().map(|c| c as u32 as u8).collect()
    }

    #[test]
    fn reads_a_flight_page() {
        let f = parse_flight("https://www.csbf.nasa.gov/map/balloon9/flight760N.htm", &page()).expect("a flight");
        assert_eq!((f.name.as_str(), f.id.as_str()), ("PUEO", "760N"));
        assert!((f.lat - -(87.0 + 22.16 / 60.0)).abs() < 1e-9);
        assert!((f.lon - (17.0 + 3.32 / 60.0)).abs() < 1e-9);
        assert_eq!((f.alt_ft, f.speed_kt, f.heading), (8739.0, 5.33, 165.0));
        assert_eq!(f.t, crate::sondehub::unix_time("2026-01-20T16:14:57Z").unwrap());
        assert!(parse_flight("x", b"<html>nothing here</html>").is_none());
        assert_eq!(f.track_url.as_deref(), Some("https://www.csbf.nasa.gov/map/balloon9/Track760N.xml"));
    }

    #[test]
    fn reads_utf8_pages_too() {
        let latin1 = page();
        let utf8: Vec<u8> = latin1.iter().map(|&b| b as char).collect::<String>().into_bytes();
        assert_ne!(latin1, utf8);
        assert_eq!(parse_flight("u", &utf8).map(|f| f.lat), parse_flight("u", &latin1).map(|f| f.lat));
    }

    #[test]
    fn reads_pages_with_entities() {
        let p = b"<H2>Columbia Scientific Balloon Facility</H2><H2>CLIC 1 - Flight 1602P - LDB Relayed Real Time GPS Data</H2> Payload position as of: 19:35:11Z 07/20/26 Latitude: 31&deg;44.46 N Longitude: 101&#176;11.65 W Altitude: 2566 Feet 0.00 Knots @ 0.00&deg;";
        let f = parse_flight("u", p).expect("a flight");
        assert_eq!((f.name.as_str(), f.id.as_str(), f.alt_ft), ("CLIC 1", "1602P", 2566.0));
        assert!((f.lon - -(101.0 + 11.65 / 60.0)).abs() < 1e-9);
    }

    #[test]
    fn reads_a_track() {
        let xml = br#"<Track>
<Waypoint lat="-77.8551" lon="167.1783" alt="109.0" name="Williams Field Antarctica" />
<trk><trkseg>
<trkpt lat="-77.8557" lon="167.1534" alt="0" speed="0.00" heading="0.00" time="12/07/25 22:58:21" />
<trkpt lat="-77.8553" lon="167.1519" alt="0" speed="0.00" heading="0.00" time="12/01/25 01:35:07" />
<trkpt lat="0" lon="0" alt="0" time="12/08/25 00:00:00" />
<trkpt lat="-78.1" lon="166.0" alt="120000" speed="20" heading="90" time="12/19/25 11:04:20" />
</trkseg></trk></Track>"#;
        let t = parse_track(xml);
        assert_eq!(t.len(), 3, "the 0,0 point left out");
        assert!(t.windows(2).all(|w| w[0].0 <= w[1].0), "in time order");
        assert_eq!(t[0].0, crate::sondehub::unix_time("2025-12-01T01:35:07Z").unwrap());
        assert_eq!((t[2].1, t[2].2, t[2].3), (166.0, -78.1, 120000.0));
        let lines = track_lines([("760N".to_string(), t)].iter().map(|(a, b)| (a, b)));
        assert_eq!(lines["features"][0]["geometry"]["coordinates"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn finds_flight_and_campaign_links() {
        let home = br#"<a href="palestine/psn.htm">Palestine</a> <a href="https://www.csbf.nasa.gov/antarctica/payloads.htm">x</a>
            <a href="mailto:a@b">m</a> <a href="http://www.nasa.gov">n</a> <a href="https://www.csbf.nasa.gov/map/balloon4/flight1602p.htm">CLIC</a>"#;
        let l = links(HOME, home);
        assert!(l.contains(&"https://www.csbf.nasa.gov/palestine/psn.htm".to_string()));
        assert_eq!(l.iter().filter(|u| is_campaign(u)).count(), 2);
        assert_eq!(l.iter().filter(|u| is_flight(u)).count(), 1);
        let camp = br#"<a href="../map/balloon6/flight759N.htm">GAPS</a><a href='../map/balloon9/flight760N.htm'>PUEO</a>"#;
        let f = links("https://www.csbf.nasa.gov/antarctica/payloads.htm", camp);
        assert_eq!(f, vec!["https://www.csbf.nasa.gov/map/balloon6/flight759N.htm", "https://www.csbf.nasa.gov/map/balloon9/flight760N.htm"]);
        assert!(f.iter().all(|u| is_flight(u)));
    }

    #[test]
    fn only_recent_flights_are_drawn() {
        let f = parse_flight("u", &page()).unwrap();
        assert_eq!(points(std::slice::from_ref(&f), f.t + 3600.0, false)["features"].as_array().unwrap().len(), 1);
        assert_eq!(points(std::slice::from_ref(&f), f.t + RECENT + 1.0, false)["features"].as_array().unwrap().len(), 0);
        let all = points(std::slice::from_ref(&f), f.t + RECENT + 1.0, true);
        assert_eq!(all["features"][0]["properties"]["recent"], false, "a past flight, dimmed");
    }
}
