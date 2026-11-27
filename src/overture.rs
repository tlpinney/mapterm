//! Overture Maps themes by short name: `overture-addresses`, `overture-buildings`,
//! `overture-places`, resolved to the latest release's STAC collection of GeoParquet files
//! (stac.overturemaps.org), so they load as the map moves like any collection. Overture's
//! data is under several open licenses, by source; each feature says where it's from.

use anyhow::{Context, Result};

const CATALOG: &str = "https://stac.overturemaps.org/catalog.json";

/// The short names and their theme and type.
const THEMES: [(&str, &str, &str); 3] =
    [("overture-addresses", "addresses", "address"), ("overture-buildings", "buildings", "building"), ("overture-places", "places", "place")];

/// Whether `spec` is one of the short names.
pub fn is_spec(spec: &str) -> bool {
    THEMES.iter().any(|(n, ..)| spec.eq_ignore_ascii_case(n))
}

/// The latest release in Overture's STAC catalog: its folder URL, from the child links
/// (`https://stac.overturemaps.org/2026-11-18.0/catalog.json`), newest by name.
pub fn latest_release(catalog: &serde_json::Value) -> Option<String> {
    catalog["links"]
        .as_array()?
        .iter()
        .filter(|l| l["rel"] == "child")
        .filter_map(|l| l["href"].as_str())
        .filter_map(|h| h.strip_prefix("https://stac.overturemaps.org/")?.strip_suffix("/catalog.json"))
        .filter(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '.'))
        .max()
        .map(|r| format!("https://stac.overturemaps.org/{r}"))
}

/// The collection URL for short name `spec` in the latest release.
pub async fn collection_url(spec: &str) -> Result<String> {
    let (_, theme, kind) = THEMES.iter().find(|(n, ..)| spec.eq_ignore_ascii_case(n)).context("not an Overture short name")?;
    let resp = crate::source::http_client().get(CATALOG).send().await.context("Overture's STAC catalog")?;
    let doc: serde_json::Value =
        serde_json::from_str(&resp.error_for_status().context("Overture's STAC catalog")?.text().await?).context("Overture's STAC catalog")?;
    let release = latest_release(&doc).context("Overture's STAC catalog lists no release")?;
    Ok(format!("{release}/{theme}/{kind}/collection.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_and_the_latest_release() {
        assert!(is_spec("overture-addresses") && is_spec("Overture-Places") && !is_spec("overture-roads"));
        let doc = serde_json::json!({"links": [
            {"rel": "self", "href": "https://stac.overturemaps.org/catalog.json"},
            {"rel": "child", "href": "https://stac.overturemaps.org/2026-10-21.0/catalog.json"},
            {"rel": "child", "href": "https://stac.overturemaps.org/2026-11-18.0/catalog.json"},
            {"rel": "child", "href": "https://elsewhere.example/2099-01-01.0/catalog.json"}
        ]});
        assert_eq!(latest_release(&doc).as_deref(), Some("https://stac.overturemaps.org/2026-11-18.0"));
    }
}
