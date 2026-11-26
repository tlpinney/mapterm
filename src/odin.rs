//! Power outages from ODIN, the Outage Data Initiative Nationwide (Oak Ridge National
//! Laboratory, for the U.S. Department of Energy): the outages participating utilities
//! report, by county, from its public endpoints (no key), updated every 5 to 15 minutes.
//!
//! - `/odi` lists the current outages, each a utility's customers out in a county, with its
//!   estimated restoration time, cause and disclaimer where given. Asked for with the ETag
//!   (an unchanged list is a 304), a page of 1,000 at a time.
//! - `/odi/status` lists the reporting utilities and when each last reported. A utility that
//!   hasn't reported for [`STALE`] is left out rather than shown with old figures.
//! - `/odi/map` gives county outlines: asked for only for counties not seen before, and kept
//!   on disk (`~/.cache/mapterm/outages/odin-counties.json`), as counties don't change.
//!
//! Coverage is only the participating utilities (159 today), so counties are shaded by
//! customers out: ODIN gives no customers served for most.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::outages::Area;

const BASE: &str = "https://odin.ornl.gov";
/// The credit shown with ODIN's outages.
pub const CREDIT: &str = "Outage Data Initiative Nationwide (ODIN), ORNL / U.S. DOE";
/// A utility that hasn't reported for this long (seconds) is left out.
pub const STALE: f64 = 3.0 * 3600.0;
/// At most this many pages of 1,000 outages.
const MAX_PAGES: usize = 20;
/// How many counties' outlines to ask for at once.
const OUTLINE_BATCH: usize = 40;

/// One utility's outage report in one county.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// The county's FIPS code: "36105".
    pub fips: String,
    /// The utility's name.
    pub utility: String,
    /// Its EIA id.
    pub eia: String,
    /// Customers (meters) out.
    pub out: u64,
    /// The estimated restoration time (Unix seconds).
    pub restore: Option<f64>,
    /// The cause given.
    pub cause: Option<String>,
    /// The utility's disclaimer.
    pub disclaimer: Option<String>,
}

/// A utility's last report: its name and when (Unix seconds).
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// The utility's name.
    pub name: String,
    /// When it last reported.
    pub received: f64,
}

/// A county's outline, with its names.
#[derive(Clone, Debug, PartialEq)]
pub struct Outline {
    /// "Sullivan".
    pub county: String,
    /// "New York".
    pub state: String,
    /// GeoJSON Polygon or MultiPolygon, lon/lat.
    pub geometry: Value,
}

/// A FIPS county code: five digits.
fn fips(s: &str) -> bool {
    s.len() == 5 && s.bytes().all(|b| b.is_ascii_digit())
}

/// Short text from a JSON value: trimmed, at most 300 characters, None when empty.
fn short(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.trim();
    (!s.is_empty()).then(|| s.chars().take(300).collect())
}

/// The reports in an `/odi?format=JSON` answer.
pub fn parse_reports(text: &str) -> Result<Vec<Report>> {
    let v: Value = serde_json::from_str(text).context("ODIN")?;
    let list = v.get("outage").and_then(Value::as_array).context("ODIN: no outages in the answer")?;
    Ok(list
        .iter()
        .filter_map(|o| {
            let county = o.get("communityDescriptor")?.as_str()?;
            if !fips(county) {
                return None;
            }
            let names = o.get("names").and_then(Value::as_array);
            let name_of =
                |kind: &str| names.into_iter().flatten().find(|n| n.get("nameType").and_then(Value::as_str) == Some(kind)).and_then(|n| short(n.get("name")));
            Some(Report {
                fips: county.to_string(),
                utility: name_of("UtilityName").unwrap_or_else(|| "a utility".into()),
                eia: name_of("UtilityId").unwrap_or_default(),
                out: o.get("metersAffected").and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0)?.round() as u64,
                restore: o.pointer("/estimatedRestorationTime/ert").and_then(Value::as_str).and_then(crate::nws::unix_time),
                cause: short(o.get("cause")),
                disclaimer: short(o.get("utilityDisclaimer")),
            })
        })
        .collect())
}

/// The utilities in an `/odi/status` answer, by EIA id.
pub fn parse_status(text: &str) -> Result<HashMap<String, Status>> {
    let v: Value = serde_json::from_str(text).context("ODIN status")?;
    let list = v.as_array().context("ODIN status: not a list")?;
    Ok(list
        .iter()
        .filter_map(|u| {
            let eia = short(u.get("eiaId"))?;
            let received = crate::nws::unix_time(u.get("receivedDate")?.as_str()?)?;
            Some((eia, Status { name: short(u.get("name")).unwrap_or_default(), received }))
        })
        .collect())
}

/// The county outlines in an `/odi/map?scale=COUNTY` answer, simplified, by FIPS code.
pub fn parse_outlines(text: &str) -> Result<HashMap<String, Outline>> {
    let v: Value = serde_json::from_str(text).context("ODIN map")?;
    let list = v.as_array().context("ODIN map: not a list")?;
    Ok(list
        .iter()
        .filter_map(|c| {
            let id = c.get("id")?.as_str()?;
            let f = c.get("feature")?;
            let kind = f.get("type")?.as_str()?;
            if !fips(id) || !matches!(kind, "Polygon" | "MultiPolygon") {
                return None;
            }
            let geometry = crate::nws::simplify(&json!({"type": kind, "coordinates": f.get("coordinates")?}), 0.002);
            if geometry.is_null() {
                return None;
            }
            Some((
                id.to_string(),
                Outline { county: short(c.get("countyName")).unwrap_or_default(), state: short(c.get("stateName")).unwrap_or_default(), geometry },
            ))
        })
        .collect())
}

/// The state postal code for a state FIPS code.
fn postal(state: &str) -> &'static str {
    const STATES: [(&str, &str); 52] = [
        ("01", "AL"),
        ("02", "AK"),
        ("04", "AZ"),
        ("05", "AR"),
        ("06", "CA"),
        ("08", "CO"),
        ("09", "CT"),
        ("10", "DE"),
        ("11", "DC"),
        ("12", "FL"),
        ("13", "GA"),
        ("15", "HI"),
        ("16", "ID"),
        ("17", "IL"),
        ("18", "IN"),
        ("19", "IA"),
        ("20", "KS"),
        ("21", "KY"),
        ("22", "LA"),
        ("23", "ME"),
        ("24", "MD"),
        ("25", "MA"),
        ("26", "MI"),
        ("27", "MN"),
        ("28", "MS"),
        ("29", "MO"),
        ("30", "MT"),
        ("31", "NE"),
        ("32", "NV"),
        ("33", "NH"),
        ("34", "NJ"),
        ("35", "NM"),
        ("36", "NY"),
        ("37", "NC"),
        ("38", "ND"),
        ("39", "OH"),
        ("40", "OK"),
        ("41", "OR"),
        ("42", "PA"),
        ("44", "RI"),
        ("45", "SC"),
        ("46", "SD"),
        ("47", "TN"),
        ("48", "TX"),
        ("49", "UT"),
        ("50", "VT"),
        ("51", "VA"),
        ("53", "WA"),
        ("54", "WV"),
        ("55", "WI"),
        ("56", "WY"),
        ("72", "PR"),
    ];
    STATES.iter().find(|(f, _)| *f == state).map_or("US", |(_, p)| p)
}

/// The counties with customers out, from `reports`, as areas (key `odin:<FIPS>`), leaving out
/// the states in `skip` (FIPS codes: they have their own feeds) and utilities that haven't
/// reported since `now - STALE`; with how many such utilities were left out. Names come
/// from `outlines` where they're in.
pub fn counties(reports: &[Report], status: &HashMap<String, Status>, outlines: &HashMap<String, Outline>, now: f64, skip: &[&str]) -> (Vec<Area>, usize) {
    let fresh = |eia: &str| status.get(eia).is_none_or(|s| s.received >= now - STALE);
    let mut stale: Vec<&str> = reports.iter().filter(|r| r.out > 0 && !fresh(&r.eia)).map(|r| r.eia.as_str()).collect();
    stale.sort_unstable();
    stale.dedup();
    let mut by_county: BTreeMap<&str, Vec<&Report>> = BTreeMap::new();
    for r in reports.iter().filter(|r| r.out > 0 && fresh(&r.eia) && !skip.contains(&&r.fips[..2])) {
        by_county.entry(r.fips.as_str()).or_default().push(r);
    }
    let areas = by_county
        .into_iter()
        .map(|(county, rs)| {
            let mut utilities: BTreeMap<&str, u64> = BTreeMap::new();
            for r in &rs {
                *utilities.entry(r.utility.as_str()).or_default() += r.out;
            }
            let mut utilities: Vec<(String, u64)> = utilities.into_iter().map(|(u, n)| (u.to_string(), n)).collect();
            utilities.sort_by_key(|u| std::cmp::Reverse(u.1));
            let mut causes: Vec<String> = rs.iter().filter_map(|r| r.cause.clone()).collect();
            causes.sort();
            causes.dedup();
            let o = outlines.get(county);
            Area {
                key: format!("odin:{county}"),
                state: postal(&county[..2]),
                kind: "county",
                name: o.map_or_else(|| format!("FIPS {county}"), |o| o.county.clone()),
                out: rs.iter().map(|r| r.out).sum(),
                updated: rs.iter().filter_map(|r| status.get(&r.eia)).map(|s| s.received).reduce(f64::max),
                source: "ODIN",
                group: "ODIN counties",
                utilities,
                restore: rs.iter().filter_map(|r| r.restore).reduce(f64::max),
                causes,
                disclaimer: rs.iter().find_map(|r| r.disclaimer.clone()),
                ..Area::default()
            }
        })
        .collect();
    (areas, stale.len())
}

/// The current reports, unless unchanged since `etag` (then None); with the new ETag.
pub async fn reports(etag: Option<&str>) -> Result<Option<(Vec<Report>, Option<String>)>> {
    let mut all = Vec::new();
    let mut tag = None;
    let mut cursor: Option<String> = None;
    for page in 0..MAX_PAGES {
        let mut url = format!("{BASE}/odi?format=JSON&pageSize=1000");
        if let Some(c) = &cursor {
            url.push_str("&cursor=");
            url.push_str(&c.replace(['&', '#', ' '], ""));
        }
        let mut req = crate::source::http_client().get(url);
        if page == 0
            && let Some(e) = etag
        {
            req = req.header("If-None-Match", e);
        }
        let resp = req.send().await.context("ODIN")?;
        if page == 0 && resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("ODIN: HTTP {}", resp.status());
        }
        let header = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
        if page == 0 {
            tag = header("etag");
        }
        cursor = header("x-next-cursor").filter(|c| !c.is_empty() && c.len() < 512);
        all.extend(parse_reports(&resp.text().await.context("ODIN")?)?);
        if cursor.is_none() {
            break;
        }
    }
    Ok(Some((all, tag)))
}

/// The reporting utilities and when each last reported.
pub async fn status() -> Result<HashMap<String, Status>> {
    let resp = crate::source::http_client().get(format!("{BASE}/odi/status")).send().await.context("ODIN status")?;
    if !resp.status().is_success() {
        bail!("ODIN status: HTTP {}", resp.status());
    }
    parse_status(&resp.text().await.context("ODIN status")?)
}

fn outline_file() -> std::path::PathBuf {
    crate::source::cache_root().join("outages").join("odin-counties.json")
}

/// The county outlines kept on disk.
pub fn cached_outlines() -> HashMap<String, Outline> {
    let Ok(text) = std::fs::read_to_string(outline_file()) else { return HashMap::new() };
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) else { return HashMap::new() };
    m.into_iter()
        .filter(|(k, _)| fips(k))
        .filter_map(|(k, v)| {
            let geometry = v.get("geometry")?.clone();
            Some((k, Outline { county: short(v.get("county")).unwrap_or_default(), state: short(v.get("state")).unwrap_or_default(), geometry }))
        })
        .collect()
}

/// Outlines for these counties (FIPS codes) from ODIN's map, a batch at a time; added to the
/// disk cache with those in `known`. Counties ODIN has no outline for (no outage there any
/// more) are left out.
pub async fn fetch_outlines(counties: &[String], known: &HashMap<String, Outline>) -> Result<HashMap<String, Outline>> {
    let mut got = HashMap::new();
    for batch in counties.iter().filter(|c| fips(c)).collect::<Vec<_>>().chunks(OUTLINE_BATCH) {
        let mut url = format!("{BASE}/odi/map?scale=COUNTY");
        for c in batch {
            url.push_str("&countyCd=");
            url.push_str(c);
        }
        let resp = crate::source::http_client().get(url).send().await.context("ODIN map")?;
        if !resp.status().is_success() {
            bail!("ODIN map: HTTP {}", resp.status());
        }
        got.extend(parse_outlines(&resp.text().await.context("ODIN map")?)?);
    }
    if !got.is_empty() {
        let all: serde_json::Map<String, Value> =
            known.iter().chain(&got).map(|(k, o)| (k.clone(), json!({"county": o.county, "state": o.state, "geometry": o.geometry}))).collect();
        let p = outline_file();
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        let tmp = p.with_extension("part");
        if std::fs::write(&tmp, serde_json::to_vec(&Value::Object(all))?).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ODI: &str = r#"{"outage":[
      {"communityDescriptor":"36105","metersAffected":1757,"cause":"Weather","utilityDisclaimer":"Figures are estimates.",
       "estimatedRestorationTime":{"ert":"2026-11-27T03:00:00Z"},
       "names":[{"name":"AVANGRID","nameType":"UtilityName"},{"name":"10000","nameType":"UtilityId"}]},
      {"communityDescriptor":"36105","metersAffected":2,
       "names":[{"name":"CENTRAL HUDSON GAS & ELEC CORP","nameType":"UtilityName"},{"name":"554","nameType":"UtilityId"}]},
      {"communityDescriptor":"42003","metersAffected":80,
       "names":[{"name":"WEST PENN POWER COMPANY","nameType":"UtilityName"},{"name":"20387","nameType":"UtilityId"}]},
      {"communityDescriptor":"54061","metersAffected":48,
       "names":[{"name":"MONONGAHELA POWER CO","nameType":"UtilityName"},{"name":"12796","nameType":"UtilityId"}]},
      {"communityDescriptor":"../x","metersAffected":3,"names":[]}
    ]}"#;
    const STATUS: &str = r#"[
      {"eiaId":"10000","name":"AVANGRID","receivedDate":"2026-11-26T22:30:00.000Z","totalOutages":1757},
      {"eiaId":"554","name":"CENTRAL HUDSON","receivedDate":"2026-11-26T22:31:00.000Z","totalOutages":2},
      {"eiaId":"12796","name":"MONONGAHELA POWER CO","receivedDate":"2026-11-23T21:27:56.499Z","totalOutages":48}
    ]"#;
    const MAP: &str = r#"[
      {"id":"36105","countyName":"Sullivan","stateName":"New York",
       "feature":{"type":"Polygon","coordinates":[[[-75.1,41.4],[-74.4,41.4],[-74.4,42.0],[-75.1,41.4]]]}},
      {"id":"bad","feature":{"type":"Polygon","coordinates":[]}}
    ]"#;

    #[test]
    fn reads_reports_status_and_outlines() {
        let r = parse_reports(ODI).unwrap();
        assert_eq!(r.len(), 4, "a bad county code is left out");
        assert_eq!((r[0].fips.as_str(), r[0].utility.as_str(), r[0].eia.as_str(), r[0].out), ("36105", "AVANGRID", "10000", 1757));
        assert_eq!(r[0].restore, crate::nws::unix_time("2026-11-27T03:00:00Z"));
        assert_eq!((r[0].cause.as_deref(), r[0].disclaimer.as_deref()), (Some("Weather"), Some("Figures are estimates.")));
        let s = parse_status(STATUS).unwrap();
        assert_eq!(s["10000"].name, "AVANGRID");
        let o = parse_outlines(MAP).unwrap();
        assert_eq!(o.len(), 1);
        assert_eq!((o["36105"].county.as_str(), o["36105"].state.as_str()), ("Sullivan", "New York"));
    }

    #[test]
    fn counties_leave_out_stale_utilities_and_states_with_feeds() {
        let (r, s, o) = (parse_reports(ODI).unwrap(), parse_status(STATUS).unwrap(), parse_outlines(MAP).unwrap());
        let now = crate::nws::unix_time("2026-11-26T22:40:00Z").unwrap();
        let (areas, stale) = counties(&r, &s, &o, now, &["42", "24"]);
        // Pennsylvania (42) has its own feed; Monongahela Power last reported three days ago.
        assert_eq!(areas.len(), 1, "{areas:?}");
        assert_eq!(stale, 1);
        let a = &areas[0];
        assert_eq!(
            (a.key.as_str(), a.title(), a.state, a.out, a.source, a.group),
            ("odin:36105", "Sullivan County".to_string(), "NY", 1759, "ODIN", "ODIN counties")
        );
        assert_eq!(a.utilities, vec![("AVANGRID".to_string(), 1757), ("CENTRAL HUDSON GAS & ELEC CORP".to_string(), 2)]);
        assert_eq!(a.causes, vec!["Weather".to_string()]);
        assert!(a.restore.is_some() && a.disclaimer.is_some() && a.served.is_none());
        // Without an outline yet, the FIPS code names it.
        let (areas, _) = counties(&r, &s, &HashMap::new(), now, &["42", "24"]);
        assert_eq!(areas[0].name, "FIPS 36105");
        // An ODIN county joins the store and the legend by count.
        let mut store = crate::outages::Outages::default();
        let (areas, stale) = counties(&r, &s, &o, now, &["42", "24"]);
        store.set_odin(areas, stale);
        assert_eq!(store.odin_missing(), vec!["odin:36105".to_string()]);
        store.add_odin_outlines(o.iter().map(|(k, v)| (format!("odin:{k}"), v.geometry.clone())).collect());
        assert!(store.odin_missing().is_empty());
        assert_eq!(store.totals(), vec![("ODIN", 1759)]);
        let fc = store.features(crate::outages::Level::Local, None);
        assert_eq!(fc["features"].as_array().unwrap().len(), 1, "ODIN counties show at the finer level too");
        let l = store.legends(crate::outages::Level::County);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].entries.len(), 6, "five bands and the utilities not reporting");
        assert_eq!(postal("36"), "NY");
    }
}
