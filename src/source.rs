//! Where tiles come from: a PMTiles archive (local file or HTTP range requests), an XYZ URL
//! template, or a TileJSON endpoint. Every network read has a timeout and a size cap, and
//! decompression is capped too, so a hostile server can't exhaust memory.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;

use crate::pmtiles::{self, Entry, Header};

/// Largest compressed tile or directory we'll download.
const MAX_FETCH: usize = 8 << 20;
/// Largest tile after decompression.
const MAX_INFLATE: usize = 32 << 20;
const DISK_TTL: Duration = Duration::from_secs(30 * 86400);

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("termtiles/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client")
}

/// gunzip if the bytes are gzip, with a hard output cap.
pub fn maybe_gunzip(b: Vec<u8>) -> Result<Vec<u8>> {
    if b.len() < 2 || b[0] != 0x1f || b[1] != 0x8b {
        return Ok(b);
    }
    let mut out = Vec::new();
    GzDecoder::new(b.as_slice()).take(MAX_INFLATE as u64 + 1).read_to_end(&mut out)?;
    if out.len() > MAX_INFLATE {
        bail!("tile inflates past {} MB", MAX_INFLATE >> 20);
    }
    Ok(out)
}

fn decompress(b: Vec<u8>, compression: u8) -> Result<Vec<u8>> {
    match compression {
        pmtiles::COMPRESSION_NONE | 0 => Ok(b),
        pmtiles::COMPRESSION_GZIP => maybe_gunzip(b),
        c => bail!("PMTiles compression {c} not supported (gzip only)"),
    }
}

async fn get(http: &reqwest::Client, url: &str, range: Option<(u64, u64)>) -> Result<Option<Vec<u8>>> {
    let mut req = http.get(url);
    if let Some((off, len)) = range {
        req = req.header("Range", format!("bytes={}-{}", off, off + len - 1));
    }
    let mut resp = req.send().await?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    if !status.is_success() {
        bail!("{url}: HTTP {status}");
    }
    if range.is_some() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        bail!("{url}: server ignored the byte range (HTTP {status})");
    }
    if resp.content_length().is_some_and(|n| n as usize > MAX_FETCH) {
        bail!("{url}: response larger than {} MB", MAX_FETCH >> 20);
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        out.extend_from_slice(&chunk);
        if out.len() > MAX_FETCH {
            bail!("{url}: response larger than {} MB", MAX_FETCH >> 20);
        }
    }
    Ok(Some(out))
}

enum Backend {
    File(PathBuf),
    Http(String),
}

pub struct PmArchive {
    backend: Backend,
    pub header: Header,
    root: Vec<Entry>,
    leaves: Mutex<HashMap<(u64, u64), Arc<Vec<Entry>>>>,
}

impl PmArchive {
    async fn read(backend: &Backend, http: &reqwest::Client, off: u64, len: u64) -> Result<Vec<u8>> {
        if len as usize > MAX_FETCH {
            bail!("PMTiles read of {len} bytes refused");
        }
        match backend {
            Backend::File(p) => {
                let p = p.clone();
                tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                    use std::os::unix::fs::FileExt;
                    let f = std::fs::File::open(&p)?;
                    let mut buf = vec![0u8; len as usize];
                    let n = f.read_at(&mut buf, off)?;
                    buf.truncate(n);
                    Ok(buf)
                })
                .await?
            }
            Backend::Http(url) => get(http, url, Some((off, len))).await?.ok_or_else(|| anyhow!("{url}: not found")),
        }
    }

    pub async fn open(spec: &str, http: &reqwest::Client) -> Result<Self> {
        let backend = if spec.starts_with("http://") || spec.starts_with("https://") {
            Backend::Http(spec.to_string())
        } else {
            Backend::File(PathBuf::from(spec))
        };
        // The spec guarantees the header and root directory fit in the first 16 KiB.
        let first = Self::read(&backend, http, 0, 16384).await.with_context(|| format!("opening {spec}"))?;
        let header = pmtiles::parse_header(&first)?;
        if header.tile_type != pmtiles::TILE_TYPE_MVT {
            bail!("{spec}: tile type {} is not vector (MVT)", header.tile_type);
        }
        let (ro, rl) = (header.root_off as usize, header.root_len as usize);
        let raw = match first.get(ro..ro.saturating_add(rl)) {
            Some(s) => s.to_vec(),
            None => Self::read(&backend, http, header.root_off, header.root_len).await?,
        };
        let root = pmtiles::parse_directory(&decompress(raw, header.internal_compression)?)?;
        Ok(Self { backend, header, root, leaves: Mutex::new(HashMap::new()) })
    }

    pub async fn tile(&self, http: &reqwest::Client, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        let id = pmtiles::tile_id(z, x, y);
        let mut dir: Arc<Vec<Entry>> = Arc::new(self.root.clone());
        for _ in 0..4 {
            let Some(e) = pmtiles::find(&dir, id) else { return Ok(None) };
            if e.run_length > 0 {
                let raw = Self::read(&self.backend, http, self.header.data_off + e.offset, e.length as u64).await?;
                return decompress(raw, self.header.tile_compression).map(Some);
            }
            let key = (self.header.leaf_off + e.offset, e.length as u64);
            let cached = self.leaves.lock().unwrap().get(&key).cloned();
            dir = match cached {
                Some(d) => d,
                None => {
                    let raw = Self::read(&self.backend, http, key.0, key.1).await?;
                    let d = Arc::new(pmtiles::parse_directory(&decompress(raw, self.header.internal_compression)?)?);
                    let mut leaves = self.leaves.lock().unwrap();
                    if leaves.len() > 256 {
                        leaves.clear();
                    }
                    leaves.insert(key, d.clone());
                    d
                }
            };
        }
        bail!("PMTiles directory nesting too deep")
    }
}

enum Kind {
    Pm(PmArchive),
    Xyz(String),
}

pub struct Source {
    kind: Kind,
    http: reqwest::Client,
    cache_dir: Option<PathBuf>,
    pub name: String,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// (lon, lat, zoom) suggested by the source.
    pub center: Option<(f64, f64, f64)>,
}

/// OpenFreeMap's public planet (OpenMapTiles schema, no key).
pub const OPENFREEMAP: &str = "https://tiles.openfreemap.org/planet";

impl Source {
    /// `spec` is a .pmtiles path or URL, an XYZ template containing {z}/{x}/{y}, or a TileJSON URL.
    pub async fn open(spec: &str) -> Result<Self> {
        let http = http_client();
        let is_http = spec.starts_with("http://") || spec.starts_with("https://");
        let cache_dir = is_http.then(|| cache_root().join(hash(spec)));
        if spec.ends_with(".pmtiles") {
            let pm = PmArchive::open(spec, &http).await?;
            let h = pm.header.clone();
            return Ok(Self {
                name: spec.rsplit('/').next().unwrap_or(spec).to_string(),
                min_zoom: h.min_zoom,
                max_zoom: h.max_zoom,
                center: Some((h.center.0, h.center.1, h.center.2 as f64)),
                kind: Kind::Pm(pm),
                http,
                cache_dir,
            });
        }
        if spec.contains("{z}") {
            return Ok(Self { name: spec.to_string(), kind: Kind::Xyz(spec.into()), http, cache_dir, min_zoom: 0, max_zoom: 14, center: None });
        }
        if !is_http {
            bail!("{spec}: expected a .pmtiles file, an XYZ URL with {{z}}/{{x}}/{{y}}, or a TileJSON URL");
        }
        let body = get(&http, spec, None).await?.ok_or_else(|| anyhow!("{spec}: not found"))?;
        let tj: serde_json::Value = serde_json::from_slice(&body).with_context(|| format!("{spec}: not TileJSON"))?;
        let template = tj["tiles"][0].as_str().ok_or_else(|| anyhow!("{spec}: TileJSON has no tiles URL"))?;
        if !template.starts_with("https://") && !template.starts_with("http://") {
            bail!("{spec}: unsupported tiles URL {template}");
        }
        let center = tj["center"].as_array().and_then(|c| Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?, c.get(2).and_then(|z| z.as_f64()).unwrap_or(10.0))));
        Ok(Self {
            name: spec.to_string(),
            min_zoom: tj["minzoom"].as_u64().unwrap_or(0) as u8,
            max_zoom: tj["maxzoom"].as_u64().unwrap_or(14).min(22) as u8,
            center,
            kind: Kind::Xyz(template.to_string()),
            http,
            cache_dir,
        })
    }

    /// Raw (decompressed) MVT bytes, or None when the source has no such tile.
    pub async fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        let path = self.cache_dir.as_ref().map(|d| d.join(format!("{z}/{x}/{y}.mvt")));
        if let Some(p) = &path
            && let Some(b) = read_fresh(p)
        {
            return Ok(if b.is_empty() { None } else { Some(b) });
        }
        let tile = match &self.kind {
            Kind::Pm(pm) => pm.tile(&self.http, z, x, y).await?,
            Kind::Xyz(t) => {
                let url = t.replace("{z}", &z.to_string()).replace("{x}", &x.to_string()).replace("{y}", &y.to_string());
                match get(&self.http, &url, None).await? {
                    Some(b) => Some(maybe_gunzip(b)?),
                    None => None,
                }
            }
        };
        if let Some(p) = &path {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, tile.as_deref().unwrap_or(&[]));
        }
        Ok(tile)
    }
}

fn cache_root() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("termtiles")
}

fn hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn read_fresh(p: &Path) -> Option<Vec<u8>> {
    let age = SystemTime::now().duration_since(std::fs::metadata(p).ok()?.modified().ok()?).ok()?;
    (age < DISK_TTL).then(|| std::fs::read(p).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gunzip_cap() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let mut e = GzEncoder::new(Vec::new(), Compression::best());
        e.write_all(&vec![0u8; MAX_INFLATE + 10]).unwrap();
        let bomb = e.finish().unwrap();
        assert!(bomb.len() < 100_000);
        assert!(maybe_gunzip(bomb).is_err());
        assert_eq!(maybe_gunzip(vec![1, 2, 3]).unwrap(), vec![1, 2, 3]);
    }
}
