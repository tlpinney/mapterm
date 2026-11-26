//! Satellites from [CelesTrak](https://celestrak.org)'s orbital elements, placed with SGP4.
//!
//! Positions are computed here, every frame, from each satellite's elements: no feed to
//! follow, and nothing on the network once the elements are in. CelesTrak updates them every
//! 2 hours and asks for one download per update, blocks a second one with HTTP 403, and
//! firewalls an address after 50 errors in 2 hours. So [`load`] keeps each group in
//! `~/.cache/mapterm/celestrak/<group>.csv` and downloads only when that copy is more than 2
//! hours old; after any error it leaves a marker so that no run asks again for 2 hours, and
//! uses the old copy. It asks for CSV (a third the size of JSON) and gzip, as CelesTrak asks.

use std::collections::HashMap;
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
    /// 0 for a circle, up to (not including) 1.
    pub eccentricity: f64,
    /// Who owns it, from the satellite catalog (see [`load_catalog`]) once that's in.
    pub catalog: Option<CatalogEntry>,
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
            eccentricity: elements.eccentricity,
            catalog: None,
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

    /// Who runs it, when the name says (the elements don't): a few well-known operators,
    /// with their former names so a search for either finds them.
    pub fn operator(&self) -> Option<&'static str> {
        const OPERATORS: [(&[&str], &str); 12] = [
            (&["WORLDVIEW", "LEGION", "GEOEYE", "QUICKBIRD", "IKONOS"], "Vantor (Maxar, formerly DigitalGlobe)"),
            (&["FLOCK", "SKYSAT", "PELICAN", "TANAGER"], "Planet"),
            (&["SENTINEL"], "ESA (Copernicus)"),
            (&["LANDSAT"], "USGS / NASA"),
            (&["NOAA", "GOES"], "NOAA"),
            (&["STARLINK"], "SpaceX"),
            (&["ONEWEB"], "Eutelsat OneWeb"),
            (&["KUIPER"], "Amazon (Kuiper)"),
            (&["IRIDIUM"], "Iridium"),
            (&["GPS"], "US Space Force (GPS)"),
            (&["ICEYE"], "ICEYE"),
            (&["CAPELLA"], "Capella Space"),
        ];
        OPERATORS.iter().find(|(prefixes, _)| prefixes.iter().any(|p| self.name.starts_with(p))).map(|(_, op)| *op)
    }

    /// The orbit's semi-major axis, km (from its period, by Kepler's third law).
    pub fn semi_major_axis(&self) -> f64 {
        let n = self.mean_motion * 2.0 * std::f64::consts::PI / 86_400.0;
        (MU / (n * n)).cbrt()
    }

    /// Its lowest and highest altitude over the orbit (perigee, apogee), km above the
    /// equator's radius.
    pub fn perigee_apogee(&self) -> (f64, f64) {
        let a = self.semi_major_axis();
        (a * (1.0 - self.eccentricity) - EARTH_A, a * (1.0 + self.eccentricity) - EARTH_A)
    }

    /// Which standard class its orbit is: highly elliptical first, then by period (one a
    /// sidereal day is geosynchronous: geostationary if circular and over the equator, else
    /// inclined), then by apogee (LEO to 2,000 km).
    pub fn orbit_class(&self) -> OrbitClass {
        let (_, apogee) = self.perigee_apogee();
        if self.eccentricity > 0.25 {
            OrbitClass::Heo
        } else if (self.mean_motion - SIDEREAL).abs() < 0.03 {
            if self.eccentricity < 0.01 && self.inclination < 5.0 { OrbitClass::Geo } else { OrbitClass::Gso }
        } else if apogee <= 2000.0 {
            OrbitClass::Leo
        } else if self.mean_motion > SIDEREAL {
            OrbitClass::Meo
        } else {
            OrbitClass::BeyondGeo
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

fn cache_file(name: &str) -> PathBuf {
    crate::source::cache_root().join("celestrak").join(name)
}

fn age(p: &std::path::Path) -> Option<Duration> {
    SystemTime::now().duration_since(std::fs::metadata(p).ok()?.modified().ok()?).ok()
}

/// A CelesTrak file by the rules (see the module docs): the cached copy `name` while it's
/// younger than `fresh`, else a download of `url` (gzip asked for) kept only when `usable`
/// says it is, and after any error the old copy, with a marker that keeps every run from
/// asking again for 2 hours. Returns the text, its age in seconds, and why the old copy was
/// used, if it was.
async fn fetch_cached(name: &str, url: &str, fresh: Duration, usable: fn(&str) -> bool) -> Result<(String, f64, Option<String>)> {
    let path = cache_file(name);
    let marker = path.with_extension("wait");
    let cached = || -> Option<(String, f64)> {
        let text = std::fs::read_to_string(&path).ok()?;
        usable(&text).then(|| (text, age(&path).map_or(0.0, |a| a.as_secs_f64())))
    };
    let stale = |note: String| -> Result<(String, f64, Option<String>)> {
        match cached() {
            Some((text, age)) => Ok((text, age, Some(note))),
            None => bail!("{note}"),
        }
    };
    if age(&path).is_some_and(|a| a < fresh)
        && let Some((text, age)) = cached()
    {
        return Ok((text, age, None));
    }
    if age(&marker).is_some_and(|a| a < FRESH) {
        let why = std::fs::read_to_string(&marker).unwrap_or_default();
        return stale(format!("not asking CelesTrak again yet ({})", why.trim()));
    }
    let leave = |why: &str| {
        let _ = std::fs::create_dir_all(marker.parent().unwrap_or(&marker));
        let _ = std::fs::write(&marker, why);
    };
    let resp = match crate::source::http_client().get(url).header("Accept-Encoding", "gzip").send().await {
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
    if !usable(&text) {
        leave("nothing usable in the answer");
        return stale(format!("CelesTrak sent nothing usable for {name}"));
    }
    std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, &path)?;
    let _ = std::fs::remove_file(&marker);
    Ok((text, 0.0, None))
}

fn check_group(group: &str) -> Result<()> {
    if group.is_empty() || !group.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("not a CelesTrak group: {group:?}");
    }
    Ok(())
}

/// The satellites in CelesTrak group `group` (such as `active`, `visual`, `stations`,
/// `starlink`): the cached copy while it's under 2 hours old, else a fresh download, and
/// after any error the old copy (with a marker that keeps every run from asking again for
/// 2 hours).
pub async fn load(group: &str) -> Result<Loaded> {
    check_group(group)?;
    let url = format!("https://celestrak.org/NORAD/elements/gp.php?GROUP={group}&FORMAT=csv");
    let (text, age, note) = fetch_cached(&format!("{group}.csv"), &url, FRESH, |t| !parse_csv(t).is_empty()).await?;
    Ok(Loaded { sats: parse_csv(&text), age, note })
}

/// What the satellite catalog says about one satellite: its owner code (see [`owner_name`])
/// and launch date.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogEntry {
    /// Owner code, such as `US` or `PRC`.
    pub owner: String,
    /// Launch date, `YYYY-MM-DD`.
    pub launched: Option<String>,
}

/// CelesTrak's satellite catalog (SATCAT) for group `group`, by NORAD number: who owns each
/// satellite and when it was launched. Kept for a day (owners don't change).
pub async fn load_catalog(group: &str) -> Result<HashMap<u64, CatalogEntry>> {
    check_group(group)?;
    let url = format!("https://celestrak.org/satcat/records.php?GROUP={group}&FORMAT=CSV");
    let (text, _, _) = fetch_cached(&format!("{group}-satcat.csv"), &url, Duration::from_secs(24 * 3600), |t| !parse_catalog(t).is_empty()).await?;
    Ok(parse_catalog(&text))
}

/// Read the SATCAT CSV (a header row of field names, then a row per object).
pub fn parse_catalog(text: &str) -> HashMap<u64, CatalogEntry> {
    let mut rows = text.lines().filter(|l| !l.trim().is_empty()).map(split_csv);
    let Some(header) = rows.next() else { return HashMap::new() };
    let col = |name: &str| header.iter().position(|h| h.trim() == name);
    let (Some(id), Some(owner)) = (col("NORAD_CAT_ID"), col("OWNER")) else { return HashMap::new() };
    let launch = col("LAUNCH_DATE");
    rows.filter_map(|r| {
        let n = r.get(id)?.trim().parse::<u64>().ok()?;
        let o = r.get(owner)?.trim().to_string();
        (!o.is_empty()).then(|| (n, CatalogEntry { owner: o, launched: launch.and_then(|l| r.get(l)).map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) }))
    })
    .collect()
}

/// The owner's name for a SATCAT owner code (`PRC`: People's Republic of China).
pub fn owner_name(code: &str) -> Option<&'static str> {
    OWNERS.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

/// The owner code a word names: a code (`prc`), or a name or everyday name that contains it
/// (`china`, `japan`, `russia`), case ignored.
pub fn owner_code_for(word: &str) -> Option<&'static str> {
    let w = word.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    // A code; then a name or everyday name that is the word (`china` is PRC, not
    // China/Brazil); then the shortest name that contains it.
    OWNERS
        .iter()
        .find(|(c, _)| c.to_lowercase() == w)
        .or_else(|| OWNERS.iter().find(|(c, n)| owner_says(c, n, &w)))
        .or_else(|| OWNERS.iter().filter(|(c, n)| n.to_lowercase().contains(&w) || owner_aliases(c).to_lowercase().contains(&w)).min_by_key(|(_, n)| n.len()))
        .map(|(c, _)| *c)
}

/// Whether `words` (lowercase) is an owner's name, one of its everyday names, or the last
/// word(s) of its name (`japan`, `china`, `south korea`).
pub fn owner_says(code: &str, name: &str, words: &str) -> bool {
    let name = name.to_lowercase();
    let aliases = owner_aliases(code).to_lowercase();
    name == words
        || name.ends_with(&format!(" {words}"))
        || aliases == words
        || aliases.split(' ').any(|a| a == words)
        || aliases.starts_with(&format!("{words} "))
}

/// Everyday names for owners whose catalog names don't say them, so a search for `china`,
/// `russia` or `usa` finds them.
pub fn owner_aliases(code: &str) -> &'static str {
    match code {
        "PRC" => "China",
        "CIS" => "Russia USSR Soviet",
        "US" => "USA America",
        "UK" => "Britain England",
        "SKOR" => "South Korea",
        "NKOR" => "North Korea",
        "ESA" => "Europe",
        "ISS" => "Space Station",
        _ => "",
    }
}

include!("satellites_owners.rs");

/// Colors for the six owners with the most satellites (Starlink counted on its own), in
/// order; everyone else is [`OTHER`].
pub const COUNTRY_COLORS: [(u8, u8, u8); 6] =
    [(0x56, 0xd4, 0xdd), (0xff, 0x7b, 0x72), (0x79, 0xc0, 0xff), (0xf2, 0xcc, 0x60), (0x7e, 0xe7, 0x87), (0xd2, 0xa8, 0xff)];

/// The group a satellite is colored and counted in by owner: its owner code, except that
/// Starlink, two thirds of everything up there, is a group of its own (`STARLINK`), so the
/// rest of the United States shows.
pub fn owner_group(s: &Sat) -> Option<&str> {
    if s.is_starlink() { Some("STARLINK") } else { s.catalog.as_ref().map(|c| c.owner.as_str()) }
}
/// The color for every other country.
pub const OTHER: (u8, u8, u8) = (0x8b, 0x94, 0x9e);

/// How satellites are colored.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Coloring {
    /// By orbit height: low, medium, geostationary ([`orbit_rgb`]).
    #[default]
    Orbit,
    /// By owner: the biggest owners' colors, by SATCAT code; the rest [`OTHER`].
    Country(Vec<(String, (u8, u8, u8))>),
}

/// The standard orbit classes (as NASA and ESA use them), from a satellite's elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OrbitClass {
    /// Low Earth orbit: apogee at most 2,000 km.
    Leo,
    /// Medium Earth orbit: above that, below geosynchronous.
    Meo,
    /// Geostationary: one orbit a sidereal day, circular, over the equator (35,786 km).
    Geo,
    /// Geosynchronous but inclined or eccentric: a figure-8 in the sky, not a fixed point.
    Gso,
    /// Highly elliptical (eccentricity over 0.25): low perigee, high apogee (Molniya, Tundra).
    Heo,
    /// Beyond geosynchronous, not highly elliptical.
    BeyondGeo,
}

impl OrbitClass {
    /// Every class, in legend order.
    pub const ALL: [OrbitClass; 6] = [OrbitClass::Leo, OrbitClass::Meo, OrbitClass::Geo, OrbitClass::Gso, OrbitClass::Heo, OrbitClass::BeyondGeo];

    /// The abbreviation: LEO, MEO, GEO, GSO, HEO.
    pub fn short(self) -> &'static str {
        match self {
            OrbitClass::Leo => "LEO",
            OrbitClass::Meo => "MEO",
            OrbitClass::Geo => "GEO",
            OrbitClass::Gso => "GSO",
            OrbitClass::Heo => "HEO",
            OrbitClass::BeyondGeo => "beyond GEO",
        }
    }

    /// What it stands for.
    pub fn long(self) -> &'static str {
        match self {
            OrbitClass::Leo => "low Earth orbit",
            OrbitClass::Meo => "medium Earth orbit",
            OrbitClass::Geo => "geostationary",
            OrbitClass::Gso => "geosynchronous, inclined",
            OrbitClass::Heo => "highly elliptical",
            OrbitClass::BeyondGeo => "beyond geosynchronous",
        }
    }

    /// Its color on the map.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            OrbitClass::Leo => (0x79, 0xc0, 0xff),
            OrbitClass::Meo => (0xf2, 0xcc, 0x60),
            OrbitClass::Geo => (0xd2, 0xa8, 0xff),
            OrbitClass::Gso => (0xff, 0x6b, 0xcb),
            OrbitClass::Heo => (0xff, 0xa6, 0x57),
            OrbitClass::BeyondGeo => (0x8b, 0x94, 0x9e),
        }
    }
}

/// Earth's gravitational parameter, km³/s².
const MU: f64 = 398_600.4418;
/// Orbits a sidereal day: one is geosynchronous.
const SIDEREAL: f64 = 1.0027379;

impl Coloring {
    /// Color by owner: the six groups ([`owner_group`]) with the most of `sats`, then
    /// everyone else.
    pub fn by_country(sats: &[Sat]) -> Coloring {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for s in sats {
            if let Some(g) = owner_group(s) {
                *counts.entry(g).or_insert(0) += 1;
            }
        }
        let mut v: Vec<(&str, usize)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        Coloring::Country(v.into_iter().zip(COUNTRY_COLORS).map(|((c, _), rgb)| (c.to_string(), rgb)).collect())
    }

    /// The color for `s` (by its orbit's class, or its owner); `_alt_km`, where it is now,
    /// is kept for callers but no longer changes the color.
    pub fn rgb(&self, s: &Sat, _alt_km: f64) -> (u8, u8, u8) {
        match self {
            Coloring::Orbit => s.orbit_class().rgb(),
            Coloring::Country(top) => owner_group(s).and_then(|g| top.iter().find(|(code, _)| code == g)).map_or(OTHER, |(_, rgb)| *rgb),
        }
    }

    /// What the colors mean, with how many of `sats` each covers.
    pub fn legend(&self, sats: &[Sat]) -> crate::legend::Legend {
        use ratatui::style::Color;
        let rgb = |(r, g, b): (u8, u8, u8)| Color::Rgb(r, g, b);
        match self {
            Coloring::Orbit => {
                let mut n: HashMap<OrbitClass, usize> = HashMap::new();
                for s in sats {
                    *n.entry(s.orbit_class()).or_insert(0) += 1;
                }
                // Each class there is, as LEO, MEO, GEO, GSO, HEO, with what it means.
                OrbitClass::ALL.iter().filter(|c| n.contains_key(c)).fold(crate::legend::Legend::new("satellites by orbit"), |l, c| {
                    let label = if *c == OrbitClass::BeyondGeo { c.short().to_string() } else { format!("{} {}", c.short(), c.long()) };
                    l.entry("●", rgb(c.rgb()), label, n.get(c).copied())
                })
            }
            Coloring::Country(top) => {
                let mut n: HashMap<&str, usize> = HashMap::new();
                for s in sats {
                    *n.entry(owner_group(s).unwrap_or("")).or_insert(0) += 1;
                }
                let mut l = crate::legend::Legend::new("satellites by country");
                let mut shown = 0;
                for (code, c) in top {
                    let count = n.get(code.as_str()).copied().unwrap_or(0);
                    shown += count;
                    l = l.entry("●", rgb(*c), short_owner(code), Some(count));
                }
                let rest = sats.len() - shown;
                if rest > 0 {
                    l = l.entry("●", rgb(OTHER), "other", Some(rest));
                }
                l
            }
        }
    }
}

/// A short name for an owner code (`PRC`: China, `CIS`: Russia), for legends and status
/// lines.
pub fn short_owner(code: &str) -> String {
    match code {
        "STARLINK" => "Starlink (USA)".to_string(),
        "SKOR" | "NKOR" => owner_aliases(code).to_string(),
        _ => match owner_aliases(code).split(' ').next().filter(|a| !a.is_empty()) {
            Some(alias) => alias.to_string(),
            None => owner_name(code).unwrap_or(code).to_string(),
        },
    }
}

/// The satellites at Unix time `t` as GeoJSON points (`glyph`, `label`, `orbit`, and the
/// basics for inspect).
pub fn points(sats: &[Sat], t: f64, coloring: &Coloring) -> Json {
    let features: Vec<Json> = sats
        .iter()
        .filter_map(|s| {
            let f = s.at(t)?;
            let orbit = s.orbit_class().short();
            let (r, g, b) = coloring.rgb(s, f.alt_km);
            Some(json!({
                "type": "Feature",
                "id": s.norad,
                "geometry": {"type": "Point", "coordinates": [f.lon, f.lat]},
                // Starlink's thousands go unnamed; the selected one still shows its name.
                "properties": {"glyph": s.glyph(), "label": if s.is_starlink() { "" } else { s.name.as_str() }, "orbit": orbit,
                               "color": format!("#{r:02x}{g:02x}{b:02x}"), "name": s.name, "norad": s.norad, "altitude_km": f.alt_km.round()},
            }))
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// The style layer for satellites from source `source`: ◇ (◆ for the ISS, · for Starlink)
/// colored by orbit height, with names (not Starlink's).
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    // Each point carries its color (see `points` and its `Coloring`).
    crate::StyleLayer::symbol(id, source)
        .glyph(json!(["get", "glyph"]))
        .text(json!(["get", "label"]))
        .color("#c9d1d9")
        .paint("icon-color", json!(["get", "color"]))
}

/// Where many satellites are, worked out with SGP4 at whole seconds and interpolated in
/// between: over a second an orbit is a straight line to within metres, so a viewer can
/// redraw many times a second for the cost of SGP4 once a second.
#[derive(Default)]
pub struct Ephemeris {
    /// Which list the positions are for: its length and first satellite.
    key: (usize, u64),
    t0: f64,
    a: Vec<Option<(f64, f64, f64)>>,
    b: Vec<Option<(f64, f64, f64)>>,
}

impl Ephemeris {
    fn all(sats: &[Sat], t: f64) -> Vec<Option<(f64, f64, f64)>> {
        sats.iter().map(|s| s.at(t).map(|f| (f.lon, f.lat, f.alt_km))).collect()
    }

    /// Each satellite's longitude, latitude and height (km) at Unix time `t`, in `sats`'s
    /// order.
    pub fn positions(&mut self, sats: &[Sat], t: f64) -> Vec<Option<(f64, f64, f64)>> {
        let key = (sats.len(), sats.first().map_or(0, |s| s.norad));
        if key != self.key || t < self.t0 || t >= self.t0 + 2.0 {
            self.key = key;
            self.t0 = t.floor();
            self.a = Ephemeris::all(sats, self.t0);
            self.b = Ephemeris::all(sats, self.t0 + 1.0);
        } else if t >= self.t0 + 1.0 {
            self.t0 += 1.0;
            self.a = std::mem::take(&mut self.b);
            self.b = Ephemeris::all(sats, self.t0 + 1.0);
        }
        let f = t - self.t0;
        self.a
            .iter()
            .zip(&self.b)
            .map(|(a, b)| match (a, b) {
                (Some(a), Some(b)) => {
                    // The short way round, across the antimeridian too.
                    let dlon = (b.0 - a.0 + 540.0).rem_euclid(360.0) - 180.0;
                    Some(((a.0 + dlon * f + 540.0).rem_euclid(360.0) - 180.0, a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f))
                }
                (a, _) => *a,
            })
            .collect()
    }
}

/// The satellites at Unix time `t` as dots in an RGBA image (not premultiplied), `w` x `h`
/// pixels, for a map drawn by `vp` in cells of `cell_px` pixels (the image may be smaller
/// than the cells and scaled up by the terminal): placed to the pixel, so they move smoothly
/// where a cell-sized glyph would hop. Starlink small, the ISS large. With `labels`, their
/// names too (not Starlink's), in [`crate::imagetext`]'s font beside each dot, the ISS
/// first and any that would overlap one already placed left out. The `selected` satellite
/// (Starlink too) gets a ring and its name, placed before any other.
#[allow(clippy::too_many_arguments)]
pub fn render_dots(
    sats: &[Sat],
    t: f64,
    vp: &crate::Viewport,
    w: u32,
    h: u32,
    cell_px: (f64, f64),
    labels: bool,
    selected: Option<u64>,
    coloring: &Coloring,
) -> Vec<u8> {
    let at: Vec<Option<(f64, f64, f64)>> = sats.iter().map(|s| s.at(t).map(|f| (f.lon, f.lat, f.alt_km))).collect();
    render_dots_at(sats, &at, vp, w, h, cell_px, labels, selected, coloring)
}

/// [`render_dots`] from positions already worked out (lon, lat, km; one per satellite, as
/// [`Ephemeris`] gives them), so a viewer can redraw often without SGP4 each time.
#[allow(clippy::too_many_arguments)]
pub fn render_dots_at(
    sats: &[Sat],
    at: &[Option<(f64, f64, f64)>],
    vp: &crate::Viewport,
    w: u32,
    h: u32,
    cell_px: (f64, f64),
    labels: bool,
    selected: Option<u64>,
    coloring: &Coloring,
) -> Vec<u8> {
    use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let Some(mut pm) = Pixmap::new(w.max(1), h.max(1)) else { return Vec::new() };
    let (sx, sy) = (cell_px.0 / 2.0, cell_px.1 / 4.0);
    let size = (cell_px.0 / 4.0).max(1.0) as f32;
    let mut named: Vec<(f32, f32, f32, &str)> = Vec::new();
    let mut chosen: Option<(f32, f32, f32, &str)> = None;
    for (s, p) in sats.iter().zip(at) {
        let Some((lon, lat, alt_km)) = *p else { continue };
        let (x, y) = vp.dot_ll(lon, lat);
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
        let (cr, cg, cb) = coloring.rgb(s, alt_km);
        let mut paint = Paint::default();
        paint.set_color_rgba8(cr, cg, cb, 235);
        paint.anti_alias = true;
        pm.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
        if Some(s.norad) == selected {
            chosen = Some((x, y, r, s.name.as_str()));
        } else if labels && !s.is_starlink() {
            named.push((x, y, r, s.name.as_str()));
        }
    }
    // The selected one: a ring round its dot (in the highlight blue).
    if let Some((x, y, r, _)) = chosen
        && let Some(ring) = PathBuilder::from_circle(x, y, r + (cell_px.0 as f32 * 0.35).max(2.0))
    {
        let mut paint = Paint::default();
        paint.set_color_rgba8(127, 178, 255, 255);
        paint.anti_alias = true;
        let stroke = Stroke { width: (cell_px.0 as f32 / 6.0).max(1.2), ..Stroke::default() };
        pm.stroke_path(&ring, &paint, &stroke, Transform::identity(), None);
    }
    if labels || chosen.is_some() {
        // The ISS first, then the rest as they come; a label over one already placed is left
        // out (as the map's own labels are).
        named.sort_by_key(|n| !n.3.starts_with("ISS"));
        let mut glyphs = crate::imagetext::Glyphs::new((cell_px.1 * 0.62) as f32);
        let mut placed: Vec<crate::imagetext::Rect> = Vec::new();
        // The selected one's name first, in the highlight blue; then the others around it.
        if let Some((x, y, r, name)) = chosen {
            let at = (x + r + cell_px.0 as f32 * 0.6, y);
            glyphs.draw(&mut pm, name, at.0, at.1, (127, 178, 255));
            placed.push(glyphs.bounds(name, at.0, at.1));
        }
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
    crate::timeseries::to_rgba(pm)
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
        let doc = points(&sats, s.epoch, &Coloring::Orbit);
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
        let rgba = render_dots(&iss, iss[0].epoch, &vp, 320, 320, (8.0, 16.0), false, None, &Coloring::Orbit);
        let px = |x: usize, y: usize| rgba[(y * 320 + x) * 4 + 3];
        assert!(px(160, 160) > 0 && px(5, 5) == 0);
    }

    #[test]
    fn names_go_beside_their_dots() {
        let iss = parse_csv(CSV);
        let f = iss[0].at(iss[0].epoch).unwrap();
        let vp = crate::Viewport::new(&crate::Camera::new(f.lon, f.lat, 3.0), ratatui::layout::Rect::new(0, 0, 40, 20));
        let plain = render_dots(&iss, iss[0].epoch, &vp, 320, 320, (8.0, 16.0), false, None, &Coloring::Orbit);
        let named = render_dots(&iss, iss[0].epoch, &vp, 320, 320, (8.0, 16.0), true, None, &Coloring::Orbit);
        // The name lights pixels to the right of the dot, not to its left.
        let lit =
            |img: &[u8], x0: usize, x1: usize| (x0..x1).flat_map(|x| (150..170).map(move |y| (x, y))).filter(|&(x, y)| img[(y * 320 + x) * 4 + 3] > 0).count();
        assert_eq!(lit(&plain, 175, 300), 0);
        assert!(lit(&named, 175, 300) > 50);
        assert_eq!(lit(&named, 20, 140), 0);
    }

    #[test]
    fn a_selected_starlink_satellite_gets_a_ring_and_its_name() {
        let csv = CSV.replace("\"ISS (ZARYA)\",1998-067A", "STARLINK-31415,2024-001B").replace(",25544,", ",58001,");
        let sats = parse_csv(&csv);
        let f = sats[0].at(sats[0].epoch).unwrap();
        let vp = crate::Viewport::new(&crate::Camera::new(f.lon, f.lat, 3.0), ratatui::layout::Rect::new(0, 0, 40, 20));
        let lit = |img: &[u8]| img.chunks(4).filter(|p| p[3] > 0).count();
        // Unselected, Starlink is a small unnamed dot even with labels on.
        let plain = render_dots(&sats, sats[0].epoch, &vp, 320, 320, (8.0, 16.0), true, None, &Coloring::Orbit);
        let chosen = render_dots(&sats, sats[0].epoch, &vp, 320, 320, (8.0, 16.0), false, Some(58001), &Coloring::Orbit);
        assert!(lit(&plain) < 40, "{}", lit(&plain));
        assert!(lit(&chosen) > lit(&plain) + 200, "ring and name: {} vs {}", lit(&chosen), lit(&plain));
    }

    #[test]
    fn operators_by_name() {
        let named = |n: &str| CSV.replace("\"ISS (ZARYA)\"", n);
        assert_eq!(parse_csv(&named("LEGION 3"))[0].operator(), Some("Vantor (Maxar, formerly DigitalGlobe)"));
        assert_eq!(parse_csv(&named("\"WORLDVIEW-3 (WV-3)\""))[0].operator(), Some("Vantor (Maxar, formerly DigitalGlobe)"));
        assert_eq!(parse_csv(&named("FLOCK 4Y-12"))[0].operator(), Some("Planet"));
        assert_eq!(parse_csv(CSV)[0].operator(), None);
    }

    #[test]
    fn owners_by_code_name_and_alias() {
        assert_eq!(owner_name("PRC"), Some("People's Republic of China"));
        assert_eq!(owner_code_for("china"), Some("PRC"));
        assert_eq!(owner_code_for("prc"), Some("PRC"));
        assert_eq!(owner_code_for("Russia"), Some("CIS"));
        assert_eq!(owner_code_for("japan"), Some("JPN"));
        assert_eq!(owner_code_for("south korea"), Some("SKOR"));
        assert_eq!(owner_code_for("atlantis"), None);
        let cat =
            parse_catalog("OBJECT_NAME,OBJECT_ID,NORAD_CAT_ID,OBJECT_TYPE,OWNER,LAUNCH_DATE\nISS (ZARYA),1998-067A,25544,PAY,ISS,1998-11-20\nBAD ROW,,x,,,\n");
        assert_eq!(cat.get(&25544), Some(&CatalogEntry { owner: "ISS".into(), launched: Some("1998-11-20".into()) }));
        assert_eq!(cat.len(), 1);
    }

    #[test]
    fn coloring_by_country() {
        let mut sats = parse_csv(CSV);
        sats.extend(parse_csv(&CSV.replace("\"ISS (ZARYA)\"", "CHINASAT 9B").replace(",25544,", ",49125,")));
        sats.extend(parse_csv(&CSV.replace("\"ISS (ZARYA)\"", "BEIDOU 3").replace(",25544,", ",49126,")));
        let own = |s: &mut Sat, o: &str| s.catalog = Some(CatalogEntry { owner: o.into(), launched: None });
        own(&mut sats[0], "ISS");
        own(&mut sats[1], "PRC");
        own(&mut sats[2], "PRC");
        let c = Coloring::by_country(&sats);
        // The biggest owner gets the first color.
        assert_eq!(c.rgb(&sats[1], 400.0), COUNTRY_COLORS[0]);
        assert_eq!(c.rgb(&sats[0], 400.0), COUNTRY_COLORS[1]);
        let l = c.legend(&sats);
        assert_eq!(l.entries[0].label, "China");
        assert_eq!(l.entries[0].count, Some(2));
        let orbit = Coloring::Orbit.legend(&sats);
        assert_eq!((orbit.entries[0].label.as_str(), orbit.entries[0].count), ("LEO low Earth orbit", Some(3)), "all three in low orbit");
        // Starlink is a group of its own, not part of its owner's.
        let mut sl = parse_csv(&CSV.replace("\"ISS (ZARYA)\"", "STARLINK-1").replace(",25544,", ",44001,")).remove(0);
        own(&mut sl, "US");
        sats.push(sl);
        let l = Coloring::by_country(&sats).legend(&sats);
        assert!(l.entries.iter().any(|e| e.label == "Starlink (USA)" && e.count == Some(1)), "{:?}", l.entries);
    }

    #[test]
    fn ephemeris_matches_sgp4_between_seconds() {
        let iss = parse_csv(CSV);
        let mut e = Ephemeris::default();
        for dt in [0.0, 0.25, 0.5, 0.99, 1.4, 3.7] {
            let t = iss[0].epoch + 100.0 + dt;
            let (lon, lat, alt) = e.positions(&iss, t)[0].unwrap();
            let f = iss[0].at(t).unwrap();
            let apart_m = crate::adsb::distance_nm((lon, lat), (f.lon, f.lat)) * 1852.0;
            assert!(apart_m < 50.0 && (alt - f.alt_km).abs() < 0.05, "{apart_m} m at +{dt} s");
        }
    }

    #[test]
    fn orbit_classes_by_the_standard_definitions() {
        // (name, orbits a day, eccentricity, inclination)
        let orbit = |mm: &str, e: &str, i: &str| {
            let csv = CSV.replace("15.50103472", mm).replace(",.0004216,", &format!(",{e},")).replace(",51.6416,", &format!(",{i},"));
            parse_csv(&csv).remove(0)
        };
        let iss = &parse_csv(CSV)[0];
        assert_eq!(iss.orbit_class(), OrbitClass::Leo);
        let (p, a) = iss.perigee_apogee();
        assert!((380.0..440.0).contains(&p) && (380.0..450.0).contains(&a), "{p} {a}");
        assert_eq!(orbit("1.00273791", ".0001", "0.02").orbit_class(), OrbitClass::Geo);
        let geo = orbit("1.00273791", ".0001", "0.02");
        assert!((geo.perigee_apogee().0 - 35786.0).abs() < 15.0, "{:?}", geo.perigee_apogee());
        assert_eq!(orbit("1.00273791", ".0002", "12.0").orbit_class(), OrbitClass::Gso, "inclined");
        assert_eq!(orbit("2.00563", ".01", "55.0").orbit_class(), OrbitClass::Meo, "GPS");
        assert_eq!(orbit("2.00612", ".72", "63.4").orbit_class(), OrbitClass::Heo, "Molniya");
        assert_eq!(orbit("0.5", ".01", "0.0").orbit_class(), OrbitClass::BeyondGeo);
    }

    #[test]
    fn group_names_are_checked() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert!(rt.block_on(load("../etc")).is_err());
        assert!(rt.block_on(load("visual&FORMAT=json")).is_err());
    }
}
