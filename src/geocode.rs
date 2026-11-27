//! Place search (geocoding) with OpenStreetMap geocoders: [Photon](https://photon.komoot.io)
//! by default (free, no key, results biased toward the current view) or
//! [Nominatim](https://nominatim.org). Both public servers ask for light use: search when the
//! user asks (on Enter), not on every keystroke.
//!
//! ```no_run
//! # use mapterm::{Camera, geocode::Geocoder};
//! # async fn demo(cam: Camera) -> anyhow::Result<()> {
//! let geo = Geocoder::from_env()?;              // MAPTERM_GEOCODER, else Photon
//! for p in geo.search("Lincoln Memorial", Some((cam.lon, cam.lat, cam.zoom)), 8).await? {
//!     println!("{} ({}) {:.4},{:.4}", p.name, p.kind, p.lat, p.lon);
//! }
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result, bail};
use serde_json::Value as Json;

/// A search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// The place's own name (or house number and street).
    pub name: String,
    /// Where it is: street, city, state, country (what isn't already in `name`).
    pub context: String,
    /// What it is: city, monument, restaurant, ...
    pub kind: String,
    /// Longitude.
    pub lon: f64,
    /// Latitude.
    pub lat: f64,
    /// Its extent (west, south, east, north), for areas like cities and parks.
    pub bounds: Option<(f64, f64, f64, f64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Which geocoding API a server speaks.
pub enum Api {
    /// Photon (komoot), GeoJSON results.
    Photon,
    /// Nominatim, `jsonv2` results.
    Nominatim,
}

/// The largest search response read: results are a few KB.
const MAX_RESPONSE: usize = 4 << 20;

/// A geocoding server.
#[derive(Clone, Debug)]
pub struct Geocoder {
    /// The API the server speaks.
    pub api: Api,
    /// The search endpoint, e.g. `https://photon.komoot.io/api/`.
    pub url: String,
    http: reqwest::Client,
}

/// The public Photon server.
pub const PHOTON: &str = "https://photon.komoot.io/api/";
/// The public Nominatim server (one request per second at most, by its usage policy).
pub const NOMINATIM: &str = "https://nominatim.openstreetmap.org/search";

impl Geocoder {
    /// A geocoder at `url` speaking `api`.
    pub fn new(api: Api, url: impl Into<String>) -> Self {
        Self { api, url: url.into(), http: crate::source::http_client() }
    }

    /// From `MAPTERM_GEOCODER`: `photon` (default), `nominatim`, or the URL of your own
    /// server (a URL ending in `/search` or naming nominatim is Nominatim, others Photon).
    pub fn from_env() -> Result<Self> {
        let v = std::env::var("MAPTERM_GEOCODER").unwrap_or_default();
        Ok(match v.trim() {
            "" | "photon" => Geocoder::new(Api::Photon, PHOTON),
            "nominatim" => Geocoder::new(Api::Nominatim, NOMINATIM),
            url if url.starts_with("http://") || url.starts_with("https://") => {
                let api = if url.contains("nominatim") || url.trim_end_matches('/').ends_with("/search") { Api::Nominatim } else { Api::Photon };
                Geocoder::new(api, url)
            }
            other => bail!("MAPTERM_GEOCODER={other}: use photon, nominatim, or a server URL"),
        })
    }

    /// Search for `query`, preferring places near `near` (lon, lat, zoom) when given.
    pub async fn search(&self, query: &str, near: Option<(f64, f64, f64)>, limit: usize) -> Result<Vec<Place>> {
        let limit = limit.clamp(1, 40).to_string();
        let mut params: Vec<(&str, String)> = vec![("q", query.to_string()), ("limit", limit)];
        match self.api {
            Api::Photon => {
                if let Some((lon, lat, zoom)) = near {
                    params.push(("lon", format!("{lon:.5}")));
                    params.push(("lat", format!("{lat:.5}")));
                    params.push(("zoom", format!("{}", zoom.round().clamp(0.0, 18.0))));
                }
            }
            Api::Nominatim => {
                params.push(("format", "jsonv2".into()));
                if let Some((lon, lat, _)) = near {
                    // A loose box around the view: preferred, not required.
                    params.push(("viewbox", format!("{},{},{},{}", lon - 1.0, lat + 1.0, lon + 1.0, lat - 1.0)));
                }
            }
        }
        let url = reqwest::Url::parse_with_params(&self.url, &params).context("geocoder URL")?;
        // Capped: a hostile or broken geocoder can't fill memory with one answer.
        let bytes = crate::source::get_limited(&self.http, url.as_str(), &[], MAX_RESPONSE).await.context("place search")?;
        let body: Json = serde_json::from_slice(&bytes.unwrap_or_default()).context("place search: not JSON")?;
        let mut places = match self.api {
            Api::Photon => parse_photon(&body),
            Api::Nominatim => parse_nominatim(&body),
        };
        dedup(&mut places);
        Ok(places)
    }
}

/// Photon answers with GeoJSON features (`extent` is west, north, east, south).
pub fn parse_photon(body: &Json) -> Vec<Place> {
    let mut out = Vec::new();
    for f in body["features"].as_array().into_iter().flatten() {
        let (Some(lon), Some(lat)) = (f["geometry"]["coordinates"][0].as_f64(), f["geometry"]["coordinates"][1].as_f64()) else { continue };
        let p = &f["properties"];
        let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
        let street = match (s("housenumber"), s("street")) {
            (n, st) if !n.is_empty() && !st.is_empty() => format!("{n} {st}"),
            (_, st) => st,
        };
        let name = Some(s("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| street.clone());
        if name.is_empty() {
            continue;
        }
        let mut parts: Vec<String> = Vec::new();
        for part in [street, s("city"), s("state"), s("country")] {
            if !part.is_empty() && part != name && !parts.contains(&part) {
                parts.push(part);
            }
        }
        let bounds = p["extent"].as_array().filter(|e| e.len() == 4).and_then(|e| Some((e[0].as_f64()?, e[3].as_f64()?, e[2].as_f64()?, e[1].as_f64()?)));
        let kind = s("osm_value").replace('_', " ");
        out.push(Place { name, context: parts.join(", "), kind, lon, lat, bounds });
    }
    out
}

/// Nominatim `jsonv2` answers with an array (coordinates as strings; `boundingbox` is
/// south, north, west, east).
pub fn parse_nominatim(body: &Json) -> Vec<Place> {
    let num = |v: &Json| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64());
    let mut out = Vec::new();
    for r in body.as_array().into_iter().flatten() {
        let (Some(lon), Some(lat)) = (num(&r["lon"]), num(&r["lat"])) else { continue };
        let display = r["display_name"].as_str().unwrap_or("");
        let name =
            r["name"].as_str().filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| display.split(',').next().unwrap_or("").trim().to_string());
        if name.is_empty() {
            continue;
        }
        let context = display.split(',').map(str::trim).filter(|p| *p != name).collect::<Vec<_>>().join(", ");
        let b = &r["boundingbox"];
        let bounds = (|| Some((num(&b[2])?, num(&b[0])?, num(&b[3])?, num(&b[1])?)))();
        let kind = r["type"].as_str().unwrap_or("").replace('_', " ");
        out.push(Place { name, context, kind, lon, lat, bounds });
    }
    out
}

/// Drop results with the same name and place as an earlier one (a road split into segments,
/// say), keeping the first.
pub fn dedup(places: &mut Vec<Place>) {
    let mut seen = std::collections::HashSet::new();
    places.retain(|p| seen.insert((p.name.clone(), p.context.clone())));
}

impl Place {
    /// The camera to show it. Settlements center on their point at a zoom for their size
    /// (a city's administrative extent can reach far past the city: San Francisco's includes
    /// the Farallon Islands); other areas fit their extent; anything else gets a close-up.
    pub fn view(&self, cols: u16, rows: u16) -> (f64, f64, f64) {
        let settlement = match self.kind.as_str() {
            "city" => Some(12.0),
            "town" => Some(13.0),
            "village" | "suburb" | "borough" | "quarter" => Some(14.0),
            "hamlet" | "neighbourhood" | "locality" | "isolated dwelling" => Some(15.0),
            _ => None,
        };
        if let Some(z) = settlement {
            return (self.lon, self.lat, z);
        }
        match self.bounds {
            Some((w, s, e, n)) if (e - w).abs() > 1e-4 || (n - s).abs() > 1e-4 => crate::geojson::fit((w, s, e, n), cols, rows, 17.0),
            _ => (self.lon, self.lat, 16.0),
        }
    }
}

/// Words of an address or name for matching: lower case, split on anything but letters and
/// digits, with the usual street abbreviations made the same ("Avenue" and "Ave" are `ave`,
/// "Northwest" and "NW" are `nw`).
pub fn address_words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            match w {
                "avenue" | "av" => "ave",
                "street" | "str" => "st",
                "road" => "rd",
                "drive" => "dr",
                "boulevard" => "blvd",
                "place" => "pl",
                "court" => "ct",
                "lane" => "ln",
                "terrace" => "ter",
                "parkway" => "pkwy",
                "highway" => "hwy",
                "circle" => "cir",
                "square" => "sq",
                "pike" => "pk",
                "northwest" => "nw",
                "northeast" => "ne",
                "southwest" => "sw",
                "southeast" => "se",
                "north" => "n",
                "south" => "s",
                "east" => "e",
                "west" => "w",
                other => other,
            }
            .to_string()
        })
        .collect()
}

/// Whether every query word starts some word of `words`.
fn all_match(query: &[String], words: &[String]) -> bool {
    query.iter().all(|q| words.iter().any(|w| w.starts_with(q.as_str())))
}

/// A text property of a feature, the first of `keys` it has.
fn prop<'a>(f: &'a crate::geojson::Feature, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| {
        f.props.iter().find(|(name, _)| name == k).and_then(|(_, v)| match v {
            crate::mvt::Value::Str(s) if !s.trim().is_empty() => Some(s.trim()),
            _ => None,
        })
    })
}

/// A feature's center, lon/lat.
fn center(f: &crate::geojson::Feature) -> (f64, f64) {
    crate::render::to_lonlat((f.bbox[0] + f.bbox[2]) / 2.0, (f.bbox[1] + f.bbox[3]) / 2.0)
}

/// Search features already on the map (data layers: GeoJSON, GeoParquet, Overture addresses)
/// for `query`, each word matching the start of a word, street abbreviations aside:
///
/// - "1600 penn": the addresses with that house number on a street matching the rest;
/// - "vermont ave": the streets matching, one result each, framing their addresses;
/// - anything else: features whose name matches.
///
/// Up to `limit` results, addresses first, then streets, then names.
pub fn search_features<'a>(features: impl IntoIterator<Item = &'a crate::geojson::Feature>, query: &str, limit: usize) -> Vec<Place> {
    let words = address_words(query);
    if words.is_empty() {
        return Vec::new();
    }
    let number = words.first().filter(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit())).cloned();
    let rest: Vec<String> = if number.is_some() { words[1..].to_vec() } else { words.clone() };
    let (mut addresses, mut names) = (Vec::new(), Vec::new());
    // Streets: their name, the extent and count of their addresses, and their city.
    type Street = (String, (f64, f64, f64, f64), usize, String);
    let mut streets: Vec<Street> = Vec::new();
    for f in features {
        let street = prop(f, &["street", "addr:street"]);
        let house = prop(f, &["number", "housenumber", "addr:housenumber"]);
        let place = || {
            let city = prop(f, &["postal_city", "city", "addr:city"]).unwrap_or("");
            let post = prop(f, &["postcode", "addr:postcode"]).unwrap_or("");
            [city, post].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ")
        };
        if let Some(street) = street {
            let sw = address_words(street);
            if all_match(&rest, &sw) {
                match (&number, house) {
                    (Some(n), Some(h)) if address_words(h).first() == Some(n) && addresses.len() < limit => {
                        let (lon, lat) = center(f);
                        let unit = prop(f, &["unit", "addr:unit"]).map(|u| format!(" #{u}")).unwrap_or_default();
                        addresses.push(Place { name: format!("{h} {street}{unit}"), context: place(), kind: "address".into(), lon, lat, bounds: None });
                    }
                    (None, _) => {
                        let (lon, lat) = center(f);
                        match streets.iter_mut().find(|s| s.0.eq_ignore_ascii_case(street)) {
                            Some(s) => {
                                s.1 = (s.1.0.min(lon), s.1.1.min(lat), s.1.2.max(lon), s.1.3.max(lat));
                                s.2 += 1;
                            }
                            None => streets.push((street.to_string(), (lon, lat, lon, lat), 1, place())),
                        }
                    }
                    _ => {}
                }
            }
        }
        if number.is_none()
            && names.len() < limit
            && let Some(name) = prop(f, &["name", "title", "label", "NAME", "Name"])
            && all_match(&words, &address_words(name))
        {
            let (lon, lat) = center(f);
            names.push(Place { name: name.to_string(), context: String::new(), kind: "on the map".into(), lon, lat, bounds: None });
        }
    }
    // The streets with the most addresses first.
    streets.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    let streets = streets.into_iter().map(|(name, b, n, place)| {
        let pad = 0.0005;
        Place {
            name,
            context: [place, format!("{n} address{}", if n == 1 { "" } else { "es" })].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", "),
            kind: "street".into(),
            lon: (b.0 + b.2) / 2.0,
            lat: (b.1 + b.3) / 2.0,
            bounds: Some((b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad)),
        }
    });
    addresses.into_iter().chain(streets).chain(names).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn photon_results() {
        let body = json!({"type": "FeatureCollection", "features": [
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-77.0502, 38.8893]},
             "properties": {"name": "Lincoln Memorial", "osm_value": "memorial", "street": "Lincoln Memorial Circle Northwest",
                            "city": "Washington", "state": "District of Columbia", "country": "United States",
                            "extent": [-77.0509, 38.8897, -77.0496, 38.8888]}},
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-77.03, 38.9]},
             "properties": {"housenumber": "1600", "street": "Pennsylvania Avenue Northwest", "city": "Washington", "osm_value": "house"}},
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [1, 2]}, "properties": {}}
        ]});
        let r = parse_photon(&body);
        assert_eq!(r.len(), 2, "nameless results are skipped");
        assert_eq!(r[0].name, "Lincoln Memorial");
        assert_eq!(r[0].context, "Lincoln Memorial Circle Northwest, Washington, District of Columbia, United States");
        assert_eq!(r[0].bounds, Some((-77.0509, 38.8888, -77.0496, 38.8897)));
        assert_eq!(r[1].name, "1600 Pennsylvania Avenue Northwest");
        assert_eq!(r[1].context, "Washington");
        assert_eq!(r[1].view(100, 40), (-77.03, 38.9, 16.0), "points get a close-up");
        let sf = Place {
            name: "San Francisco".into(),
            context: "California, United States".into(),
            kind: "city".into(),
            lon: -122.42,
            lat: 37.78,
            bounds: Some((-123.17, 37.64, -122.28, 37.93)),
        };
        assert_eq!(sf.view(100, 40), (-122.42, 37.78, 12.0), "cities center on their point, not their county-wide extent");
        let mut dup = vec![r[1].clone(), r[0].clone(), r[1].clone()];
        dedup(&mut dup);
        assert_eq!(dup.len(), 2);
        let (lon, lat, z) = r[0].view(100, 40);
        assert!((lon - -77.05025).abs() < 1e-4 && (lat - 38.88925).abs() < 1e-3 && z > 15.0 && z <= 17.0, "{z}");
    }

    #[test]
    fn nominatim_results() {
        let body = json!([{"lat": "38.8893", "lon": "-77.0502", "name": "Lincoln Memorial", "type": "memorial",
            "display_name": "Lincoln Memorial, Lincoln Memorial Circle Northwest, Washington, District of Columbia, United States",
            "boundingbox": ["38.8888", "38.8897", "-77.0509", "-77.0496"]}]);
        let r = parse_nominatim(&body);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].context, "Lincoln Memorial Circle Northwest, Washington, District of Columbia, United States");
        assert_eq!(r[0].bounds, Some((-77.0509, 38.8888, -77.0496, 38.8897)));
        assert_eq!(r[0].kind, "memorial");
    }

    #[test]
    fn geocoder_from_env_values() {
        // Only the parsing rules; no network.
        let g = Geocoder::new(Api::Photon, PHOTON);
        assert_eq!(g.api, Api::Photon);
        let url = reqwest::Url::parse_with_params(PHOTON, &[("q", "Ben's Chili Bowl & more")]).unwrap();
        assert!(url.as_str().contains("q=Ben%27s+Chili+Bowl+%26+more"), "{url}");
    }

    #[test]
    fn searches_addresses_streets_and_names_on_the_map() {
        let doc = serde_json::json!({"type": "FeatureCollection", "features": [
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-77.0365, 38.8977]},
             "properties": {"number": "1600", "street": "Pennsylvania Avenue NW", "postcode": "20500", "postal_city": "Washington"}},
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-77.0340, 38.9004]},
             "properties": {"number": "800", "street": "Vermont Ave NW", "unit": "2"}},
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-77.0338, 38.9010]},
             "properties": {"number": "810", "street": "Vermont Ave NW"}},
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-77.03, 38.89]},
             "properties": {"name": "Freedom Plaza"}}
        ]});
        let (features, _) = crate::geojson::features(&doc).unwrap();
        let r = search_features(&features, "1600 penn", 10);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].name.as_str(), r[0].context.as_str(), r[0].kind.as_str()), ("1600 Pennsylvania Avenue NW", "Washington 20500", "address"));
        assert!((r[0].lon + 77.0365).abs() < 1e-6 && (r[0].lat - 38.8977).abs() < 1e-6);
        // "Avenue" finds "Ave", "Northwest" finds "NW".
        let r = search_features(&features, "vermont avenue northwest", 10);
        assert_eq!((r.len(), r[0].kind.as_str(), r[0].context.as_str()), (1, "street", "2 addresses"), "{r:?}");
        assert!(r[0].bounds.is_some());
        assert_eq!(search_features(&features, "800 vermont", 10)[0].name, "800 Vermont Ave NW #2");
        assert_eq!(search_features(&features, "freedom", 10)[0].name, "Freedom Plaza");
        assert!(search_features(&features, "1700 penn", 10).is_empty());
        assert!(search_features(&features, "  ", 10).is_empty());
        // Labels: a name, else the number and street.
        assert_eq!(features[0].label.as_deref(), Some("1600 Pennsylvania Avenue NW"));
        assert_eq!(features[3].label.as_deref(), Some("Freedom Plaza"));
    }
}
