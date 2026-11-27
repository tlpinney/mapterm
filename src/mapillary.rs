//! Street-level photos from [Mapillary](https://www.mapillary.com) (CC BY-SA 4.0): the photos
//! taken near a spot, nearest and best first, each with when, which way it looks, whether it's
//! a 360° panorama, who took it, and a thumbnail.
//!
//! Needs your own client token (register an app at mapillary.com/dashboard/developers): from
//! `--mapillary-key-cmd`, else `MAPILLARY_TOKEN`. The thumbnail links Mapillary returns are
//! signed and expire, so only the images are kept (by photo id, a week), never the links.
//! Mapillary asks apps to credit it and link to it; the viewer shows both, with the
//! photographer.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

/// How far from the spot photos are looked for, meters (Mapillary's most).
pub const RADIUS: u32 = 50;
/// How many photos are asked for.
pub const LIMIT: u32 = 8;
/// The credit.
pub const CREDIT: &str = "© Mapillary · CC BY-SA 4.0";
/// Photos are kept this long, seconds.
const KEEP: std::time::Duration = std::time::Duration::from_secs(7 * 86400);

/// A photo near the spot asked about.
#[derive(Clone, Debug, PartialEq)]
pub struct Photo {
    /// Mapillary's id for it.
    pub id: String,
    /// When it was taken (Unix seconds).
    pub taken: f64,
    /// Which way the camera looked, degrees from north.
    pub heading: Option<f64>,
    /// A 360° panorama.
    pub pano: bool,
    /// Who took it (their Mapillary username).
    pub creator: Option<String>,
    /// Where it was taken (lon, lat).
    pub at: Option<(f64, f64)>,
    /// Its thumbnail's (signed, expiring) link: used at once, never kept.
    pub thumb: Option<String>,
}

impl Photo {
    /// Its page on Mapillary.
    pub fn page(&self) -> String {
        format!("https://www.mapillary.com/app/?pKey={}", self.id)
    }
}

/// The photos in a `/images` answer.
pub fn parse(body: &[u8]) -> Result<Vec<Photo>> {
    let v: serde_json::Value = serde_json::from_slice(body).context("Mapillary: not JSON")?;
    if let Some(e) = v.get("error") {
        bail!("Mapillary: {}", e["message"].as_str().unwrap_or("error"));
    }
    Ok(v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let coords = p.get("computed_geometry").or_else(|| p.get("geometry")).and_then(|g| g["coordinates"].as_array().cloned());
            Some(Photo {
                id: p["id"].as_str()?.to_string(),
                taken: p["captured_at"].as_f64().map(|ms| ms / 1000.0).unwrap_or(0.0),
                heading: p["compass_angle"].as_f64(),
                pano: p["is_pano"].as_bool().unwrap_or(false),
                creator: p["creator"]["username"].as_str().map(str::to_string),
                at: coords.and_then(|c| Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?))),
                thumb: p["thumb_1024_url"].as_str().map(str::to_string),
            })
        })
        .collect())
}

/// The photos within [`RADIUS`] of (`lon`, `lat`), Mapillary's best first.
pub async fn near(lon: f64, lat: f64, token: &str) -> Result<Vec<Photo>> {
    let url = format!(
        "https://graph.mapillary.com/images?lat={lat:.6}&lng={lon:.6}&radius={RADIUS}&limit={LIMIT}&fields=id,captured_at,compass_angle,is_pano,creator,computed_geometry,geometry,thumb_1024_url"
    );
    let headers = [("Authorization".to_string(), format!("OAuth {token}"))];
    let body = crate::source::get_limited(crate::source::keyed_client(), &url, &headers, 4 << 20)
        .await
        .map_err(|e| anyhow::anyhow!("{}", format!("{e:#}").replace(token, "<token>")))?
        .context("Mapillary: nothing found")?;
    parse(&body)
}

/// Where photos are kept.
fn cache_file(id: &str) -> PathBuf {
    crate::source::cache_root().join("mapillary").join(format!("{}.jpg", id.replace(['/', '.'], "_")))
}

/// The photo's image as RGBA (width, height, pixels): kept on disk a week, else fetched from
/// its thumbnail link (CPU work for the decoding: call from a blocking task, or await).
pub async fn image(p: &Photo) -> Result<(u32, u32, Vec<u8>)> {
    let path = cache_file(&p.id);
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < KEEP);
    let bytes = match fresh.then(|| std::fs::read(&path).ok()).flatten() {
        Some(b) => b,
        None => {
            let url = p.thumb.as_deref().context("Mapillary: this photo has no thumbnail")?;
            let b = crate::source::get_limited(&crate::source::http_client(), url, &[], 16 << 20).await?.context("Mapillary: the photo is gone")?;
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, &b);
            b
        }
    };
    tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).context("Mapillary: not an image")?.into_rgba8();
        let (w, h) = img.dimensions();
        Ok((w, h, img.into_raw()))
    })
    .await?
}

/// A heading as a compass point (`N`, `NE`, ...).
pub fn compass8(deg: f64) -> &'static str {
    ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][((deg.rem_euclid(360.0) / 45.0).round() as usize) % 8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_photos() {
        let body = br#"{"data":[
            {"id":"1979725246078356","captured_at":1786233600000,"compass_angle":3.2,"is_pano":true,"creator":{"username":"cdelia","id":"1"},
             "computed_geometry":{"type":"Point","coordinates":[-77.4105,39.4143]},"thumb_1024_url":"https://scontent.example/x.jpg?sig=1"},
            {"id":"2","captured_at":1700000000000,"geometry":{"type":"Point","coordinates":[1.0,2.0]}}
        ]}"#;
        let p = parse(body).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].pano, p[0].creator.as_deref(), p[0].at), (true, Some("cdelia"), Some((-77.4105, 39.4143))));
        assert_eq!(p[0].taken, 1_786_233_600.0);
        assert_eq!(p[0].page(), "https://www.mapillary.com/app/?pKey=1979725246078356");
        assert_eq!((p[1].at, p[1].thumb.as_deref(), p[1].heading), (Some((1.0, 2.0)), None, None));
        assert!(parse(br#"{"error":{"message":"Invalid OAuth access token"}}"#).unwrap_err().to_string().contains("Invalid OAuth"));
    }

    #[test]
    fn compass_points() {
        assert_eq!((compass8(3.0), compass8(44.0), compass8(181.0), compass8(-10.0)), ("N", "NE", "S", "N"));
    }
}
