//! Satellites from [CelesTrak](https://celestrak.org)'s orbital elements, placed with SGP4.
//!
//! Positions are computed here, every frame, from each satellite's elements: no feed to
//! follow, and nothing on the network once the elements are in. CelesTrak updates them every
//! 2 hours and asks for one download per update, blocks a second one with HTTP 403, and
//! firewalls an address after 50 errors in 2 hours. So [`load`] keeps each group in
//! `~/.cache/mapterm/celestrak/<group>.csv` and downloads only when that copy is more than 2
//! hours old; after any error it leaves a marker so that no run asks again for 2 hours, and
//! uses the old copy. It asks for CSV (a third the size of JSON) and gzip, as CelesTrak asks.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value as Json, json};

/// How long CelesTrak's elements stay current.
const FRESH: Duration = Duration::from_secs(2 * 3600);
/// J2000 (2000-01-01 12:00 UTC) as Unix seconds.
const J2000: f64 = 946_728_000.0;
/// WGS84.
const EARTH_A: f64 = 6378.137;
const EARTH_F: f64 = 1.0 / 298.257_223_563;

/// One satellite and its orbit.
pub struct Sat {
    /// Its name, as CelesTrak gives it (`ISS (ZARYA)`).
    pub name: String,
    /// NORAD catalog number.
    pub norad: u64,
    /// International designator (launch year and piece, `1998-067A`).
    pub intl: Option<String>,
    /// Orbits a day.
    pub mean_motion: f64,
    /// Degrees.
    pub inclination: f64,
    constants: sgp4::Constants,
    /// The elements' epoch, Unix seconds.
    epoch: f64,
}

/// Where a satellite is at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    /// Degrees.
    pub lon: f64,
    /// Degrees.
    pub lat: f64,
    /// Kilometers above the ellipsoid.
    pub alt_km: f64,
    /// Speed, km/s.
    pub speed: f64,
    /// Earth-fixed position, km (for look angles).
    ecef: [f64; 3],
}

/// A pass over an observer: when it rises above the horizon, peaks and sets (Unix seconds),
/// and how high it gets (degrees).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    /// Rises above the horizon.
    pub rise: f64,
    /// Highest point.
    pub peak: f64,
    /// Its elevation then, degrees.
    pub max_elevation: f64,
    /// Sets below the horizon.
    pub set: f64,
}

/// Greenwich sidereal angle at Unix time `t` (radians), for turning SGP4's inertial (TEME)
/// positions into Earth-fixed ones.
fn sidereal(t: f64) -> f64 {
    sgp4::iau_epoch_to_sidereal_time((t - J2000) / (365.25 * 86400.0))
}

/// Earth-fixed position (km) to longitude, latitude (degrees) and height (km), WGS84.
fn geodetic([x, y, z]: [f64; 3]) -> (f64, f64, f64) {
    let e2 = EARTH_F * (2.0 - EARTH_F);
    let p = (x * x + y * y).sqrt();
    let mut lat = z.atan2(p * (1.0 - e2));
    let mut h = 0.0;
    for _ in 0..6 {
        let n = EARTH_A / (1.0 - e2 * lat.sin().powi(2)).sqrt();
        h = p / lat.cos() - n;
        lat = z.atan2(p * (1.0 - e2 * n / (n + h)));
    }
    (y.atan2(x).to_degrees(), lat.to_degrees(), h)
}

/// A point on the ground (degrees, km) as an Earth-fixed position.
fn ecef(lon: f64, lat: f64, h: f64) -> [f64; 3] {
    let e2 = EARTH_F * (2.0 - EARTH_F);
    let (lat, lon) = (lat.to_radians(), lon.to_radians());
    let n = EARTH_A / (1.0 - e2 * lat.sin().powi(2)).sqrt();
    [(n + h) * lat.cos() * lon.cos(), (n + h) * lat.cos() * lon.sin(), (n * (1.0 - e2) + h) * lat.sin()]
}

/// Azimuth (from north, clockwise) and elevation, degrees, of a satellite at `sat` seen from
/// (`lon`, `lat`, `h` km).
pub fn look((lon, lat, h): (f64, f64, f64), sat: &Fix) -> (f64, f64) {
    let o = ecef(lon, lat, h);
    let r = [sat.ecef[0] - o[0], sat.ecef[1] - o[1], sat.ecef[2] - o[2]];
    let (la, lo) = (lat.to_radians(), lon.to_radians());
    let south = la.sin() * lo.cos() * r[0] + la.sin() * lo.sin() * r[1] - la.cos() * r[2];
    let east = -lo.sin() * r[0] + lo.cos() * r[1];
    let up = la.cos() * lo.cos() * r[0] + la.cos() * lo.sin() * r[1] + la.sin() * r[2];
    let range = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
    (east.atan2(-south).to_degrees().rem_euclid(360.0), (up / range).asin().to_degrees())
}

impl Sat {
    /// Build one from its OMM fields (CelesTrak's CSV columns or JSON keys).
    pub fn from_omm(fields: Map<String, Json>) -> Result<Sat> {
        let elements: sgp4::Elements = serde_json::from_value(Json::Object(fields)).context("orbital elements")?;
        let constants = sgp4::Constants::from_elements(&elements).map_err(|e| anyhow::anyhow!("orbital elements: {e}"))?;
        let epoch = elements.datetime.and_utc().timestamp_micros() as f64 / 1e6;
        Ok(Sat {
            name: elements.object_name.clone().unwrap_or_else(|| elements.norad_id.to_string()),
            norad: elements.norad_id,
            intl: elements.international_designator.clone(),
            mean_motion: elements.mean_motion,
            inclination: elements.inclination,
            constants,
            epoch,
        })
    }

    /// Where it is at Unix time `t` (None when SGP4 gives up, as for a decayed orbit).
    pub fn at(&self, t: f64) -> Option<Fix> {
        let p = self.constants.propagate(sgp4::MinutesSinceEpoch((t - self.epoch) / 60.0)).ok()?;
        let th = sidereal(t);
        let [x, y, z] = p.position;
        let e = [th.cos() * x + th.sin() * y, -th.sin() * x + th.cos() * y, z];
        let (lon, lat, alt_km) = geodetic(e);
        let [vx, vy, vz] = p.velocity;
        Some(Fix { lon, lat, alt_km, speed: (vx * vx + vy * vy + vz * vz).sqrt(), ecef: e })
    }

    /// Whether it's one of SpaceX's Starlink satellites (thousands of them: drawn small and
    /// unnamed).
    pub fn is_starlink(&self) -> bool {
        self.name.starts_with("STARLINK")
    }

    /// Its marker: ◆ the ISS, · a Starlink satellite, ◇ any other.
    pub fn glyph(&self) -> &'static str {
        match self {
            s if s.norad == 25544 => "◆",
            s if s.is_starlink() => "·",
            _ => "◇",
        }
    }

    /// Minutes per orbit.
    pub fn period_min(&self) -> f64 {
        1440.0 / self.mean_motion
    }

    /// The ground track from `t` on, `minutes` long, as lines broken where it crosses the
    /// antimeridian.
    pub fn track(&self, t: f64, minutes: f64) -> Vec<Vec<(f64, f64)>> {
        let mut parts: Vec<Vec<(f64, f64)>> = vec![Vec::new()];
        let mut last: Option<f64> = None;
        let steps = (minutes * 2.0).ceil() as usize;
        for i in 0..=steps {
            let Some(f) = self.at(t + i as f64 * 30.0) else { break };
            if last.is_some_and(|l| (f.lon - l).abs() > 180.0) {
                parts.push(Vec::new());
            }
            last = Some(f.lon);
            if let Some(p) = parts.last_mut() {
                p.push((f.lon, f.lat));
            }
        }
        parts.retain(|p| p.len() >= 2);
        parts
    }

    /// The next pass over `observer` (lon, lat, km) that peaks at least `min_elevation`
    /// degrees up, within `hours` from `t`.
    pub fn next_pass(&self, observer: (f64, f64, f64), t: f64, hours: f64, min_elevation: f64) -> Option<Pass> {
        let el = |t: f64| self.at(t).map(|f| look(observer, &f).1);
        // The moment between a and b (one below the horizon, one above) where it crosses.
        let cross = |mut a: f64, mut b: f64| {
            let up_at_b = el(b).unwrap_or(-90.0) > 0.0;
            for _ in 0..20 {
                let m = (a + b) / 2.0;
                if (el(m).unwrap_or(-90.0) > 0.0) == up_at_b {
                    b = m;
                } else {
                    a = m;
                }
            }
            (a + b) / 2.0
        };
        let step = 30.0;
        let end = t + hours * 3600.0;
        let mut prev = (t, el(t)?);
        let mut cur: Option<Pass> = None;
        let mut s = t + step;
        while s <= end {
            let e = el(s)?;
            match (&mut cur, prev.1 > 0.0, e > 0.0) {
                (None, false, true) => cur = Some(Pass { rise: cross(prev.0, s), peak: s, max_elevation: e, set: s }),
                // Already up at the start: the pass in progress counts from now.
                (None, true, true) if prev.0 == t => cur = Some(Pass { rise: t, peak: s, max_elevation: e.max(prev.1), set: s }),
                (Some(p), _, true) => {
                    if e > p.max_elevation {
                        p.max_elevation = e;
                        p.peak = s;
                    }
                }
                (Some(p), true, false) => {
                    p.set = cross(prev.0, s);
                    if p.max_elevation >= min_elevation {
                        return cur;
                    }
                    cur = None;
                }
                _ => {}
            }
            prev = (s, e);
            s += step;
        }
        None
    }
}

/// Read CelesTrak's CSV (a header row of OMM keywords, then a satellite per row), skipping
/// rows it can't use.
pub fn parse_csv(text: &str) -> Vec<Sat> {
    let mut rows = text.lines().filter(|l| !l.trim().is_empty()).map(split_csv);
    let Some(header) = rows.next() else { return Vec::new() };
    rows.filter_map(|r| {
        let fields: Map<String, Json> = header.iter().zip(r).filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.trim().to_string(), Json::String(v))).collect();
        Sat::from_omm(fields).ok()
    })
    .collect()
}

/// One CSV line's fields (quotes and doubled quotes inside them handled).
fn split_csv(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.trim_end_matches('\r').chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// What [`load`] found.
pub struct Loaded {
    /// The satellites.
    pub sats: Vec<Sat>,
    /// How old the elements are, seconds (0 when just downloaded).
    pub age: f64,
    /// Why a download wasn't tried or failed, when the cached copy was used anyway.
    pub note: Option<String>,
}

fn group_path(group: &str) -> PathBuf {
    crate::source::cache_root().join("celestrak").join(format!("{group}.csv"))
}

fn age(p: &std::path::Path) -> Option<Duration> {
    SystemTime::now().duration_since(std::fs::metadata(p).ok()?.modified().ok()?).ok()
}

/// The satellites in CelesTrak group `group` (such as `visual`, `stations`, `amateur`,
/// `weather`): the cached copy while it's under 2 hours old, else a fresh download, and
/// after any error the old copy (with a marker that keeps every run from asking again for
/// 2 hours).
pub async fn load(group: &str) -> Result<Loaded> {
    if group.is_empty() || !group.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("not a CelesTrak group: {group:?}");
    }
    let path = group_path(group);
    let marker = path.with_extension("wait");
    let cached = || -> Option<(Vec<Sat>, f64)> {
        let sats = parse_csv(&std::fs::read_to_string(&path).ok()?);
        (!sats.is_empty()).then(|| (sats, age(&path).map_or(0.0, |a| a.as_secs_f64())))
    };
    let stale = |note: String| -> Result<Loaded> {
        match cached() {
            Some((sats, age)) => Ok(Loaded { sats, age, note: Some(note) }),
            None => bail!("{note}"),
        }
    };
    if age(&path).is_some_and(|a| a < FRESH)
        && let Some((sats, age)) = cached()
    {
        return Ok(Loaded { sats, age, note: None });
    }
    if age(&marker).is_some_and(|a| a < FRESH) {
        let why = std::fs::read_to_string(&marker).unwrap_or_default();
        return stale(format!("not asking CelesTrak again yet ({})", why.trim()));
    }
    let url = format!("https://celestrak.org/NORAD/elements/gp.php?GROUP={group}&FORMAT=csv");
    let leave = |why: &str| {
        let _ = std::fs::create_dir_all(marker.parent().unwrap_or(&marker));
        let _ = std::fs::write(&marker, why);
    };
    let resp = match crate::source::http_client().get(&url).header("Accept-Encoding", "gzip").send().await {
        Ok(r) => r,
        Err(e) => {
            leave("the download failed");
            return stale(format!("CelesTrak: {e}"));
        }
    };
    let status = resp.status();
    if !status.is_success() {
        // Retrying doesn't help (403: already downloaded this update; 5xx: wait), and
        // errors count toward CelesTrak's firewall.
        leave(&format!("HTTP {status}"));
        return stale(format!("CelesTrak answered HTTP {status}"));
    }
    let body = resp.bytes().await.context("CelesTrak")?;
    if body.len() > 64 << 20 {
        bail!("CelesTrak: answer too large");
    }
    let text = if body.starts_with(&[0x1f, 0x8b]) {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::Read::take(flate2::read::GzDecoder::new(&body[..]), 256 << 20), &mut s).context("CelesTrak: bad gzip")?;
        s
    } else {
        String::from_utf8_lossy(&body).into_owned()
    };
    let sats = parse_csv(&text);
    if sats.is_empty() {
        leave("no satellites in the answer");
        return stale(format!("CelesTrak sent no satellites for group {group}"));
    }
    std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
    let tmp = path.with_extension("csv.part");
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, &path)?;
    let _ = std::fs::remove_file(&marker);
    Ok(Loaded { sats, age: 0.0, note: None })
}

/// The colour for a satellite's height: low orbit, medium, geostationary.
pub fn orbit_rgb(alt_km: f64) -> (u8, u8, u8) {
    match alt_km {
        a if a < 2000.0 => (0x79, 0xc0, 0xff),
        a if a < 30000.0 => (0xf2, 0xcc, 0x60),
        _ => (0xd2, 0xa8, 0xff),
    }
}

/// The satellites at Unix time `t` as GeoJSON points (`glyph`, `label`, `orbit`, and the
/// basics for inspect).
pub fn points(sats: &[Sat], t: f64) -> Json {
    let features: Vec<Json> = sats
        .iter()
        .filter_map(|s| {
            let f = s.at(t)?;
            let orbit = if f.alt_km < 2000.0 {
                "low"
            } else if f.alt_km < 30000.0 {
                "medium"
            } else {
                "geostationary"
            };
            Some(json!({
                "type": "Feature",
                "id": s.norad,
                "geometry": {"type": "Point", "coordinates": [f.lon, f.lat]},
                // Starlink's thousands go unnamed; the selected one still shows its name.
                "properties": {"glyph": s.glyph(), "label": if s.is_starlink() { "" } else { s.name.as_str() }, "orbit": orbit,
                               "name": s.name, "norad": s.norad, "altitude_km": f.alt_km.round()},
            }))
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// The style layer for satellites from source `source`: ◇ (◆ for the ISS, · for Starlink)
/// colored by orbit height, with names (not Starlink's).
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    let hex = |(r, g, b): (u8, u8, u8)| format!("#{r:02x}{g:02x}{b:02x}");
    let color = json!(["match", ["get", "orbit"], "low", hex(orbit_rgb(500.0)), "medium", hex(orbit_rgb(20000.0)), hex(orbit_rgb(36000.0))]);
    crate::StyleLayer::symbol(id, source).glyph(json!(["get", "glyph"])).text(json!(["get", "label"])).color("#c9d1d9").paint("icon-color", color)
}

/// The satellites at Unix time `t` as dots in an RGBA image (not premultiplied), `w` x `h`
/// pixels, for a map drawn by `vp` in cells of `cell_px` pixels (the image may be smaller
/// than the cells and scaled up by the terminal): placed to the pixel, so they move smoothly
/// where a cell-sized glyph would hop. Starlink small, the ISS large. With `labels`, their
/// names too (not Starlink's), in [`crate::imagetext`]'s font beside each dot, the ISS
/// first and any that would overlap one already placed left out.
pub fn render_dots(sats: &[Sat], t: f64, vp: &crate::Viewport, w: u32, h: u32, cell_px: (f64, f64), labels: bool) -> Vec<u8> {
    use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};
    let Some(mut pm) = Pixmap::new(w.max(1), h.max(1)) else { return Vec::new() };
    let (sx, sy) = (cell_px.0 / 2.0, cell_px.1 / 4.0);
    let size = (cell_px.0 / 4.0).max(1.0) as f32;
    let mut named: Vec<(f32, f32, f32, &str)> = Vec::new();
    for s in sats {
        let Some(f) = s.at(t) else { continue };
        let (x, y) = vp.dot_ll(f.lon, f.lat);
        let (x, y) = ((x * sx) as f32, (y * sy) as f32);
        if x < -8.0 || y < -8.0 || x > w as f32 + 8.0 || y > h as f32 + 8.0 {
            continue;
        }
        let r = match s {
            s if s.norad == 25544 => size * 1.6,
            s if s.is_starlink() => size * 0.6,
            _ => size,
        };
        let Some(dot) = PathBuilder::from_circle(x, y, r.max(0.8)) else { continue };
        let (cr, cg, cb) = orbit_rgb(f.alt_km);
        let mut paint = Paint::default();
        paint.set_color_rgba8(cr, cg, cb, 235);
        paint.anti_alias = true;
        pm.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
        if labels && !s.is_starlink() {
            named.push((x, y, r, s.name.as_str()));
        }
    }
    if labels {
        // The ISS first, then the rest as they come; a label over one already placed is left
        // out (as the map's own labels are).
        named.sort_by_key(|n| !n.3.starts_with("ISS"));
        let mut glyphs = crate::imagetext::Glyphs::new((cell_px.1 * 0.62) as f32);
        let mut placed: Vec<crate::imagetext::Rect> = Vec::new();
        for (x, y, r, name) in named {
            let at = (x + r + cell_px.0 as f32 * 0.4, y);
            let b = glyphs.bounds(name, at.0, at.1);
            if b.0 > w as f32 || placed.iter().any(|p| crate::imagetext::overlaps(*p, b)) {
                continue;
            }
            glyphs.draw(&mut pm, name, at.0, at.1, (0xc9, 0xd1, 0xd9));
            placed.push(b);
        }
    }
    pm.pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// The satellite drawn nearest the braille dot `dot` at time `t`, within `within` dots.
pub fn nearest(sats: &[Sat], t: f64, vp: &crate::Viewport, dot: (f64, f64), within: f64) -> Option<u64> {
    sats.iter()
        .filter_map(|s| {
            let f = s.at(t)?;
            let (x, y) = vp.dot_ll(f.lon, f.lat);
            let d = ((x - dot.0).powi(2) + ((y - dot.1) / 2.0).powi(2)).sqrt();
            (d <= within).then_some((d, s.norad))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, n)| n)
}

/// The satellites by distance from (`lon`, `lat`) at time `t`, nearest first.
pub fn by_distance(sats: &[Sat], t: f64, lon: f64, lat: f64) -> Vec<u64> {
    let mut v: Vec<(f64, u64)> = sats.iter().filter_map(|s| s.at(t).map(|f| (crate::adsb::distance_nm((lon, lat), (f.lon, f.lat)), s.norad))).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|(_, n)| n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ISS as CelesTrak's CSV gives it (elements of 2026-11-25).
    const CSV: &str = "OBJECT_NAME,OBJECT_ID,EPOCH,MEAN_MOTION,ECCENTRICITY,INCLINATION,RA_OF_ASC_NODE,ARG_OF_PERICENTER,MEAN_ANOMALY,EPHEMERIS_TYPE,CLASSIFICATION_TYPE,NORAD_CAT_ID,ELEMENT_SET_NO,REV_AT_EPOCH,BSTAR,MEAN_MOTION_DOT,MEAN_MOTION_DDOT
\"ISS (ZARYA)\",1998-067A,2026-11-25T12:00:00.000000,15.50103472,.0004216,51.6416,247.4627,130.5360,325.0288,0,U,25544,999,47000,.00027,.00016717,0
BROKEN ROW,,,,
";

    #[test]
    fn parses_csv() {
        let sats = parse_csv(CSV);
        assert_eq!(sats.len(), 1, "the broken row is skipped");
        let iss = &sats[0];
        assert_eq!((iss.name.as_str(), iss.norad, iss.intl.as_deref()), ("ISS (ZARYA)", 25544, Some("1998-067A")));
        assert!((iss.period_min() - 92.9).abs() < 0.1);
        assert_eq!(split_csv(r#"a,"b, c","d ""e""""#), vec!["a", "b, c", "d \"e\""]);
    }

    #[test]
    fn the_iss_is_where_low_orbit_is() {
        let iss = &parse_csv(CSV)[0];
        let t = iss.epoch + 3600.0;
        let f = iss.at(t).unwrap();
        assert!((400.0..440.0).contains(&f.alt_km), "{f:?}");
        assert!((7.5..7.8).contains(&f.speed), "{f:?}");
        assert!(f.lat.abs() <= 51.7 && f.lon.abs() <= 180.0, "{f:?}");
        // Its ground track: 460 km of orbit a minute, about 430 km projected to the ground,
        // give or take the Earth turning underneath (up to ~28 km a minute).
        let g = iss.at(t + 60.0).unwrap();
        let moved = crate::adsb::distance_nm((f.lon, f.lat), (g.lon, g.lat)) * 1.852;
        assert!((395.0..465.0).contains(&moved), "{moved} km in a minute");
    }

    #[test]
    fn geodetic_round_trip_and_look_angles() {
        let p = ecef(-77.03, 38.89, 0.1);
        let (lon, lat, h) = geodetic(p);
        assert!((lon + 77.03).abs() < 1e-9 && (lat - 38.89).abs() < 1e-9 && (h - 0.1).abs() < 1e-6);
        // Something straight up is at 90 degrees.
        let above = Fix { lon: -77.03, lat: 38.89, alt_km: 400.0, speed: 0.0, ecef: ecef(-77.03, 38.89, 400.0) };
        assert!((look((-77.03, 38.89, 0.0), &above).1 - 90.0).abs() < 1e-6);
        // Something due east on the horizon-ish is at azimuth 90.
        let east = Fix { lon: -70.0, lat: 38.89, alt_km: 400.0, speed: 0.0, ecef: ecef(-70.0, 38.89, 400.0) };
        let (az, el) = look((-77.03, 38.89, 0.0), &east);
        assert!((az - 90.0).abs() < 6.0 && el > 0.0 && el < 30.0, "{az} {el}");
    }

    #[test]
    fn finds_a_pass_and_draws_a_track() {
        let iss = &parse_csv(CSV)[0];
        let p = iss.next_pass((-77.03, 38.89, 0.0), iss.epoch, 24.0, 10.0).expect("the ISS passes over DC in a day");
        assert!(p.rise < p.peak && p.peak < p.set && p.max_elevation >= 10.0, "{p:?}");
        assert!((60.0..900.0).contains(&(p.set - p.rise)), "a pass lasts minutes: {p:?}");
        let up = look((-77.03, 38.89, 0.0), &iss.at(p.peak).unwrap()).1;
        assert!((up - p.max_elevation).abs() < 1e-9);
        let track = iss.track(iss.epoch, iss.period_min());
        assert!(track.iter().all(|seg| seg.windows(2).all(|w| (w[0].0 - w[1].0).abs() < 180.0)));
        assert!(track.iter().map(Vec::len).sum::<usize>() >= 180);
    }

    #[test]
    fn a_geostationary_satellite_stays_put() {
        // Synthetic elements: one orbit per sidereal day, circular, over the equator. If the
        // Earth's rotation were applied backwards it would sweep twice round the world a day.
        let geo = "OBJECT_NAME,OBJECT_ID,EPOCH,MEAN_MOTION,ECCENTRICITY,INCLINATION,RA_OF_ASC_NODE,ARG_OF_PERICENTER,MEAN_ANOMALY,EPHEMERIS_TYPE,CLASSIFICATION_TYPE,NORAD_CAT_ID,ELEMENT_SET_NO,REV_AT_EPOCH,BSTAR,MEAN_MOTION_DOT,MEAN_MOTION_DDOT
GEO TEST,2026-001A,2026-11-25T00:00:00.000000,1.00273791,.0001,0.02,90,0,180,0,U,99001,999,100,0,0,0
";
        let sat = &parse_csv(geo)[0];
        let a = sat.at(sat.epoch).unwrap();
        for hours in [3.0, 6.0, 12.0, 24.0] {
            let b = sat.at(sat.epoch + hours * 3600.0).unwrap();
            let drift = ((b.lon - a.lon + 540.0).rem_euclid(360.0) - 180.0).abs();
            assert!(drift < 0.5 && b.lat.abs() < 0.1, "{drift} degrees after {hours} h");
            assert!((35700.0..35900.0).contains(&b.alt_km), "{}", b.alt_km);
        }
    }

    #[test]
    fn starlink_is_small_and_unnamed() {
        let csv = CSV.replace("\"ISS (ZARYA)\",1998-067A", "STARLINK-31415,2024-001B").replace(",25544,", ",58001,");
        let sats = parse_csv(&csv);
        let s = &sats[0];
        assert!(s.is_starlink());
        assert_eq!(s.glyph(), "·");
        let doc = points(&sats, s.epoch);
        assert_eq!(doc["features"][0]["properties"]["label"], "");
        assert_eq!(doc["features"][0]["properties"]["name"], "STARLINK-31415");
        assert_eq!(parse_csv(CSV)[0].glyph(), "◆");
    }

    #[test]
    fn dots_land_where_the_satellite_is() {
        let iss = parse_csv(CSV);
        let f = iss[0].at(iss[0].epoch).unwrap();
        let vp = crate::Viewport::new(&crate::Camera::new(f.lon, f.lat, 3.0), ratatui::layout::Rect::new(0, 0, 40, 20));
        // 40 x 20 cells of 8 x 16 pixels: 320 x 320, the satellite in the middle.
        let rgba = render_dots(&iss, iss[0].epoch, &vp, 320, 320, (8.0, 16.0), false);
        let px = |x: usize, y: usize| rgba[(y * 320 + x) * 4 + 3];
        assert!(px(160, 160) > 0 && px(5, 5) == 0);
    }

    #[test]
    fn names_go_beside_their_dots() {
        let iss = parse_csv(CSV);
        let f = iss[0].at(iss[0].epoch).unwrap();
        let vp = crate::Viewport::new(&crate::Camera::new(f.lon, f.lat, 3.0), ratatui::layout::Rect::new(0, 0, 40, 20));
        let plain = render_dots(&iss, iss[0].epoch, &vp, 320, 320, (8.0, 16.0), false);
        let named = render_dots(&iss, iss[0].epoch, &vp, 320, 320, (8.0, 16.0), true);
        // The name lights pixels to the right of the dot, not to its left.
        let lit =
            |img: &[u8], x0: usize, x1: usize| (x0..x1).flat_map(|x| (150..170).map(move |y| (x, y))).filter(|&(x, y)| img[(y * 320 + x) * 4 + 3] > 0).count();
        assert_eq!(lit(&plain, 175, 300), 0);
        assert!(lit(&named, 175, 300) > 50);
        assert_eq!(lit(&named, 20, 140), 0);
    }

    #[test]
    fn group_names_are_checked() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert!(rt.block_on(load("../etc")).is_err());
        assert!(rt.block_on(load("visual&FORMAT=json")).is_err());
    }
}
