//! mapterm's disk caches under `~/.cache/mapterm`, by name, for `mapterm --cache` (sizes and
//! limits) and `--cache-clear` (empty one or all).

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// One cache: its name, where it is, and how much it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Cache {
    /// The name `--cache-clear` takes.
    pub name: &'static str,
    /// What's in it.
    pub about: &'static str,
    /// Its directories.
    pub dirs: Vec<PathBuf>,
    /// Bytes on disk.
    pub bytes: u64,
    /// Files.
    pub files: u64,
    /// Its limit: a size in bytes, or how long entries are kept.
    pub limit: String,
}

/// The cache names, in report order.
pub const NAMES: [&str; 5] = ["tiles", "imagery", "geoparquet", "buildings", "stac"];

/// The named subdirectories; top-level directories that aren't one of these are vector
/// tile caches (one per source).
const SUBDIRS: [(&str, &str); 4] = [("imagery", "raster"), ("geoparquet", "geoparquet"), ("buildings", "msbuildings"), ("stac", "stac")];

/// Every cache under `root` (normally [`caches_root`]).
pub fn caches_in(root: &Path) -> Vec<Cache> {
    NAMES
        .iter()
        .map(|&name| {
            let dirs = dirs_of(root, name);
            let (bytes, files) = dirs.iter().map(|d| usage(d)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
            let (about, limit) = match name {
                "tiles" => ("vector map tiles, one folder per source", "each tile up to 30 days".to_string()),
                "imagery" => ("raster imagery tiles (--raster)", "each tile up to 30 days, or as its server says".to_string()),
                "geoparquet" => ("parts of remote GeoParquet files already read", size(crate::geoparquet::cache_limit())),
                "buildings" => ("Microsoft building tiles, split into pieces", size(crate::msbuildings::cache_limit())),
                _ => ("STAC collection indexes (which file covers where)", "re-read after 7 days".to_string()),
            };
            Cache { name, about, dirs, bytes, files, limit }
        })
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

fn dirs_of(root: &Path, name: &str) -> Vec<PathBuf> {
    match SUBDIRS.iter().find(|(n, _)| *n == name) {
        Some((_, sub)) => {
            let d = root.join(sub);
            if d.is_dir() { vec![d] } else { Vec::new() }
        }
        None => std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| !SUBDIRS.iter().any(|(_, sub)| n == *sub)))
            .collect(),
    }
}

/// Bytes and files under `dir`, not following symbolic links.
fn usage(dir: &Path) -> (u64, u64) {
    let mut out = (0, 0);
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(m) = std::fs::symlink_metadata(e.path()) else { continue };
        if m.is_dir() {
            let (b, f) = usage(&e.path());
            out = (out.0 + b, out.1 + f);
        } else if m.is_file() {
            out = (out.0 + m.len(), out.1 + 1);
        }
    }
    out
}

/// Empty cache `name` (one of [`NAMES`], or `all`) under `root`; the bytes freed. Removes
/// files and then the emptied directories, never following symbolic links, and only inside
/// `root`.
pub fn clear_in(root: &Path, name: &str) -> Result<u64> {
    let names: Vec<&str> = match name {
        "all" => NAMES.to_vec(),
        n if NAMES.contains(&n) => vec![n],
        n => bail!("no cache {n:?}: use {} or all", NAMES.join(", ")),
    };
    let mut freed = 0;
    for n in names {
        for d in dirs_of(root, n) {
            if d.parent() != Some(root) {
                continue;
            }
            freed += remove_tree(&d)?;
        }
    }
    Ok(freed)
}

/// Empty cache `name` under `~/.cache/mapterm`.
pub fn clear(name: &str) -> Result<u64> {
    clear_in(&crate::source::cache_root(), name)
}

/// Remove `dir`'s files and subdirectories, then `dir`; a symbolic link is removed itself,
/// never followed. The bytes of the files removed.
fn remove_tree(dir: &Path) -> Result<u64> {
    let mut freed = 0;
    for e in std::fs::read_dir(dir)?.flatten() {
        let p = e.path();
        let m = std::fs::symlink_metadata(&p)?;
        if m.is_dir() {
            freed += remove_tree(&p)?;
        } else {
            freed += if m.is_file() { m.len() } else { 0 };
            std::fs::remove_file(&p)?;
        }
    }
    std::fs::remove_dir(dir)?;
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
        w("1e77e06f15af9955/14/1/2.pbf", 100);
        w("1e77e06f15af9955/14/1/3.pbf", 50);
        w("raster/abc/1/2/3.png", 400);
        w("geoparquet/d1/0-10", 10);
        w("stac/x.json", 7);
        let c = caches_in(&root);
        let by = |n: &str| c.iter().find(|x| x.name == n).unwrap().clone();
        assert_eq!((by("tiles").bytes, by("tiles").files), (150, 2));
        assert_eq!((by("imagery").bytes, by("geoparquet").bytes, by("stac").bytes, by("buildings").bytes), (400, 10, 7, 0));
        assert_eq!(clear_in(&root, "tiles").unwrap(), 150);
        assert!(!root.join("1e77e06f15af9955").exists() && root.join("raster").exists(), "only the tile cache went");
        assert!(clear_in(&root, "nope").is_err());
        assert_eq!(clear_in(&root, "all").unwrap(), 417);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let _ = std::fs::remove_dir(&root);
        assert_eq!(size(3 << 29), "1.5 GB");
        assert_eq!(size(340 << 20), "340 MB");
    }
}
