//! Named sessions: what the viewer shows, saved under a name (`S`) to open again later
//! (`--session NAME`, or the `b` switcher). A session keeps the options mapterm was started
//! with, which start its live layers again, and what changed since: the view, the base map,
//! the imagery overlays, the layers turned off, the vector overlay.
//!
//! Every run is also kept when it ends, in a history of the last [`HISTORY_KEEP`]
//! (`--continue` opens the latest, `--resume` lists them with the named ones).
//!
//! Files are JSON in `$XDG_CONFIG_HOME/mapterm/sessions` (else `~/.config/mapterm/sessions`),
//! one per name (`NAME.json`), and the history in its `history` folder.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Options that aren't kept: the view (kept on its own), the session itself, and request
/// headers, which can hold credentials (key commands are kept: they name a command, not a key).
const NOT_KEPT: &[(&str, bool)] = &[("--at", true), ("-z", true), ("--zoom", true), ("--session", true), ("--raster-header", true)];

/// A saved session.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// Its name (the file's).
    pub name: String,
    /// The command-line options to start it with (see [`keep_args`]).
    pub args: Vec<String>,
    /// The view: longitude, latitude, zoom.
    pub lon: f64,
    /// Latitude of the view's center.
    pub lat: f64,
    /// Zoom of the view.
    pub zoom: f64,
    /// The base map: a style name or imagery spec (as the `b` switcher lists them).
    pub base: Option<String>,
    /// Imagery overlays on, by provider name.
    pub overlays: Vec<String>,
    /// Live layers turned off, by key.
    pub live_off: Vec<String>,
    /// The vector map over imagery: 0 all, 1 names only, 2 none.
    pub vector: u8,
    /// When it was saved (Unix seconds).
    pub saved_at: f64,
    /// When it was first saved under its name (Unix seconds; for a run, when it started).
    pub created_at: f64,
    /// When it was last opened (Unix seconds; 0: never).
    pub viewed_at: f64,
}

/// How many runs the history keeps.
pub const HISTORY_KEEP: usize = 50;

/// Where the history of runs is kept.
pub fn history_dir() -> PathBuf {
    dir().join("history")
}

/// The options worth keeping from a command line (without the program name): all but the
/// view, `--session` and request headers.
pub fn keep_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match NOT_KEPT.iter().find(|(n, _)| n == a) {
            Some((_, takes_value)) => {
                if *takes_value {
                    it.next();
                }
            }
            None => out.push(a.clone()),
        }
    }
    out
}

/// Whether `name` can name a session (a file name: letters, digits, `-`, `_`, `.` and spaces,
/// not starting with a dot).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
}

/// Where sessions are kept.
pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        home.join(".config")
    });
    base.join("mapterm").join("sessions")
}

impl Session {
    /// As JSON.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "args": self.args,
            "view": { "lon": self.lon, "lat": self.lat, "zoom": self.zoom },
            "base": self.base,
            "overlays": self.overlays,
            "live_off": self.live_off,
            "vector": self.vector,
            "saved_at": self.saved_at,
            "created_at": self.created_at,
            "viewed_at": self.viewed_at,
        })
    }

    /// From JSON (what [`Session::to_json`] wrote; missing parts take defaults).
    pub fn from_json(name: &str, v: &Value) -> Result<Session> {
        let strings = |k: &str| -> Vec<String> { v[k].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default() };
        let view = &v["view"];
        let num = |k: &str| view[k].as_f64().with_context(|| format!("session {name}: no view {k}"));
        Ok(Session {
            // Its own name (a run in the history is filed by time but keeps the session's).
            name: v["name"].as_str().filter(|n| valid_name(n)).unwrap_or(name).to_string(),
            args: strings("args"),
            lon: num("lon")?,
            lat: num("lat")?,
            zoom: num("zoom")?,
            base: v["base"].as_str().map(str::to_string),
            overlays: strings("overlays"),
            live_off: strings("live_off"),
            vector: v["vector"].as_u64().unwrap_or(0).min(2) as u8,
            saved_at: v["saved_at"].as_f64().unwrap_or(0.0),
            created_at: v["created_at"].as_f64().or(v["saved_at"].as_f64()).unwrap_or(0.0),
            viewed_at: v["viewed_at"].as_f64().unwrap_or(0.0),
        })
    }

    /// The command line it starts with: its options, then the view.
    pub fn command_line(&self) -> Vec<String> {
        let mut v = self.args.clone();
        v.extend(["--at".into(), format!("{:.6},{:.6}", self.lat, self.lon), "-z".into(), format!("{:.2}", self.zoom)]);
        v
    }

    /// What it shows, in a few words: the layers its options start, and its base map.
    pub fn summary(&self) -> String {
        let mut words: Vec<String> = Vec::new();
        for a in &self.args {
            if let Some(flag) = a.strip_prefix("--")
                && !flag.contains("cmd")
                && !matches!(flag, "light" | "no-labels" | "utc" | "raster-cache")
                && !self.live_off.iter().any(|k| flag.starts_with(k.as_str()))
            {
                words.push(flag.to_string());
            }
        }
        if let Some(b) = &self.base {
            // Styles by the name the base map picker shows (mapterm-weather is "weather").
            let name = if crate::raster::preset(b).is_some() { b.clone() } else { crate::basemaps::style_entry(b).label };
            words.push(format!("on {name}"));
        }
        if words.is_empty() { "map".to_string() } else { words.join(" ") }
    }

    /// Keep it in the history (named by the time), dropping all but the latest
    /// [`HISTORY_KEEP`]. Returns the file.
    pub fn save_history(&self) -> Result<PathBuf> {
        let dir = history_dir();
        let path = self.write(&dir, &format!("{:.0}", self.saved_at * 1000.0))?;
        for old in list_in(&dir).into_iter().skip(HISTORY_KEEP) {
            let _ = std::fs::remove_file(dir.join(format!("{old}.json")));
        }
        Ok(path)
    }

    /// Save it in [`dir`], replacing one of the same name (keeping when that was created).
    /// Returns the file.
    pub fn save(&self) -> Result<PathBuf> {
        let mut s = self.clone();
        if let Ok(old) = load_in(&dir(), &self.name) {
            s.created_at = old.created_at;
            s.viewed_at = old.viewed_at;
        }
        s.save_in(&dir())
    }

    /// Save it in `dir`.
    pub fn save_in(&self, dir: &std::path::Path) -> Result<PathBuf> {
        self.write(dir, &self.name)
    }

    /// Write it to `dir` as `file`.json.
    fn write(&self, dir: &std::path::Path, file: &str) -> Result<PathBuf> {
        if !valid_name(&self.name) || !valid_name(file) {
            bail!("{:?} can't name a session: letters, digits, spaces, - _ and . only", self.name);
        }
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!("{file}.json"));
        // Written aside and moved into place, so a crash never leaves half a file.
        let tmp = dir.join(format!(".{file}.json.tmp"));
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.to_json())? + "\n")?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }
}

/// The session called `name`.
pub fn load(name: &str) -> Result<Session> {
    load_in(&dir(), name)
}

/// The session called `name`, noted as opened now (`now`, Unix seconds).
pub fn open(name: &str, now: f64) -> Result<Session> {
    let mut s = load(name)?;
    s.viewed_at = now;
    // Noting it is a nicety: a session that can't be written still opens.
    let _ = s.save_in(&dir());
    Ok(s)
}

/// The session called `name` in `dir`.
pub fn load_in(dir: &std::path::Path, name: &str) -> Result<Session> {
    if !valid_name(name) {
        bail!("no session called {name:?}");
    }
    let path = dir.join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).with_context(|| match list_in(dir) {
        names if names.is_empty() => format!("no session called {name:?} (none saved yet: S saves one)"),
        names => format!("no session called {name:?} (saved: {})", names.join(", ")),
    })?;
    let v: Value = serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    Session::from_json(name, &v)
}

/// The sessions saved, the most recently saved first.
pub fn list() -> Vec<String> {
    list_in(&dir())
}

/// The named sessions and then the history, each newest first: (session, from the history).
/// Unreadable files are left out.
pub fn all() -> Vec<(Session, bool)> {
    let named = list_in(&dir()).into_iter().filter_map(|n| load_in(&dir(), &n).ok()).map(|s| (s, false));
    let hist = history_dir();
    let past = list_in(&hist).into_iter().filter_map(|n| load_in(&hist, &n).ok()).map(|s| (s, true));
    named.chain(past).collect()
}

/// The latest run in the history (for `--continue`).
pub fn latest() -> Option<Session> {
    let hist = history_dir();
    list_in(&hist).into_iter().find_map(|n| load_in(&hist, &n).ok())
}

/// How long ago `t` (Unix seconds) was, `now` being now: `just now`, `5 min ago`, `3 h ago`,
/// `2 days ago`.
pub fn ago(t: f64, now: f64) -> String {
    let s = (now - t).max(0.0);
    match s {
        s if s < 60.0 => "just now".into(),
        s if s < 3600.0 => format!("{:.0} min ago", (s / 60.0).floor()),
        s if s < 86400.0 => format!("{:.0} h ago", (s / 3600.0).floor()),
        s if s < 2.0 * 86400.0 => "yesterday".into(),
        s => format!("{:.0} days ago", (s / 86400.0).floor()),
    }
}

/// The sessions saved in `dir`, the most recently saved first.
pub fn list_in(dir: &std::path::Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<(std::time::SystemTime, String)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().strip_suffix(".json")?.to_string();
            valid_name(&name).then(|| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), name))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|(_, n)| n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn keeps_the_layers_not_the_view_or_headers() {
        let args = s(&["--quakes", "--at", "39,-77", "-z", "9", "--raster-header", "Authorization: Bearer x", "--fires", "--session", "a", "--adsb-key-cmd", "pass x"]);
        assert_eq!(keep_args(&args), s(&["--quakes", "--fires", "--adsb-key-cmd", "pass x"]));
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(1000.0, 1030.0), "just now");
        assert_eq!(ago(0.0, 600.0), "10 min ago");
        assert_eq!(ago(0.0, 3.0 * 3600.0), "3 h ago");
        assert_eq!(ago(0.0, 30.0 * 3600.0), "yesterday");
        assert_eq!(ago(0.0, 5.0 * 86400.0), "5 days ago");
    }

    #[test]
    fn names() {
        assert!(valid_name("frederick closures") && valid_name("md-2026.11"));
        assert!(!valid_name("") && !valid_name(".hidden") && !valid_name("a/b") && !valid_name("../x"));
    }

    #[test]
    fn save_load_list() {
        let dir = std::env::temp_dir().join(format!("mapterm-sessions-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Session {
            name: "frederick".into(),
            args: s(&["--closures"]),
            lon: -77.41,
            lat: 39.414,
            zoom: 12.5,
            base: Some("esri".into()),
            overlays: s(&["esri-hillshade"]),
            live_off: s(&["traffic"]),
            vector: 1,
            saved_at: 1_800_000_000.0,
            created_at: 1_799_000_000.0,
            viewed_at: 0.0,
        };
        a.save_in(&dir).unwrap();
        assert_eq!(load_in(&dir, "frederick").unwrap(), a);
        assert_eq!(list_in(&dir), vec!["frederick".to_string()]);
        assert_eq!(a.command_line(), s(&["--closures", "--at", "39.414000,-77.410000", "-z", "12.50"]));
        let e = load_in(&dir, "nope").unwrap_err();
        assert!(format!("{e:#}").contains("saved: frederick"));
        assert_eq!(a.summary(), "closures on esri");
        assert!(Session { name: "../x".into(), ..a }.save_in(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
