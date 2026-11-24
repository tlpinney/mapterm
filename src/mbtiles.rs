//! MBTiles files: tiles in a SQLite database (the `tiles` table, rows counted from the
//! bottom as in TMS) with a `metadata` table of name/value pairs. Vector MBTiles
//! (`format` pbf, tiles usually gzipped) open as a map source like PMTiles; raster MBTiles
//! (png, jpg, webp) as `--raster` imagery. Read-only; local files only.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// An open MBTiles file.
pub struct MbTiles {
    conn: Mutex<Connection>,
    /// From the `metadata` table.
    pub meta: Meta,
}

/// The `metadata` table's values mapterm uses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    /// `name`.
    pub name: Option<String>,
    /// `format`: pbf, png, jpg, webp.
    pub format: String,
    /// `minzoom` (else the lowest zoom in `tiles`).
    pub min_zoom: u8,
    /// `maxzoom` (else the highest zoom in `tiles`).
    pub max_zoom: u8,
    /// `center`: lon, lat, zoom.
    pub center: Option<(f64, f64, f64)>,
    /// `bounds`: west, south, east, north.
    pub bounds: Option<(f64, f64, f64, f64)>,
    /// `attribution` (often HTML).
    pub attribution: Option<String>,
}

impl Meta {
    /// Whether the tiles are vector tiles (MVT) rather than images.
    pub fn is_vector(&self) -> bool {
        matches!(self.format.as_str(), "pbf" | "mvt")
    }
}

/// Whether `spec` names an MBTiles file.
pub fn is_mbtiles(spec: &str) -> bool {
    spec.to_ascii_lowercase().ends_with(".mbtiles")
}

impl MbTiles {
    /// Open `path` read-only and read its metadata.
    pub fn open(path: &str) -> Result<MbTiles> {
        if path.starts_with("http://") || path.starts_with("https://") {
            bail!("{path}: MBTiles must be a local file (download it first, or use PMTiles for a URL)");
        }
        if !Path::new(path).is_file() {
            bail!("{path}: no such file");
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_context(|| format!("{path}: can't open"))?;
        let pairs: Vec<(String, String)> = conn
            .prepare("SELECT name, value FROM metadata")
            .and_then(|mut s| s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect())
            .with_context(|| format!("{path}: not MBTiles (no metadata table)"))?;
        let get = |k: &str| pairs.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty());
        let nums = |k: &str| get(k).map(|v| v.split(',').filter_map(|x| x.trim().parse::<f64>().ok()).collect::<Vec<f64>>());
        let (lo, hi): (Option<u8>, Option<u8>) = conn
            .query_row("SELECT MIN(zoom_level), MAX(zoom_level) FROM tiles", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .with_context(|| format!("{path}: not MBTiles (no tiles table)"))?;
        let meta = Meta {
            name: get("name"),
            format: get("format").unwrap_or_else(|| "pbf".into()).to_ascii_lowercase(),
            min_zoom: get("minzoom").and_then(|v| v.parse().ok()).or(lo).unwrap_or(0),
            max_zoom: get("maxzoom").and_then(|v| v.parse().ok()).or(hi).unwrap_or(14).min(crate::render::MAX_TILE_ZOOM),
            center: nums("center").filter(|c| c.len() >= 2).map(|c| (c[0], c[1], c.get(2).copied().unwrap_or(10.0))),
            bounds: nums("bounds").filter(|b| b.len() == 4).map(|b| (b[0], b[1], b[2], b[3])),
            attribution: get("attribution"),
        };
        Ok(MbTiles { conn: Mutex::new(conn), meta })
    }

    /// The tile's bytes as stored (gzipped for most vector MBTiles), or None. MBTiles counts
    /// rows from the bottom, so `y` (from the top) is flipped.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        let row = (1u64 << z).saturating_sub(1).saturating_sub(y as u64);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT tile_data FROM tiles WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3", (z as i64, x as i64, row as i64), |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .optional()
        .context("reading a tile")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tile (z, x, y from the top) and its bytes.
    pub(crate) type TileBytes = ((u8, u32, u32), Vec<u8>);

    /// A small MBTiles file at a fresh temporary path: `tiles` maps (z, x, y-from-top) to
    /// bytes, stored with TMS rows.
    pub(crate) fn write(format: &str, tiles: &[TileBytes], meta: &[(&str, &str)]) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("mapterm-mbt-{}-{}.mbtiles", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let _ = std::fs::remove_file(&path);
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE metadata (name TEXT, value TEXT); CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB);",
        )
        .unwrap();
        c.execute("INSERT INTO metadata VALUES ('format', ?1)", [format]).unwrap();
        for (k, v) in meta {
            c.execute("INSERT INTO metadata VALUES (?1, ?2)", [k, v]).unwrap();
        }
        for ((z, x, y), b) in tiles {
            c.execute("INSERT INTO tiles VALUES (?1, ?2, ?3, ?4)", (*z as i64, *x as i64, ((1i64 << z) - 1 - *y as i64), b)).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn vector_and_raster_sources() {
        // A gzipped vector tile opens as the map; an image tile as --raster imagery.
        let mvt = crate::mvt::tests_support::square_tile_with_id("water", 4096, Some(1));
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &mvt).unwrap();
        let p = write("pbf", &[((0, 0, 0), gz.finish().unwrap())], &[("name", "Squares")]);
        let src = crate::source::Source::open(p.to_str().unwrap()).await.unwrap();
        assert_eq!(src.name, "Squares");
        assert_eq!(src.tile(0, 0, 0).await.unwrap(), Some(mvt), "gunzipped");
        assert!(crate::raster::RasterSource::from_mbtiles(p.to_str().unwrap()).is_err(), "vector MBTiles isn't imagery");
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 255, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let q = write("png", &[((1, 0, 1), png.clone())], &[]);
        let r = crate::raster::RasterSource::from_mbtiles(q.to_str().unwrap()).unwrap();
        assert_eq!(r.tile(1, 0, 1).await.unwrap(), Some(png));
        assert!(crate::source::Source::open(q.to_str().unwrap()).await.is_err(), "image MBTiles isn't a vector map");
        let _ = (std::fs::remove_file(p), std::fs::remove_file(q));
    }

    #[test]
    fn reads_metadata_and_flips_rows() {
        let p = write(
            "pbf",
            &[((2, 1, 0), b"top".to_vec()), ((2, 1, 3), b"bottom".to_vec())],
            &[("name", "Test"), ("center", "-77.03,38.9,13"), ("bounds", "-78,38,-76,40"), ("attribution", "<a>© Me</a>")],
        );
        let m = MbTiles::open(p.to_str().unwrap()).unwrap();
        assert_eq!(m.meta.name.as_deref(), Some("Test"));
        assert!(m.meta.is_vector());
        assert_eq!((m.meta.min_zoom, m.meta.max_zoom), (2, 2), "zooms from the tiles when metadata has none");
        assert_eq!(m.meta.center, Some((-77.03, 38.9, 13.0)));
        assert_eq!(m.meta.bounds, Some((-78.0, 38.0, -76.0, 40.0)));
        assert_eq!(m.tile(2, 1, 0).unwrap().as_deref(), Some(&b"top"[..]), "y from the top, rows from the bottom");
        assert_eq!(m.tile(2, 1, 3).unwrap().as_deref(), Some(&b"bottom"[..]));
        assert_eq!(m.tile(2, 0, 0).unwrap(), None);
        assert!(MbTiles::open("https://x/y.mbtiles").is_err() && MbTiles::open("/nonexistent.mbtiles").is_err());
        let _ = std::fs::remove_file(p);
    }
}
