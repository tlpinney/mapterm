//! mapterm's tile caches as SQLite: each tile source's tiles in its own MBTiles file
//! (`~/.cache/mapterm/<source>.mbtiles` for map tiles, `raster/<source>.mbtiles` for
//! imagery), so a cache opens in mapterm or any MBTiles tool; and the ranges read from remote
//! GeoParquet files in `geoparquet.sqlite`. Next to the standard `tiles` table, a
//! `mapterm_cache` table keeps each tile's expiry, ETag (for revalidation), size and last
//! use, so each kind of cache stays under a size limit, least recently used out first.
//! WAL mode lets several mapterm windows share the files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

/// Default size limit for each of the map tile and imagery caches.
pub const DEFAULT_LIMIT: u64 = 2 << 30;

static TILE_LIMIT: AtomicU64 = AtomicU64::new(DEFAULT_LIMIT);
static IMAGERY_LIMIT: AtomicU64 = AtomicU64::new(DEFAULT_LIMIT);

/// Disk for cached map tiles (all sources together); 0 keeps none.
pub fn set_tile_limit(bytes: u64) {
    TILE_LIMIT.store(bytes, Ordering::Relaxed);
}

/// Disk for cached imagery tiles (all sources together); 0 keeps none.
pub fn set_imagery_limit(bytes: u64) {
    IMAGERY_LIMIT.store(bytes, Ordering::Relaxed);
}

/// The map tile cache's limit.
pub fn tile_limit() -> u64 {
    TILE_LIMIT.load(Ordering::Relaxed)
}

/// The imagery cache's limit.
pub fn imagery_limit() -> u64 {
    IMAGERY_LIMIT.load(Ordering::Relaxed)
}

/// Seconds since 1970.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A last-use time is rewritten only when older than this (reads stay reads).
const TOUCH_AFTER: u64 = 600;

/// The size limit is checked every this many writes.
const TRIM_EVERY: u32 = 64;

/// Open (or make) a cache database: WAL, a 5 s busy timeout, and space from deleted rows
/// given back on `PRAGMA incremental_vacuum`.
pub(crate) fn connect(path: &Path) -> Result<Connection> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let c = Connection::open(path)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    // Before any table exists, so deleted tiles' space can be given back.
    c.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
    c.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    c.execute_batch("PRAGMA synchronous = NORMAL;")?;
    Ok(c)
}

/// One tile from a cache.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    /// The bytes (empty: the source has no such tile).
    pub data: Vec<u8>,
    /// Seconds since 1970 after which it must be fetched (or revalidated) again.
    pub expires: u64,
    /// The ETag it came with, for `If-None-Match`.
    pub etag: Option<String>,
}

/// A tile source's cache: an MBTiles file.
pub(crate) struct TileCache {
    conn: Mutex<Connection>,
    path: PathBuf,
    writes: AtomicU32,
}

/// The cache that replaces the old tile directory `dir`: `dir.mbtiles` beside it (one
/// connection per file in this process).
pub(crate) fn for_dir(dir: &Path) -> Option<Arc<TileCache>> {
    static OPEN: OnceLock<Mutex<HashMap<PathBuf, Arc<TileCache>>>> = OnceLock::new();
    let path = dir.with_extension("mbtiles");
    let mut open = OPEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = open.get(&path) {
        return Some(c.clone());
    }
    let imagery = dir.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "raster");
    let c = Arc::new(TileCache::open(&path, if imagery { "png" } else { "pbf" }).ok()?);
    open.insert(path, c.clone());
    Some(c)
}

impl TileCache {
    fn open(path: &Path, format: &str) -> Result<TileCache> {
        let c = connect(path)?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS metadata (name TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB,
                 PRIMARY KEY (zoom_level, tile_column, tile_row)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS mapterm_cache (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER,
                 expires INTEGER, etag TEXT, size INTEGER, used INTEGER, PRIMARY KEY (zoom_level, tile_column, tile_row)) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS mapterm_cache_used ON mapterm_cache (used);",
        )?;
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        c.execute("INSERT OR IGNORE INTO metadata VALUES ('name', ?1), ('format', ?2)", params![format!("mapterm cache {name}"), format])?;
        Ok(TileCache { conn: Mutex::new(c), path: path.to_path_buf(), writes: AtomicU32::new(0) })
    }

    fn row(z: u8, y: u32) -> i64 {
        (1i64 << z) - 1 - y as i64
    }

    /// The cached tile, if any (expired or not).
    pub(crate) fn get(&self, z: u8, x: u32, y: u32) -> Option<Entry> {
        let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let k = (z as i64, x as i64, Self::row(z, y));
        let (data, expires, etag, used): (Vec<u8>, i64, Option<String>, i64) = c
            .query_row(
                "SELECT t.tile_data, m.expires, m.etag, m.used FROM tiles t JOIN mapterm_cache m
                 ON m.zoom_level = t.zoom_level AND m.tile_column = t.tile_column AND m.tile_row = t.tile_row
                 WHERE t.zoom_level = ?1 AND t.tile_column = ?2 AND t.tile_row = ?3",
                params![k.0, k.1, k.2],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .ok()
            .flatten()?;
        let t = now();
        if t.saturating_sub(used as u64) > TOUCH_AFTER {
            let _ =
                c.execute("UPDATE mapterm_cache SET used = ?4 WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3", params![k.0, k.1, k.2, t as i64]);
        }
        Some(Entry { data, expires: expires.max(0) as u64, etag })
    }

    /// The cached tile if it hasn't expired.
    pub(crate) fn fresh(&self, z: u8, x: u32, y: u32) -> Option<Vec<u8>> {
        self.get(z, x, y).filter(|e| e.expires > now()).map(|e| e.data)
    }

    /// Keep a tile (empty `data`: the source has none) until `expires`.
    pub(crate) fn put(&self, z: u8, x: u32, y: u32, data: &[u8], expires: u64, etag: Option<&str>) {
        {
            let mut c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            let k = (z as i64, x as i64, Self::row(z, y));
            let ok = (|| -> rusqlite::Result<()> {
                let tx = c.transaction()?;
                tx.execute("INSERT OR REPLACE INTO tiles VALUES (?1, ?2, ?3, ?4)", params![k.0, k.1, k.2, data])?;
                tx.execute(
                    "INSERT OR REPLACE INTO mapterm_cache VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![k.0, k.1, k.2, expires as i64, etag, data.len() as i64, now() as i64],
                )?;
                tx.commit()
            })();
            if ok.is_err() {
                return;
            }
        }
        if self.writes.fetch_add(1, Ordering::Relaxed) % TRIM_EVERY == TRIM_EVERY - 1 {
            trim_tiles(&self.path);
        }
    }

    /// Forget a tile.
    pub(crate) fn remove(&self, z: u8, x: u32, y: u32) {
        let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let k = (z as i64, x as i64, Self::row(z, y));
        let _ = c.execute("DELETE FROM tiles WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3", params![k.0, k.1, k.2]);
        let _ = c.execute("DELETE FROM mapterm_cache WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3", params![k.0, k.1, k.2]);
    }

    /// Tests: set a tile's expiry.
    #[cfg(test)]
    pub(crate) fn set_expires(&self, z: u8, x: u32, y: u32, expires: u64) {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE mapterm_cache SET expires = ?4 WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3",
            params![z as i64, x as i64, Self::row(z, y), expires as i64],
        )
        .unwrap();
    }
}

/// Keep the caches in `path`'s folder (all its `.mbtiles`) under their kind's limit.
fn trim_tiles(path: &Path) {
    let Some(dir) = path.parent() else { return };
    let imagery = dir.file_name().is_some_and(|n| n == "raster");
    let limit = if imagery { imagery_limit() } else { tile_limit() };
    let files: Vec<PathBuf> =
        std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "mbtiles")).collect();
    let _ = trim_dbs(&files, "mapterm_cache", limit, |c, cutoff| {
        c.execute(
            "DELETE FROM tiles WHERE (zoom_level, tile_column, tile_row) IN
             (SELECT zoom_level, tile_column, tile_row FROM mapterm_cache WHERE used <= ?1)",
            [cutoff],
        )?;
        c.execute("DELETE FROM mapterm_cache WHERE used <= ?1", [cutoff])?;
        Ok(())
    });
}

/// Over `limit` (the sizes in `table` across `files`): find the last-use time below which
/// removing everything brings them to 90% of it, and remove that with `delete` in each file.
fn trim_dbs(files: &[PathBuf], table: &str, limit: u64, delete: impl Fn(&Connection, i64) -> rusqlite::Result<()>) -> Result<u64> {
    let mut rows: Vec<(i64, u64)> = Vec::new();
    for f in files {
        let Ok(c) = Connection::open(f) else { continue };
        let _ = c.busy_timeout(std::time::Duration::from_secs(5));
        let Ok(mut s) = c.prepare(&format!("SELECT used, size FROM {table}")) else { continue };
        if let Ok(it) = s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?.max(0) as u64))) {
            rows.extend(it.flatten());
        }
    }
    let total: u64 = rows.iter().map(|r| r.1).sum();
    if total <= limit {
        return Ok(0);
    }
    rows.sort_by_key(|r| r.0);
    let target = limit / 10 * 9;
    let (mut left, mut cutoff, mut freed) = (total, i64::MIN, 0);
    for (used, size) in rows {
        if left <= target {
            break;
        }
        cutoff = used;
        left -= size;
        freed += size;
    }
    for f in files {
        if let Ok(c) = Connection::open(f) {
            let _ = c.busy_timeout(std::time::Duration::from_secs(5));
            if delete(&c, cutoff).is_ok() {
                let _ = c.execute_batch("PRAGMA incremental_vacuum;");
            }
        }
    }
    Ok(freed)
}

// ---- documents (styles, TileJSON) ----

/// How long a cached document is used without asking its server (styles and TileJSON
/// change rarely; this overrides shorter server cache times).
pub(crate) const DOC_FRESH: u64 = 24 * 3600;

/// Small documents fetched at start (styles, TileJSON) in `docs.sqlite`, keyed by a hash
/// of the URL (URLs can carry access tokens).
pub(crate) struct DocCache {
    conn: Mutex<Connection>,
}

/// The cached copy of a document.
pub(crate) struct Doc {
    pub data: Vec<u8>,
    /// Seconds since it was fetched or last confirmed unchanged.
    pub age: u64,
    pub etag: Option<String>,
}

impl DocCache {
    /// The document cache under `root` (one connection per process).
    pub(crate) fn at(root: &Path) -> Option<&'static DocCache> {
        static OPEN: OnceLock<Option<DocCache>> = OnceLock::new();
        OPEN.get_or_init(|| DocCache::open(&root.join("docs.sqlite"))).as_ref()
    }

    /// Open (creating) the document cache at `path`.
    pub(crate) fn open(path: &Path) -> Option<DocCache> {
        let c = connect(path).ok()?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS docs (key TEXT PRIMARY KEY, data BLOB, fetched INTEGER, etag TEXT) WITHOUT ROWID;").ok()?;
        Some(DocCache { conn: Mutex::new(c) })
    }

    /// Make the copy of `url` look `secs` older (tests).
    #[cfg(test)]
    pub(crate) fn age_by(&self, url: &str, secs: u64) {
        let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        c.execute("UPDATE docs SET fetched = fetched - ?2 WHERE key = ?1", params![crate::source::hash(url), secs as i64]).unwrap();
    }

    pub(crate) fn get(&self, url: &str) -> Option<Doc> {
        let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (data, fetched, etag): (Vec<u8>, i64, Option<String>) = c
            .query_row("SELECT data, fetched, etag FROM docs WHERE key = ?1", [crate::source::hash(url)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .ok()
            .flatten()?;
        Some(Doc { data, age: now().saturating_sub(fetched.max(0) as u64), etag })
    }

    pub(crate) fn put(&self, url: &str, data: &[u8], etag: Option<&str>) {
        let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let _ = c.execute("INSERT OR REPLACE INTO docs VALUES (?1, ?2, ?3, ?4)", params![crate::source::hash(url), data, now() as i64, etag]);
    }

    pub(crate) fn forget(&self, url: &str) {
        let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let _ = c.execute("DELETE FROM docs WHERE key = ?1", [crate::source::hash(url)]);
    }
}

// ---- GeoParquet ranges ----

/// The ranges read from remote GeoParquet files: one SQLite file.
pub(crate) struct RangeCache {
    conn: Mutex<Connection>,
    path: PathBuf,
    writes: AtomicU32,
}

impl RangeCache {
    /// The cache at `path` (one connection per file in this process).
    pub(crate) fn at(path: &Path) -> Option<Arc<RangeCache>> {
        static OPEN: OnceLock<Mutex<HashMap<PathBuf, Arc<RangeCache>>>> = OnceLock::new();
        let mut open = OPEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = open.get(path) {
            return Some(c.clone());
        }
        let c = connect(path).ok()?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS ranges (file TEXT, start INTEGER, end INTEGER, data BLOB, size INTEGER, used INTEGER,
                 PRIMARY KEY (file, start, end)) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS ranges_used ON ranges (used);",
        )
        .ok()?;
        let r = Arc::new(RangeCache { conn: Mutex::new(c), path: path.to_path_buf(), writes: AtomicU32::new(0) });
        open.insert(path.to_path_buf(), r.clone());
        Some(r)
    }

    /// Bytes `start..end` of `file` (a URL-and-ETag key), if kept.
    pub(crate) fn get(&self, file: &str, start: u64, end: u64) -> Option<Vec<u8>> {
        let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (data, used): (Vec<u8>, i64) = c
            .query_row("SELECT data, used FROM ranges WHERE file = ?1 AND start = ?2 AND end = ?3", params![file, start as i64, end as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .ok()
            .flatten()?;
        let t = now();
        if t.saturating_sub(used as u64) > TOUCH_AFTER {
            let _ = c.execute("UPDATE ranges SET used = ?4 WHERE file = ?1 AND start = ?2 AND end = ?3", params![file, start as i64, end as i64, t as i64]);
        }
        Some(data)
    }

    /// Keep bytes `start..end` of `file`, then stay under `limit`.
    pub(crate) fn put(&self, file: &str, start: u64, end: u64, data: &[u8], limit: u64) {
        {
            let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            if c.execute(
                "INSERT OR REPLACE INTO ranges VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![file, start as i64, end as i64, data, data.len() as i64, now() as i64],
            )
            .is_err()
            {
                return;
            }
        }
        if self.writes.fetch_add(1, Ordering::Relaxed) % 8 == 7 || data.len() > 1 << 20 {
            let _ =
                trim_dbs(std::slice::from_ref(&self.path), "ranges", limit, |c, cutoff| c.execute("DELETE FROM ranges WHERE used <= ?1", [cutoff]).map(|_| ()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mapterm-tilecache-{}-{name}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn tiles_round_trip_as_mbtiles() {
        let dir = tmp("rt").join("abc");
        let c = for_dir(&dir).unwrap();
        assert_eq!(c.get(3, 2, 1), None);
        c.put(3, 2, 1, b"tile", now() + 100, Some("\"v1\""));
        assert_eq!(c.fresh(3, 2, 1).as_deref(), Some(&b"tile"[..]));
        assert_eq!(c.get(3, 2, 1).unwrap().etag.as_deref(), Some("\"v1\""));
        c.set_expires(3, 2, 1, 0);
        assert_eq!(c.fresh(3, 2, 1), None, "expired: fetch again");
        assert!(c.get(3, 2, 1).is_some(), "but still there to revalidate");
        // The file is an MBTiles that mapterm (and other tools) can open.
        let mb = crate::mbtiles::MbTiles::open(dir.with_extension("mbtiles").to_str().unwrap()).unwrap();
        assert_eq!(mb.tile(3, 2, 1).unwrap().as_deref(), Some(&b"tile"[..]));
        assert!(mb.meta.is_vector());
        c.remove(3, 2, 1);
        assert_eq!(c.get(3, 2, 1), None);
    }

    #[test]
    fn least_recently_used_go_first() {
        let dir = tmp("lru");
        let files: Vec<PathBuf> = (0..2).map(|i| dir.join(format!("s{i}.mbtiles"))).collect();
        for (i, f) in files.iter().enumerate() {
            let c = TileCache::open(f, "pbf").unwrap();
            for t in 0..5u32 {
                c.put(5, t, i as u32, &[0u8; 100], now() + 999, None);
                // Older use for lower t, across both files.
                c.conn
                    .lock()
                    .unwrap()
                    .execute("UPDATE mapterm_cache SET used = ?1 WHERE tile_column = ?2", params![1000 + t as i64 * 10 + i as i64, t as i64])
                    .unwrap();
            }
        }
        // 1000 bytes kept; a 600-byte limit trims to 540: the five oldest (by use) go.
        let freed = trim_dbs(&files, "mapterm_cache", 600, |c, cutoff| {
            c.execute(
                "DELETE FROM tiles WHERE (zoom_level, tile_column, tile_row) IN (SELECT zoom_level, tile_column, tile_row FROM mapterm_cache WHERE used <= ?1)",
                [cutoff],
            )?;
            c.execute("DELETE FROM mapterm_cache WHERE used <= ?1", [cutoff]).map(|_| ())
        })
        .unwrap();
        assert_eq!(freed, 500);
        let left: i64 = files.iter().map(|f| Connection::open(f).unwrap().query_row("SELECT COUNT(*) FROM tiles", [], |r| r.get::<_, i64>(0)).unwrap()).sum();
        assert_eq!(left, 5);
        let oldest_left: i64 = Connection::open(&files[0]).unwrap().query_row("SELECT MIN(used) FROM mapterm_cache", [], |r| r.get(0)).unwrap();
        assert!(oldest_left >= 1020, "{oldest_left}");
    }

    #[test]
    fn ranges() {
        let r = RangeCache::at(&tmp("ranges").join("geoparquet.sqlite")).unwrap();
        assert_eq!(r.get("f", 0, 4), None);
        r.put("f", 0, 4, b"abcd", 1 << 30);
        assert_eq!(r.get("f", 0, 4).as_deref(), Some(&b"abcd"[..]));
        assert_eq!(r.get("g", 0, 4), None);
    }
}
