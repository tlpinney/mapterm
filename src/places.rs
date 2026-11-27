//! Saved places: spots you named (`B` in the viewer) to find again and fly to. Kept exactly
//! (they're your own) in `$XDG_CONFIG_HOME/mapterm/places.json` (else
//! `~/.config/mapterm/places.json`), beside the sessions.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// A saved place.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// Its name.
    pub name: String,
    /// Degrees.
    pub lat: f64,
    /// Degrees.
    pub lon: f64,
    /// The zoom it was saved at (flying there shows it so).
    pub zoom: f64,
    /// When it was saved (Unix seconds).
    pub created: f64,
    /// When it was last flown to (Unix seconds; 0: never).
    pub visited: f64,
}

/// Where places are kept.
pub fn file() -> PathBuf {
    crate::sessions::dir().parent().map_or_else(|| PathBuf::from("places.json"), |d| d.join("places.json"))
}

/// The places in `path`, the most recently visited (else saved) first. None saved: empty.
pub fn load_from(path: &Path) -> Vec<Place> {
    let Ok(raw) = std::fs::read(path) else { return Vec::new() };
    let Ok(Value::Array(list)) = serde_json::from_slice::<Value>(&raw) else { return Vec::new() };
    let mut v: Vec<Place> = list
        .iter()
        .filter_map(|p| {
            let (lat, lon) = (p["lat"].as_f64()?, p["lon"].as_f64()?);
            let name = p["name"].as_str()?.to_string();
            (lat.abs() <= 90.0 && lon.abs() <= 180.0).then(|| Place {
                name,
                lat,
                lon,
                zoom: p["zoom"].as_f64().unwrap_or(14.0),
                created: p["created"].as_f64().unwrap_or(0.0),
                visited: p["visited"].as_f64().unwrap_or(0.0),
            })
        })
        .collect();
    sort(&mut v);
    v
}

/// The places saved.
pub fn load() -> Vec<Place> {
    load_from(&file())
}

fn sort(v: &mut [Place]) {
    v.sort_by(|a, b| b.visited.max(b.created).total_cmp(&a.visited.max(a.created)).then(a.name.cmp(&b.name)));
}

/// Write `places` to `path` (aside, then moved into place).
pub fn save_to(path: &Path, places: &[Place]) -> Result<()> {
    let v: Vec<Value> =
        places.iter().map(|p| json!({"name": p.name, "lat": p.lat, "lon": p.lon, "zoom": p.zoom, "created": p.created, "visited": p.visited})).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.part");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v)? + "\n")?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Save a place called `name` at (`lat`, `lon`, `zoom`) at time `now`, replacing one of that
/// name (keeping when it was first saved). Returns the places.
pub fn add(path: &Path, name: &str, lat: f64, lon: f64, zoom: f64, now: f64) -> Result<Vec<Place>> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        bail!("a place needs a name (up to 80 characters)");
    }
    let mut v = load_from(path);
    let created = v.iter().find(|p| p.name == name).map_or(now, |p| p.created);
    v.retain(|p| p.name != name);
    v.push(Place { name: name.to_string(), lat, lon, zoom, created, visited: now });
    sort(&mut v);
    save_to(path, &v)?;
    Ok(v)
}

/// Forget the place called `name`. Returns the places.
pub fn remove(path: &Path, name: &str) -> Result<Vec<Place>> {
    let mut v = load_from(path);
    v.retain(|p| p.name != name);
    save_to(path, &v)?;
    Ok(v)
}

/// Note `name` as visited at `now`. Returns it.
pub fn visit(path: &Path, name: &str, now: f64) -> Option<Place> {
    let mut v = load_from(path);
    let p = v.iter_mut().find(|p| p.name == name)?;
    p.visited = now;
    let out = p.clone();
    sort(&mut v);
    let _ = save_to(path, &v);
    Some(out)
}

/// The place called `name` (exactly, else ignoring case), for `--at NAME`.
pub fn find(name: &str) -> Option<Place> {
    let v = load();
    v.iter().find(|p| p.name == name).or_else(|| v.iter().find(|p| p.name.eq_ignore_ascii_case(name))).cloned()
}

/// The places matching `query` fzf-style, best first, as search results.
pub fn search(places: &[Place], query: &str, max: usize) -> Vec<crate::geocode::Place> {
    let mut v: Vec<(i32, &Place)> = places.iter().filter_map(|p| crate::fuzzy::score(query, &p.name).map(|s| (s, p))).collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.0));
    v.into_iter()
        .take(max)
        .map(|(_, p)| crate::geocode::Place {
            name: p.name.clone(),
            context: format!("{:.4}, {:.4}", p.lat, p.lon),
            kind: "saved place".into(),
            lon: p.lon,
            lat: p.lat,
            bounds: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_find_visit_remove() {
        let dir = std::env::temp_dir().join(format!("mapterm-places-{}", std::process::id()));
        let path = dir.join("places.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_from(&path).is_empty());
        add(&path, "Cunningham Falls", 39.6253, -77.4614, 14.0, 100.0).unwrap();
        add(&path, "Home field", 39.4171, -77.3744, 15.0, 200.0).unwrap();
        let v = load_from(&path);
        assert_eq!(v.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Home field", "Cunningham Falls"], "latest first");
        // Visiting moves it to the front; saving again keeps when it was made.
        visit(&path, "Cunningham Falls", 300.0).unwrap();
        assert_eq!(load_from(&path)[0].name, "Cunningham Falls");
        let v = add(&path, "Home field", 39.0, -77.0, 12.0, 400.0).unwrap();
        let h = v.iter().find(|p| p.name == "Home field").unwrap();
        assert_eq!((h.created, h.lat, h.zoom), (200.0, 39.0, 12.0));
        assert_eq!(v.len(), 2);
        // Found fzf-style, as search results.
        let r = search(&v, "cfalls", 5);
        assert_eq!((r[0].name.as_str(), r[0].kind.as_str()), ("Cunningham Falls", "saved place"));
        assert!(add(&path, "  ", 0.0, 0.0, 1.0, 0.0).is_err());
        let v = remove(&path, "Home field").unwrap();
        assert_eq!(v.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
