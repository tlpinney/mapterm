//! River and creek levels from NOAA's National Water Prediction Service (NWPS; public, no
//! key): every gauge in view with its stage and the NWS flood category (no flooding, action,
//! minor, moderate, major), the forecast where there is one, and for a selected gauge its
//! flood stages, record crests and the stage over the last days with the forecast.
//!
//! The gauge list is asked for by area (the whole country is too much for the service), so
//! only from [`MIN_ZOOM`]: for the view and a margin, again when the view leaves it, and every
//! [`REFRESH`] seconds.

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};

const API: &str = "https://api.water.noaa.gov/nwps/v1";
/// The credit shown while gauges are on.
pub const CREDIT: &str = "Water levels: NOAA NWPS, USGS";
/// Gauges are asked for from this zoom.
pub const MIN_ZOOM: f64 = 8.0;
/// Seconds between asking again for the same area.
pub const REFRESH: u64 = 600;

/// A flood category, least first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Out of service, or no category given.
    #[default]
    Unknown,
    /// Below action stage.
    None,
    /// Action stage: near flooding.
    Action,
    /// Minor flooding.
    Minor,
    /// Moderate flooding.
    Moderate,
    /// Major flooding.
    Major,
}

impl Category {
    /// From NWPS's `floodCategory`.
    pub fn parse(s: &str) -> Category {
        match s {
            "no_flooding" => Category::None,
            "action" => Category::Action,
            "minor" => Category::Minor,
            "moderate" => Category::Moderate,
            "major" => Category::Major,
            _ => Category::Unknown,
        }
    }

    /// "no flooding", "action", "minor flooding"...
    pub fn label(self) -> &'static str {
        match self {
            Category::Unknown => "no data",
            Category::None => "no flooding",
            Category::Action => "action stage",
            Category::Minor => "minor flooding",
            Category::Moderate => "moderate flooding",
            Category::Major => "major flooding",
        }
    }

    /// Its color, after the NWS's hydrograph colors.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Category::Unknown => (0x6e, 0x76, 0x81),
            Category::None => (0x3f, 0xb9, 0x50),
            Category::Action => (0xff, 0xe0, 0x3d),
            Category::Minor => (0xff, 0x9f, 0x0a),
            Category::Moderate => (0xff, 0x45, 0x3a),
            Category::Major => (0xc8, 0x4d, 0xe8),
        }
    }
}

/// One gauge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gauge {
    /// NWS location id: "ADEV2".
    pub lid: String,
    /// "Cedar Run near Aden".
    pub name: String,
    /// Longitude.
    pub lon: f64,
    /// Latitude.
    pub lat: f64,
    /// The latest stage, and its units ("ft").
    pub stage: Option<f64>,
    /// Stage units.
    pub units: String,
    /// The latest flow, cubic feet a second.
    pub flow: Option<f64>,
    /// The observed category.
    pub category: Category,
    /// When it was observed (Unix seconds).
    pub time: Option<f64>,
    /// The forecast's category, where there is a current forecast.
    pub forecast: Option<Category>,
    /// The forecast stage shown with it.
    pub forecast_stage: Option<f64>,
}

impl Gauge {
    /// The worse of the observed and forecast categories: what the gauge is drawn as.
    pub fn worst(&self) -> Category {
        self.forecast.map_or(self.category, |f| f.max(self.category))
    }
}

/// A number, or None for NWPS's -999 (missing).
fn val(v: Option<&Json>) -> Option<f64> {
    v?.as_f64().filter(|n| n.is_finite() && *n > -998.0)
}

fn time(v: Option<&Json>) -> Option<f64> {
    v?.as_str().filter(|s| !s.starts_with("0001")).and_then(crate::nws::unix_time)
}

/// The gauges in a `/gauges` answer.
pub fn parse_gauges(text: &str) -> Result<Vec<Gauge>> {
    let v: Json = serde_json::from_str(text).context("NWPS")?;
    let list = v.get("gauges").and_then(Json::as_array).context("NWPS: no gauges in the answer")?;
    Ok(list
        .iter()
        .filter_map(|g| {
            let lid = g.get("lid")?.as_str()?;
            if lid.is_empty() || lid.len() > 10 || !lid.chars().all(|c| c.is_ascii_alphanumeric()) {
                return None;
            }
            let (lon, lat) = (g.get("longitude")?.as_f64()?, g.get("latitude")?.as_f64()?);
            if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
                return None;
            }
            let obs = g.pointer("/status/observed");
            let fc = g.pointer("/status/forecast");
            let fc_cat = fc.and_then(|f| f.get("floodCategory")?.as_str()).map(Category::parse).filter(|c| *c != Category::Unknown);
            Some(Gauge {
                lid: lid.to_string(),
                name: g.get("name").and_then(Json::as_str).unwrap_or(lid).chars().take(120).collect(),
                lon,
                lat,
                stage: val(obs.and_then(|o| o.get("primary"))),
                units: obs.and_then(|o| o.get("primaryUnit")?.as_str()).unwrap_or("").to_string(),
                flow: val(obs.and_then(|o| o.get("secondary"))).map(|kcfs| kcfs * 1000.0),
                category: obs.and_then(|o| o.get("floodCategory")?.as_str()).map_or(Category::Unknown, Category::parse),
                time: time(obs.and_then(|o| o.get("validTime"))),
                forecast: fc_cat,
                forecast_stage: fc_cat.and(val(fc.and_then(|f| f.get("primary")))),
            })
        })
        .collect())
}

/// The gauges in (west, south, east, north).
pub async fn gauges(b: (f64, f64, f64, f64)) -> Result<Vec<Gauge>> {
    let url = format!("{API}/gauges?bbox.xmin={:.4}&bbox.ymin={:.4}&bbox.xmax={:.4}&bbox.ymax={:.4}&srid=EPSG_4326", b.0, b.1, b.2, b.3);
    let resp = crate::source::http_client().get(url).send().await.context("NWPS")?;
    if !resp.status().is_success() {
        bail!("NWPS: HTTP {}", resp.status());
    }
    parse_gauges(&resp.text().await.context("NWPS")?)
}

/// A gauge's details: flood stages, record crests, and the stage over time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    /// Action, minor, moderate and major stages, where defined.
    pub stages: Vec<(Category, f64)>,
    /// The highest crests on record: (stage, when).
    pub crests: Vec<(f64, f64)>,
    /// Observed stages: (when, stage).
    pub observed: Vec<(f64, f64)>,
    /// Forecast stages: (when, stage).
    pub forecast: Vec<(f64, f64)>,
    /// Who the data is from ("USGS"...).
    pub attribution: Vec<String>,
}

/// The flood stages, crests and attribution in a `/gauges/{lid}` answer.
pub fn parse_detail(text: &str) -> Result<Detail> {
    let v: Json = serde_json::from_str(text).context("NWPS gauge")?;
    let cats = v.pointer("/flood/categories");
    let stages = [("action", Category::Action), ("minor", Category::Minor), ("moderate", Category::Moderate), ("major", Category::Major)]
        .into_iter()
        .filter_map(|(k, c)| Some((c, val(cats?.get(k)?.get("stage"))?)))
        .filter(|(_, s)| *s > 0.0)
        .collect();
    let mut crests: Vec<(f64, f64)> = v
        .pointer("/flood/crests/historic")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| Some((val(c.get("stage"))?, time(c.get("occurredTime"))?)))
        .collect();
    crests.sort_by(|a, b| b.0.total_cmp(&a.0));
    crests.truncate(3);
    let attribution = v
        .get("dataAttribution")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| a.get("abbrev").or_else(|| a.get("text")).and_then(Json::as_str).map(|s| s.chars().take(40).collect()))
        .collect();
    Ok(Detail { stages, crests, attribution, ..Detail::default() })
}

/// Stages over time: (Unix seconds, stage).
pub type Series = Vec<(f64, f64)>;

/// The observed and forecast stages in a `/stageflow` answer.
pub fn parse_stageflow(text: &str) -> Result<(Series, Series)> {
    let v: Json = serde_json::from_str(text).context("NWPS stage")?;
    let series = |k: &str| -> Vec<(f64, f64)> {
        let mut s: Vec<(f64, f64)> = v
            .pointer(&format!("/{k}/data"))
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| Some((time(d.get("validTime"))?, val(d.get("primary"))?)))
            .collect();
        s.sort_by(|a, b| a.0.total_cmp(&b.0));
        s
    };
    Ok((series("observed"), series("forecast")))
}

/// A gauge's details and stage series (two requests).
pub async fn detail(lid: &str) -> Result<Detail> {
    if lid.is_empty() || !lid.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("not a gauge id: {lid:?}");
    }
    let get = |url: String| async move {
        let resp = crate::source::http_client().get(url).send().await.context("NWPS")?;
        if !resp.status().is_success() {
            bail!("NWPS: HTTP {}", resp.status());
        }
        resp.text().await.context("NWPS")
    };
    let mut d = parse_detail(&get(format!("{API}/gauges/{lid}")).await?)?;
    (d.observed, d.forecast) = parse_stageflow(&get(format!("{API}/gauges/{lid}/stageflow")).await?)?;
    Ok(d)
}

/// The gauges as GeoJSON points (`id`, `glyph`, `label`, `color`): ◆ in the worse of the
/// observed and forecast category, · out of service; names from action stage up and for the
/// selected one, with ↑ when the forecast is worse. The worst draw last.
pub fn points(gauges: &[Gauge], selected: Option<&str>) -> Json {
    let mut v: Vec<&Gauge> = gauges.iter().collect();
    v.sort_by_key(|g| (Some(g.lid.as_str()) == selected, g.worst()));
    let features: Vec<Json> = v
        .into_iter()
        .map(|g| {
            let worst = g.worst();
            let (r, gr, b) = worst.rgb();
            let sel = Some(g.lid.as_str()) == selected;
            let rising = g.forecast.is_some_and(|f| f > g.category);
            let label = if sel || worst >= Category::Action { format!("{}{}", if rising { "↑ " } else { "" }, short_name(&g.name)) } else { String::new() };
            json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [g.lon, g.lat]},
                "properties": {"id": g.lid, "glyph": if sel { "◎" } else if worst == Category::Unknown { "·" } else { "◆" },
                               "label": label, "color": format!("#{r:02x}{gr:02x}{b:02x}")},
            })
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// "Cedar Run near Aden" stays; long names are cut at " at " / " near ".
fn short_name(name: &str) -> String {
    if name.chars().count() <= 28 {
        return name.to_string();
    }
    name.split(" near ").next().unwrap_or(name).split(" at ").next().unwrap_or(name).to_string()
}

/// The style layer for gauges from source `source`.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source)
        .glyph(json!(["get", "glyph"]))
        .text(json!(["get", "label"]))
        .color("#c9d1d9")
        .paint("icon-color", json!(["get", "color"]))
}

/// The gauge nearest `dot` (braille dots), within `within` cells; the worse first when two
/// are about as near.
pub fn nearest(gauges: &[Gauge], vp: &crate::Viewport, dot: (f64, f64), within: f64) -> Option<String> {
    gauges
        .iter()
        .filter_map(|g| {
            let (x, y) = vp.dot_ll(g.lon, g.lat);
            let d = ((x - dot.0).powi(2) + ((y - dot.1) / 2.0).powi(2)).sqrt();
            (d <= within).then_some((d - g.worst() as u8 as f64 * 0.3, g))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, g)| g.lid.clone())
}

/// A legend of the categories, with how many gauges are in each (by the worse of observed
/// and forecast).
pub fn legend(gauges: &[Gauge]) -> crate::legend::Legend {
    use ratatui::style::Color;
    let mut l = crate::legend::Legend::new("river gauges");
    for c in [Category::Major, Category::Moderate, Category::Minor, Category::Action, Category::None, Category::Unknown] {
        let n = gauges.iter().filter(|g| g.worst() == c).count();
        if n > 0 || c >= Category::Action {
            let (r, g, b) = c.rgb();
            l = l.entry(if c == Category::Unknown { "·" } else { "◆" }, Color::Rgb(r, g, b), c.label(), Some(n));
        }
    }
    l
}

/// The stage over time as one row of block characters, `width` wide: the observed stages
/// then the forecast, each column the highest in its span; with the range shown, and the
/// column where the forecast starts. Empty with no data.
pub fn sparkline(observed: &[(f64, f64)], forecast: &[(f64, f64)], since: f64, width: usize) -> (String, Option<usize>, (f64, f64)) {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let pts: Vec<(f64, f64)> = observed.iter().chain(forecast).filter(|p| p.0 >= since).copied().collect();
    if pts.is_empty() || width == 0 {
        return (String::new(), None, (0.0, 0.0));
    }
    let (t0, t1) = (pts.first().map_or(0.0, |p| p.0).min(pts.iter().map(|p| p.0).fold(f64::MAX, f64::min)), pts.iter().map(|p| p.0).fold(f64::MIN, f64::max));
    let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let span = (t1 - t0).max(1.0);
    let mut cols: Vec<Option<f64>> = vec![None; width];
    for &(t, s) in &pts {
        let i = (((t - t0) / span) * (width - 1) as f64).round() as usize;
        cols[i] = Some(cols[i].map_or(s, |c: f64| c.max(s)));
    }
    let start = forecast.first().filter(|f| f.0 >= t0).map(|f| (((f.0 - t0) / span) * (width - 1) as f64).round() as usize);
    let line = cols.iter().map(|c| c.map_or(' ', |s| BARS[(((s - lo) / (hi - lo).max(1e-9)) * 7.0).round() as usize])).collect();
    (line, start, (lo, hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"{"gauges":[
      {"lid":"ADEV2","name":"Cedar Run near Aden","latitude":38.61508,"longitude":-77.55342,
       "status":{"observed":{"primary":9.04,"primaryUnit":"ft","secondary":-999,"floodCategory":"action","validTime":"2026-11-27T00:05:00Z"},
                 "forecast":{"primary":-999,"floodCategory":"fcst_not_current","validTime":"0001-01-01T00:00:00Z"}}},
      {"lid":"AFRV2","name":"Four Mile Run at Alexandria","latitude":38.8433,"longitude":-77.0858,
       "status":{"observed":{"primary":5.02,"primaryUnit":"ft","secondary":0.189,"floodCategory":"no_flooding","validTime":"2026-11-26T23:50:00Z"},
                 "forecast":{"primary":11.2,"floodCategory":"minor","validTime":"2026-11-27T06:00:00Z"}}},
      {"lid":"ACRV2","name":"Cameron Run at Alexandria","latitude":38.8064,"longitude":-77.11,
       "status":{"observed":{"primary":-999,"floodCategory":"out_of_service"},"forecast":{"floodCategory":"out_of_service"}}},
      {"lid":"bad id!","name":"x","latitude":0,"longitude":0,"status":{}}
    ]}"#;

    #[test]
    fn reads_gauges_and_categories() {
        let g = parse_gauges(LIST).unwrap();
        assert_eq!(g.len(), 3, "a bad id is left out");
        assert_eq!((g[0].lid.as_str(), g[0].stage, g[0].category, g[0].forecast, g[0].flow), ("ADEV2", Some(9.04), Category::Action, None, None));
        assert_eq!((g[1].flow, g[1].forecast, g[1].forecast_stage, g[1].worst()), (Some(189.0), Some(Category::Minor), Some(11.2), Category::Minor));
        assert_eq!((g[2].stage, g[2].category, g[2].time), (None, Category::Unknown, None));
        assert!(Category::Major > Category::Minor && Category::None > Category::Unknown);
        let fc = points(&g, None);
        let f = fc["features"].as_array().unwrap();
        assert_eq!(f[0]["properties"]["glyph"], "·", "out of service first, a dot");
        let last = f.last().unwrap();
        assert_eq!(
            (last["properties"]["id"].as_str(), last["properties"]["label"].as_str()),
            (Some("AFRV2"), Some("↑ Four Mile Run at Alexandria")),
            "forecast to flood: on top, rising"
        );
        assert_eq!(f[1]["properties"]["label"], "Cedar Run near Aden");
        let l = legend(&g);
        assert!(l.entries.len() >= 5);
        let cam = crate::Camera::new(-77.5, 38.6, 10.0);
        let vp = crate::Viewport::new(&cam, ratatui::layout::Rect::new(0, 0, 80, 24));
        assert_eq!(nearest(&g, &vp, vp.dot_ll(-77.5534, 38.615), 4.0).as_deref(), Some("ADEV2"));
    }

    #[test]
    fn reads_detail_and_draws_the_stage() {
        let d = parse_detail(
            r#"{"flood":{"categories":{"major":{"stage":20},"moderate":{"stage":15},"minor":{"stage":10},"action":{"stage":9}},
                "crests":{"historic":[{"occurredTime":"2024-01-10T12:05:00Z","stage":13.51},{"occurredTime":"2018-08-01T14:53:00Z","stage":12.95},
                                       {"occurredTime":"2025-07-17T17:05:00Z","stage":12.11},{"occurredTime":"2010-01-01T00:00:00Z","stage":5.0}]}},
                "dataAttribution":[{"abbrev":"USGS","text":"U.S. Geological Survey"}]}"#,
        )
        .unwrap();
        assert_eq!(d.stages, vec![(Category::Action, 9.0), (Category::Minor, 10.0), (Category::Moderate, 15.0), (Category::Major, 20.0)]);
        assert_eq!(d.crests.len(), 3);
        assert_eq!(d.crests[0].0, 13.51);
        assert_eq!(d.attribution, vec!["USGS".to_string()]);
        let (obs, fc) = parse_stageflow(
            r#"{"observed":{"data":[{"validTime":"2026-11-26T00:00:00Z","primary":1.0},{"validTime":"2026-11-26T12:00:00Z","primary":5.0},
                                    {"validTime":"2026-11-27T00:00:00Z","primary":9.0}]},
                "forecast":{"data":[{"validTime":"2026-11-27T06:00:00Z","primary":11.0},{"validTime":"2026-11-27T03:00:00Z","primary":-999}]}}"#,
        )
        .unwrap();
        assert_eq!((obs.len(), fc.len()), (3, 1), "missing values left out");
        let (line, start, range) = sparkline(&obs, &fc, 0.0, 10);
        assert_eq!(line.chars().count(), 10);
        assert_eq!(line.chars().next(), Some('▁'));
        assert_eq!(line.chars().last(), Some('█'));
        assert_eq!(start, Some(9));
        assert_eq!(range, (1.0, 11.0));
        assert_eq!(sparkline(&[], &[], 0.0, 10).0, "");
    }
}
