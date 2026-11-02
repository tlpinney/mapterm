//! Raster (image) tile layers: XYZ or TMS URLs of PNG/JPEG tiles, drawn underneath the vector
//! map. Tiles are fetched and cached like vector tiles, decoded once, and laid out by giving
//! every screen cell to exactly one tile, so blocks of cells can be drawn from a cropped tile
//! (kitty graphics) or sampled into cell colors (any terminal).

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use anyhow::{Context, Result, bail};
use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};

use crate::{
    render::{Camera, to_world},
    source::{PmArchive, cache_root, get_with, hash, http_client, read_fresh},
    store::TileId,
};

/// An image tile source: an XYZ/TMS URL template (with optional key, headers and parameters) or a PMTiles archive of images, with a disk cache.
pub struct RasterSource {
    template: String,
    tms: bool,
    http: reqwest::Client,
    cache_dir: std::path::PathBuf,
    /// Request headers; `{key}` in a value is replaced by the key.
    headers: Vec<(String, String)>,
    /// API key for `{key}` in the URL or headers. Never shown: it is blanked out of errors.
    key: Option<String>,
    /// Keep tiles on disk (30 days). Off by default for keyed commercial providers, whose
    /// licenses often limit storing tiles; on for Google, whose tiles are kept only as long
    /// as their `Cache-Control` headers allow.
    pub cache: bool,
    /// Cache by each response's `Cache-Control` and `ETag` (up to 30 days) instead of a
    /// flat 30 days: what Google's Map Tiles API terms ask for.
    by_headers: bool,
    /// Tile width and height in pixels (512 for Retina "@2x" tiles).
    pub tile_px: u32,
    /// Values for other `{name}` placeholders in the URL (e.g. a Planet mosaic).
    params: Vec<(String, String)>,
    /// A PMTiles archive of images instead of a URL template.
    pm: Option<Arc<PmArchive>>,
    /// Highest zoom with tiles.
    pub max_zoom: u8,
    /// Lowest zoom to request tiles at (0 for most): below it nothing is fetched, for
    /// services that only render when zoomed in, such as Planetary Computer mosaics.
    pub min_zoom: u8,
    /// A short name for the status bar.
    pub name: String,
    /// Credit to show for the imagery (known providers are filled in automatically).
    pub attribution: String,
}

/// A named raster provider: `--raster vantor` instead of a URL.
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    /// The name used with `--raster`.
    pub name: &'static str,
    /// What it is.
    pub about: &'static str,
    /// The tile URL template.
    pub template: &'static str,
    /// Request headers, with `{key}` where the key goes.
    pub headers: &'static [(&'static str, &'static str)],
    /// Environment variables checked for the key, in order (none: no key needed).
    pub key_env: &'static [&'static str],
    /// Other `{name}` placeholders in the URL: (name, environment variable, default). An empty
    /// default means it must be given; "yesterday" means the previous UTC day.
    pub params: &'static [(&'static str, &'static str, &'static str)],
    /// Credit to show.
    pub attribution: &'static str,
    /// Tile size in pixels.
    pub tile_px: u32,
    /// Highest zoom with tiles.
    pub max_zoom: u8,
    /// Whether tiles may be kept on disk.
    pub cache: bool,
}

/// The named providers (`--raster NAME`).
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "osm",
        about: "OpenStreetMap standard map (light use only, per OSM's tile policy)",
        template: "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
        headers: &[],
        key_env: &[],
        params: &[],
        attribution: "© OpenStreetMap contributors",
        tile_px: 256,
        max_zoom: 19,
        cache: true,
    },
    Preset {
        name: "esri",
        about: "Esri World Imagery, global satellite (Esri's terms apply)",
        template: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        headers: &[],
        key_env: &[],
        params: &[],
        attribution: "Esri, Maxar, Earthstar Geographics",
        tile_px: 256,
        max_zoom: 19,
        cache: true,
    },
    Preset {
        name: "usgs",
        about: "USGS National Map imagery, United States only (public domain)",
        template: "https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer/tile/{z}/{y}/{x}",
        headers: &[],
        key_env: &[],
        params: &[],
        attribution: "USGS The National Map",
        tile_px: 256,
        max_zoom: 16,
        cache: true,
    },
    Preset {
        name: "eox",
        about: "Sentinel-2 cloudless 2024 by EOX, global at ~10 m (CC BY-NC-SA 4.0)",
        template: "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2024_3857/default/g/{z}/{y}/{x}.jpg",
        headers: &[],
        key_env: &[],
        params: &[],
        attribution: "Sentinel-2 cloudless 2024 by EOX (modified Copernicus Sentinel data)",
        tile_px: 256,
        max_zoom: 14,
        cache: true,
    },
    Preset {
        name: "esri-light",
        about: "Esri Light Gray Canvas, a quiet light background for overlays (Esri's terms apply)",
        template: "https://server.arcgisonline.com/ArcGIS/rest/services/Canvas/World_Light_Gray_Base/MapServer/tile/{z}/{y}/{x}",
        headers: &[],
        key_env: &[],
        params: &[],
        attribution: "Esri, HERE, Garmin, © OpenStreetMap contributors",
        tile_px: 256,
        max_zoom: 16,
        cache: true,
    },
    Preset {
        name: "esri-dark",
        about: "Esri Dark Gray Canvas, a quiet dark background for overlays (Esri's terms apply)",
        template: "https://server.arcgisonline.com/ArcGIS/rest/services/Canvas/World_Dark_Gray_Base/MapServer/tile/{z}/{y}/{x}",
        headers: &[],
        key_env: &[],
        params: &[],
        attribution: "Esri, HERE, Garmin, © OpenStreetMap contributors",
        tile_px: 256,
        max_zoom: 16,
        cache: true,
    },
    Preset {
        name: "gibs",
        about: "NASA GIBS daily satellite imagery, ~250 m (MODIS true color; --date, layer=...)",
        template: "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/{layer}/default/{date}/GoogleMapsCompatible_Level9/{z}/{y}/{x}.jpg",
        headers: &[],
        key_env: &[],
        params: &[("layer", "GIBS_LAYER", "MODIS_Terra_CorrectedReflectance_TrueColor"), ("date", "GIBS_DATE", "yesterday")],
        attribution: "NASA EOSDIS GIBS",
        tile_px: 256,
        max_zoom: 9,
        cache: true,
    },
    Preset {
        name: "naip",
        about: "USDA NAIP aerial photos, US only, ~60 cm, public domain; from zoom 12 (Microsoft Planetary Computer)",
        template: "https://planetarycomputer.microsoft.com/api/data/v1/mosaic/{search}/tiles/WebMercatorQuad/{z}/{x}/{y}?collection=naip&assets=image&asset_bidx=image%7C1,2,3&format=png",
        headers: &[],
        key_env: &[],
        params: &[],
        attribution: "USDA NAIP via Microsoft Planetary Computer",
        tile_px: 256,
        max_zoom: 18,
        cache: true,
    },
    Preset {
        name: "sentinel2",
        about: "Sentinel-2 latest clear scenes, global at ~10 m; from zoom 9 (Microsoft Planetary Computer)",
        template: "https://planetarycomputer.microsoft.com/api/data/v1/mosaic/{search}/tiles/WebMercatorQuad/{z}/{x}/{y}?collection=sentinel-2-l2a&assets=B04&assets=B03&assets=B02&nodata=0&color_formula=Gamma+RGB+3.2+Saturation+0.8+Sigmoidal+RGB+25+0.35&format=png",
        headers: &[],
        key_env: &[],
        params: &[],
        attribution: "Contains modified Copernicus Sentinel data, via Microsoft Planetary Computer",
        tile_px: 256,
        max_zoom: 14,
        cache: true,
    },
    Preset {
        name: "vantor",
        about: "Vantor (Maxar) Vivid basemap via the Vantor Hub API, needs an API key",
        template: "https://api.maxar.com/basemaps/v1/ogc/gwc/service/wmts?service=WMTS&request=GetTile&version=1.0.0&layer=Maxar:Imagery&tileMatrixSet=EPSG:3857&tileMatrix=EPSG:3857:{z}&tileRow={y}&tileCol={x}&format=image/jpeg",
        headers: &[("maxar-api-key", "{key}")],
        key_env: &["VANTOR_API_KEY", "MAXAR_API_KEY"],
        params: &[],
        attribution: "Vantor (Maxar) Vivid",
        tile_px: 256,
        max_zoom: 21,
        cache: false,
    },
    Preset {
        name: "planet",
        about: "Planet basemaps (your subscription's mosaics), needs an API key and a mosaic",
        template: "https://tiles.planet.com/basemaps/v1/planet-tiles/{mosaic}/gmap/{z}/{x}/{y}.png?api_key={key}",
        headers: &[],
        key_env: &["PL_API_KEY", "PLANET_API_KEY"],
        params: &[("mosaic", "PLANET_MOSAIC", "")],
        attribution: "Imagery © Planet Labs PBC",
        tile_px: 256,
        max_zoom: 18,
        cache: false,
    },
    Preset {
        name: "mapbox",
        about: "Mapbox Satellite (512 px Retina tiles), needs an access token",
        template: "https://api.mapbox.com/v4/mapbox.satellite/{z}/{x}/{y}@2x.jpg90?access_token={key}",
        headers: &[],
        key_env: &["MAPBOX_ACCESS_TOKEN"],
        params: &[],
        attribution: "© Mapbox © Maxar",
        tile_px: 512,
        max_zoom: 22,
        cache: true,
    },
    Preset {
        name: "maptiler",
        about: "MapTiler Satellite, needs an API key",
        template: "https://api.maptiler.com/tiles/satellite-v2/{z}/{x}/{y}.jpg?key={key}",
        headers: &[],
        key_env: &["MAPTILER_KEY"],
        params: &[],
        attribution: "© MapTiler © OpenStreetMap contributors",
        tile_px: 512,
        max_zoom: 20,
        cache: false,
    },
    Preset {
        name: "google",
        about: "Google Maps satellite (Map Tiles API 2D tiles), needs an API key; billed per tile",
        template: GOOGLE_2D,
        headers: &[],
        key_env: &["GOOGLE_MAP_TILES_KEY"],
        params: &[("maptype", "GOOGLE_MAP_TYPE", "satellite")],
        attribution: "Google Maps",
        tile_px: 256,
        max_zoom: 22,
        cache: true,
    },
    Preset {
        name: "google-roadmap",
        about: "Google Maps road map (Map Tiles API 2D tiles), needs an API key; billed per tile",
        template: GOOGLE_2D,
        headers: &[],
        key_env: &["GOOGLE_MAP_TILES_KEY"],
        params: &[("maptype", "", "roadmap")],
        attribution: "Google Maps",
        tile_px: 256,
        max_zoom: 22,
        cache: true,
    },
    Preset {
        name: "google-terrain",
        about: "Google Maps terrain (Map Tiles API 2D tiles), needs an API key; billed per tile",
        template: GOOGLE_2D,
        headers: &[],
        key_env: &["GOOGLE_MAP_TILES_KEY"],
        params: &[("maptype", "", "terrain")],
        attribution: "Google Maps",
        tile_px: 256,
        max_zoom: 22,
        cache: true,
    },
];

/// Google Map Tiles API 2D tiles. `{session}` comes from [`RasterSource::start_session`].
const GOOGLE_2D: &str = "https://tile.googleapis.com/v1/2dtiles/{z}/{x}/{y}?session={session}&key={key}";

/// Imagery from several providers by zoom (`--raster planetary`), drawn bottom to top.
#[derive(Clone, Copy, Debug)]
pub struct Stack {
    /// The name used with `--raster`.
    pub name: &'static str,
    /// What it is.
    pub about: &'static str,
    /// Its layers, bottom first.
    pub layers: &'static [StackLayer],
}

/// One provider in a [`Stack`].
#[derive(Clone, Copy, Debug)]
pub struct StackLayer {
    /// The provider ([`preset`]).
    pub preset: &'static str,
    /// The layer shows from this zoom on (0: always; the provider's own lowest zoom still
    /// applies, and the viewer says to zoom in below it).
    pub min_zoom: u8,
    /// Fetch tiles up to this zoom at most, scaled up past it (coarse imagery gains
    /// nothing from finer tiles).
    pub max_zoom: Option<u8>,
}

/// The stacks `--raster` knows.
pub const STACKS: &[Stack] = &[Stack {
    name: "planetary",
    about: "Microsoft Planetary Computer, free: Sentinel-2 (global, ~10 m) from zoom 9, with NAIP aerial photos (US, ~60 cm) on top from zoom 12; Sentinel-2 shows where NAIP has none",
    layers: &[StackLayer { preset: "sentinel2", min_zoom: 0, max_zoom: Some(14) }, StackLayer { preset: "naip", min_zoom: 12, max_zoom: None }],
}];

/// A stack by name (any case).
pub fn stack(name: &str) -> Option<&'static Stack> {
    STACKS.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

/// A provider by name (any case).
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name) || (name.eq_ignore_ascii_case("maxar") && p.name == "vantor"))
}

/// The previous day in UTC as YYYY-MM-DD (GIBS's newest complete day).
pub fn yesterday_utc() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86400) as i64 - 1;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Find a provider's key: a command's output (e.g. a password manager's CLI) if given,
/// else the first of `env` that is set.
pub fn find_key(key_cmd: Option<&str>, env: &[&str]) -> Result<Option<String>> {
    if let Some(cmd) = key_cmd {
        let out = std::process::Command::new("sh").args(["-c", cmd]).output().context("running the key command")?;
        if !out.status.success() {
            bail!("the key command failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let key = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string();
        if key.is_empty() {
            bail!("the key command printed nothing");
        }
        return Ok(Some(key));
    }
    Ok(env.iter().find_map(|v| std::env::var(v).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())))
}

/// The credit that well-known tile providers ask for, from the tile URL's host.
pub fn known_attribution(template: &str) -> Option<&'static str> {
    let host = template.split('/').nth(2).unwrap_or("");
    if host.ends_with("arcgisonline.com") || host.ends_with("arcgis.com") {
        Some("Esri, Maxar, Earthstar Geographics")
    } else if host.ends_with("nationalmap.gov") {
        Some("USGS The National Map")
    } else if host.ends_with("openstreetmap.org") || host.ends_with("openstreetmap.fr") {
        Some("© OpenStreetMap contributors")
    } else if host.ends_with("earthdata.nasa.gov") {
        Some("NASA EOSDIS GIBS")
    } else if host.ends_with("s2maps-tiles.eu") {
        Some("Sentinel-2 cloudless by EOX")
    } else {
        None
    }
}

impl RasterSource {
    /// `template` has `{z}`, `{x}`, and `{y}` (or `{-y}` for TMS rows counted from the bottom).
    /// `tms` flips `{y}` the same way, for TMS services that use plain `{y}`.
    pub fn new(template: &str, tms: bool, max_zoom: u8) -> Result<Self> {
        if !(template.starts_with("https://") || template.starts_with("http://")) {
            bail!("{template}: raster layers need an http(s) URL template");
        }
        if !template.contains("{z}") || !template.contains("{x}") || !(template.contains("{y}") || template.contains("{-y}")) {
            bail!("{template}: the URL needs {{z}}, {{x}} and {{y}} (or {{-y}} for TMS)");
        }
        Ok(Self {
            template: template.to_string(),
            tms,
            http: http_client(),
            // Keyed by the template, which holds `{key}` rather than the key itself.
            cache_dir: cache_root().join("raster").join(hash(template)),
            headers: Vec::new(),
            key: None,
            cache: true,
            by_headers: false,
            tile_px: 256,
            params: Vec::new(),
            pm: None,
            max_zoom,
            min_zoom: 0,
            name: template.split('/').nth(2).unwrap_or(template).to_string(),
            attribution: known_attribution(template).unwrap_or_default().to_string(),
        })
    }

    /// A source for a named provider. Fails with a hint if it needs a key and none was given.
    pub fn from_preset(p: &Preset, key: Option<String>) -> Result<Self> {
        if !p.key_env.is_empty() && key.is_none() {
            bail!("{} needs a key: set {} or pass --raster-key-cmd with a command that prints it", p.name, p.key_env.join(" or "));
        }
        let mut s = Self::new(p.template, false, p.max_zoom)?;
        for (name, env, default) in p.params {
            let value = std::env::var(env).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).or_else(|| match *default {
                "" => None,
                "yesterday" => Some(yesterday_utc()),
                d => Some(d.to_string()),
            });
            if let Some(v) = value {
                s.params.push((name.to_string(), v));
            }
        }
        s.headers = p.headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        s.key = key;
        s.cache = p.cache;
        s.tile_px = p.tile_px;
        s.name = p.name.to_string();
        s.attribution = p.attribution.to_string();
        // Mapbox's tiles are kept as long as their headers say (12 hours, as of 2026), and so
        // are the Planetary Computer's (an hour): its "latest" mosaics change.
        s.by_headers = crate::mapbox::is_mapbox_host(p.template) || crate::planetary::is_planetary(p.template);
        if let Some(c) = crate::planetary::collection(p.template).filter(|_| crate::planetary::is_planetary(p.template)) {
            s.min_zoom = crate::planetary::min_zoom(c);
        }
        if s.is_google() {
            // Kept only as long as each tile's headers say (a day, as of 2026), and apart per
            // map type: the URL template is the same for all of them.
            s.by_headers = true;
            let map_type = s.param("maptype").unwrap_or("satellite").to_string();
            s.cache_dir = cache_root().join("raster").join(hash(&format!("{}#{map_type}", p.template)));
        }
        Ok(s)
    }

    /// A layer from a PMTiles archive of PNG, JPEG or WebP tiles (a file or an http(s) URL).
    pub async fn from_pmtiles(spec: &str) -> Result<Self> {
        let http = http_client();
        let pm = PmArchive::open_raster(spec, &http).await?;
        let name = spec.rsplit('/').next().unwrap_or(spec).to_string();
        let attribution = pm.attribution.as_deref().map(crate::source::plain_attribution).unwrap_or_default();
        Ok(Self {
            template: String::new(),
            tms: false,
            http,
            cache_dir: cache_root().join("raster").join(hash(spec)),
            headers: Vec::new(),
            key: None,
            // Local files need no cache; remote archives are read with range requests.
            cache: false,
            by_headers: false,
            tile_px: 256,
            params: Vec::new(),
            max_zoom: pm.header.max_zoom,
            min_zoom: 0,
            pm: Some(Arc::new(pm)),
            name,
            attribution,
        })
    }

    /// Whether this is a Google Map Tiles API source (needs a session, and Google's credits).
    pub fn is_google(&self) -> bool {
        self.template.starts_with("https://tile.googleapis.com/")
    }

    fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// Get a session for sources that need one before the first tile: Google's Map Tiles API
    /// (`createSession` with the `maptype` parameter, which fills `{session}` and the tile
    /// size), and Planetary Computer mosaics (a registered search, which fills `{search}`).
    /// Does nothing for other sources. Sessions last about two weeks; this asks for a new one
    /// each run.
    pub async fn start_session(&mut self) -> Result<()> {
        if crate::planetary::is_planetary(&self.template) && self.param("search").is_none() {
            let collection = crate::planetary::collection(&self.template).context("Planetary Computer URL without a collection")?.to_string();
            let id = crate::planetary::register(&self.http, &collection).await?;
            self.set_param("search", &id);
            return Ok(());
        }
        if !self.is_google() || self.param("session").is_some() {
            return Ok(());
        }
        let key = self.key.clone().context("Google Map Tiles need a key")?;
        let map_type = self.param("maptype").unwrap_or("satellite").to_string();
        let mut body = serde_json::json!({ "mapType": map_type, "language": "en-US", "region": "US" });
        match map_type.as_str() {
            "satellite" | "roadmap" => {}
            // Terrain only comes with the road layer on top.
            "terrain" => body["layerTypes"] = serde_json::json!(["layerRoadmap"]),
            other => bail!("Google map type {other:?}: use satellite, roadmap or terrain"),
        }
        let url = format!("https://tile.googleapis.com/v1/createSession?key={key}");
        let resp = crate::source::keyed_client()
            .post(&url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| self.redact(e.into()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| self.redact(e.into()))?;
        if !status.is_success() {
            bail!("Google createSession: HTTP {status}: {}", self.redact(anyhow::anyhow!(text.chars().take(300).collect::<String>())));
        }
        let v: serde_json::Value = serde_json::from_str(&text).context("Google createSession: not JSON")?;
        let session = v["session"].as_str().context("Google createSession: no session in the reply")?;
        self.set_param("session", session);
        if let Some(w) = v["tileWidth"].as_u64().filter(|w| (64..=1024).contains(w)) {
            self.tile_px = w as u32;
        }
        self.name = format!("google {map_type}");
        Ok(())
    }

    /// Google's credits for a view, from its viewport endpoint: "Google Maps · ...". They
    /// change with the imagery on screen, so ask again when the view moves (and show them with
    /// [`crate::Map::set_attribution`]). `bounds` is (north, south, east, west) in degrees.
    pub async fn google_credits(&self, zoom: u8, bounds: (f64, f64, f64, f64)) -> Result<String> {
        let (Some(session), Some(key)) = (self.param("session").map(str::to_string), self.key.clone()) else {
            bail!("no Google session yet");
        };
        let (n, s, e, w) = bounds;
        let url = format!("https://tile.googleapis.com/tile/v1/viewport?session={session}&key={key}&zoom={zoom}&north={n}&south={s}&east={e}&west={w}");
        let bytes = get_with(crate::source::keyed_client(), &url, None, &[]).await.map_err(|e| self.redact(e))?.unwrap_or_default();
        let v: serde_json::Value = serde_json::from_slice(&bytes).context("Google viewport info: not JSON")?;
        let copyright = v["copyright"].as_str().unwrap_or("").trim();
        Ok(if copyright.is_empty() { "Google Maps".to_string() } else { format!("Google Maps · {copyright}") })
    }

    /// Add a request header (`Name: value`; `{key}` in the value is replaced by the key).
    pub fn add_header(&mut self, line: &str) -> Result<()> {
        let (k, v) = line.split_once(':').context("headers look like `Name: value`")?;
        self.headers.push((k.trim().to_string(), v.trim().to_string()));
        Ok(())
    }

    /// Set (or clear) the API key for `{key}`.
    pub fn set_key(&mut self, key: Option<String>) {
        self.key = key;
    }

    /// Fill a `{name}` placeholder in the URL (e.g. `mosaic`); later calls win.
    pub fn set_param(&mut self, name: &str, value: &str) {
        self.params.retain(|(n, _)| n != name);
        self.params.push((name.to_string(), value.to_string()));
    }

    /// Placeholders still unfilled, other than z/x/y/-y/key.
    pub fn missing_params(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = self.template.as_str();
        while let Some(i) = rest.find('{') {
            let Some(j) = rest[i..].find('}') else { break };
            let name = &rest[i + 1..i + j];
            if !["z", "x", "y", "-y", "key"].contains(&name) && !self.params.iter().any(|(n, _)| n == name) {
                out.push(name.to_string());
            }
            rest = &rest[i + j + 1..];
        }
        out
    }

    /// Whether the URL or a header uses `{key}`.
    pub fn wants_key(&self) -> bool {
        self.template.contains("{key}") || self.headers.iter().any(|(_, v)| v.contains("{key}"))
    }

    fn fill_key(&self, s: &str) -> String {
        match &self.key {
            Some(k) => s.replace("{key}", k),
            None => s.to_string(),
        }
    }

    /// Error text with the key blanked out (reqwest errors include the URL).
    fn redact(&self, e: anyhow::Error) -> anyhow::Error {
        let mut msg = format!("{e:#}");
        if let Some(k) = self.key.as_ref().filter(|k| k.len() >= 4) {
            msg = msg.replace(k.as_str(), "***");
        }
        anyhow::anyhow!(msg)
    }

    fn url(&self, z: u8, x: u32, y: u32) -> String {
        let flipped = (1u32 << z) - 1 - y;
        let y_val = if self.tms { flipped } else { y };
        let mut t = self.template.clone();
        for (name, value) in &self.params {
            t = t.replace(&format!("{{{name}}}"), value);
        }
        t.replace("{z}", &z.to_string()).replace("{x}", &x.to_string()).replace("{-y}", &flipped.to_string()).replace("{y}", &y_val.to_string())
    }

    /// The tile's image bytes (PNG or JPEG), from the 30-day disk cache or the network.
    pub async fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        if let Some(pm) = &self.pm {
            return pm.tile(&self.http, z, x, y).await;
        }
        if self.by_headers {
            return self.tile_by_headers(z, x, y).await;
        }
        let path = self.cache_dir.join(format!("{z}/{x}/{y}.img"));
        if self.cache
            && let Some(b) = read_fresh(&path)
        {
            return Ok(if b.is_empty() { None } else { Some(b) });
        }
        let url = self.fill_key(&self.url(z, x, y));
        let headers: Vec<(String, String)> = self.headers.iter().map(|(k, v)| (k.clone(), self.fill_key(v))).collect();
        // A key in a header or the URL must not follow a redirect to another host.
        let http = if self.key.is_some() || !headers.is_empty() { crate::source::keyed_client() } else { &self.http };
        let bytes = get_with(http, &url, None, &headers).await.map_err(|e| self.redact(e))?;
        if self.cache {
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&path, bytes.as_deref().unwrap_or(&[]));
        }
        Ok(bytes)
    }
}

impl RasterSource {
    /// A tile cached by its HTTP headers (see [`crate::source::fetch_by_headers`]).
    async fn tile_by_headers(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        let file = self.cache_dir.join(format!("{z}/{x}/{y}.img"));
        let url = self.fill_key(&self.url(z, x, y));
        let headers: Vec<(String, String)> = self.headers.iter().map(|(k, v)| (k.clone(), self.fill_key(v))).collect();
        crate::source::fetch_by_headers(crate::source::keyed_client(), &file, &url, &headers, self.cache).await.map_err(|e| self.redact(e))
    }
}

static NEXT_IMAGE_ID: AtomicU32 = AtomicU32::new(1);

/// A decoded raster tile.
pub struct RasterTile {
    /// Image id for terminal graphics protocols, unique in this process.
    pub id: u32,
    /// PNG bytes (re-encoded if the source was JPEG), for the kitty graphics protocol.
    pub png: Vec<u8>,
    /// Decoded pixels, for cell colors.
    pub rgba: image::RgbaImage,
}

impl RasterTile {
    /// Width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.rgba.dimensions()
    }

    /// Average color of a source rectangle (for drawing into cell backgrounds).
    pub(crate) fn average(&self, x: u32, y: u32, w: u32, h: u32) -> Color {
        let (iw, ih) = self.size();
        let (x1, y1) = ((x + w).min(iw), (y + h).min(ih));
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        // Sample a few points instead of every pixel: cells are small next to tiles.
        let step = ((w.max(h)) / 4).max(1);
        // At least one pixel, inside the image (the bounds are fixed before the loops: a
        // bound that moves with the cursor would run on to the image's edge).
        let (x0, y0) = (x.min(iw.saturating_sub(1)), y.min(ih.saturating_sub(1)));
        let (x1, y1) = (x1.max(x0 + 1).min(iw), y1.max(y0 + 1).min(ih));
        let mut yy = y0;
        while yy < y1 {
            let mut xx = x0;
            while xx < x1 {
                let p = self.rgba.get_pixel(xx, yy);
                (r, g, b, n) = (r + p[0] as u32, g + p[1] as u32, b + p[2] as u32, n + 1);
                xx += step;
            }
            yy += step;
        }
        if n == 0 {
            return Color::Reset;
        }
        Color::Rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
    }
}

/// The widest or tallest raster tile mapterm decodes, in pixels.
pub const MAX_TILE_PX: u32 = 4096;
/// The most memory one raster tile may take while decoding (a 4096 px RGBA tile is 64 MiB).
const MAX_TILE_ALLOC: u64 = 80 << 20;

/// Decode PNG or JPEG bytes (CPU work: call from a blocking task).
pub fn decode(bytes: &[u8]) -> Result<RasterTile> {
    // Tiles are 256 or 512 pixels; the limits stop a small compressed image that declares
    // huge dimensions from taking hundreds of megabytes per tile.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_TILE_PX);
    limits.max_image_height = Some(MAX_TILE_PX);
    limits.max_alloc = Some(MAX_TILE_ALLOC);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().context("decoding raster tile")?;
    reader.limits(limits);
    let img = reader.decode().context("decoding raster tile")?;
    let png = if bytes.starts_with(b"\x89PNG") {
        bytes.to_vec()
    } else {
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)?;
        out.into_inner()
    };
    // Move the pixels instead of copying them (no copy at all for RGBA images).
    let rgba = img.into_rgba8();
    Ok(RasterTile { id: NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed), png, rgba })
}

/// The most memory a [`RasterStore`] keeps by default (pixels and PNG bytes): 512 MiB holds
/// all 256 tiles at 512 px, but only a few dozen of the largest (4096 px) tiles a hostile
/// server could send.
pub const RASTER_STORE_BUDGET: usize = 512 << 20;

/// Decoded raster tiles with a size bound: at most 256 tiles and [`RASTER_STORE_BUDGET`]
/// bytes, oldest dropped first. Evicted image ids are kept for the caller to free from the
/// terminal.
pub struct RasterStore {
    tiles: HashMap<TileId, Arc<RasterTile>>,
    order: VecDeque<TileId>,
    cap: usize,
    bytes: usize,
    budget: usize,
    evicted: Vec<u32>,
}

impl Default for RasterStore {
    fn default() -> Self {
        Self { tiles: HashMap::new(), order: VecDeque::new(), cap: 256, bytes: 0, budget: RASTER_STORE_BUDGET, evicted: Vec::new() }
    }
}

/// Memory a stored tile holds: its pixels and its PNG bytes.
fn tile_bytes(t: &RasterTile) -> usize {
    t.rgba.as_raw().len() + t.png.len()
}

impl RasterStore {
    /// Whether the tile is loaded (or known to be missing).
    pub fn contains(&self, id: &TileId) -> bool {
        self.tiles.contains_key(id)
    }

    /// Store a tile (`None`: none there, stored so it isn't fetched again).
    pub fn insert(&mut self, id: TileId, t: Option<RasterTile>) {
        // A missing tile is remembered as a 1x1 transparent image so it isn't fetched again.
        let t = t.unwrap_or_else(|| RasterTile { id: NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed), png: Vec::new(), rgba: image::RgbaImage::new(1, 1) });
        self.bytes += tile_bytes(&t);
        if let Some(old) = self.tiles.insert(id, Arc::new(t)) {
            self.bytes -= tile_bytes(&old);
            self.evicted.push(old.id);
        } else {
            self.order.push_back(id);
        }
        // Drop the oldest until both limits hold; the newest tile always stays.
        while self.order.len() > 1 && (self.order.len() > self.cap || self.bytes > self.budget) {
            if let Some(old) = self.order.pop_front()
                && let Some(t) = self.tiles.remove(&old)
            {
                self.bytes -= tile_bytes(&t);
                self.evicted.push(t.id);
            }
        }
    }

    /// Memory the stored tiles hold now (pixels and PNG bytes).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Change the byte budget (default [`RASTER_STORE_BUDGET`]); takes effect on the next insert.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes.max(1);
    }

    /// Image ids dropped since the last call, to delete from the terminal.
    pub fn take_evicted(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.evicted)
    }

    /// The tile, or the nearest loaded ancestor (up to 6 levels up) to stand in.
    pub fn get_or_ancestor(&self, id: TileId) -> Option<(TileId, Arc<RasterTile>)> {
        let (z, x, y) = id;
        (0..=6u8.min(z)).find_map(|k| {
            let a = (z - k, x >> k, y >> k);
            self.tiles.get(&a).filter(|t| !t.png.is_empty()).map(|t| (a, t.clone()))
        })
    }
}

/// The raster zoom to fetch so one source pixel covers about `screen_per_px` braille dots.
pub fn raster_zoom(cam: &Camera, tile_px: u32, dots_per_source_px: f64, max_zoom: u8) -> u8 {
    // At zoom z a tile spans 256 * 2^(cam.zoom - z) dots over tile_px source pixels.
    let z = cam.zoom - (dots_per_source_px * tile_px as f64 / 256.0).log2();
    (z.round() as i64).clamp(0, max_zoom.min(crate::render::MAX_TILE_ZOOM) as i64) as u8
}

/// The tiles a view needs at raster zoom `rz`, nearest the center first.
pub fn tiles_for(cam: &Camera, area: Rect, rz: u8) -> Vec<TileId> {
    let n = (1u64 << rz) as f64;
    let scale = 256.0 * 2f64.powf(cam.zoom);
    let (cx, cy) = to_world(cam.lon, cam.lat);
    let (hw, hh) = (area.width as f64 / scale, area.height as f64 * 2.0 / scale);
    let clamp = |v: f64| (v.floor() as i64).clamp(0, n as i64 - 1) as u32;
    let (x0, x1) = (clamp((cx - hw) * n), clamp((cx + hw) * n));
    let (y0, y1) = (clamp((cy - hh) * n), clamp((cy + hh) * n));
    let mut out: Vec<TileId> = (x0..=x1).flat_map(|x| (y0..=y1).map(move |y| (rz, x, y))).collect();
    let (ccx, ccy) = (cx * n - 0.5, cy * n - 0.5);
    out.sort_by(|a, b| {
        let d = |t: &TileId| (t.1 as f64 - ccx).powi(2) + (t.2 as f64 - ccy).powi(2);
        d(a).total_cmp(&d(b))
    });
    out
}

/// A rectangle of screen cells drawn from one tile's source rectangle.
pub struct Block {
    /// The tile drawn.
    pub tile: Arc<RasterTile>,
    /// Absolute screen cells.
    pub cells: Rect,
    /// Source rectangle in tile pixels (x, y, w, h).
    pub src: (u32, u32, u32, u32),
}

/// Give every cell of `area` to the raster tile under its center and group the cells into
/// rectangles, one per tile, with the matching source rectangle in that tile.
pub fn layout(store: &RasterStore, cam: &Camera, area: Rect, rz: u8) -> Vec<Block> {
    layout_max(store, cam, area, rz, (u16::MAX, u16::MAX))
}

/// Like [`layout`], but no block is larger than `max` (columns, rows): bigger ones are split
/// into pieces of the same tile. herdr drops kitty placements that cover a very large area
/// (a tile stretched far past its source zoom), while the same image in smaller pieces shows.
/// Each piece's edges come from the same world coordinates, so neighbors meet without gaps.
pub fn layout_max(store: &RasterStore, cam: &Camera, area: Rect, rz: u8, max: (u16, u16)) -> Vec<Block> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let scale = 256.0 * 2f64.powf(cam.zoom);
    let (cx, cy) = to_world(cam.lon, cam.lat);
    let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
    // World coordinate of a dot boundary (x in dots across, y in dots down the area).
    let wx = |dot: f64| cx + (dot - w / 2.0) / scale;
    let wy = |dot: f64| cy + (dot - h / 2.0) / scale;
    let n = (1u64 << rz) as f64;
    // Runs of columns (and rows) whose centers fall in the same tile column (row).
    let runs = |count: u16, per_cell: f64, world: &dyn Fn(f64) -> f64, limit: u16| {
        let mut out: Vec<(u16, u16, i64)> = Vec::new();
        for c in 0..count {
            let t = (world((c as f64 + 0.5) * per_cell) * n).floor() as i64;
            match out.last_mut() {
                Some((start, end, last)) if *last == t && *end - *start < limit => *end = c + 1,
                _ => out.push((c, c + 1, t)),
            }
        }
        out
    };
    let cols = runs(area.width, 2.0, &wx, max.0.max(1));
    let rows = runs(area.height, 4.0, &wy, max.1.max(1));
    let mut blocks = Vec::new();
    for &(c0, c1, tx) in &cols {
        for &(r0, r1, ty) in &rows {
            if tx < 0 || ty < 0 || tx >= n as i64 || ty >= n as i64 {
                continue;
            }
            let Some(((az, ax, ay), tile)) = store.get_or_ancestor((rz, tx as u32, ty as u32)) else { continue };
            let an = (1u64 << az) as f64;
            let (iw, ih) = tile.size();
            // The block's world edges, in source pixels of the (possibly ancestor) tile.
            let sx0 = (wx(c0 as f64 * 2.0) * an - ax as f64) * iw as f64;
            let sx1 = (wx(c1 as f64 * 2.0) * an - ax as f64) * iw as f64;
            let sy0 = (wy(r0 as f64 * 4.0) * an - ay as f64) * ih as f64;
            let sy1 = (wy(r1 as f64 * 4.0) * an - ay as f64) * ih as f64;
            let x = (sx0.round().max(0.0) as u32).min(iw - 1);
            let y = (sy0.round().max(0.0) as u32).min(ih - 1);
            let x_end = (sx1.round().max(0.0) as u32).clamp(x + 1, iw);
            let y_end = (sy1.round().max(0.0) as u32).clamp(y + 1, ih);
            blocks.push(Block { tile, cells: Rect::new(area.x + c0, area.y + r0, c1 - c0, r1 - r0), src: (x, y, x_end - x, y_end - y) });
        }
    }
    blocks
}

/// Draws raster blocks as cell background colors: works in any terminal, one color per cell.
/// Draw the vector map on top without fills to keep the colors.
pub struct RasterCells<'a> {
    /// The blocks to draw.
    pub blocks: &'a [Block],
}

impl Widget for RasterCells<'_> {
    fn render(self, _area: Rect, buf: &mut Buffer) {
        for b in self.blocks {
            let (sx, sy, sw, sh) = b.src;
            let (cw, ch) = (sw as f64 / b.cells.width as f64, sh as f64 / b.cells.height as f64);
            for r in 0..b.cells.height {
                for c in 0..b.cells.width {
                    let x = sx + (c as f64 * cw) as u32;
                    let y = sy + (r as f64 * ch) as u32;
                    let color = b.tile.average(x, y, cw.ceil() as u32, ch.ceil() as u32);
                    if let Some(cell) = buf.cell_mut((b.cells.x + c, b.cells.y + r)) {
                        cell.set_bg(color);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn average_covers_only_its_rectangle() {
        let mut rgba = image::RgbaImage::new(8, 8);
        for (x, y, p) in rgba.enumerate_pixels_mut() {
            *p = if x < 2 && y < 2 { image::Rgba([200, 0, 0, 255]) } else { image::Rgba([0, 0, 200, 255]) };
        }
        let t = RasterTile { id: 0, png: Vec::new(), rgba };
        assert_eq!(t.average(0, 0, 2, 2), Color::Rgb(200, 0, 0));
        assert_eq!(t.average(1, 1, 1, 1), Color::Rgb(200, 0, 0));
        assert_eq!(t.average(4, 4, 2, 2), Color::Rgb(0, 0, 200));
        assert_eq!(t.average(7, 7, 4, 4), Color::Rgb(0, 0, 200), "clipped at the edge");
    }

    #[test]
    fn urls_xyz_and_tms() {
        let xyz = RasterSource::new("https://t.example/{z}/{x}/{y}.png", false, 19).unwrap();
        assert_eq!(xyz.url(3, 2, 1), "https://t.example/3/2/1.png");
        let tms = RasterSource::new("https://t.example/{z}/{x}/{-y}.png", false, 19).unwrap();
        assert_eq!(tms.url(3, 2, 1), "https://t.example/3/2/6.png");
        let flag = RasterSource::new("https://t.example/{z}/{x}/{y}.png", true, 19).unwrap();
        assert_eq!(flag.url(3, 2, 1), "https://t.example/3/2/6.png");
        assert!(RasterSource::new("https://t.example/{z}/{x}.png", false, 19).is_err());
        assert!(RasterSource::new("file:///x/{z}/{x}/{y}.png", false, 19).is_err());
        let esri = RasterSource::new("https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}", false, 19).unwrap();
        assert_eq!(esri.url(15, 9372, 12536), "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/15/12536/9372");
        assert_eq!(esri.attribution, "Esri, Maxar, Earthstar Geographics");
    }

    #[test]
    fn google_presets() {
        for (name, map_type) in [("google", "satellite"), ("google-roadmap", "roadmap"), ("google-terrain", "terrain")] {
            let p = preset(name).unwrap();
            assert!(RasterSource::from_preset(p, None).is_err(), "{name} needs a key");
            let s = RasterSource::from_preset(p, Some("AIzaTESTKEY".into())).unwrap();
            assert!(s.is_google() && s.cache && s.by_headers, "{name}: Google tiles are kept only as their headers allow");
            assert_eq!(s.param("maptype"), Some(map_type));
            // The session comes from start_session; until then it is missing.
            assert_eq!(s.missing_params(), vec!["session".to_string()]);
            assert!(!s.redact(anyhow::anyhow!("GET https://tile.googleapis.com/v1/2dtiles/1/0/0?key=AIzaTESTKEY")).to_string().contains("AIza"));
        }
        let mut s = RasterSource::from_preset(preset("google").unwrap(), Some("k3y-123".into())).unwrap();
        s.set_param("session", "SESS");
        assert_eq!(s.fill_key(&s.url(3, 2, 1)), "https://tile.googleapis.com/v1/2dtiles/3/2/1?session=SESS&key=k3y-123");
        assert!(!RasterSource::from_preset(preset("osm").unwrap(), None).unwrap().is_google());
    }

    #[test]
    fn stacks_name_real_providers() {
        let st = stack("Planetary").unwrap();
        assert_eq!(st.layers.iter().map(|l| (l.preset, l.min_zoom)).collect::<Vec<_>>(), vec![("sentinel2", 0), ("naip", 12)], "bottom first");
        for st in STACKS {
            assert!(preset(st.name).is_none(), "{}: a stack can't share a provider's name", st.name);
            for l in st.layers {
                assert!(preset(l.preset).is_some(), "{}: no provider {}", st.name, l.preset);
            }
        }
    }

    #[test]
    fn planetary_computer_presets() {
        for (name, collection, min) in [("naip", "naip", 12), ("sentinel2", "sentinel-2-l2a", 9)] {
            let s = RasterSource::from_preset(preset(name).unwrap(), None).expect("no key needed");
            assert_eq!(crate::planetary::collection(&s.template), Some(collection));
            assert_eq!(s.min_zoom, min, "{name} renders from zoom {min}");
            assert!(s.cache && s.by_headers, "{name}: kept as long as Cache-Control says");
            // The mosaic id comes from start_session; until then it is missing.
            assert_eq!(s.missing_params(), vec!["search".to_string()]);
            assert!(!s.wants_key());
        }
        let mut s = RasterSource::from_preset(preset("naip").unwrap(), None).unwrap();
        s.set_param("search", "pc1.1.abc");
        assert!(
            s.url(15, 9372, 12536)
                .starts_with("https://planetarycomputer.microsoft.com/api/data/v1/mosaic/pc1.1.abc/tiles/WebMercatorQuad/15/9372/12536?collection=naip&")
        );
        assert_eq!(RasterSource::from_preset(preset("osm").unwrap(), None).unwrap().min_zoom, 0);
    }

    #[test]
    fn cache_control_and_google_cache_dirs() {
        use crate::source::cache_policy;
        let g = cache_policy(Some("private, max-age=86400, must-revalidate, no-transform"));
        assert_eq!((g.max_age, g.no_store), (86400, false));
        assert!(cache_policy(Some("no-cache, no-store, max-age=0, must-revalidate")).no_store);
        assert!(cache_policy(Some("No-Cache")).no_store);
        assert_eq!(cache_policy(None).max_age, 0, "no header: nothing to keep");
        // Satellite and road map share a URL template but not a cache.
        let key = || Some("k".to_string());
        let sat = RasterSource::from_preset(preset("google").unwrap(), key()).unwrap();
        let road = RasterSource::from_preset(preset("google-roadmap").unwrap(), key()).unwrap();
        assert_ne!(sat.cache_dir, road.cache_dir);
    }

    /// A one-path HTTP server: answers 200 with `cache_control` and an ETag, or 304 when
    /// asked with that ETag. Returns its port and the request count.
    fn tile_server(cache_control: &'static str) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let h = h.clone();
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        // One request at a time on a kept-alive connection.
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            match s.read(&mut chunk) {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let req = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                        buf.drain(..end);
                        h.fetch_add(1, Ordering::SeqCst);
                        let resp = if req.contains("if-none-match: \"v1\"") {
                            format!("HTTP/1.1 304 Not Modified\r\nCache-Control: {cache_control}\r\nETag: \"v1\"\r\nContent-Length: 0\r\n\r\n").into_bytes()
                        } else {
                            let mut r = format!("HTTP/1.1 200 OK\r\nCache-Control: {cache_control}\r\nETag: \"v1\"\r\nContent-Length: 4\r\n\r\n").into_bytes();
                            r.extend_from_slice(b"tile");
                            r
                        };
                        if s.write_all(&resp).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (port, hits)
    }

    fn header_cached_source(port: u16, dir: &std::path::Path) -> RasterSource {
        let mut s = RasterSource::new(&format!("http://127.0.0.1:{port}/{{z}}/{{x}}/{{y}}"), false, 19).unwrap();
        s.by_headers = true;
        s.cache_dir = dir.to_path_buf();
        s
    }

    #[tokio::test]
    async fn tiles_cached_by_their_headers() {
        let dir = std::env::temp_dir().join(format!("mapterm-hdrcache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (port, hits) = tile_server("private, max-age=86400, must-revalidate");
        let s = header_cached_source(port, &dir);
        assert_eq!(s.tile(3, 2, 1).await.unwrap().as_deref(), Some(&b"tile"[..]));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // Within max-age: from disk, no request.
        assert_eq!(s.tile(3, 2, 1).await.unwrap().as_deref(), Some(&b"tile"[..]));
        assert_eq!(hits.load(Ordering::SeqCst), 1, "served from the cache");
        let meta = dir.join("3/2/1.meta");
        let (expires, etag) = crate::source::read_meta(&meta).unwrap();
        assert_eq!(etag.as_deref(), Some("\"v1\""));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert!(expires > now + 86000 && expires <= now + 86400, "kept for the header's max-age");
        // Expired: asked again with If-None-Match, 304 keeps the bytes and renews the expiry.
        crate::source::write_meta(&meta, 0, etag.as_deref()).unwrap();
        assert_eq!(s.tile(3, 2, 1).await.unwrap().as_deref(), Some(&b"tile"[..]));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(crate::source::read_meta(&meta).unwrap().0 > now, "renewed");
        let _ = std::fs::remove_dir_all(&dir);

        // no-store: nothing kept, every use asks again.
        let dir2 = dir.with_extension("nostore");
        let _ = std::fs::remove_dir_all(&dir2);
        let (port, hits) = tile_server("no-cache, no-store, max-age=0");
        let s = header_cached_source(port, &dir2);
        s.tile(3, 2, 1).await.unwrap();
        s.tile(3, 2, 1).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(!dir2.join("3/2/1.img").exists());
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn presets_and_keys() {
        let v = preset("vantor").unwrap();
        assert!(preset("MAXAR").is_some_and(|p| p.name == "vantor"));
        assert!(RasterSource::from_preset(v, None).is_err(), "vantor needs a key");
        let s = RasterSource::from_preset(v, Some("secret-key-123".into())).unwrap();
        assert!(!s.cache && s.wants_key());
        // The key goes in the header, not the URL.
        let url = s.fill_key(&s.url(15, 9372, 12536));
        assert!(url.contains("tileMatrix=EPSG:3857:15&tileRow=12536&tileCol=9372") && !url.contains("secret"));
        assert_eq!(s.headers[0].0, "maxar-api-key");
        assert_eq!(s.fill_key(&s.headers[0].1), "secret-key-123");
        // Errors never show it.
        let e = s.redact(anyhow::anyhow!("https://x/?access_token=secret-key-123: HTTP 401"));
        assert!(!e.to_string().contains("secret") && e.to_string().contains("***"));
        // Free presets need nothing.
        // Planet: the key is in the URL (its only option) but still redacted; needs a mosaic.
        let mut pl = RasterSource::from_preset(preset("planet").unwrap(), Some("PLAK123456".into())).unwrap();
        pl.params.clear();
        assert_eq!(pl.missing_params(), vec!["mosaic".to_string()]);
        pl.set_param("mosaic", "global_monthly_2025_09_mosaic");
        assert!(pl.missing_params().is_empty());
        assert_eq!(
            pl.fill_key(&pl.url(12, 1171, 1566)),
            "https://tiles.planet.com/basemaps/v1/planet-tiles/global_monthly_2025_09_mosaic/gmap/12/1171/1566.png?api_key=PLAK123456"
        );
        assert!(!pl.redact(anyhow::anyhow!("GET ...?api_key=PLAK123456 failed")).to_string().contains("PLAK"));
        // GIBS fills its layer and a date by default.
        let g = RasterSource::from_preset(preset("gibs").unwrap(), None).unwrap();
        assert!(g.missing_params().is_empty());
        let u = g.url(6, 18, 24);
        assert!(u.contains("/MODIS_Terra_CorrectedReflectance_TrueColor/default/") && u.ends_with("/GoogleMapsCompatible_Level9/6/24/18.jpg"), "{u}");
        let y = yesterday_utc();
        assert!(u.contains(&y) && y.len() == 10 && &y[4..5] == "-", "{y}");
        let osm = RasterSource::from_preset(preset("osm").unwrap(), None).unwrap();
        assert!(osm.cache && !osm.wants_key());
        assert_eq!(RasterSource::from_preset(preset("mapbox").unwrap(), Some("t".into())).unwrap().tile_px, 512);
    }

    #[test]
    fn capped_layout_still_covers_every_cell_once() {
        let mut store = RasterStore::default();
        // Zoomed far past the tiles' zoom: each tile spans many cells and must be split.
        let cam = Camera::new(-77.0353, 38.8895, 17.5);
        let area = Rect::new(0, 1, 150, 45);
        let rz = 14;
        for id in tiles_for(&cam, area, rz) {
            store.insert(id, Some(RasterTile { id: 0, png: vec![1], rgba: image::RgbaImage::new(256, 256) }));
        }
        let blocks = layout_max(&store, &cam, area, rz, (32, 16));
        let mut covered = vec![0u8; area.width as usize * area.height as usize];
        for b in &blocks {
            assert!(b.cells.width <= 32 && b.cells.height <= 16);
            for r in b.cells.y..b.cells.bottom() {
                for c in b.cells.x..b.cells.right() {
                    covered[(r - area.y) as usize * area.width as usize + (c - area.x) as usize] += 1;
                }
            }
        }
        assert!(covered.iter().all(|&n| n == 1));
        // Neighboring pieces of one tile share their source edge exactly.
        for a in &blocks {
            if let Some(b) = blocks.iter().find(|b| b.cells.x == a.cells.right() && b.cells.y == a.cells.y && Arc::ptr_eq(&a.tile, &b.tile)) {
                assert_eq!(a.src.0 + a.src.2, b.src.0, "no gap or overlap between pieces");
            }
        }
    }

    #[test]
    fn layout_covers_every_cell_once() {
        let mut store = RasterStore::default();
        let cam = Camera::new(-77.0353, 38.8895, 14.3);
        let area = Rect::new(3, 2, 90, 30);
        let rz = 14;
        for id in tiles_for(&cam, area, rz) {
            store.insert(id, Some(RasterTile { id: 0, png: vec![1], rgba: image::RgbaImage::new(256, 256) }));
        }
        let blocks = layout(&store, &cam, area, rz);
        let mut covered = vec![0u8; area.width as usize * area.height as usize];
        for b in &blocks {
            assert!(b.src.0 + b.src.2 <= 256 && b.src.1 + b.src.3 <= 256);
            for r in b.cells.y..b.cells.bottom() {
                for c in b.cells.x..b.cells.right() {
                    covered[(r - area.y) as usize * area.width as usize + (c - area.x) as usize] += 1;
                }
            }
        }
        assert!(covered.iter().all(|&n| n == 1), "every cell drawn exactly once");
    }
}

#[cfg(test)]
mod pm_pipeline_tests {

    #[test]
    fn the_store_keeps_to_its_byte_budget() {
        let tile = |px: u32| RasterTile { id: 0, png: vec![0; 1000], rgba: image::RgbaImage::new(px, px) };
        let mut store = RasterStore::default();
        store.set_budget(40 << 20);
        // 1024 px tiles are 4 MiB of pixels each: about ten fit in 40 MiB.
        for i in 0..30 {
            store.insert((10, i, 0), Some(tile(1024)));
            assert!(store.bytes() <= 40 << 20, "over budget after {i}: {}", store.bytes());
        }
        assert!(store.contains(&(10, 29, 0)), "the newest tile stays");
        assert!(!store.contains(&(10, 0, 0)), "the oldest went first");
        assert_eq!(store.take_evicted().len(), 30 - store.order.len());
        // One tile bigger than the whole budget is still kept (alone), so the view can draw.
        store.set_budget(1 << 20);
        store.insert((11, 0, 0), Some(tile(1024)));
        assert_eq!(store.order.len(), 1);
    }

    use super::*;

    /// End to end for a local raster PMTiles named by MAPTERM_TEST_PMTILES (skipped when unset).
    #[tokio::test]
    async fn pmtiles_layer_lays_out_blocks() {
        let Ok(path) = std::env::var("MAPTERM_TEST_PMTILES") else { return };
        let src = RasterSource::from_pmtiles(&path).await.expect("open");
        let cam = Camera::new(-77.0353, 38.8895, 15.0);
        let area = Rect::new(0, 0, 150, 45);
        let rz = raster_zoom(&cam, src.tile_px, 2.0 / 10.0, src.max_zoom);
        let ids = tiles_for(&cam, area, rz);
        let mut store = RasterStore::default();
        let mut found = 0;
        for id in &ids {
            let t = src.tile(id.0, id.1, id.2).await.expect("tile");
            if let Some(b) = &t {
                found += 1;
                store.insert(*id, Some(decode(b).expect("decode")));
            } else {
                store.insert(*id, None);
            }
        }
        let blocks = layout(&store, &cam, area, rz);
        eprintln!("rz {rz} max {} wanted {} found {found} blocks {}", src.max_zoom, ids.len(), blocks.len());
        assert!(!blocks.is_empty());
    }
}
