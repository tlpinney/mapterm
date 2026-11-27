//! Data sources: where the viewer's features come from, how they relate to time, and how
//! they're doing. A source is anything that brings features (a live feed, a data file), as
//! opposed to a base map or an imagery overlay.
//!
//! Each has a fixed [`Desc`] (what it is, where from, its [`TimeKind`], its credit) and a
//! running [`Status`] that the viewer updates as data arrives or fails: when it last updated,
//! how many features came, the last error, and how often it updates (learned from the gaps
//! between updates, so a feed that backs off after errors shows it).

/// How a source's features relate to time, after VisIt's and ParaView's time-varying data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeKind {
    /// The same at any time (most data files).
    Timeless,
    /// Things that happened at a moment (earthquakes, a CSV of events).
    Instants,
    /// Things in effect for a span (alerts, flight restrictions, closures, outages).
    Intervals,
    /// Positions sampled over time, interpolated between samples (aircraft, ships, balloons,
    /// GPX tracks).
    Tracks,
    /// Worked out for any time (satellites from their orbital elements).
    Computed,
    /// Images, one per time (radar scans).
    Frames,
}

impl TimeKind {
    /// A mark for it in lists.
    pub fn mark(self) -> &'static str {
        match self {
            TimeKind::Timeless => "·",
            TimeKind::Instants => "◷",
            TimeKind::Intervals => "▭",
            TimeKind::Tracks => "↝",
            TimeKind::Computed => "∿",
            TimeKind::Frames => "▦",
        }
    }

    /// Its name.
    pub fn label(self) -> &'static str {
        match self {
            TimeKind::Timeless => "timeless",
            TimeKind::Instants => "events",
            TimeKind::Intervals => "in effect for a time",
            TimeKind::Tracks => "tracks",
            TimeKind::Computed => "computed",
            TimeKind::Frames => "frames",
        }
    }
}

/// What a source is, fixed.
#[derive(Clone, Debug)]
pub struct Desc {
    /// Its key (the live layers' keys in [`crate::basemaps::LIVE`]; a data layer's id).
    pub key: String,
    /// Its name.
    pub title: String,
    /// Where it comes from: a service, a URL, a file.
    pub origin: String,
    /// How it relates to time.
    pub time: TimeKind,
    /// It updates by itself (a live feed), as opposed to being read once.
    pub live: bool,
    /// How far behind the present it's drawn, in seconds (tracks are drawn a little in the
    /// past to interpolate between reports rather than guess past the last).
    pub latency: Option<f64>,
    /// Its credit.
    pub credit: String,
}

/// The live layers' descriptions (by [`crate::basemaps::LIVE`] key): name, where from, time.
pub fn live_desc(key: &str) -> Option<Desc> {
    let (title, origin, time, credit) = match key {
        "aircraft" => ("aircraft", "ADS-B feed (adsb.lol by default), for the area in view", TimeKind::Tracks, "ADS-B feed"),
        "ships" => ("ships", "aisstream.io (AIS), streamed", TimeKind::Tracks, "AIS: aisstream.io"),
        "satellites" => ("satellites", "CelesTrak orbital elements; SatNOGS for radios", TimeKind::Computed, "Orbits: CelesTrak"),
        "balloons" => ("weather balloons", "SondeHub radiosondes, streamed", TimeKind::Tracks, "Radiosondes: SondeHub"),
        "mapillary" => ("street-level coverage", "Mapillary coverage tiles (with your token)", TimeKind::Timeless, crate::mapillary::CREDIT),
        "cells" => ("mobile cells", "OpenCelliD country files (downloaded with your key)", TimeKind::Timeless, crate::opencellid::CREDIT),
        "towers" => ("antenna structures", "FCC Antenna Structure Registration, weekly file", TimeKind::Timeless, crate::towers::CREDIT),
        "nasa" => ("NASA balloons", "NASA CSBF flight pages and tracks (csbf.nasa.gov)", TimeKind::Tracks, crate::csbf::CREDIT),
        "radar" => ("radar", "RainViewer scans, the last two hours", TimeKind::Frames, crate::rainviewer::CREDIT),
        "alerts" => ("weather alerts", "NWS api.weather.gov", TimeKind::Intervals, crate::nws::CREDIT),
        "quakes" => ("earthquakes", "USGS earthquake feeds", TimeKind::Instants, crate::quakes::CREDIT),
        "fires" => ("wildfires", "NIFC WFIGS incidents and perimeters", TimeKind::Instants, crate::fires::CREDIT),
        "gauges" => ("river levels", "NOAA NWPS gauges", TimeKind::Timeless, crate::floods::CREDIT),
        "outages" => ("power outages", "utilities' outage maps, DOE ODIN", TimeKind::Intervals, "Outages: utilities, DOE ODIN"),
        "tfrs" => ("flight restrictions", "FAA tfr.faa.gov and standing areas", TimeKind::Intervals, crate::tfrs::CREDIT),
        "closures" => ("road closures", "state and county closure feeds, WZDx", TimeKind::Intervals, crate::closures::CREDIT),
        "traffic" => ("traffic speeds", "MDOT SHA CHART speed sensors", TimeKind::Timeless, crate::traffic::CREDIT),
        _ => return None,
    };
    Some(Desc { key: key.into(), title: title.into(), origin: origin.into(), time, live: true, latency: None, credit: credit.into() })
}

/// How a source is doing, as the viewer sees it.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// When its data last came (Unix seconds).
    pub updated: Option<f64>,
    /// How many features it has.
    pub features: Option<usize>,
    /// Its last error since data last came: when, and what.
    pub error: Option<(f64, String)>,
    /// Errors in a row.
    pub failures: u32,
    /// The gaps between its last few updates (seconds), to tell how often it updates.
    gaps: Vec<f64>,
}

/// Updates closer together than this (seconds) are parts of one.
const BURST: f64 = 10.0;

/// How many gaps between updates are kept.
const GAPS: usize = 5;

/// What a source's state is, in short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing yet.
    Waiting,
    /// Up to date.
    Ok,
    /// Late: much longer since the last update than usual.
    Stale,
    /// Its last attempt failed.
    Failing,
}

impl Status {
    /// Data came at `now`, with `features` features (if known).
    pub fn ok(&mut self, now: f64, features: Option<usize>) {
        // Parts of one update come close together (a source of several feeds): one burst.
        if let Some(t) = self.updated.filter(|t| now - t >= BURST) {
            self.gaps.push(now - t);
            if self.gaps.len() > GAPS {
                self.gaps.remove(0);
            }
        }
        self.updated = Some(now);
        if features.is_some() {
            self.features = features;
        }
        self.error = None;
        self.failures = 0;
    }

    /// A report came on a stream at `now`: up to date, though it has no rhythm to learn.
    pub fn heard(&mut self, now: f64, features: Option<usize>) {
        self.updated = Some(now);
        if features.is_some() {
            self.features = features;
        }
        self.error = None;
        self.failures = 0;
    }

    /// An attempt failed at `now`.
    pub fn fail(&mut self, now: f64, error: impl Into<String>) {
        self.error = Some((now, error.into()));
        self.failures += 1;
    }

    /// How often it updates, in seconds: the median gap between its updates (None before
    /// two updates).
    pub fn every(&self) -> Option<f64> {
        if self.gaps.is_empty() {
            return None;
        }
        let mut g = self.gaps.clone();
        g.sort_by(f64::total_cmp);
        Some(g[g.len() / 2])
    }

    /// Its state at `now`.
    pub fn state(&self, now: f64) -> State {
        if self.error.is_some() {
            return State::Failing;
        }
        match (self.updated, self.every()) {
            (None, _) => State::Waiting,
            // Three updates missed (and over two minutes): late.
            (Some(t), Some(e)) if now - t > (3.0 * e).max(120.0) => State::Stale,
            _ => State::Ok,
        }
    }

    /// In a few words: `updated 2 min ago · every 5 min · 143`, or what failed.
    pub fn describe(&self, now: f64) -> String {
        let ago = |t: f64| crate::sessions::ago(t, now);
        let mut parts = Vec::new();
        match self.state(now) {
            State::Waiting => parts.push(match &self.error {
                Some((_, e)) => format!("failing: {e}"),
                None => "waiting for data".into(),
            }),
            State::Failing => {
                let (t, e) = self.error.as_ref().expect("failing has an error");
                let n = if self.failures > 1 { format!(" ({}×)", self.failures) } else { String::new() };
                parts.push(format!("failed {}{n}: {e}", ago(*t)));
                if let Some(u) = self.updated {
                    parts.push(format!("data from {}", ago(u)));
                }
            }
            State::Ok | State::Stale => {
                let late = if self.state(now) == State::Stale { "late: " } else { "" };
                parts.push(format!("{late}updated {}", ago(self.updated.unwrap_or(now))));
            }
        }
        if let Some(e) = self.every() {
            parts.push(format!("every {}", span(e)));
        }
        parts.join(" · ")
    }
}

/// A span of seconds in words: `30 s`, `5 min`, `2 h`.
pub fn span(s: f64) -> String {
    match s {
        s if s < 90.0 => format!("{:.0} s", s.max(0.0)),
        s if s < 5400.0 => format!("{:.0} min", s / 60.0),
        s => format!("{:.0} h", s / 3600.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_live_layer_is_described() {
        for l in crate::basemaps::LIVE {
            let d = live_desc(l.key).unwrap_or_else(|| panic!("{} has no description", l.key));
            assert!(d.live && !d.credit.is_empty() && !d.origin.is_empty());
        }
        assert!(live_desc("nope").is_none());
    }

    #[test]
    fn status_learns_how_often_and_goes_late() {
        let mut s = Status::default();
        assert_eq!(s.state(0.0), State::Waiting);
        assert_eq!(s.describe(0.0), "waiting for data");
        s.ok(1000.0, Some(10));
        assert_eq!(s.every(), None);
        s.ok(1300.0, None);
        s.ok(1600.0, Some(12));
        assert_eq!(s.every(), Some(300.0));
        assert_eq!(s.features, Some(12));
        assert_eq!(s.state(1700.0), State::Ok);
        assert_eq!(s.describe(1720.0), "updated 2 min ago · every 5 min");
        assert_eq!(s.state(2600.0), State::Stale, "over three updates missed");
        assert!(s.describe(2600.0).starts_with("late: updated"));
        s.fail(2610.0, "HTTP 503");
        s.fail(2700.0, "HTTP 503");
        assert_eq!(s.state(2700.0), State::Failing);
        assert_eq!(s.describe(2700.0), "failed just now (2×): HTTP 503 · data from 18 min ago · every 5 min");
        s.ok(2800.0, None);
        assert_eq!(s.state(2800.0), State::Ok);
        let mut b = Status::default();
        for t in [0.0, 1.0, 2.0, 300.0, 301.0, 600.0] {
            b.ok(t, None);
        }
        assert_eq!(b.every(), Some(299.0), "bursts of feeds count once");
        let mut st = Status::default();
        st.heard(10.0, Some(3));
        st.heard(10.5, None);
        assert_eq!(st.every(), None, "a stream has no rhythm");
        assert_eq!(st.describe(12.0), "updated just now");
        assert_eq!(s.failures, 0);
    }

    #[test]
    fn spans() {
        assert_eq!(span(30.0), "30 s");
        assert_eq!(span(300.0), "5 min");
        assert_eq!(span(7200.0), "2 h");
    }
}
