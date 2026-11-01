//! Mapbox: `mapbox://` style and tileset URLs, and the access token they need (like Mapbox
//! GL's `mapboxgl.accessToken`).
//!
//! ```no_run
//! # async fn f() -> anyhow::Result<()> {
//! mapterm::mapbox::set_access_token(std::env::var("MAPBOX_ACCESS_TOKEN").ok());
//! let map = mapterm::Map::from_style("mapbox://styles/mapbox/streets-v12").await?;
//! # Ok(()) }
//! ```
//!
//! The token is added to the API URLs and blanked out of errors. Mapbox's tiles are cached
//! by their `Cache-Control` and `ETag` headers (12 hours, as of 2026), not a flat 30 days.

use std::sync::RwLock;

use anyhow::{Context, Result, bail};

static TOKEN: RwLock<Option<String>> = RwLock::new(None);

/// Set (or clear) the access token for `mapbox://` URLs.
pub fn set_access_token(token: Option<String>) {
    *TOKEN.write().unwrap_or_else(|e| e.into_inner()) = token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
}

/// The access token, if one was set.
pub fn access_token() -> Option<String> {
    TOKEN.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Whether a URL is a `mapbox://` one.
pub fn is_mapbox_url(url: &str) -> bool {
    url.starts_with("mapbox://")
}

/// The API URL for a `mapbox://` URL: `mapbox://styles/USER/STYLE` is a style document,
/// `mapbox://TILESET[,TILESET...]` a TileJSON. Needs [`set_access_token`].
pub fn resolve(url: &str) -> Result<String> {
    resolve_with(url, access_token().as_deref())
}

fn resolve_with(url: &str, token: Option<&str>) -> Result<String> {
    let rest = url.strip_prefix("mapbox://").with_context(|| format!("{url}: not a mapbox:// URL"))?;
    let token = token.with_context(|| format!("{url}: mapbox:// URLs need a Mapbox access token (MAPBOX_ACCESS_TOKEN, or --mapbox-token-cmd)"))?;
    if rest.is_empty() || rest.contains(['?', '#', ' ']) || rest.contains("..") {
        bail!("{url}: not a Mapbox style or tileset");
    }
    if let Some(style) = rest.strip_prefix("styles/") {
        return Ok(format!("https://api.mapbox.com/styles/v1/{style}?access_token={token}"));
    }
    if rest.starts_with("sprites/") || rest.starts_with("fonts/") {
        bail!("{url}: sprites and fonts aren't used in a terminal");
    }
    Ok(format!("https://api.mapbox.com/v4/{rest}.json?secure&access_token={token}"))
}

/// Text with the access token blanked out.
pub fn redact_str(s: &str) -> String {
    redact_with(s, access_token().as_deref())
}

fn redact_with(s: &str, token: Option<&str>) -> String {
    match token.filter(|t| t.len() >= 8) {
        Some(t) => s.replace(t, "***"),
        None => s.to_string(),
    }
}

/// An error with the access token blanked out (errors often include the URL).
pub fn redact(e: anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!(redact_str(&format!("{e:#}")))
}

/// Whether a tile URL is served by Mapbox (its tiles are cached by their HTTP headers).
pub fn is_mapbox_host(url: &str) -> bool {
    let host = url.split('/').nth(2).unwrap_or("");
    host == "api.mapbox.com" || host == "tiles.mapbox.com" || host.ends_with(".tiles.mapbox.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_redaction() {
        // The token is passed in: the global one stays untouched while tests run in parallel.
        let t = Some("pk.TESTTOKEN123");
        assert!(resolve_with("mapbox://styles/mapbox/streets-v12", None).unwrap_err().to_string().contains("access token"));
        assert_eq!(
            resolve_with("mapbox://styles/mapbox/streets-v12", t).unwrap(),
            "https://api.mapbox.com/styles/v1/mapbox/streets-v12?access_token=pk.TESTTOKEN123"
        );
        assert_eq!(
            resolve_with("mapbox://mapbox.mapbox-streets-v8,mapbox.mapbox-terrain-v2", t).unwrap(),
            "https://api.mapbox.com/v4/mapbox.mapbox-streets-v8,mapbox.mapbox-terrain-v2.json?secure&access_token=pk.TESTTOKEN123"
        );
        assert!(resolve_with("mapbox://sprites/mapbox/streets-v12", t).is_err());
        assert!(resolve_with("mapbox://styles/../x", t).is_err() && resolve_with("mapbox://a?b", t).is_err());
        assert!(!redact_with("GET https://x/?access_token=pk.TESTTOKEN123: HTTP 401", t).contains("TESTTOKEN"));
        assert!(
            is_mapbox_host("https://a.tiles.mapbox.com/v4/x/{z}/{x}/{y}.vector.pbf")
                && is_mapbox_host("https://api.mapbox.com/v4/mapbox.satellite/1/0/0@2x.jpg90")
        );
        assert!(!is_mapbox_host("https://tiles.openfreemap.org/planet") && !is_mapbox_host("https://evil.example/api.mapbox.com/"));
    }
}
