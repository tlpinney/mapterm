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
pub(crate) const DISK_TTL: Duration = Duration::from_secs(30 * 86400);

/// An HTTP client with mapterm's user agent and timeouts. It follows redirects (some tile
/// and archive hosts redirect to storage URLs) but never sends a `Referer`, which would carry
/// the previous URL, query and all, to the next host. For requests that carry an API key, use
/// [`keyed_client`].
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("mapterm/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .referer(false)
        .build()
        .expect("http client")
}

/// The client for requests that carry an API key (in a header or the URL): like
/// [`http_client`], but it follows a redirect only to the same origin (scheme, host and
/// port). reqwest drops `Authorization` on a cross-origin redirect but keeps custom headers
/// such as `maxar-api-key`, so a tile server that redirects elsewhere would hand the key to
/// another host; here such a redirect comes back as an error instead.
pub fn keyed_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("mapterm/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .referer(false)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let same = attempt.previous().last().is_some_and(|prev| same_origin(prev, attempt.url()));
                if same && attempt.previous().len() < 10 { attempt.follow() } else { attempt.stop() }
            }))
            .build()
            .expect("http client")
    })
}

/// Whether two URLs share scheme, host and port (so a redirect between them stays with the
/// server the request was meant for).
pub(crate) fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
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

pub(crate) async fn get(http: &reqwest::Client, url: &str, range: Option<(u64, u64)>) -> Result<Option<Vec<u8>>> {
    get_with(http, url, range, &[]).await
}

/// [`get`] with extra request headers (e.g. an API key).
pub(crate) async fn get_with(http: &reqwest::Client, url: &str, range: Option<(u64, u64)>, headers: &[(String, String)]) -> Result<Option<Vec<u8>>> {
    get_capped(http, url, range, headers, MAX_FETCH).await
}

/// A whole data file (GeoJSON, CSV, FlatGeobuf...) by URL, up to `max` bytes: data files are
/// often far bigger than a tile.
pub(crate) async fn get_data(http: &reqwest::Client, url: &str, max: usize) -> Result<Option<Vec<u8>>> {
    get_capped(http, url, None, &[], max).await
}

/// A response body of at most `max` bytes, read as it streams in, with extra request
/// headers; `None` for 404 and 204. For API responses (search results, listings), which
/// must not grow without bound however long the request takes.
pub async fn get_limited(http: &reqwest::Client, url: &str, headers: &[(String, String)], max: usize) -> Result<Option<Vec<u8>>> {
    get_capped(http, url, None, headers, max).await
}

async fn get_capped(http: &reqwest::Client, url: &str, range: Option<(u64, u64)>, headers: &[(String, String)], max: usize) -> Result<Option<Vec<u8>>> {
    let mut req = http.get(url);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some((off, len)) = range {
        // Offsets and lengths come from archive directories: a zero or wrapping range is corrupt.
        let last = off.checked_add(len).and_then(|end| end.checked_sub(1)).filter(|_| len > 0);
        let last = last.ok_or_else(|| anyhow!("{url}: bad byte range {off}+{len}"))?;
        req = req.header("Range", format!("bytes={off}-{last}"));
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
    if resp.content_length().is_some_and(|n| n as usize > max) {
        bail!("{url}: response larger than {} MB", max >> 20);
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        out.extend_from_slice(&chunk);
        if out.len() > max {
            bail!("{url}: response larger than {} MB", max >> 20);
        }
    }
    Ok(Some(out))
}

/// Where entry `e` starts in the file, checking it lies inside its section (tile data or
/// leaf directories). Directory offsets come from the archive, so they may be anything.
fn section(off: u64, len: u64, e: &Entry) -> Result<u64> {
    let end = e.offset.checked_add(e.length as u64).filter(|&end| len == 0 || end <= len);
    match (end, off.checked_add(e.offset)) {
        (Some(_), Some(start)) => Ok(start),
        _ => bail!("PMTiles entry at {}+{} is outside its section", e.offset, e.length),
    }
}

enum Backend {
    File(PathBuf),
    Http(String),
}

/// Leaf directories already read, by (offset, length).
type LeafCache = HashMap<(u64, u64), Arc<Vec<Entry>>>;

/// A PMTiles v3 archive, local or remote (HTTP range requests), with cached leaf directories.
pub struct PmArchive {
    backend: Backend,
    /// The archive's header.
    pub header: Header,
    /// The archive's credit from its metadata (`attribution`), as given (often HTML).
    pub attribution: Option<String>,
    root: Vec<Entry>,
    leaves: Mutex<LeafCache>,
}

impl PmArchive {
    async fn read(backend: &Backend, http: &reqwest::Client, off: u64, len: u64) -> Result<Vec<u8>> {
        if len as usize > MAX_FETCH {
            bail!("PMTiles read of {len} bytes refused");
        }
        if len == 0 {
            return Ok(Vec::new());
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

    /// Open an archive of vector tiles from a path or URL.
    pub async fn open(spec: &str, http: &reqwest::Client) -> Result<Self> {
        Self::open_kind(spec, http, false).await
    }

    /// Open an archive of image tiles (PNG, JPEG or WebP) instead of vector tiles.
    pub async fn open_raster(spec: &str, http: &reqwest::Client) -> Result<Self> {
        Self::open_kind(spec, http, true).await
    }

    async fn open_kind(spec: &str, http: &reqwest::Client, raster: bool) -> Result<Self> {
        let backend =
            if spec.starts_with("http://") || spec.starts_with("https://") { Backend::Http(spec.to_string()) } else { Backend::File(PathBuf::from(spec)) };
        // The spec guarantees the header and root directory fit in the first 16 KiB.
        let first = Self::read(&backend, http, 0, 16384).await.with_context(|| format!("opening {spec}"))?;
        let header = pmtiles::parse_header(&first)?;
        let is_image = matches!(header.tile_type, pmtiles::TILE_TYPE_PNG | pmtiles::TILE_TYPE_JPEG | pmtiles::TILE_TYPE_WEBP);
        if raster && !is_image {
            bail!("{spec}: tile type {} is not PNG, JPEG or WebP images", header.tile_type);
        }
        if !raster && header.tile_type != pmtiles::TILE_TYPE_MVT {
            let hint = if is_image { " (it holds images: use it with --raster)" } else { "" };
            bail!("{spec}: tile type {} is not vector (MVT){hint}", header.tile_type);
        }
        let (ro, rl) = (header.root_off as usize, header.root_len as usize);
        let raw = match first.get(ro..ro.saturating_add(rl)) {
            Some(s) => s.to_vec(),
            None => Self::read(&backend, http, header.root_off, header.root_len).await?,
        };
        let root = pmtiles::parse_directory(&decompress(raw, header.internal_compression)?)?;
        // The metadata JSON holds the credit; skip it when absent, huge or unreadable.
        let attribution = if header.meta_len > 0 && header.meta_len <= 1 << 20 {
            let (mo, ml) = (header.meta_off as usize, header.meta_len as usize);
            let raw = match first.get(mo..mo.saturating_add(ml)) {
                Some(s) => Some(s.to_vec()),
                None => Self::read(&backend, http, header.meta_off, header.meta_len).await.ok(),
            };
            raw.and_then(|r| decompress(r, header.internal_compression).ok())
                .and_then(|j| serde_json::from_slice::<serde_json::Value>(&j).ok())
                .and_then(|m| m["attribution"].as_str().map(str::to_string))
                .filter(|a| !a.trim().is_empty())
        } else {
            None
        };
        Ok(Self { backend, header, attribution, root, leaves: Mutex::new(HashMap::new()) })
    }

    /// A tile's bytes (decompressed), or `None` if the archive has none there.
    pub async fn tile(&self, http: &reqwest::Client, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        let id = pmtiles::tile_id(z, x, y);
        let mut dir: Arc<Vec<Entry>> = Arc::new(self.root.clone());
        for _ in 0..4 {
            let Some(e) = pmtiles::find(&dir, id) else { return Ok(None) };
            if e.run_length > 0 {
                let raw = Self::read(&self.backend, http, section(self.header.data_off, self.header.data_len, &e)?, e.length as u64).await?;
                return decompress(raw, self.header.tile_compression).map(Some);
            }
            let key = (section(self.header.leaf_off, self.header.leaf_len, &e)?, e.length as u64);
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
    Pm(Box<PmArchive>),
    Xyz(String),
}

/// A vector tile source: PMTiles (file or URL), an XYZ URL template, or TileJSON; tiles are cached on disk for 30 days when fetched over HTTP.
pub struct Source {
    kind: Kind,
    http: reqwest::Client,
    cache_dir: Option<PathBuf>,
    /// A short name (the file name or URL).
    pub name: String,
    /// Lowest zoom with tiles.
    pub min_zoom: u8,
    /// Highest zoom with tiles.
    pub max_zoom: u8,
    /// (lon, lat, zoom) suggested by the source.
    pub center: Option<(f64, f64, f64)>,
    /// The source's credit (TileJSON or PMTiles metadata `attribution`, often HTML; see
    /// [`plain_attribution`]).
    pub attribution: Option<String>,
}

/// Attribution HTML as plain text for a terminal: tags dropped, entities decoded,
/// whitespace collapsed. `<a href="...">&copy; OpenMapTiles</a>` -> `© OpenMapTiles`.
pub fn plain_attribution(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    for (entity, ch) in [("&copy;", "©"), ("&#169;", "©"), ("&nbsp;", " "), ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&amp;", "&")] {
        text = text.replace(entity, ch);
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// OpenFreeMap's public planet (OpenMapTiles schema, no key).
pub const OPENFREEMAP: &str = "https://tiles.openfreemap.org/planet";

impl Source {
    /// An XYZ source that is never fetched, for tests.
    #[cfg(test)]
    pub(crate) fn xyz_for_tests() -> Self {
        let t = "https://tiles.invalid/{z}/{x}/{y}.pbf";
        Self { name: t.into(), kind: Kind::Xyz(t.into()), http: http_client(), cache_dir: None, min_zoom: 0, max_zoom: 14, center: None, attribution: None }
    }

    /// `spec` is a .pmtiles path or URL, an XYZ template containing {z}/{x}/{y}, or a TileJSON URL.
    pub async fn open(spec: &str) -> Result<Self> {
        Self::open_inner(spec).await.map_err(crate::mapbox::redact)
    }

    async fn open_inner(spec: &str) -> Result<Self> {
        // Mapbox URLs carry the access token: no redirects to other hosts.
        let http = if crate::mapbox::is_mapbox_host(spec) { keyed_client().clone() } else { http_client() };
        let is_http = spec.starts_with("http://") || spec.starts_with("https://");
        let cache_dir = is_http.then(|| cache_root().join(hash(spec)));
        if spec.ends_with(".pmtiles") {
            let pm = PmArchive::open(spec, &http).await?;
            let h = pm.header.clone();
            let attribution = pm.attribution.clone();
            return Ok(Self {
                attribution,
                name: spec.rsplit('/').next().unwrap_or(spec).to_string(),
                min_zoom: h.min_zoom,
                max_zoom: h.max_zoom,
                center: Some((h.center.0, h.center.1, h.center.2 as f64)),
                kind: Kind::Pm(Box::new(pm)),
                http,
                cache_dir,
            });
        }
        if spec.contains("{z}") {
            return Ok(Self {
                name: spec.to_string(),
                kind: Kind::Xyz(spec.into()),
                http,
                cache_dir,
                min_zoom: 0,
                max_zoom: 14,
                center: None,
                attribution: None,
            });
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
            attribution: tj["attribution"].as_str().filter(|a| !a.trim().is_empty()).map(str::to_string),
            kind: Kind::Xyz(template.to_string()),
            http,
            cache_dir,
        })
    }

    /// Raw (decompressed) MVT bytes, or None when the source has no such tile.
    pub async fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        // Mapbox's tiles: cached by their HTTP headers, as received (gzipped).
        if let (Kind::Xyz(t), Some(dir)) = (&self.kind, &self.cache_dir)
            && crate::mapbox::is_mapbox_host(t)
        {
            let url = t.replace("{z}", &z.to_string()).replace("{x}", &x.to_string()).replace("{y}", &y.to_string());
            let file = dir.join(format!("{z}/{x}/{y}.pbf"));
            let bytes = fetch_by_headers(&self.http, &file, &url, &[], true).await.map_err(crate::mapbox::redact)?;
            return bytes.map(maybe_gunzip).transpose();
        }
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

pub(crate) fn cache_root() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("mapterm")
}

pub(crate) fn hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// A response with what HTTP caching needs (`Cache-Control`, `ETag`), for providers whose
/// terms say to cache by those headers (Google's Map Tiles API).
pub(crate) struct Revalidated {
    /// None for a 304 (the cached copy is still good) or a 404/204 (no tile).
    pub bytes: Option<Vec<u8>>,
    pub not_modified: bool,
    pub policy: CachePolicy,
    pub etag: Option<String>,
}

/// What a `Cache-Control` header allows.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct CachePolicy {
    /// Seconds the response may be used without asking again (0: ask every time).
    pub max_age: u64,
    /// `no-store` (or `no-cache`): keep nothing on disk.
    pub no_store: bool,
}

/// Parse `Cache-Control`. `private` is fine (this is the user's own cache); `no-cache` and
/// `no-store` keep nothing; `must-revalidate` is what happens after `max-age` anyway.
pub(crate) fn cache_policy(header: Option<&str>) -> CachePolicy {
    let mut p = CachePolicy::default();
    for d in header.unwrap_or("").split(',').map(|d| d.trim().to_ascii_lowercase()) {
        if d == "no-store" || d == "no-cache" {
            p.no_store = true;
        } else if let Some(v) = d.strip_prefix("max-age=") {
            p.max_age = v.trim_matches('"').parse().unwrap_or(0);
        }
    }
    p
}

/// GET with `If-None-Match` when there's a cached `etag`, returning the caching headers.
/// Same size cap as other fetches.
pub(crate) async fn get_revalidated(http: &reqwest::Client, url: &str, headers: &[(String, String)], etag: Option<&str>) -> Result<Revalidated> {
    let mut req = http.get(url);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    let mut resp = req.send().await?;
    let status = resp.status();
    let h = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    let (policy, etag) = (cache_policy(h("cache-control").as_deref()), h("etag"));
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Revalidated { bytes: None, not_modified: true, policy, etag });
    }
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::NO_CONTENT {
        return Ok(Revalidated { bytes: None, not_modified: false, policy, etag });
    }
    if !status.is_success() {
        bail!("{url}: HTTP {status}");
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
    Ok(Revalidated { bytes: Some(out), not_modified: false, policy, etag })
}

/// A tile cached by its HTTP headers at `file` (with a `.meta` beside it: "expires etag"):
/// used as is until its `max-age` runs out (at most 30 days), then asked for again with
/// `If-None-Match`, so an unchanged tile comes back as a 304 and is kept. `no-store` and
/// `no-cache` responses aren't kept. Bytes are stored as received. With `cache` off it
/// only fetches.
pub(crate) async fn fetch_by_headers(http: &reqwest::Client, file: &Path, url: &str, headers: &[(String, String)], cache: bool) -> Result<Option<Vec<u8>>> {
    let meta = file.with_extension("meta");
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let cached = if cache { read_meta(&meta).zip(std::fs::read(file).ok()) } else { None };
    if let Some(((expires, _), bytes)) = &cached
        && now < *expires
    {
        return Ok(Some(bytes.clone()));
    }
    let etag = cached.as_ref().and_then(|((_, e), _)| e.clone());
    let r = get_revalidated(http, url, headers, etag.as_deref()).await?;
    let keep = r.policy.max_age.min(DISK_TTL.as_secs());
    if r.not_modified {
        let Some(((_, old_etag), bytes)) = cached else { bail!("{url}: HTTP 304 without a cached tile") };
        if cache && !r.policy.no_store {
            let _ = write_meta(&meta, now + keep, r.etag.as_deref().or(old_etag.as_deref()));
        }
        return Ok(Some(bytes));
    }
    if cache {
        match &r.bytes {
            Some(b) if !r.policy.no_store && keep > 0 => {
                if let Some(d) = file.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                if std::fs::write(file, b).is_ok() {
                    let _ = write_meta(&meta, now + keep, r.etag.as_deref());
                }
            }
            _ => {
                // Not to be kept (or no tile): drop any older copy.
                let _ = std::fs::remove_file(file);
                let _ = std::fs::remove_file(&meta);
            }
        }
    }
    Ok(r.bytes)
}

/// "expires etag" from a `.meta` file (etag may be absent).
pub(crate) fn read_meta(p: &Path) -> Option<(u64, Option<String>)> {
    let s = std::fs::read_to_string(p).ok()?;
    let mut it = s.trim().splitn(2, ' ');
    let expires = it.next()?.parse().ok()?;
    Some((expires, it.next().map(str::to_string).filter(|e| !e.is_empty())))
}

pub(crate) fn write_meta(p: &Path, expires: u64, etag: Option<&str>) -> std::io::Result<()> {
    std::fs::write(p, format!("{expires} {}", etag.unwrap_or("")))
}

pub(crate) fn read_fresh(p: &Path) -> Option<Vec<u8>> {
    let age = SystemTime::now().duration_since(std::fs::metadata(p).ok()?.modified().ok()?).ok()?;
    (age < DISK_TTL).then(|| std::fs::read(p).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_outside_their_section_are_refused() {
        let e = |offset, length| Entry { tile_id: 0, offset, length, run_length: 1 };
        assert_eq!(section(100, 50, &e(10, 40)).unwrap(), 110);
        assert!(section(100, 50, &e(10, 41)).is_err(), "past the section");
        assert!(section(100, 50, &e(u64::MAX, 1)).is_err(), "offset + length wraps");
        assert!(section(u64::MAX, 0, &e(1, 1)).is_err(), "section start + offset wraps");
    }

    #[tokio::test]
    async fn bad_byte_ranges_are_refused_before_sending() {
        let http = http_client();
        for range in [(10, 0), (u64::MAX, 2)] {
            let err = get(&http, "http://127.0.0.1:9/never", Some(range)).await.unwrap_err();
            assert!(format!("{err:#}").contains("bad byte range"), "{err:#}");
        }
    }

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

#[cfg(test)]
mod pm_file_tests {

    #[test]
    fn keyed_requests_only_follow_same_origin_redirects() {
        let u = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(super::same_origin(&u("https://api.maxar.com/a"), &u("https://api.maxar.com/b?x=1")));
        assert!(super::same_origin(&u("https://t.example/a"), &u("https://t.example:443/b")));
        assert!(!super::same_origin(&u("https://t.example/a"), &u("https://evil.example/a")));
        assert!(!super::same_origin(&u("https://t.example/a"), &u("http://t.example/a")), "no downgrade");
        assert!(!super::same_origin(&u("http://127.0.0.1:8201/a"), &u("http://127.0.0.1:8202/a")), "other port");
    }

    /// Reads a local raster PMTiles named by MAPTERM_TEST_PMTILES (skipped when unset).
    #[tokio::test]
    async fn reads_local_raster_pmtiles() {
        let Ok(path) = std::env::var("MAPTERM_TEST_PMTILES") else { return };
        let http = super::http_client();
        let pm = super::PmArchive::open_raster(&path, &http).await.expect("open");
        eprintln!(
            "header: type {} comp {} internal {} zoom {}..{} root {}+{}",
            pm.header.tile_type,
            pm.header.tile_compression,
            pm.header.internal_compression,
            pm.header.min_zoom,
            pm.header.max_zoom,
            pm.header.root_off,
            pm.header.root_len
        );
        let t = pm.tile(&http, 15, 9372, 12536).await.expect("read");
        eprintln!("tile 15/9372/12536 -> {:?} bytes", t.as_ref().map(|b| b.len()));
        assert!(t.is_some());
    }

    #[test]
    fn attribution_html_to_text() {
        let ofm = r#"<a href="https://openfreemap.org" target="_blank">OpenFreeMap</a> <a href="https://www.openmaptiles.org/" target="_blank">&copy; OpenMapTiles</a> Data from <a href="https://www.openstreetmap.org/copyright" target="_blank">OpenStreetMap</a>"#;
        assert_eq!(crate::source::plain_attribution(ofm), "OpenFreeMap © OpenMapTiles Data from OpenStreetMap");
        assert_eq!(crate::source::plain_attribution("© OpenStreetMap contributors"), "© OpenStreetMap contributors");
        assert_eq!(crate::source::plain_attribution("<a href='x'>Protomaps</a> &amp; &#169; OSM"), "Protomaps & © OSM");
        assert_eq!(crate::source::plain_attribution("  "), "");
    }
}
