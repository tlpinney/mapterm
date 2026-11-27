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
        if Some(p.name) == start || usable(p, &has_key) {
            v.push(imagery_entry(p));
        }
    }
    v
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
}

/// The `b` switcher: what's typed, the rows that match it and the chosen one, and the base map
/// shown when it opened (Esc goes back to it).
#[derive(Clone, Debug)]
pub struct Switcher {
    /// What's typed.
    pub query: String,
    /// The rows matching it.
    pub rows: Vec<Row>,
    /// The chosen row (an index into `rows`).
    pub sel: usize,
    /// The base map shown when it opened (an index into the list).
    pub opened_on: usize,
}

impl Switcher {
    /// Open on the list with the current base map chosen.
    pub fn open(list: &[Entry], current: usize) -> Self {
        let mut s = Switcher { query: String::new(), rows: Vec::new(), sel: 0, opened_on: current };
        s.refilter(list);
        s.sel = s.rows.iter().position(|r| *r == Row::Base(current)).unwrap_or(0);
        s
    }

    /// The rows matching the query: in list order while it's empty, best first once typed.
    pub fn refilter(&mut self, list: &[Entry]) {
        let keep = self.row().filter(|_| self.query.is_empty());
        let mut scored: Vec<(i32, usize, Row)> = list
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let text = format!("{} {} {}", e.label, e.detail, if e.base.is_imagery() { "imagery satellite" } else { "map style" });
                crate::fuzzy::score(&self.query, &text).map(|sc| (sc, i, Row::Base(i)))
            })
            .collect();
        let n = list.len();
        for (i, (row, text)) in [(Row::Labels, "labels names places roads"), (Row::Vector, "roads vector overlay over imagery")].into_iter().enumerate() {
            if let Some(sc) = crate::fuzzy::score(&self.query, text) {
                scored.push((sc, n + i, row));
            }
        }
        if !self.query.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        self.rows = scored.into_iter().map(|(_, _, r)| r).collect();
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
        let mut s = Switcher::open(&l, 1);
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
    }

    #[test]
    fn steps_wrap() {
        assert_eq!(step(3, 2, true), 0);
        assert_eq!(step(3, 0, false), 2);
        assert_eq!(step(0, 0, true), 0);
    }
}
