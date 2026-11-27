//! The base maps the viewer switches between: map styles and imagery, in one list that `m`
//! and `M` step through, `'` flips back along, and the `b` switcher shows.

use crate::raster::Preset;

/// A base map: a map style (drawn from vector tiles) or imagery (raster tiles) under the
/// current style's roads and names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Base {
    /// A style for the `m` cycle: a built-in style name, an OpenFreeMap style, or `--style`.
    Style(String),
    /// A `--raster` provider name, or what `--raster` named at start.
    Imagery(String),
}

impl Base {
    /// The style name or imagery spec.
    pub fn spec(&self) -> &str {
        match self {
            Base::Style(s) | Base::Imagery(s) => s,
        }
    }

    /// Whether it's imagery rather than a style.
    pub fn is_imagery(&self) -> bool {
        matches!(self, Base::Imagery(_))
    }
}

/// One base map in the list, as the switcher shows it.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The base map.
    pub base: Base,
    /// Its name in the list.
    pub label: String,
    /// A few words about it.
    pub detail: String,
    /// Loading it is billed or uses the user's key: the switcher doesn't preview it while you
    /// move through the list, only when picked.
    pub keyed: bool,
}

/// The name and a word about a style of the `m` cycle.
pub fn style_entry(spec: &str) -> Entry {
    let (label, detail) = match spec {
        "built-in" => ("mapterm".to_string(), "terminal style"),
        "mapterm-aviation" => ("aviation".to_string(), "dark, for aircraft and ships"),
        "mapterm-weather" => ("weather".to_string(), "dark, for radar and alerts"),
        "mapterm-roads" => ("roads".to_string(), "dark, for closures and traffic"),
        s if crate::stylejson::OPENFREEMAP_STYLES.contains(&s) => (s.to_string(), "OpenFreeMap"),
        s => (short(s), "--style"),
    };
    Entry { base: Base::Style(spec.to_string()), label, detail: detail.to_string(), keyed: false }
}

/// A provider's line in the list: its name and the start of what it is.
pub fn imagery_entry(p: &Preset) -> Entry {
    // "Esri World Imagery, global satellite (...)" → "Esri World Imagery".
    let detail = p.about.split([',', '(', ';']).next().unwrap_or(p.about).trim().to_string();
    Entry { base: Base::Imagery(p.name.to_string()), label: p.name.to_string(), detail, keyed: !p.key_env.is_empty() }
}

/// What `--raster` named at start, when it isn't one of the providers (a URL, a file, or a
/// stack such as `planetary`).
pub fn custom_imagery_entry(spec: &str) -> Entry {
    let detail = match crate::raster::stack(spec) {
        Some(st) => st.about.split([',', '(', ':']).next().unwrap_or(st.about).trim().to_string(),
        None => "--raster".to_string(),
    };
    Entry { base: Base::Imagery(spec.to_string()), label: short(spec), detail, keyed: false }
}

/// A URL or path cut to its last part, for a label.
fn short(spec: &str) -> String {
    let s = spec.trim_end_matches('/');
    if s.contains("://") || s.contains('/') {
        let last = s.rsplit('/').next().unwrap_or(s);
        if last.contains('{') { s.split('/').nth(2).unwrap_or(s).to_string() } else { last.to_string() }
    } else {
        s.to_string()
    }
}

/// Whether a provider can be shown without asking for anything: it needs no key or its key is
/// set (`has_key`), and every placeholder of its URL has a default.
pub fn usable(p: &Preset, has_key: impl Fn(&Preset) -> bool) -> bool {
    p.params.iter().all(|(_, env, default)| !default.is_empty() || (!env.is_empty() && std::env::var(env).is_ok_and(|v| !v.is_empty())))
        && (p.key_env.is_empty() || has_key(p))
}

/// The whole list: the styles, then the imagery (with `start`, what `--raster` named, among
/// them even if it's not a provider).
pub fn entries(styles: &[String], start: Option<&str>, has_key: impl Fn(&Preset) -> bool) -> Vec<Entry> {
    let mut v: Vec<Entry> = styles.iter().map(|s| style_entry(s)).collect();
    if let Some(s) = start
        && crate::raster::preset(s).is_none()
    {
        v.push(custom_imagery_entry(s));
    }
    for p in crate::raster::PRESETS {
        // Overlays (clear but for their names or shading) aren't base maps, unless asked for.
        let overlay = OVERLAYS.iter().any(|o| o.preset == p.name);
        if Some(p.name) == start || (usable(p, &has_key) && !overlay) {
            v.push(imagery_entry(p));
        }
    }
    v
}

/// Imagery drawn over the base map, any number at once: a provider and how it's drawn.
#[derive(Clone, Copy, Debug)]
pub struct Overlay {
    /// The provider ([`crate::raster::preset`]).
    pub preset: &'static str,
    /// Its name in the switcher.
    pub label: &'static str,
    /// A few words about it.
    pub detail: &'static str,
    /// How opaque it's drawn.
    pub opacity: f32,
    /// Drawn as shading (see [`crate::raster::RasterSource::shade`]).
    pub shade: bool,
}

/// The imagery overlays the switcher offers.
pub const OVERLAYS: &[Overlay] = &[
    Overlay { preset: "esri-hillshade", label: "hillshade", detail: "relief shading (Esri)", opacity: 0.6, shade: true },
    Overlay { preset: "esri-roads", label: "roads", detail: "roads and rail (Esri)", opacity: 1.0, shade: false },
    Overlay { preset: "esri-places", label: "places", detail: "borders and place names (Esri)", opacity: 1.0, shade: false },
    Overlay { preset: "gibs", label: "today from space", detail: "yesterday's MODIS, clouds and smoke (NASA)", opacity: 0.55, shade: false },
];

/// A live layer the switcher turns on and off (when it was started).
#[derive(Clone, Copy, Debug)]
pub struct Live {
    /// Its name in the viewer (`aircraft`, `radar`, ...).
    pub key: &'static str,
    /// Its name in the switcher.
    pub label: &'static str,
    /// A few words about it.
    pub detail: &'static str,
}

/// The live layers, in the switcher's order.
pub const LIVE: &[Live] = &[
    Live { key: "aircraft", label: "aircraft", detail: "ADS-B, live" },
    Live { key: "ships", label: "ships", detail: "AIS, live" },
    Live { key: "satellites", label: "satellites", detail: "orbits from CelesTrak" },
    Live { key: "balloons", label: "weather balloons", detail: "radiosondes, SondeHub" },
    Live { key: "radar", label: "radar", detail: "rain and snow · R" },
    Live { key: "alerts", label: "weather alerts", detail: "NWS warnings and watches" },
    Live { key: "quakes", label: "earthquakes", detail: "USGS" },
    Live { key: "fires", label: "wildfires", detail: "NIFC incidents and perimeters" },
    Live { key: "gauges", label: "river levels", detail: "NWS gauges, flooding" },
    Live { key: "outages", label: "power outages", detail: "by county" },
    Live { key: "tfrs", label: "flight restrictions", detail: "FAA TFRs" },
    Live { key: "closures", label: "road closures", detail: "closures and work zones" },
    Live { key: "traffic", label: "traffic speeds", detail: "speed sensors" },
];

/// The index in [`LIVE`] of the layer called `key`.
pub fn live_index(key: &str) -> Option<usize> {
    LIVE.iter().position(|l| l.key == key)
}

/// One row of the switcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// A base map (its index in the list).
    Base(usize),
    /// Place and road names on or off (`n`).
    Labels,
    /// The vector map over imagery: all of it, names only, or none (`v`).
    Vector,
    /// An imagery overlay on or off (its index in [`OVERLAYS`]).
    Overlay(usize),
    /// A live layer on or off (its index in [`LIVE`]), when it was started.
    Live(usize),
    /// A saved or recent session to open (its index in [`Switcher::sessions`]).
    Session(usize),
}

/// The overlay rows' names and the words they're found by.
const LABELS_ROW: (&str, &str) = ("labels", "place and road names");
const VECTOR_ROW: (&str, &str) = ("over imagery", "roads and names, vector overlay");

impl Row {
    /// Its name in the switcher, and the words after it that the filter also searches
    /// (`sessions`: each session's name and description).
    pub fn text(self, list: &[Entry], sessions: &[(String, String)]) -> (String, String) {
        match self {
            Row::Session(i) => (sessions[i].0.clone(), format!("{} session", sessions[i].1)),
            Row::Base(i) => {
                let e = &list[i];
                (e.label.clone(), format!("{} {}", e.detail, if e.base.is_imagery() { "imagery satellite" } else { "map style" }))
            }
            Row::Labels => (LABELS_ROW.0.into(), LABELS_ROW.1.into()),
            Row::Vector => (VECTOR_ROW.0.into(), VECTOR_ROW.1.into()),
            Row::Overlay(i) => (OVERLAYS[i].label.into(), format!("{} overlay", OVERLAYS[i].detail)),
            Row::Live(i) => (LIVE[i].label.into(), format!("{} {} live layer", LIVE[i].detail, LIVE[i].key)),
        }
    }
}

/// The `b` switcher, fzf-style: what's typed, the rows that match it (best first) with where
/// in each row's name the query matched, the chosen one, and the base map shown when it opened
/// (Esc goes back to it).
#[derive(Clone, Debug)]
pub struct Switcher {
    /// What's typed.
    pub query: String,
    /// The rows matching it.
    pub rows: Vec<Row>,
    /// For each row, the characters of its name the query matched (to highlight).
    pub marks: Vec<Vec<usize>>,
    /// How many rows there are with nothing typed.
    pub total: usize,
    /// The chosen row (an index into `rows`).
    pub sel: usize,
    /// The base map shown when it opened (an index into the list).
    pub opened_on: usize,
    /// The live layers running, which have rows (indices in [`LIVE`]).
    pub live: Vec<usize>,
    /// Sessions to open: name (or when, for a recent one) and a description.
    pub sessions: Vec<(String, String)>,
    /// Sessions come first (opened to resume one).
    pub sessions_first: bool,
}

impl Switcher {
    /// Open on the list with the current base map chosen, and rows for the `live` layers
    /// running (indices in [`LIVE`]).
    pub fn open(list: &[Entry], current: usize, live: &[usize]) -> Self {
        let total = list.len() + OVERLAYS.len() + 2 + live.len();
        let mut s = Switcher {
            query: String::new(),
            rows: Vec::new(),
            marks: Vec::new(),
            total,
            sel: 0,
            opened_on: current,
            live: live.to_vec(),
            sessions: Vec::new(),
            sessions_first: false,
        };
        s.refilter(list);
        s.sel = s.rows.iter().position(|r| *r == Row::Base(current)).unwrap_or(0);
        s
    }

    /// With `sessions` to open as well; `first`: listed first and the first one chosen (to
    /// resume one).
    pub fn with_sessions(mut self, list: &[Entry], sessions: Vec<(String, String)>, first: bool) -> Self {
        self.total += sessions.len();
        self.sessions = sessions;
        self.sessions_first = first;
        self.refilter(list);
        if first && !self.sessions.is_empty() {
            self.sel = 0;
        }
        self
    }

    /// The rows matching the query: in list order while it's empty, best first once typed.
    pub fn refilter(&mut self, list: &[Entry]) {
        let keep = self.row().filter(|_| self.query.is_empty());
        // Live layers first (what's turned on and off most), then base maps, imagery
        // overlays, and the vector map's parts.
        // Sessions last, or first to resume one.
        let sessions: Vec<Row> = (0..self.sessions.len()).map(Row::Session).collect();
        let (before, after) = if self.sessions_first { (sessions, Vec::new()) } else { (Vec::new(), sessions) };
        let all = before
            .into_iter()
            .chain(self.live.iter().map(|&i| Row::Live(i)))
            .chain((0..list.len()).map(Row::Base))
            .chain((0..OVERLAYS.len()).map(Row::Overlay))
            .chain([Row::Labels, Row::Vector])
            .chain(after);
        let mut scored: Vec<(i32, usize, Row, Vec<usize>)> = all
            .enumerate()
            .filter_map(|(n, row)| {
                let (name, more) = row.text(list, &self.sessions);
                let (sc, at) = crate::fuzzy::matches(&self.query, &format!("{name} {more}"))?;
                let len = name.chars().count();
                Some((sc, n, row, at.into_iter().filter(|&i| i < len).collect()))
            })
            .collect();
        if !self.query.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        (self.rows, self.marks) = scored.into_iter().map(|(_, _, r, m)| (r, m)).unzip();
        self.sel = keep.and_then(|k| self.rows.iter().position(|r| *r == k)).unwrap_or(0);
    }

    /// The chosen row.
    pub fn row(&self) -> Option<Row> {
        self.rows.get(self.sel).copied()
    }

    /// Choose the next row (`forward`) or the one before, wrapping around.
    pub fn step(&mut self, forward: bool) {
        let n = self.rows.len();
        if n > 0 {
            self.sel = if forward { (self.sel + 1) % n } else { (self.sel + n - 1) % n };
        }
    }
}

/// The next base map along the list (`m`), or the one before (`M`).
pub fn step(len: usize, at: usize, forward: bool) -> usize {
    if len == 0 {
        0
    } else if forward {
        (at + 1) % len
    } else {
        (at + len - 1) % len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<Entry> {
        let styles: Vec<String> = ["built-in", "mapterm-aviation", "liberty"].iter().map(|s| s.to_string()).collect();
        entries(&styles, None, |_| false)
    }

    #[test]
    fn styles_then_imagery_without_keys() {
        let l = list();
        assert_eq!(l[0].label, "mapterm");
        assert_eq!(l[1].label, "aviation");
        assert_eq!(l[2].detail, "OpenFreeMap");
        assert!(l[3..].iter().all(|e| e.base.is_imagery() && !e.keyed));
        let names: Vec<&str> = l.iter().map(|e| e.base.spec()).collect();
        assert!(names.contains(&"esri") && names.contains(&"usgs"));
        // Keyed and mosaic-needing providers stay out without a key.
        assert!(!names.contains(&"google") && !names.contains(&"planet"));
        // Overlays aren't base maps, and each names a provider.
        assert!(!names.contains(&"esri-hillshade") && !names.contains(&"esri-places"));
        assert!(OVERLAYS.iter().all(|o| crate::raster::preset(o.preset).is_some()));
        assert_eq!(l.iter().find(|e| e.base.spec() == "esri").unwrap().detail, "Esri World Imagery");
    }

    #[test]
    fn start_imagery_is_listed() {
        let styles = vec!["built-in".to_string()];
        let l = entries(&styles, Some("planetary"), |_| false);
        assert!(l.iter().any(|e| e.base == Base::Imagery("planetary".into())));
        let l = entries(&styles, Some("https://tiles.example.com/{z}/{x}/{y}.png"), |_| false);
        assert_eq!(l[1].label, "tiles.example.com");
        // A keyed provider named at start is there although no key is in the environment.
        let l = entries(&styles, Some("mapbox"), |_| false);
        assert!(l.iter().any(|e| e.base.spec() == "mapbox" && e.keyed));
        let l = entries(&styles, None, |p| p.name == "maptiler");
        assert!(l.iter().any(|e| e.base.spec() == "maptiler"));
    }

    #[test]
    fn switcher_filters_and_wraps() {
        let l = list();
        let mut s = Switcher::open(&l, 1, &[]);
        assert_eq!(s.row(), Some(Row::Base(1)));
        assert_eq!(*s.rows.last().unwrap(), Row::Vector);
        s.step(false);
        s.step(false);
        assert_eq!(s.row(), Some(*s.rows.last().unwrap()));
        s.query = "sat".into();
        s.refilter(&l);
        assert!(matches!(s.row(), Some(Row::Base(i)) if l[i].base.is_imagery()));
        s.query = "usgs".into();
        s.refilter(&l);
        assert_eq!(s.row().map(|r| match r {
            Row::Base(i) => l[i].base.spec().to_string(),
            _ => String::new(),
        }), Some("usgs".into()));
        s.query = "zzzz".into();
        s.refilter(&l);
        assert_eq!(s.row(), None);
        s.step(true);
        s.query = "usg".into();
        s.refilter(&l);
        assert_eq!(s.marks[0], vec![0, 1, 2]);
        assert_eq!(s.total, l.len() + OVERLAYS.len() + 2);
        s.query = "hillsh".into();
        s.refilter(&l);
        assert_eq!(s.row(), Some(Row::Overlay(0)));
        assert!(!s.rows.iter().any(|r| matches!(r, Row::Live(_))));
        let radar = live_index("radar").unwrap();
        let fires = live_index("fires").unwrap();
        let mut r = Switcher::open(&l, 0, &[radar, fires]);
        assert_eq!(r.rows[0], Row::Live(radar), "live layers first");
        assert_eq!(r.row(), Some(Row::Base(0)), "opens on the current base map");
        r.query = "wildf".into();
        r.refilter(&l);
        assert_eq!(r.row(), Some(Row::Live(fires)));
        // Sessions: last, or first (and chosen) to resume one; found by name.
        let sessions = vec![("frederick".to_string(), "closures on esri".to_string())];
        let r = Switcher::open(&l, 0, &[]).with_sessions(&l, sessions.clone(), false);
        assert_eq!(*r.rows.last().unwrap(), Row::Session(0));
        let mut r = Switcher::open(&l, 0, &[]).with_sessions(&l, sessions, true);
        assert_eq!(r.row(), Some(Row::Session(0)));
        r.query = "freder".into();
        r.refilter(&l);
        assert_eq!(r.row(), Some(Row::Session(0)));
    }

    #[test]
    fn steps_wrap() {
        assert_eq!(step(3, 2, true), 0);
        assert_eq!(step(3, 0, false), 2);
        assert_eq!(step(0, 0, true), 0);
    }
}
