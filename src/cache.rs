//! mapterm's disk caches under `~/.cache/mapterm`, by name, for `mapterm --cache` (sizes and
//! limits) and `--cache-clear` (empty one or all).
//!
//! Map tiles and imagery are SQLite (MBTiles) files, one per source; GeoParquet ranges one
//! SQLite file and fetched styles and TileJSON another (see [`crate::tilecache`]). Microsoft building pieces and STAC indexes are
//! plain files, as are the live layers' (orbits, radio, alert zones, outage outlines); radar
//! scans are an SQLite file. `old` is what earlier versions left (a folder per source, named
//! by a hash, with a file per tile), no longer read; anything else in the folder, mapterm's
//! or not, is left alone.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// One cache: its name, where it is, and how much it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Cache {
    /// The name `--cache-clear` takes.
    pub name: &'static str,
    /// What's in it.
    pub about: &'static str,
    /// Its files and folders.
    pub paths: Vec<PathBuf>,
    /// Bytes on disk.
    pub bytes: u64,
    /// Files.
    pub files: u64,
    /// Its limit.
    pub limit: String,
}

/// The cache names, in report order.
pub const NAMES: [&str; 12] = ["tiles", "imagery", "geoparquet", "docs", "buildings", "stac", "orbits", "radio", "zones", "radar", "outages", "old"];

/// A folder earlier versions kept a source's tiles in: named by [`crate::source::hash`], 16
/// hex digits.
fn is_old_folder(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
        && p.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.len() == 16 && n.chars().all(|c| c.is_ascii_hexdigit())
        })
}

/// Every cache under `root` (normally [`caches_root`]); `old` only when
/// there's something in it.
pub fn caches_in(root: &Path) -> Vec<Cache> {
    NAMES
        .iter()
        .map(|&name| {
            let paths = paths_of(root, name);
            let (bytes, files) = paths.iter().map(|p| usage(p)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
            let (about, limit) = match name {
                "tiles" => ("map tiles, an MBTiles file per source", size(crate::tilecache::tile_limit())),
                "imagery" => ("imagery tiles (--raster), an MBTiles file per source", size(crate::tilecache::imagery_limit())),
                "geoparquet" => ("parts of remote GeoParquet files already read", size(crate::geoparquet::cache_limit())),
                "docs" => ("styles and TileJSON, so a known map opens fast and offline", "re-checked after a day".to_string()),
                "buildings" => ("Microsoft building tiles, split into pieces", size(crate::msbuildings::cache_limit())),
                "stac" => ("STAC collection indexes (which file covers where)", "re-read after 7 days".to_string()),
                "orbits" => ("satellite orbits from CelesTrak (--sats), a file per group", "re-read after 2 hours".to_string()),
                "radio" => ("satellite transmitters from the SatNOGS DB (--sats)", "re-read after a day".to_string()),
                "zones" => ("weather alert zone outlines from the NWS (--alerts)", "kept 90 days".to_string()),
                "radar" => ("weather radar scans from RainViewer (--radar)", "scans over 3 hours old go".to_string()),
                "outages" => ("power outage outlines from the states (--outages)", "re-read after 30 days".to_string()),
                _ => ("left by earlier versions (a file per tile), no longer read", "none: clear it".to_string()),
            };
            Cache { name, about, paths, bytes, files, limit }
        })
        .filter(|c| c.name != "old" || c.files > 0)
        .collect()
}

/// Where the caches live: `~/.cache/mapterm`.
pub fn caches_root() -> PathBuf {
    crate::source::cache_root()
}

/// Every cache under `~/.cache/mapterm`.
pub fn caches() -> Vec<Cache> {
    caches_in(&crate::source::cache_root())
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect()
}

/// An SQLite file and its WAL companions.
fn is_db(p: &Path, ext: &str) -> bool {
    let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    [format!(".{ext}"), format!(".{ext}-wal"), format!(".{ext}-shm")].iter().any(|s| n.ends_with(s.as_str()))
}

fn paths_of(root: &Path, name: &str) -> Vec<PathBuf> {
    let dir = |d: &str| {
        let p = root.join(d);
        if p.is_dir() { vec![p] } else { Vec::new() }
    };
    match name {
        "tiles" => entries(root).into_iter().filter(|p| p.is_file() && is_db(p, "mbtiles")).collect(),
        "imagery" => entries(&root.join("raster")).into_iter().filter(|p| p.is_file() && is_db(p, "mbtiles")).collect(),
        "geoparquet" => entries(root)
            .into_iter()
            .filter(|p| p.is_file() && is_db(p, "sqlite") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("geoparquet.")))
            .collect(),
        "docs" => entries(root)
            .into_iter()
            .filter(|p| p.is_file() && is_db(p, "sqlite") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("docs.")))
            .collect(),
        "buildings" => dir("msbuildings"),
        "stac" => dir("stac"),
        "orbits" => dir("celestrak"),
        "radio" => dir("satnogs"),
        "zones" => dir("nws"),
        "radar" => entries(root)
            .into_iter()
            .filter(|p| p.is_file() && is_db(p, "sqlite") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rainviewer.")))
            .collect(),
        "outages" => dir("outages"),
        // Earlier versions: a folder per source (named by its hash) at the top and under
        // raster/, and the old geoparquet/ folder. Nothing else: other folders may not be
        // mapterm's.
        _ => {
            let top = entries(root).into_iter().filter(|p| is_old_folder(p));
            let raster = entries(&root.join("raster")).into_iter().filter(|p| is_old_folder(p));
            top.chain(raster).chain(dir("geoparquet")).collect()
        }
    }
}

/// Bytes and files of a file, or under a folder, not following symbolic links.
fn usage(p: &Path) -> (u64, u64) {
    let Ok(m) = std::fs::symlink_metadata(p) else { return (0, 0) };
    if m.is_file() {
        return (m.len(), 1);
    }
    if !m.is_dir() {
        return (0, 0);
    }
    entries(p).iter().map(|e| usage(e)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}

/// Empty cache `name` (one of [`NAMES`], or `all`) under `root`; the bytes freed. Removes
/// files and then emptied folders, never following symbolic links, and only inside `root`.
pub fn clear_in(root: &Path, name: &str) -> Result<u64> {
    let names: Vec<&str> = match name {
        "all" => NAMES.to_vec(),
        n if NAMES.contains(&n) => vec![n],
        n => bail!("no cache {n:?}: use {} or all", NAMES.join(", ")),
    };
    let mut freed = 0;
    for n in names {
        for p in paths_of(root, n) {
            if !p.starts_with(root) {
                continue;
            }
            freed += remove(&p)?;
        }
    }
    Ok(freed)
}

/// Empty cache `name` under `~/.cache/mapterm`.
pub fn clear(name: &str) -> Result<u64> {
    clear_in(&crate::source::cache_root(), name)
}

/// Remove a file, or a folder's files and subfolders and then the folder; a symbolic link
/// is removed itself, never followed. The bytes of the files removed.
fn remove(p: &Path) -> Result<u64> {
    let m = std::fs::symlink_metadata(p)?;
    if !m.is_dir() {
        std::fs::remove_file(p)?;
        return Ok(if m.is_file() { m.len() } else { 0 });
    }
    let mut freed = 0;
    for e in entries(p) {
        freed += remove(&e)?;
    }
    std::fs::remove_dir(p)?;
    Ok(freed)
}

/// Bytes as `1.4 GB`, `340 MB`, `12 KB`.
pub fn size(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.0} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_and_clear() {
        let root = std::env::temp_dir().join(format!("mapterm-cache-{}", std::process::id()));
        let w = |p: &str, n: usize| {
            let p = root.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, vec![0u8; n]).unwrap();
        };
        w("1e77e06f15af9955.mbtiles", 100);
        w("1e77e06f15af9955.mbtiles-wal", 50);
        w("raster/abc.mbtiles", 400);
        w("geoparquet.sqlite", 10);
        w("docs.sqlite", 3);
        w("stac/x.json", 7);
        w("2ebdc32559b3691b/14/1/2.mvt", 30);
        w("raster/0b3e7421d1d5927d/1/2/3.img", 20);
        w("satnogs/transmitters.json", 4);
        w("nws/zones/forecast_VAZ092.json", 5);
        w("rainviewer.sqlite", 6);
        w("rainviewer.sqlite-wal", 1);
        w("outages/counties.geojson", 8);
        // Not mapterm's: never listed or cleared.
        w("plugins/someone-elses/data.json", 9);
        let c = caches_in(&root);
        let by = |n: &str| c.iter().find(|x| x.name == n).unwrap().clone();
        assert_eq!((by("tiles").bytes, by("tiles").files), (150, 2));
        assert_eq!((by("imagery").bytes, by("geoparquet").bytes, by("stac").bytes, by("buildings").bytes, by("docs").bytes), (400, 10, 7, 0, 3));
        assert_eq!((by("old").bytes, by("old").files), (50, 2), "the file-per-tile folders of earlier versions");
        assert_eq!((by("radio").bytes, by("zones").bytes, by("radar").bytes, by("radar").files, by("outages").bytes), (4, 5, 7, 2, 8));
        assert_eq!(clear_in(&root, "old").unwrap(), 50);
        assert!(root.join("1e77e06f15af9955.mbtiles").exists() && root.join("raster/abc.mbtiles").exists(), "only the old folders went");
        assert!(!caches_in(&root).iter().any(|c| c.name == "old"), "nothing old left: not listed");
        assert_eq!(clear_in(&root, "tiles").unwrap(), 150);
        assert!(clear_in(&root, "nope").is_err());
        assert_eq!(clear_in(&root, "all").unwrap(), 444);
        let left: Vec<_> = entries(&root).into_iter().filter(|p| p.is_file()).collect();
        assert!(left.is_empty(), "{left:?}");
        assert!(root.join("plugins/someone-elses/data.json").exists(), "another program's folder is left alone");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(size(3 << 29), "1.5 GB");
        assert_eq!(size(340 << 20), "340 MB");
    }
}
