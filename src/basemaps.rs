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

/// Which picker it is: `b` picks the one base map; `L` turns on and off what's drawn over it;
/// `--resume` opens a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picker {
    /// The base map: one style or one imagery.
    Base,
    /// What's over the base map: live layers, imagery overlays, data layers, the map's names.
    Layers,
    /// Saved sessions and recent runs.
    Sessions,
}

impl Picker {
    /// Its title.
    pub fn title(self) -> &'static str {
        match self {
            Picker::Base => "base map",
            Picker::Layers => "layers",
            Picker::Sessions => "sessions",
        }
    }
}

/// One row of a picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// A base map (its index in the list).
    Base(usize),
    /// Place and road names on or off (`n`).
    Labels,
    /// The vector map over imagery: all of it, names only, or none (`v`).
    Vector,
    /// The legends on or off.
    Legends,
    /// An imagery overlay on or off (its index in [`OVERLAYS`]).
    Overlay(usize),
    /// A live layer on or off (its index in [`LIVE`]), when it was started.
    Live(usize),
    /// A data layer on or off (`--geojson` and the like; its index in [`Switcher::data`]).
    Data(usize),
    /// A saved or recent session to open (its index in [`Switcher::sessions`]).
    Session(usize),
    /// Save what's shown as a session (named by what's typed, if anything).
    SaveAs,
}

/// The fixed rows' names and the words they're found by.
const LABELS_ROW: (&str, &str) = ("labels", "place and road names");
const VECTOR_ROW: (&str, &str) = ("over imagery", "roads and names, vector overlay");
const LEGENDS_ROW: (&str, &str) = ("legends", "keys to the colors");

/// A picker, fzf-style: which one, what's typed, the rows that match it (best first) with
/// where in each row's name the query matched, the chosen one, and (for the base map) the one
/// shown when it opened (Esc goes back to it).
#[derive(Clone, Debug)]
pub struct Switcher {
    /// Which picker.
    pub kind: Picker,
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
    /// The data layers' names.
    pub data: Vec<String>,
    /// Sessions to open: name (or when, for a recent one) and a description.
    pub sessions: Vec<(String, String)>,
}

impl Switcher {
    fn new(kind: Picker) -> Self {
        Switcher {
            kind,
            query: String::new(),
            rows: Vec::new(),
            marks: Vec::new(),
            total: 0,
            sel: 0,
            opened_on: 0,
            live: Vec::new(),
            data: Vec::new(),
            sessions: Vec::new(),
        }
    }

    /// The base map picker, on the `current` one.
    pub fn base(list: &[Entry], current: usize) -> Self {
        let mut s = Switcher { opened_on: current, ..Switcher::new(Picker::Base) };
        s.refilter(list);
        s.total = s.rows.len();
        s.sel = s.rows.iter().position(|r| *r == Row::Base(current)).unwrap_or(0);
        s
    }

    /// The layers picker: the `live` layers running (indices in [`LIVE`]), the imagery
    /// overlays, the `data` layers, and the map's names, overlay and legends.
    pub fn layers(live: &[usize], data: Vec<String>) -> Self {
        let mut s = Switcher { live: live.to_vec(), data, ..Switcher::new(Picker::Layers) };
        s.refilter(&[]);
        s.total = s.rows.len();
        s
    }

    /// The sessions picker, with saving first; `resume`: the first session chosen.
    pub fn sessions(sessions: Vec<(String, String)>, resume: bool) -> Self {
        let mut s = Switcher { sessions, ..Switcher::new(Picker::Sessions) };
        s.refilter(&[]);
        s.total = s.rows.len() - 1;
        if resume && s.rows.len() > 1 {
            s.sel = 1;
        }
        s
    }

    /// A row's name, and the words after it that the filter also searches.
    pub fn text(&self, row: Row, list: &[Entry]) -> (String, String) {
        match row {
            Row::Session(i) => (self.sessions[i].0.clone(), self.sessions[i].1.clone()),
            Row::Base(i) => {
                let e = &list[i];
                (e.label.clone(), format!("{} {}", e.detail, if e.base.is_imagery() { "imagery satellite" } else { "map style" }))
            }
            Row::Labels => (LABELS_ROW.0.into(), LABELS_ROW.1.into()),
            Row::Vector => (VECTOR_ROW.0.into(), VECTOR_ROW.1.into()),
            Row::Legends => (LEGENDS_ROW.0.into(), LEGENDS_ROW.1.into()),
            Row::Overlay(i) => (OVERLAYS[i].label.into(), format!("{} imagery overlay", OVERLAYS[i].detail)),
            Row::Live(i) => (LIVE[i].label.into(), format!("{} {} live", LIVE[i].detail, LIVE[i].key)),
            Row::Data(i) => (self.data[i].clone(), "data layer".into()),
            Row::SaveAs => match self.query.trim() {
                "" => ("save this view…".into(), String::new()),
                q => (format!("save as \"{q}\""), String::new()),
            },
        }
    }

    /// The rows matching the query: in list order while it's empty, best first once typed.
    pub fn refilter(&mut self, list: &[Entry]) {
        let keep = self.row().filter(|_| self.query.is_empty());
        let all: Vec<Row> = match self.kind {
            Picker::Base => (0..list.len()).map(Row::Base).collect(),
            // Live layers first (what's turned on and off most), then imagery overlays, the
            // data, and the map's own parts.
            Picker::Layers => self
                .live
                .iter()
                .map(|&i| Row::Live(i))
                .chain((0..OVERLAYS.len()).map(Row::Overlay))
                .chain((0..self.data.len()).map(Row::Data))
                .chain([Row::Labels, Row::Vector, Row::Legends])
                .collect(),
            Picker::Sessions => (0..self.sessions.len()).map(Row::Session).collect(),
        };
        let mut scored: Vec<(i32, usize, Row, Vec<usize>)> = all
            .into_iter()
            .enumerate()
            .filter_map(|(n, row)| {
                let (name, more) = self.text(row, list);
                let (sc, at) = crate::fuzzy::matches(&self.query, &format!("{name} {more}"))?;
                let len = name.chars().count();
                Some((sc, n, row, at.into_iter().filter(|&i| i < len).collect()))
            })
            .collect();
        if !self.query.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        (self.rows, self.marks) = scored.into_iter().map(|(_, _, r, m)| (r, m)).unzip();
        // Saving is always there, at the top: chosen when nothing typed matches a session.
        if self.kind == Picker::Sessions {
            self.rows.insert(0, Row::SaveAs);
            self.marks.insert(0, Vec::new());
        }
        let first = usize::from(self.kind == Picker::Sessions && self.rows.len() > 1 && !self.query.trim().is_empty());
        self.sel = keep.and_then(|k| self.rows.iter().position(|r| *r == k)).unwrap_or(first);
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
    fn base_picker_has_only_base_maps() {
        let l = list();
        let mut s = Switcher::base(&l, 1);
        assert_eq!(s.row(), Some(Row::Base(1)));
        assert!(s.rows.iter().all(|r| matches!(r, Row::Base(_))));
        assert_eq!(s.total, l.len());
        s.step(false);
        s.step(false);
        assert_eq!(s.row(), Some(*s.rows.last().unwrap()), "wraps");
        s.query = "sat".into();
        s.refilter(&l);
        assert!(matches!(s.row(), Some(Row::Base(i)) if l[i].base.is_imagery()));
        s.query = "usg".into();
        s.refilter(&l);
        assert_eq!(s.marks[0], vec![0, 1, 2]);
        s.query = "hillsh".into();
        s.refilter(&l);
        assert_eq!(s.row(), None, "overlays aren't base maps");
        s.step(true);
    }

    #[test]
    fn layers_picker() {
        let radar = live_index("radar").unwrap();
        let fires = live_index("fires").unwrap();
        let mut s = Switcher::layers(&[radar, fires], vec!["parcels".into()]);
        assert_eq!(s.rows[0], Row::Live(radar), "live layers first");
        assert!(s.rows.contains(&Row::Data(0)) && s.rows.contains(&Row::Legends) && s.rows.contains(&Row::Overlay(0)));
        assert!(!s.rows.iter().any(|r| matches!(r, Row::Base(_))));
        s.query = "wildf".into();
        s.refilter(&[]);
        assert_eq!(s.row(), Some(Row::Live(fires)));
        s.query = "hillsh".into();
        s.refilter(&[]);
        assert_eq!(s.row(), Some(Row::Overlay(0)));
        s.query = "parc".into();
        s.refilter(&[]);
        assert_eq!(s.row(), Some(Row::Data(0)));
        assert!(!Switcher::layers(&[], Vec::new()).rows.iter().any(|r| matches!(r, Row::Live(_) | Row::Data(_))));
    }

    #[test]
    fn sessions_picker() {
        let list = vec![("frederick".into(), "closures on esri".into()), ("2 h ago".into(), "quakes".into())];
        let mut s = Switcher::sessions(list.clone(), false);
        assert_eq!(s.rows[0], Row::SaveAs);
        assert_eq!(s.row(), Some(Row::SaveAs), "S opens on saving");
        assert_eq!(s.total, 2);
        assert_eq!(Switcher::sessions(list, true).row(), Some(Row::Session(0)), "--resume opens on the latest");
        s.query = "quak".into();
        s.refilter(&[]);
        assert_eq!(s.row(), Some(Row::Session(1)), "a match is chosen");
        s.query = "beach".into();
        s.refilter(&[]);
        assert_eq!(s.rows, vec![Row::SaveAs], "nothing matches: save under it");
        assert_eq!(s.text(Row::SaveAs, &[]).0, "save as \"beach\"");
    }

    #[test]
    fn steps_wrap() {
        assert_eq!(step(3, 2, true), 0);
        assert_eq!(step(3, 0, false), 2);
        assert_eq!(step(0, 0, true), 0);
    }
}
