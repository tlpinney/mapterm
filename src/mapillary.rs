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
    /// The sequence it belongs to: one person's run of photos along a way.
    pub sequence: Option<String>,
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
    // `/images` answers {"data": [...]}; looking photos up by id, {"<id>": {...}, ...}.
    let list: Vec<serde_json::Value> = match v.get("data") {
        Some(d) => d.as_array().cloned().unwrap_or_default(),
        None => v.as_object().map(|o| o.values().cloned().collect()).unwrap_or_default(),
    };
    Ok(list
        .iter()
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
                sequence: p["sequence"].as_str().map(str::to_string),
            })
        })
        .collect())
}

/// The fields asked for each photo.
const FIELDS: &str = "id,captured_at,compass_angle,is_pano,creator,computed_geometry,geometry,thumb_1024_url,sequence";

/// A Graph API answer for `path` (with the token in the header, kept out of errors).
async fn graph(path: &str, token: &str) -> Result<Vec<u8>> {
    let url = format!("https://graph.mapillary.com/{path}");
    let headers = [("Authorization".to_string(), format!("OAuth {token}"))];
    crate::source::get_limited(crate::source::keyed_client(), &url, &headers, 8 << 20)
        .await
        .map_err(|e| anyhow::anyhow!("{}", format!("{e:#}").replace(token, "<token>")))?
        .context("Mapillary: nothing found")
}

/// The photos within [`RADIUS`] of (`lon`, `lat`), Mapillary's best first.
pub async fn near(lon: f64, lat: f64, token: &str) -> Result<Vec<Photo>> {
    within(lon, lat, RADIUS, LIMIT, token).await
}

/// Up to `limit` photos within `radius` meters (50 at most) of (`lon`, `lat`).
pub async fn within(lon: f64, lat: f64, radius: u32, limit: u32, token: &str) -> Result<Vec<Photo>> {
    parse(&graph(&format!("images?lat={lat:.6}&lng={lon:.6}&radius={}&limit={}&fields={FIELDS}", radius.min(RADIUS), limit.min(100)), token).await?)
}

/// The photos of a sequence, in order (their ids).
pub async fn sequence(id: &str, token: &str) -> Result<Vec<String>> {
    let body = graph(&format!("image_ids?sequence_id={id}"), token).await?;
    let v: serde_json::Value = serde_json::from_slice(&body).context("Mapillary: not JSON")?;
    if let Some(e) = v.get("error") {
        bail!("Mapillary: {}", e["message"].as_str().unwrap_or("error"));
    }
    Ok(v["data"].as_array().into_iter().flatten().filter_map(|x| x["id"].as_str().map(str::to_string)).collect())
}

/// Photos by id (any order).
pub async fn by_id(ids: &[String], token: &str) -> Result<Vec<Photo>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    parse(&graph(&format!("?ids={}&fields={FIELDS}", ids.join(",")), token).await?)
}

/// The bearing from `a` to `b` (lon, lat), degrees from north.
pub fn bearing(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2) = (a.1.to_radians(), b.1.to_radians());
    let dl = (b.0 - a.0).to_radians();
    let y = dl.sin() * la2.cos();
    let x = la1.cos() * la2.sin() - la1.sin() * la2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// The distance from `a` to `b` (lon, lat), meters.
pub fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    crate::adsb::distance_nm(a, b) * 1852.0
}

/// How far apart two directions are, 0 to 180 degrees.
pub fn turn(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// The signed turn from `a` to `b`, -180 (left) to 180 (right).
pub fn signed_turn(a: f64, b: f64) -> f64 {
    let d = (b - a).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

/// A step in walk mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The next photo along the way you face.
    Forward,
    /// The one behind.
    Back,
    /// Turn left: a photo here facing further left (a cross street, the other side).
    Left,
    /// Turn right.
    Right,
    /// An older photo of this spot.
    Older,
    /// A newer one.
    Newer,
}

/// What walk mode knows: the sequence it's on, in order, and the photos seen (by id).
#[derive(Clone, Debug, Default)]
pub struct Walk {
    /// The sequence's id.
    pub seq_id: Option<String>,
    /// Its photos' ids, in order.
    pub seq: Vec<String>,
    /// Photos already looked up, by id.
    pub known: std::collections::HashMap<String, Photo>,
}

/// Steps a photo has to be from the last to count as one (closer ones are skipped).
const MIN_STEP: f64 = 2.0;

impl Walk {
    /// The photo a step from `cur` (facing `facing`) leads to, and the way you then face; None
    /// when there's nowhere to go that way.
    pub async fn step(&mut self, cur: &Photo, facing: f64, step: Step, token: &str) -> Result<Option<(Photo, f64)>> {
        let Some(here) = cur.at else { return Ok(None) };
        match step {
            Step::Forward | Step::Back => {
                let ahead = if step == Step::Forward { facing } else { (facing + 180.0) % 360.0 };
                // Along the sequence first: its neighbours, the first far enough the way asked.
                if let Some(next) = self.along(cur, here, ahead, token).await? {
                    let at = next.at.unwrap_or(here);
                    let face = if step == Step::Forward { bearing(here, at) } else { facing };
                    return Ok(Some((next, face)));
                }
                // At its end: the nearest photo ahead, from any sequence.
                let around = within(here.0, here.1, 30, 60, token).await?;
                let best = around
                    .into_iter()
                    .filter(|p| p.id != cur.id)
                    .filter_map(|p| {
                        let at = p.at?;
                        let d = distance(here, at);
                        (d >= MIN_STEP && turn(bearing(here, at), ahead) < 60.0).then_some((d, p))
                    })
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                Ok(best.map(|(_, p)| {
                    let face = if step == Step::Forward { bearing(here, p.at.unwrap_or(here)) } else { facing };
                    (p, face)
                }))
            }
            Step::Left | Step::Right => {
                let want = if step == Step::Left { facing - 90.0 } else { facing + 90.0 }.rem_euclid(360.0);
                let around = within(here.0, here.1, 20, 60, token).await?;
                // A photo here that looks that way, else one that lies that way.
                let looking = around
                    .iter()
                    .filter(|p| p.id != cur.id)
                    .filter_map(|p| Some((turn(p.heading?, want), p)))
                    .filter(|(t, _)| *t < 50.0)
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, p)| (p.clone(), p.heading.unwrap_or(want)));
                let lying = || {
                    around
                        .iter()
                        .filter(|p| p.id != cur.id)
                        .filter_map(|p| {
                            let at = p.at?;
                            let d = distance(here, at);
                            (d >= MIN_STEP && turn(bearing(here, at), want) < 50.0).then_some((d, p))
                        })
                        .min_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|(_, p)| (p.clone(), want))
                };
                Ok(looking.or_else(lying))
            }
            Step::Older | Step::Newer => {
                let around = within(here.0, here.1, 12, 100, token).await?;
                // Other captures of this spot, facing about the same way, nearest in time.
                let mut v: Vec<Photo> = around
                    .into_iter()
                    .filter(|p| p.id != cur.id && p.sequence != cur.sequence)
                    .filter(|p| p.heading.is_none_or(|h| turn(h, facing) < 90.0) || p.pano)
                    .collect();
                v.retain(|p| if step == Step::Older { p.taken < cur.taken } else { p.taken > cur.taken });
                let best = if step == Step::Older {
                    v.into_iter().max_by(|a, b| a.taken.total_cmp(&b.taken))
                } else {
                    v.into_iter().min_by(|a, b| a.taken.total_cmp(&b.taken))
                };
                Ok(best.map(|p| {
                    let face = p.heading.filter(|_| !p.pano).unwrap_or(facing);
                    (p, face)
                }))
            }
        }
    }

    /// The next photo of `cur`'s sequence that's at least a step away the way `ahead`.
    async fn along(&mut self, cur: &Photo, here: (f64, f64), ahead: f64, token: &str) -> Result<Option<Photo>> {
        let Some(seq) = cur.sequence.clone() else { return Ok(None) };
        if self.seq_id.as_deref() != Some(seq.as_str()) {
            self.seq = sequence(&seq, token).await?;
            self.seq_id = Some(seq);
        }
        let Some(pos) = self.seq.iter().position(|id| *id == cur.id) else { return Ok(None) };
        // Look up the photos around it not seen yet (a window, 25 a side).
        let window: Vec<String> =
            self.seq[pos.saturating_sub(25)..(pos + 26).min(self.seq.len())].iter().filter(|id| !self.known.contains_key(*id)).cloned().collect();
        for chunk in window.chunks(50) {
            for p in by_id(chunk, token).await? {
                self.known.insert(p.id.clone(), p);
            }
        }
        // Whichever way the sequence runs, the side that leads `ahead`.
        for dir in [1isize, -1] {
            let mut i = pos as isize + dir;
            while i >= 0 && (i as usize) < self.seq.len() && (i - pos as isize).abs() <= 25 {
                let Some(p) = self.known.get(&self.seq[i as usize]) else { break };
                if let Some(at) = p.at {
                    let d = distance(here, at);
                    if d >= MIN_STEP {
                        if turn(bearing(here, at), ahead) < 70.0 {
                            return Ok(Some(p.clone()));
                        }
                        break;
                    }
                }
                i += dir;
            }
        }
        Ok(None)
    }
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
    fn bearings_and_turns() {
        let a = (-77.41, 39.41);
        assert!((bearing(a, (-77.41, 39.42)) - 0.0).abs() < 0.1, "north");
        assert!((bearing(a, (-77.40, 39.41)) - 90.0).abs() < 0.1, "east");
        assert!((distance(a, (-77.41, 39.42)) - 1112.0).abs() < 5.0);
        assert_eq!(turn(350.0, 10.0), 20.0);
        assert_eq!(signed_turn(350.0, 10.0), 20.0);
        assert_eq!(signed_turn(10.0, 350.0), -20.0);
        let by_id = parse(br#"{"1":{"id":"1","sequence":"s","computed_geometry":{"coordinates":[1,2]}}}"#).unwrap();
        assert_eq!((by_id[0].sequence.as_deref(), by_id[0].at), (Some("s"), Some((1.0, 2.0))));
    }

    #[test]
    fn compass_points() {
        assert_eq!((compass8(3.0), compass8(44.0), compass8(181.0), compass8(-10.0)), ("N", "NE", "S", "N"));
    }
}
