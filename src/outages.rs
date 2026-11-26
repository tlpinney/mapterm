//! Power outages from the states that publish them: customers without power (and, where
//! given, customers served) by county and by a finer area, from the utilities' reports to
//! the state, updated every 15 minutes. Public ArcGIS services, no key.
//!
//! - Pennsylvania: PEMA (the Pennsylvania Emergency Management Agency) and PennDOT, by
//!   county and municipality.
//! - Maryland: MDEM (the Maryland Department of Emergency Management), by county and ZIP
//!   code. Its ZIP codes have no customer counts, so they're shaded by customers out.
//!
//! Each state is a [`Feed`] in [`FEEDS`]: where its layers are and which fields hold what.
//! The outlines don't change, so each layer's are fetched once and kept on disk for 30 days
//! in `~/.cache/mapterm/outages` ([`outlines`]); the counts are asked for without them
//! ([`counts`]), with the ETag, so an unchanged answer is a tiny reply. [`Outages`] holds
//! what's arrived, for the map. Only counts, no outage areas or restoration times: the
//! services have none.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// From this zoom the map shows the finer areas (municipalities, ZIP codes), below it
/// counties.
pub const LOCAL_ZOOM: f64 = 9.0;
/// How long outlines are kept.
const OUTLINES_FRESH: Duration = Duration::from_secs(30 * 24 * 3600);
/// At most this many pages of features (the services give 2,000 to 5,000 a page;
/// Pennsylvania has 2,572 municipalities).
const MAX_PAGES: usize = 5;

/// Counties, or the finer areas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// Counties (and independent cities).
    County,
    /// Municipalities or ZIP codes, as the state gives them.
    Local,
}

impl Level {
    /// The level shown at map zoom `zoom`.
    pub fn at(zoom: f64) -> Level {
        if zoom >= LOCAL_ZOOM { Level::Local } else { Level::County }
    }

    /// "county" or "local", in file names and feature properties.
    pub fn name(self) -> &'static str {
        match self {
            Level::County => "county",
            Level::Local => "local",
        }
    }
}

/// One layer of a state's outages: where it is and which fields hold what.
pub struct Layer {
    /// The ArcGIS layer's URL (ending in its number).
    pub url: &'static str,
    /// What an area is: "county", "municipality", "ZIP code".
    pub kind: &'static str,
    /// The fields that identify an area, joined with `-` (the same in the counts and the
    /// outlines).
    pub key: &'static [&'static str],
    /// The area's name.
    pub name: &'static str,
    /// A code shown with the name (a ZIP code).
    pub code: Option<&'static str>,
    /// The name is in capitals: shown in title case.
    pub shouting: bool,
    /// The county's name, for a finer area.
    pub county: Option<&'static str>,
    /// Customers without power.
    pub out: &'static str,
    /// Customers served, where given.
    pub served: Option<&'static str>,
    /// The county's customers without power, for a finer area.
    pub county_out: Option<&'static str>,
    /// When the figures were gathered (an ArcGIS date: milliseconds).
    pub updated: Option<&'static str>,
    /// Only the areas with customers out (a URL-encoded `where`), or all (`1%3D1`).
    pub filter: &'static str,
    /// What its areas are called in a legend: "MD ZIP codes".
    pub group: &'static str,
    /// How far (degrees) the service may move an outline's points when simplifying it.
    pub simplify: f64,
}

/// A state's outages: its counties, and a finer level where it has one.
pub struct Feed {
    /// A short id, also the start of each area's key and its cache files: "pa".
    pub id: &'static str,
    /// The state's postal code, for the status line: "PA".
    pub state: &'static str,
    /// The state's FIPS code: "42". Other sources (ODIN) leave its counties to this feed.
    pub fips: &'static str,
    /// Who to credit: "PEMA / PennDOT".
    pub credit: &'static str,
    /// Its county layer.
    pub county: Layer,
    /// Its finer layer.
    pub local: Option<Layer>,
}

impl Feed {
    /// The layer for `level`.
    pub fn layer(&self, level: Level) -> Option<&Layer> {
        match level {
            Level::County => Some(&self.county),
            Level::Local => self.local.as_ref(),
        }
    }
}

/// The states mapterm knows.
pub const FEEDS: [Feed; 2] = [
    Feed {
        id: "pa",
        state: "PA",
        fips: "42",
        credit: "PEMA / PennDOT",
        county: Layer {
            url: "https://gis.penndot.pa.gov/gis/rest/services/poweroutages/papoweroutages/FeatureServer/1",
            kind: "county",
            key: &["FIPS"],
            name: "NAME",
            code: None,
            shouting: false,
            county: None,
            out: "CUSTOMERS_OUT",
            served: Some("EST_CUST_SERVED"),
            county_out: None,
            updated: Some("UPDATE_DATE"),
            filter: "1%3D1",
            group: "PA counties",
            simplify: 0.002,
        },
        local: Some(Layer {
            url: "https://gis.penndot.pa.gov/gis/rest/services/poweroutages/papoweroutages/FeatureServer/4",
            kind: "municipality",
            key: &["FIPS", "FIPS_MUNICIPAL_CODE"],
            name: "FIPS_MUNICIPAL_NAME",
            code: None,
            shouting: true,
            county: Some("COUNTY_NAME"),
            out: "MUNICIPAL_OUT",
            served: Some("EST_CUST_SERVED"),
            county_out: Some("COUNTY_OUT"),
            updated: Some("UPDATE_DATE"),
            filter: "MUNICIPAL_OUT%3E0",
            group: "PA municipalities",
            simplify: 0.0005,
        }),
    },
    Feed {
        id: "md",
        state: "MD",
        fips: "24",
        credit: "Maryland Dept. of Emergency Management",
        county: Layer {
            url: "https://services.arcgis.com/njFNhDsUCentVYJW/arcgis/rest/services/PowerOutage_CountiesGeneralDEV_Join/FeatureServer/0",
            kind: "county",
            key: &["COUNTY"],
            name: "COUNTY",
            code: None,
            shouting: false,
            county: None,
            out: "outages",
            served: Some("customers"),
            county_out: None,
            updated: Some("dt_stamp"),
            filter: "1%3D1",
            group: "MD counties",
            simplify: 0.002,
        },
        local: Some(Layer {
            url: "https://services.arcgis.com/njFNhDsUCentVYJW/arcgis/rest/services/PowerOutage_ZIP_Codes_5_DigitDEV_Join/FeatureServer/0",
            kind: "ZIP code",
            key: &["ZIPCODE1"],
            name: "ZIPName",
            code: Some("ZIPCODE1"),
            shouting: false,
            county: None,
            out: "outages",
            served: None,
            county_out: None,
            updated: Some("dt_stamp"),
            filter: "outages%3E0",
            group: "MD ZIP codes",
            simplify: 0.0005,
        }),
    },
];

/// One area's outage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Area {
    /// The feed's id and the area's own: `pa:42029`, `pa:42029-83080`, `md:Anne Arundel`.
    pub key: String,
    /// The state's postal code.
    pub state: &'static str,
    /// What it is: "county", "municipality", "ZIP code".
    pub kind: &'static str,
    /// "Chester", "West Goshen Twp", "Waldorf".
    pub name: String,
    /// A code shown with the name: "20601".
    pub code: Option<String>,
    /// The county's name, for a finer area.
    pub county: Option<String>,
    /// Customers without power.
    pub out: u64,
    /// Customers served (an estimate), where the state gives it.
    pub served: Option<u64>,
    /// For a finer area, its county's customers without power.
    pub county_out: Option<u64>,
    /// When the utilities' figures were gathered (Unix seconds).
    pub updated: Option<f64>,
    /// Who the figures are from, for the panel: "PEMA / PennDOT", "ODIN".
    pub source: &'static str,
    /// What it's counted with in a legend by customers out: "MD ZIP codes", "ODIN counties".
    pub group: &'static str,
    /// The utilities reporting it, with their customers out (ODIN).
    pub utilities: Vec<(String, u64)>,
    /// The latest estimated restoration time given (Unix seconds; ODIN).
    pub restore: Option<f64>,
    /// The causes given (ODIN).
    pub causes: Vec<String>,
    /// A utility's disclaimer about its figures (ODIN).
    pub disclaimer: Option<String>,
}

impl Area {
    /// The share of customers without power, 0 to 1, where customers served are known.
    pub fn share(&self) -> Option<f64> {
        let s = self.served?;
        Some(if s == 0 { if self.out > 0 { 1.0 } else { 0.0 } } else { (self.out as f64 / s as f64).min(1.0) })
    }

    /// How it's named in a panel: "Chester County", "Baltimore City", "West Goshen Twp",
    /// "Waldorf (20601)".
    pub fn title(&self) -> String {
        match (&self.code, self.kind) {
            (Some(c), _) => format!("{} ({c})", self.name),
            (None, "county") if !self.name.to_lowercase().contains("city") => format!("{} County", self.name),
            _ => self.name.clone(),
        }
    }
}

/// The color bands by the share of customers out: (from, label, color).
pub const BANDS: [(f64, &str, (u8, u8, u8)); 5] = [
    (0.0, "under 1%", (0xff, 0xe0, 0x78)),
    (0.01, "1–5%", (0xff, 0xb4, 0x46)),
    (0.05, "5–15%", (0xff, 0x7a, 0x32)),
    (0.15, "15–30%", (0xe8, 0x40, 0x3a)),
    (0.30, "30% or more", (0xc8, 0x2a, 0x8c)),
];

/// The same colors by customers out, for areas without customers served: (from, label).
pub const COUNT_BANDS: [(u64, &str); 5] = [(1, "1–9"), (10, "10–99"), (100, "100–499"), (500, "500–1,999"), (2000, "2,000 or more")];

/// The band an area falls in (by share where known, else by count), None with nobody out.
pub fn band(a: &Area) -> Option<usize> {
    if a.out == 0 {
        return None;
    }
    Some(match a.share() {
        Some(s) => BANDS.iter().rposition(|b| s >= b.0).unwrap_or(0),
        None => COUNT_BANDS.iter().rposition(|b| a.out >= b.0).unwrap_or(0),
    })
}

/// An area's color: its band's, or grey with nobody out.
pub fn rgb(a: &Area) -> (u8, u8, u8) {
    band(a).map_or((0x80, 0x80, 0x80), |i| BANDS[i].2)
}

/// A field's value as text: a string, or a whole number.
fn text(p: &Value, k: &str) -> Option<String> {
    let v = p.get(k)?;
    let s = v.as_str().map(|s| s.trim().to_string()).or_else(|| v.as_i64().map(|n| n.to_string()))?;
    (!s.is_empty()).then_some(s)
}

/// An area's key from its attributes (or GeoJSON properties): the feed's id and the key
/// fields. Plain text only (letters, digits, spaces, `.`, `'`, `-`), at most 40 characters.
fn key(feed: &Feed, layer: &Layer, p: &Value) -> Option<String> {
    let parts: Option<Vec<String>> = layer.key.iter().map(|k| text(p, k)).collect();
    let k = parts?.join("-");
    let plain = k.len() <= 40 && k.chars().all(|c| c.is_ascii_alphanumeric() || " .'-".contains(c));
    plain.then(|| format!("{}:{k}", feed.id))
}

/// "WEST GOSHEN TWP" as "West Goshen Twp".
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The areas in an ArcGIS query answer (`f=json`) of a feed's level, and whether there are
/// more pages.
pub fn parse_counts(feed: &Feed, level: Level, text_: &str) -> Result<(Vec<Area>, bool)> {
    let layer = feed.layer(level).context("outages: no such level")?;
    let v: Value = serde_json::from_str(text_).context("outages")?;
    if let Some(e) = v.get("error") {
        bail!("outages ({}): {}", feed.state, e.get("message").and_then(Value::as_str).unwrap_or("the service answered with an error"));
    }
    let feats = v.get("features").and_then(Value::as_array).context("outages: no features")?;
    let more = v.get("exceededTransferLimit").and_then(Value::as_bool).unwrap_or(false);
    let num = |p: &Value, k: &str| p.get(k).and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0).map(|n| n.round() as u64);
    let areas = feats
        .iter()
        .filter_map(|f| {
            let p = f.get("attributes")?;
            let name = text(p, layer.name).unwrap_or_default();
            Some(Area {
                key: key(feed, layer, p)?,
                state: feed.state,
                kind: layer.kind,
                name: if layer.shouting { title_case(&name) } else { name },
                code: layer.code.and_then(|c| text(p, c)),
                county: layer.county.and_then(|c| text(p, c)),
                out: num(p, layer.out).unwrap_or(0),
                served: layer.served.and_then(|s| num(p, s)),
                county_out: layer.county_out.and_then(|c| num(p, c)),
                updated: layer.updated.and_then(|u| p.get(u)?.as_f64()).map(|ms| ms / 1000.0),
                source: feed.credit,
                group: layer.group,
                ..Area::default()
            })
        })
        .collect();
    Ok((areas, more))
}

/// The query for a feed's level's counts, from `offset`.
fn counts_url(feed: &Feed, level: Level, offset: usize) -> Option<String> {
    let l = feed.layer(level)?;
    let mut fields: Vec<&str> = l.key.to_vec();
    for f in [Some(l.name), l.code, l.county, Some(l.out), l.served, l.county_out, l.updated].into_iter().flatten() {
        if !fields.contains(&f) {
            fields.push(f);
        }
    }
    Some(format!("{}/query?where={}&outFields={}&returnGeometry=false&resultOffset={offset}&f=json", l.url, l.filter, fields.join(",")))
}

/// A feed's level's counts, unless they're unchanged since `etag` (then None); with the new
/// ETag. Finer areas: only those with customers out.
pub async fn counts(feed: &Feed, level: Level, etag: Option<&str>) -> Result<Option<(Vec<Area>, Option<String>)>> {
    let mut all = Vec::new();
    let mut tag = None;
    for page in 0..MAX_PAGES {
        let url = counts_url(feed, level, all.len()).context("outages: no such level")?;
        let mut req = crate::source::http_client().get(url);
        if page == 0
            && let Some(e) = etag
        {
            req = req.header("If-None-Match", e);
        }
        let resp = req.send().await.with_context(|| format!("outages ({})", feed.state))?;
        if page == 0 && resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("outages ({}): HTTP {}", feed.state, resp.status());
        }
        if page == 0 {
            tag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
        }
        let (areas, more) = parse_counts(feed, level, &resp.text().await.context("outages")?)?;
        let got = areas.len();
        all.extend(areas);
        if !more || got == 0 {
            break;
        }
    }
    Ok(Some((all, tag)))
}

/// The outlines in a GeoJSON FeatureCollection of a feed's level, by key.
pub fn parse_outlines(feed: &Feed, level: Level, text_: &str) -> Result<HashMap<String, Value>> {
    let layer = feed.layer(level).context("outages: no such level")?;
    let v: Value = serde_json::from_str(text_).context("outage outlines")?;
    if let Some(e) = v.get("error") {
        bail!("outage outlines ({}): {}", feed.state, e.get("message").and_then(Value::as_str).unwrap_or("the service answered with an error"));
    }
    Ok(v.get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| Some((key(feed, layer, f.get("properties")?)?, f.get("geometry").filter(|g| !g.is_null())?.clone())))
        .collect())
}

fn outline_file(feed: &Feed, level: Level) -> std::path::PathBuf {
    crate::source::cache_root().join("outages").join(format!("{}-{}.geojson", feed.id, level.name()))
}

/// A feed's level's outlines (GeoJSON geometries, lon/lat) by key: from the disk cache when
/// under 30 days old, else from the service (a page or two), kept. If the service can't be
/// reached, an older copy will do.
pub async fn outlines(feed: &Feed, level: Level) -> Result<HashMap<String, Value>> {
    let layer = feed.layer(level).context("outages: no such level")?;
    let p = outline_file(feed, level);
    let read = || -> Option<HashMap<String, Value>> { parse_outlines(feed, level, &std::fs::read_to_string(&p).ok()?).ok().filter(|m| !m.is_empty()) };
    let age = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok());
    if age.is_some_and(|a| a < OUTLINES_FRESH)
        && let Some(m) = read()
    {
        return Ok(m);
    }
    let fetched = async {
        let mut feats: Vec<Value> = Vec::new();
        for _ in 0..MAX_PAGES {
            let url = format!(
                "{}/query?where=1%3D1&outFields={}&outSR=4326&maxAllowableOffset={}&geometryPrecision=4&resultOffset={}&f=geojson",
                layer.url,
                layer.key.join(","),
                layer.simplify,
                feats.len()
            );
            let resp = crate::source::http_client().get(url).send().await.with_context(|| format!("outage outlines ({})", feed.state))?;
            if !resp.status().is_success() {
                bail!("outage outlines ({}): HTTP {}", feed.state, resp.status());
            }
            let v: Value = serde_json::from_str(&resp.text().await?).context("outage outlines")?;
            if let Some(e) = v.get("error") {
                bail!("outage outlines ({}): {}", feed.state, e.get("message").and_then(Value::as_str).unwrap_or("the service answered with an error"));
            }
            let page = v.get("features").and_then(Value::as_array).cloned().unwrap_or_default();
            let more = v.get("exceededTransferLimit").and_then(Value::as_bool).unwrap_or(false)
                || v.pointer("/properties/exceededTransferLimit").and_then(Value::as_bool).unwrap_or(false);
            let got = page.len();
            feats.extend(page);
            if !more || got == 0 {
                break;
            }
        }
        let text_ = serde_json::to_string(&json!({"type": "FeatureCollection", "features": feats}))?;
        let m = parse_outlines(feed, level, &text_)?;
        if m.is_empty() {
            bail!("outage outlines ({}): none in the answer", feed.state);
        }
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        let tmp = p.with_extension("part");
        if std::fs::write(&tmp, &text_).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
        Ok::<_, anyhow::Error>(m)
    }
    .await;
    match fetched {
        Ok(m) => Ok(m),
        Err(e) => read().ok_or(e),
    }
}

/// What's arrived from every feed: each level's figures and outlines.
#[derive(Default)]
pub struct Outages {
    counts: HashMap<(usize, Level), Vec<Area>>,
    outlines: HashMap<(usize, Level), HashMap<String, Value>>,
    /// ODIN's counties (outside the states above), shown at both levels.
    odin: Vec<Area>,
    /// Their outlines, by key.
    odin_outlines: HashMap<String, Value>,
    /// ODIN utilities left out as not reporting lately.
    odin_stale: usize,
}

impl Outages {
    /// Feed `feed`'s (an index into [`FEEDS`]) figures for `level`.
    pub fn set_counts(&mut self, feed: usize, level: Level, areas: Vec<Area>) {
        self.counts.insert((feed, level), areas);
    }

    /// Feed `feed`'s outlines for `level`.
    pub fn set_outlines(&mut self, feed: usize, level: Level, outlines: HashMap<String, Value>) {
        self.outlines.insert((feed, level), outlines);
    }

    /// ODIN's counties, and how many utilities were left out as not reporting lately.
    pub fn set_odin(&mut self, areas: Vec<Area>, stale: usize) {
        (self.odin, self.odin_stale) = (areas, stale);
    }

    /// Outlines of ODIN's counties, by key (added to those already in).
    pub fn add_odin_outlines(&mut self, outlines: HashMap<String, Value>) {
        self.odin_outlines.extend(outlines);
    }

    /// The ODIN counties without an outline yet.
    pub fn odin_missing(&self) -> Vec<String> {
        self.odin.iter().filter(|a| !self.odin_outlines.contains_key(&a.key)).map(|a| a.key.clone()).collect()
    }

    /// Whether any county figures have arrived.
    pub fn has_counts(&self) -> bool {
        self.counts.keys().any(|(_, l)| *l == Level::County) || !self.odin.is_empty()
    }

    /// The area with this key, at either level.
    pub fn find(&self, key: &str) -> Option<&Area> {
        self.counts.values().flatten().chain(&self.odin).find(|a| a.key == key)
    }

    /// The areas at `level`, the states' and ODIN's.
    fn at_level(&self, level: Level) -> impl Iterator<Item = &Area> {
        self.counts.iter().filter(move |((_, l), _)| *l == level).flat_map(|(_, v)| v).chain(&self.odin)
    }

    /// Every feed's areas at `level`, each with its outline where it's in: counties with
    /// nobody out too (a faint outline), finer areas only with customers out. In drawing
    /// order: nobody out first, then by band, the selected one last.
    fn drawn(&self, level: Level, selected: Option<&str>) -> Vec<(&Area, &Value)> {
        let mut v: Vec<(&Area, &Value)> = self
            .counts
            .iter()
            .filter(|((_, l), _)| *l == level)
            .flat_map(|((f, l), areas)| {
                let outlines = self.outlines.get(&(*f, *l));
                areas.iter().filter_map(move |a| Some((a, outlines?.get(&a.key)?)))
            })
            .chain(self.odin.iter().filter_map(|a| Some((a, self.odin_outlines.get(&a.key)?))))
            .filter(|(a, _)| level == Level::County || a.out > 0)
            .collect();
        let rank = |a: &Area| (Some(a.key.as_str()) == selected, band(a).map_or(-1, |b| b as i64), a.out);
        v.sort_by_key(|(a, _)| rank(a));
        v
    }

    /// The areas at `level` as tiles to draw: shaded by band, counties with nobody out a faint
    /// outline, the one with key `selected` highlighted.
    pub fn shapes(&self, level: Level, selected: Option<&str>) -> Vec<crate::areas::Shape> {
        self.drawn(level, selected)
            .into_iter()
            .map(|(a, g)| {
                let mut s = crate::areas::Shape::new(rgb(a), Some(a.key.as_str()) == selected);
                (s.fill, s.line) = if a.out > 0 { (110, 200) } else { (0, 70) };
                s.add(g);
                s
            })
            .collect()
    }

    /// The areas at `level` as GeoJSON for the map (braille, and clicks): properties `key`,
    /// `level`, `color`, `out` and `selected`, in drawing order.
    pub fn features(&self, level: Level, selected: Option<&str>) -> Value {
        let feats: Vec<Value> = self
            .drawn(level, selected)
            .into_iter()
            .map(|(a, g)| {
                let (r, gr, b) = rgb(a);
                json!({
                    "type": "Feature",
                    "geometry": g,
                    "properties": {"key": a.key, "level": level.name(), "color": format!("#{r:02x}{gr:02x}{b:02x}"), "out": a.out,
                                   "selected": Some(a.key.as_str()) == selected},
                })
            })
            .collect();
        json!({"type": "FeatureCollection", "features": feats})
    }

    /// Legends for `level`: the share bands, with how many areas are in each; and, where some
    /// areas have no customers served (Maryland's ZIP codes), the count bands.
    pub fn legends(&self, level: Level) -> Vec<crate::legend::Legend> {
        use ratatui::style::Color;
        let areas: Vec<&Area> = self.at_level(level).collect();
        let (mut by_share, mut by_count) = ([0usize; 5], [0usize; 5]);
        let (mut shares, mut counts) = (false, false);
        for a in &areas {
            match a.share() {
                Some(_) => shares = true,
                None => counts = true,
            }
            if let Some(i) = band(a) {
                if a.share().is_some() { by_share[i] += 1 } else { by_count[i] += 1 }
            }
        }
        let mut out = Vec::new();
        let place = if level == Level::County { "counties" } else { "areas" };
        if shares {
            out.push(BANDS.iter().zip(by_share).rev().fold(crate::legend::Legend::new(format!("{place} by share out")), |l, ((_, label, (r, g, b)), n)| {
                l.entry("■", Color::Rgb(*r, *g, *b), *label, Some(n))
            }));
        }
        if counts {
            let groups: Vec<&str> = {
                let mut k: Vec<&str> = areas.iter().filter(|a| a.share().is_none()).map(|a| a.group).collect();
                k.sort_unstable();
                k.dedup();
                k
            };
            let title = format!("{} by customers out", groups.join(", "));
            let mut l = COUNT_BANDS
                .iter()
                .zip(BANDS.iter())
                .zip(by_count)
                .rev()
                .fold(crate::legend::Legend::new(title), |l, (((_, label), (_, _, (r, g, b))), n)| l.entry("■", Color::Rgb(*r, *g, *b), *label, Some(n)));
            if self.odin_stale > 0 {
                l = l.entry("·", Color::DarkGray, "ODIN utilities not reporting", Some(self.odin_stale));
            }
            out.push(l);
        }
        out
    }

    /// Each state's customers out (from its counties), in [`FEEDS`] order, then ODIN's
    /// elsewhere: ("PA", 197069), ("ODIN", 25431).
    pub fn totals(&self) -> Vec<(&'static str, u64)> {
        let mut t: Vec<(&'static str, u64)> =
            (0..FEEDS.len()).filter_map(|f| Some((FEEDS[f].state, self.counts.get(&(f, Level::County))?.iter().map(|a| a.out).sum()))).collect();
        if !self.odin.is_empty() {
            t.push(("ODIN", self.odin.iter().map(|a| a.out).sum()));
        }
        t
    }
}

/// The map layers for outages from GeoJSON source `source`: areas with customers out filled
/// in their band's color, every area outlined, and the selected one bright.
pub fn layers(source: &str) -> Result<Vec<crate::StyleLayer>> {
    let fill = json!({"id": format!("{source}-fill"), "type": "fill", "source": source, "filter": [">", ["get", "out"], 0],
                      "paint": {"fill-color": ["get", "color"], "fill-opacity": 0.35}});
    let line = json!({"id": format!("{source}-line"), "type": "line", "source": source, "paint": {"line-color": ["get", "color"], "line-width": 1}});
    let selected = json!({"id": format!("{source}-selected"), "type": "line", "source": source, "filter": ["==", ["get", "selected"], true],
                          "paint": {"line-color": "#ffffff", "line-width": 2}});
    [fill, line, selected].iter().map(|j| Ok(crate::StyleLayer::from_json(j)?.0)).collect()
}

/// The credit shown while outages are on: every feed's, and ODIN's.
pub fn credit() -> String {
    format!("Power outages: {}, {}", FEEDS.iter().map(|f| f.credit).collect::<Vec<_>>().join(", "), crate::odin::CREDIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PA_COUNTIES: &str = r#"{"features":[
      {"attributes":{"FIPS":"42029","NAME":"Chester","CUSTOMERS_OUT":39962,"EST_CUST_SERVED":208818,"UPDATE_DATE":1795726920000}},
      {"attributes":{"FIPS":"42001","NAME":"Adams","CUSTOMERS_OUT":0,"EST_CUST_SERVED":41124,"UPDATE_DATE":1795726920000}},
      {"attributes":{"FIPS":"../x","NAME":"Bad","CUSTOMERS_OUT":1,"EST_CUST_SERVED":2}}
    ],"exceededTransferLimit":false}"#;
    const PA_TOWNS: &str = r#"{"features":[
      {"attributes":{"FIPS":"42029","FIPS_MUNICIPAL_CODE":"83080","FIPS_MUNICIPAL_NAME":"WEST GOSHEN TWP","COUNTY_NAME":"Chester",
        "COUNTY_OUT":39962,"MUNICIPAL_OUT":4111,"EST_CUST_SERVED":9733.0,"UPDATE_DATE":1795726920000}}
    ],"exceededTransferLimit":true}"#;
    const PA_OUTLINES: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-76,39.7],[-75.5,39.7],[-75.5,40.2],[-76,39.7]]]},"properties":{"FIPS":"42029"}},
      {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-77.5,39.7],[-77,39.7],[-77,40.1],[-77.5,39.7]]]},"properties":{"FIPS":"42001"}},
      {"type":"Feature","geometry":null,"properties":{"FIPS":"42003"}}
    ]}"#;
    const MD_COUNTIES: &str = r#"{"features":[
      {"attributes":{"COUNTY":"Anne Arundel","outages":16807,"customers":265997,"dt_stamp":1795730458000}},
      {"attributes":{"COUNTY":"Baltimore City","outages":5,"customers":290000,"dt_stamp":1795730458000}}
    ]}"#;
    const MD_ZIPS: &str = r#"{"features":[
      {"attributes":{"ZIPCODE1":"20601","ZIPName":"Waldorf","outages":313,"dt_stamp":1795730458000}}
    ]}"#;

    fn pa() -> &'static Feed {
        &FEEDS[0]
    }
    fn md() -> &'static Feed {
        &FEEDS[1]
    }

    #[test]
    fn reads_each_states_counts() {
        let (c, more) = parse_counts(pa(), Level::County, PA_COUNTIES).unwrap();
        assert!(!more);
        assert_eq!(c.len(), 2, "a bad key is left out");
        assert_eq!((c[0].key.as_str(), c[0].title(), c[0].out, c[0].served), ("pa:42029", "Chester County".to_string(), 39962, Some(208818)));
        assert_eq!(c[0].updated, Some(1_795_726_920.0));
        assert!((c[0].share().unwrap() - 0.1914).abs() < 0.001);
        let (t, more) = parse_counts(pa(), Level::Local, PA_TOWNS).unwrap();
        assert!(more, "more pages");
        assert_eq!((t[0].key.as_str(), t[0].title(), t[0].county.as_deref()), ("pa:42029-83080", "West Goshen Twp".to_string(), Some("Chester")));
        assert_eq!((t[0].out, t[0].county_out, t[0].kind), (4111, Some(39962), "municipality"));
        let (m, _) = parse_counts(md(), Level::County, MD_COUNTIES).unwrap();
        assert_eq!((m[0].key.as_str(), m[0].title(), m[0].state), ("md:Anne Arundel", "Anne Arundel County".to_string(), "MD"));
        assert_eq!(m[1].title(), "Baltimore City");
        let (z, _) = parse_counts(md(), Level::Local, MD_ZIPS).unwrap();
        assert_eq!((z[0].key.as_str(), z[0].title(), z[0].served, z[0].share()), ("md:20601", "Waldorf (20601)".to_string(), None, None));
        assert!(parse_counts(pa(), Level::County, r#"{"error":{"code":400,"message":"Invalid query"}}"#).unwrap_err().to_string().contains("Invalid query"));
    }

    #[test]
    fn bands_by_share_or_count() {
        let a = |out, served| Area { key: "x:1".into(), state: "PA", kind: "county", out, served, ..Area::default() };
        assert_eq!(band(&a(0, Some(100))), None);
        assert_eq!(band(&a(1, Some(1000))), Some(0));
        assert_eq!(band(&a(1, Some(100))), Some(1));
        assert_eq!(band(&a(10, Some(100))), Some(2));
        assert_eq!(band(&a(20, Some(100))), Some(3));
        assert_eq!(band(&a(4111, Some(9733))), Some(4));
        assert_eq!(band(&a(5, Some(0))), Some(4), "out with no customers counted: the top band");
        assert_eq!(band(&a(313, None)), Some(2), "no customers served: by count");
        assert_eq!(band(&a(2500, None)), Some(4));
        assert_eq!(Level::at(8.9), Level::County);
        assert_eq!(Level::at(9.0), Level::Local);
    }

    #[test]
    fn urls_are_each_layers() {
        let u = counts_url(pa(), Level::Local, 5000).unwrap();
        assert!(u.starts_with("https://gis.penndot.pa.gov/gis/rest/services/poweroutages/papoweroutages/FeatureServer/4/query?where=MUNICIPAL_OUT%3E0"));
        assert!(u.contains("resultOffset=5000") && u.contains("FIPS,FIPS_MUNICIPAL_CODE,FIPS_MUNICIPAL_NAME"));
        let u = counts_url(md(), Level::Local, 0).unwrap();
        assert!(u.contains("PowerOutage_ZIP_Codes_5_DigitDEV_Join/FeatureServer/0/query?where=outages%3E0&outFields=ZIPCODE1,ZIPName,outages,dt_stamp"), "{u}");
        for f in &FEEDS {
            for l in [Level::County, Level::Local] {
                assert!(f.layer(l).is_some_and(|l| l.url.starts_with("https://")));
            }
        }
        assert_eq!(
            credit(),
            "Power outages: PEMA / PennDOT, Maryland Dept. of Emergency Management, Outage Data Initiative Nationwide (ODIN), ORNL / U.S. DOE"
        );
    }

    #[test]
    fn states_draw_together() {
        let mut o = Outages::default();
        o.set_outlines(0, Level::County, parse_outlines(pa(), Level::County, PA_OUTLINES).unwrap());
        o.set_counts(0, Level::County, parse_counts(pa(), Level::County, PA_COUNTIES).unwrap().0);
        let md_outline = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[-76.8,38.8],[-76.4,38.8],[-76.4,39.2],[-76.8,38.8]]]},"properties":{"COUNTY":"Anne Arundel"}}]}"#;
        o.set_outlines(1, Level::County, parse_outlines(md(), Level::County, md_outline).unwrap());
        o.set_counts(1, Level::County, parse_counts(md(), Level::County, MD_COUNTIES).unwrap().0);
        assert!(o.has_counts());
        assert_eq!(o.totals(), vec![("PA", 39962), ("MD", 16812)]);
        let fc = o.features(Level::County, Some("pa:42029"));
        let f = fc["features"].as_array().unwrap();
        assert_eq!(f.len(), 3, "Baltimore City has no outline here");
        assert_eq!((f[0]["properties"]["key"].as_str(), f[0]["properties"]["color"].as_str()), (Some("pa:42001"), Some("#808080")), "nobody out: grey, first");
        assert_eq!((f[2]["properties"]["key"].as_str(), f[2]["properties"]["selected"].as_bool()), (Some("pa:42029"), Some(true)), "selected last");
        assert_eq!(f[1]["properties"]["level"], "county");
        assert_eq!(o.find("md:Anne Arundel").map(|a| a.out), Some(16807));
        // ZIP codes without customers served: a second legend, by count.
        o.set_counts(1, Level::Local, parse_counts(md(), Level::Local, MD_ZIPS).unwrap().0);
        o.set_counts(0, Level::Local, parse_counts(pa(), Level::Local, PA_TOWNS).unwrap().0);
        let l = o.legends(Level::Local);
        assert_eq!(l.len(), 2);
        assert_eq!(o.legends(Level::County).len(), 1);
        // Tiles: Chester filled, Adams only outlined.
        let tiles = crate::areas::AreaTiles::default();
        tiles.set(o.shapes(Level::County, None));
        let alpha = |lon: f64, lat: f64| {
            let n = 256.0;
            let (wx, wy) = crate::areas::world(lon, lat);
            let (x, y) = ((wx * n) as u32, (wy * n) as u32);
            let rgba = tiles.draw(8, x, y, 512).expect("a tile");
            rgba[((((wy * n - y as f64) * 512.0) as usize) * (512 + crate::areas::BLEED as usize) + ((wx * n - x as f64) * 512.0) as usize) * 4 + 3]
        };
        assert!(alpha(-75.6, 39.8) > 80, "filled");
        assert_eq!(alpha(-77.1, 39.75), 0, "not filled");
        assert!(alpha(-76.5, 38.9) > 80, "Maryland too");
    }

    #[test]
    fn a_click_finds_the_area() {
        let mut o = Outages::default();
        o.set_outlines(0, Level::County, parse_outlines(pa(), Level::County, PA_OUTLINES).unwrap());
        o.set_counts(0, Level::County, parse_counts(pa(), Level::County, PA_COUNTIES).unwrap().0);
        let mut map = crate::Map::new();
        map.add_source("outages", crate::MapSource::geojson(&o.features(Level::County, None)).unwrap()).unwrap();
        map.add_layers(layers("outages").unwrap(), None).unwrap();
        let (cam, area) = (crate::Camera::new(-76.2, 39.9, 7.0), ratatui::layout::Rect::new(0, 0, 80, 24));
        let keys = |lon: f64, lat: f64| -> Vec<String> {
            let dot = crate::Viewport::new(&cam, area).dot_ll(lon, lat);
            map.query_features(&cam, area, dot)
                .into_iter()
                .filter(|p| p.source.as_deref() == Some("outages"))
                .filter_map(|p| p.prop("key")?.as_str().map(str::to_string))
                .collect()
        };
        assert!(keys(-75.6, 39.8).contains(&"pa:42029".to_string()));
        assert!(keys(-76.5, 40.15).is_empty());
    }
}
