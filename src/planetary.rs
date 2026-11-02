//! Microsoft Planetary Computer imagery as raster layers (`--raster naip`, `sentinel2`): its
//! data API renders mosaics of a collection's scenes as Web Mercator tiles, free and without
//! a key. A mosaic is a registered search (which collection, which scenes first); the reply's
//! id goes in the tile URLs. Ids change with every registration, so a source registers once
//! per run, in [`crate::raster::RasterSource::start_session`].
//!
//! Mosaics combine many scenes per tile, so the service renders them only when zoomed in
//! (Microsoft's own viewer: NAIP from zoom 12, Sentinel-2 from 9); the presets set that as
//! their minimum zoom. Tiles say `Cache-Control: max-age=3600` and are kept that long.

use anyhow::{Context, Result, bail};
use serde_json::json;

/// The data API that renders the tiles.
pub const DATA_API: &str = "https://planetarycomputer.microsoft.com/api/data/v1";

/// Whether a tile URL template is a Planetary Computer mosaic.
pub fn is_planetary(template: &str) -> bool {
    template.starts_with(DATA_API)
}

/// The `collection=` of a mosaic tile URL template.
pub fn collection(template: &str) -> Option<&str> {
    let query = template.split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix("collection="))
}

/// The lowest zoom Microsoft renders a collection's mosaic at (its Explorer's `minZoom`).
pub fn min_zoom(collection: &str) -> u8 {
    match collection {
        "naip" => 12,
        _ => 9,
    }
}

/// The search for a collection's mosaic, newest scenes first: Microsoft's default mosaic for
/// it (for Sentinel-2 and Landsat, scenes at most 10% cloudy).
pub fn search(collection: &str) -> serde_json::Value {
    let mut filter = json!({"op": "=", "args": [{"property": "collection"}, collection]});
    if matches!(collection, "sentinel-2-l2a" | "landsat-c2-l2") {
        filter = json!({"op": "and", "args": [filter, {"op": "<=", "args": [{"property": "eo:cloud_cover"}, 10]}]});
    }
    json!({"filter-lang": "cql2-json", "filter": filter, "sortby": [{"field": "datetime", "direction": "desc"}]})
}

/// Register the mosaic search for `collection` and return its id for the tile URLs.
pub async fn register(http: &reqwest::Client, collection: &str) -> Result<String> {
    let url = format!("{DATA_API}/mosaic/register");
    let resp = http
        .post(&url)
        .header("content-type", "application/json")
        .body(search(collection).to_string())
        .send()
        .await
        .context("Planetary Computer: registering the mosaic")?;
    let status = resp.status();
    // A registration reply is a few hundred bytes.
    let body = resp.bytes().await.context("Planetary Computer: reading the mosaic registration")?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&body[..body.len().min(300)]).into_owned();
        bail!("Planetary Computer mosaic registration: HTTP {status}: {text}");
    }
    id(&body)
}

/// The id in a registration reply: `searchid` (or, in some replies, `id`).
fn id(body: &[u8]) -> Result<String> {
    let v: serde_json::Value = serde_json::from_slice(body).context("Planetary Computer: the mosaic registration isn't JSON")?;
    let id = v["searchid"].as_str().or_else(|| v["id"].as_str()).context("Planetary Computer: no id in the mosaic registration")?;
    // It goes into a URL path: refuse anything but the plain ids the service hands out.
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_') {
        bail!("Planetary Computer: unexpected mosaic id {id:?}");
    }
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collections_and_searches() {
        let t = "https://planetarycomputer.microsoft.com/api/data/v1/mosaic/{search}/tiles/WebMercatorQuad/{z}/{x}/{y}?collection=naip&assets=image";
        assert!(is_planetary(t));
        assert_eq!(collection(t), Some("naip"));
        assert!(!is_planetary("https://tile.openstreetmap.org/{z}/{x}/{y}.png"));
        assert_eq!(search("naip")["filter"]["args"][1], "naip");
        assert_eq!(search("sentinel-2-l2a")["filter"]["op"], "and", "Sentinel-2 keeps to clear scenes");
    }

    #[test]
    fn registration_ids() {
        assert_eq!(id(br#"{"searchid":"pc1.17936.c5967fda","links":[]}"#).unwrap(), "pc1.17936.c5967fda");
        assert_eq!(id(br#"{"id":"pc1.17936.4f087e20","links":[]}"#).unwrap(), "pc1.17936.4f087e20");
        assert!(id(br#"{"searchid":"../../x?y"}"#).is_err(), "only plain ids go into the URL");
        assert!(id(b"not json").is_err());
    }
}
