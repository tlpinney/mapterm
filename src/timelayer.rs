//! Image layers that change with the [clock](crate::clock): one frame for each span of time,
//! such as the last two hours of weather radar in 10-minute scans.
//!
//! Each frame is its own raster source and layer in the [`Map`], hidden but the one for the
//! clock's time (Cesium keeps a tile set per interval the same way). [`TimeRaster::update`]
//! picks it each frame and asks the map to load the next one ahead
//! ([`Map::set_prefetch`]). Until a frame's tiles for the view are in, the one before stays
//! up, so stepping or playing never shows a blank map.
//!
//! While playing, a frame can fade into the next: between them, a blend of the two (made
//! here, a third and two thirds of the way) is shown in their place. Kitty images can't be
//! drawn half transparent, so the blend is a new image rather than two at half opacity.

use std::sync::Arc;

use anyhow::Result;
use ratatui::layout::Rect;

use crate::clock::Intervals;
use crate::raster::RasterSource;
use crate::spec::StyleLayer;
use crate::{Camera, Map, MapSource};

/// A time layer: its frames as map sources and layers, and which is shown.
pub struct TimeRaster {
    id: String,
    opacity: f64,
    /// Each frame's source and layer id, by time.
    frames: Intervals<String>,
    /// The frame on screen.
    shown: Option<usize>,
    /// Hidden altogether.
    off: bool,
    /// What the blend layer holds now: the frame fading out and the step.
    blend_key: Option<(String, u32)>,
}

/// Steps in a fade from one frame to the next (the blends are the ones between).
const FADE_STEPS: u32 = 3;

impl TimeRaster {
    /// A time layer called `id`, drawn at `opacity` (0 to 1). It has no frames until
    /// [`TimeRaster::set_frames`].
    pub fn new(id: impl Into<String>, opacity: f64) -> TimeRaster {
        TimeRaster { id: id.into(), opacity: opacity.clamp(0.0, 1.0), frames: Intervals::default(), shown: None, off: false, blend_key: None }
    }

    /// The frames: each one's source and layer id over its span of time.
    pub fn frames(&self) -> &Intervals<String> {
        &self.frames
    }

    /// The span of time the frames cover.
    pub fn span(&self) -> Option<(f64, f64)> {
        self.frames.span()
    }

    /// When the frame on screen starts.
    pub fn shown_time(&self) -> Option<f64> {
        Some(self.frames.all().get(self.shown?)?.start)
    }

    /// Replace the frames with `frames` (a time and its source each, in any order), each
    /// in force until the next and the last for `last_for` seconds. Frames already in the
    /// map keep their tiles; ones no longer listed are removed. New layers go on top of the
    /// map, or below layer `before`.
    pub fn set_frames(&mut self, map: &mut Map, frames: Vec<(f64, Arc<RasterSource>)>, last_for: f64, before: Option<&str>) -> Result<()> {
        let shown_id = self.shown.and_then(|i| self.frames.all().get(i)).map(|f| f.data.clone());
        let ids: Vec<(f64, String)> = frames.iter().map(|(t, _)| (*t, self.frame_id(*t))).collect();
        // Gone: out of the map.
        for f in self.frames.all() {
            if !ids.iter().any(|(_, id)| *id == f.data) {
                map.remove_layer(&f.data);
                map.remove_source(&f.data);
            }
        }
        // The layer blends go in: made once, filled here, never fetched (it's only shown
        // when every tile the view needs is in it).
        let blend = self.blend_id();
        if map.source(&blend).is_none()
            && let Some((_, src)) = frames.first()
        {
            map.add_source(&blend, MapSource::raster(src.clone()))?;
            let layer = StyleLayer::raster(blend.clone(), blend.clone());
            match before {
                Some(b) if map.layer_ids().contains(&b) => map.add_layer_before(layer, b)?,
                _ => map.add_layer(layer)?,
            }
            map.set_paint(&blend, "raster-opacity", &serde_json::json!(self.opacity))?;
            map.set_visibility(&blend, false)?;
        }
        // New: in, hidden.
        for ((_, src), (_, id)) in frames.into_iter().zip(&ids) {
            if map.source(id).is_some() {
                continue;
            }
            map.add_source(id, MapSource::raster(src))?;
            let layer = StyleLayer::raster(id.clone(), id.clone());
            match before {
                Some(b) if map.layer_ids().contains(&b) => map.add_layer_before(layer, b)?,
                _ => map.add_layer(layer)?,
            }
            map.set_paint(id, "raster-opacity", &serde_json::json!(self.opacity))?;
            map.set_visibility(id, false)?;
        }
        self.frames = Intervals::from_times(ids, last_for);
        self.shown = shown_id.and_then(|id| self.frames.all().iter().position(|f| f.data == id));
        Ok(())
    }

    fn blend_id(&self) -> String {
        format!("{}@blend", self.id)
    }

    fn frame_id(&self, t: f64) -> String {
        format!("{}@{}", self.id, t.round() as i64)
    }

    /// Show the frame for time `t` (the one in force, or the last before it), or the one
    /// before that while its tiles for this view are still loading; and ask the map to load
    /// the next frame ahead (`ahead` frames, while playing more than one). With `fade`, a
    /// frame fades into the next once both are in (not the last into the first). Returns the
    /// start of the frame on screen.
    pub fn update(&mut self, map: &mut Map, t: f64, ahead: usize, fade: bool, cam: &Camera, area: Rect) -> Option<f64> {
        let blend = self.blend_id();
        let all = self.frames.all();
        if all.is_empty() || self.off {
            map.set_prefetch(Vec::new());
            let _ = map.set_visibility(&blend, false);
            return None;
        }
        let want = self.frames.at_or_before(t)?;
        // The frame wanted if it's ready (or nothing is up yet), else hold what's up.
        let ready = map.raster_ready(&all[want].data, cam, area);
        let show = if ready || self.shown.is_none() { want } else { self.shown.unwrap_or(want) };
        // Fading: on the way from this frame to the next, both in, a blend in its place.
        let step = (self.frames.fraction(want, t) * FADE_STEPS as f64).floor() as u32;
        let fading = fade && ready && show == want && want + 1 < all.len() && step > 0 && map.raster_ready(&all[want + 1].data, cam, area);
        if fading {
            let (a, b) = (all[want].data.clone(), all[want + 1].data.clone());
            let key = (a.clone(), step);
            // A new step starts the layer afresh; the same step only fills what's missing
            // (tiles a pan brought into view).
            let mut clear = self.blend_key.as_ref() != Some(&key);
            for id in map.raster_view_tiles(&a, cam, area) {
                if !clear && map.raster_tile(&blend, id).is_some() {
                    continue;
                }
                if let (Some(ta), Some(tb)) = (map.raster_tile(&a, id), map.raster_tile(&b, id))
                    && let Ok(mixed) = crate::raster::blend(&ta, &tb, step as f64 / FADE_STEPS as f64)
                {
                    map.put_raster_tile(&blend, Some((id, mixed)), clear);
                    clear = false;
                }
            }
            if clear {
                map.put_raster_tile(&blend, None, true);
            }
            self.blend_key = Some(key);
        }
        let all = self.frames.all();
        for (i, f) in all.iter().enumerate() {
            let _ = map.set_visibility(&f.data, i == show && !fading);
        }
        let _ = map.set_visibility(&blend, fading);
        self.shown = Some(show);
        // Load the one wanted, then the ones after it (wrapping, as a loop does).
        let mut fetch = Vec::new();
        if want != show {
            fetch.push(all[want].data.clone());
        }
        for k in 1..=ahead.min(all.len().saturating_sub(1)) {
            fetch.push(all[(want + k) % all.len()].data.clone());
        }
        map.set_prefetch(fetch);
        Some(all[show].start)
    }

    /// Hide the layer, or show it again.
    pub fn set_off(&mut self, map: &mut Map, off: bool) {
        self.off = off;
        if off {
            for f in self.frames.all() {
                let _ = map.set_visibility(&f.data, false);
            }
            map.set_prefetch(Vec::new());
            self.shown = None;
        }
    }

    /// Whether it's hidden.
    pub fn is_off(&self) -> bool {
        self.off
    }

    /// Whether `source` is one of its frames (for request limits and the like).
    pub fn owns(&self, source: &str) -> bool {
        source.strip_prefix(self.id.as_str()).is_some_and(|r| r.starts_with('@'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Loaded;

    fn src(t: i64) -> Arc<RasterSource> {
        let mut s = RasterSource::new(&format!("https://radar.example/{t}/{{z}}/{{x}}/{{y}}.png"), false, 2).unwrap();
        s.overlay = true;
        Arc::new(s)
    }

    fn visible(map: &Map) -> Vec<String> {
        map.layer_ids().into_iter().filter(|id| map.layer(id).is_some_and(|l| l.visible)).map(str::to_string).collect()
    }

    #[test]
    fn shows_the_frame_for_the_time_and_holds_until_the_next_is_in() {
        let mut map = Map::new();
        let mut radar = TimeRaster::new("radar", 0.7);
        radar.set_frames(&mut map, vec![(1200.0, src(1200)), (0.0, src(0)), (600.0, src(600))], 600.0, None).unwrap();
        assert_eq!(radar.span(), Some((0.0, 1800.0)));
        let (cam, area) = (Camera::new(0.0, 0.0, 0.5), Rect::new(0, 0, 40, 20));
        // Nothing up yet: the frame wanted goes up straight away, and the next is asked for.
        assert_eq!(radar.update(&mut map, 650.0, 1, false, &cam, area), Some(600.0));
        assert_eq!(visible(&map), vec!["radar@600"]);
        let reqs = map.tile_requests(&cam, area);
        assert!(reqs.iter().any(|r| r.source == "radar@600") && reqs.iter().any(|r| r.source == "radar@1200"), "the shown one, then the next ahead");
        assert!(!reqs.iter().any(|r| r.source == "radar@0"), "not the others");
        let first = reqs.iter().position(|r| r.source == "radar@1200").unwrap();
        assert!(reqs[..first].iter().all(|r| r.source == "radar@600"), "the view's frame first");
        // The clock moves on, but the next frame's tiles aren't in: the last one stays up.
        assert_eq!(radar.update(&mut map, 1300.0, 1, false, &cam, area), Some(600.0));
        assert_eq!(visible(&map), vec!["radar@600"]);
        // Its tiles arrive: now it shows.
        for r in map.tile_requests(&cam, area).into_iter().filter(|r| r.source == "radar@1200") {
            map.insert_loaded(&r.source, r.id, Ok(Loaded::empty_raster())).unwrap();
        }
        assert_eq!(radar.update(&mut map, 1300.0, 1, false, &cam, area), Some(1200.0));
        assert_eq!(visible(&map), vec!["radar@1200"]);
        assert!(radar.owns("radar@1200") && !radar.owns("radarx@1") && !radar.owns("other"));
    }

    #[test]
    fn new_frames_replace_old_ones() {
        let mut map = Map::new();
        let mut radar = TimeRaster::new("radar", 1.0);
        radar.set_frames(&mut map, vec![(0.0, src(0)), (600.0, src(600))], 600.0, None).unwrap();
        // Ten minutes on: the oldest scan drops off and a new one arrives.
        radar.set_frames(&mut map, vec![(600.0, src(600)), (1200.0, src(1200))], 600.0, None).unwrap();
        assert!(map.source("radar@0").is_none() && map.layer("radar@0").is_none());
        assert!(map.source("radar@600").is_some() && map.source("radar@1200").is_some());
        assert_eq!(radar.frames().len(), 2);
        radar.set_off(&mut map, true);
        assert!(visible(&map).is_empty());
        let (cam, area) = (Camera::new(0.0, 0.0, 0.5), Rect::new(0, 0, 40, 20));
        assert_eq!(radar.update(&mut map, 700.0, 1, false, &cam, area), None, "off: nothing shown or fetched");
        assert!(map.tile_requests(&cam, area).is_empty());
    }

    #[test]
    fn fades_one_frame_into_the_next_while_playing() {
        let mut map = Map::new();
        let mut radar = TimeRaster::new("radar", 1.0);
        radar.set_frames(&mut map, vec![(0.0, src(0)), (600.0, src(600))], 600.0, None).unwrap();
        let (cam, area) = (Camera::new(0.0, 0.0, 0.5), Rect::new(0, 0, 40, 20));
        radar.update(&mut map, 0.0, 1, true, &cam, area);
        // Both frames' tiles in.
        for r in map.tile_requests(&cam, area) {
            map.insert_loaded(&r.source, r.id, Ok(Loaded::empty_raster())).unwrap();
        }
        assert!(map.tile_requests(&cam, area).is_empty(), "the blend layer is never fetched");
        radar.update(&mut map, 100.0, 1, true, &cam, area);
        assert_eq!(visible(&map), vec!["radar@0"], "the first third: the frame itself");
        radar.update(&mut map, 300.0, 1, true, &cam, area);
        assert_eq!(visible(&map), vec!["radar@blend"], "half way: the blend");
        let tiles = map.raster_view_tiles("radar@0", &cam, area);
        assert!(tiles.iter().all(|t| map.raster_tile("radar@blend", *t).is_some()), "every tile the view needs");
        assert!(map.tile_requests(&cam, area).is_empty());
        radar.update(&mut map, 300.0, 1, false, &cam, area);
        assert_eq!(visible(&map), vec!["radar@0"], "not playing: no fade");
        radar.update(&mut map, 900.0, 1, true, &cam, area);
        assert_eq!(visible(&map), vec!["radar@600"], "the last frame doesn't fade into the first");
    }

    #[test]
    fn an_overlay_isnt_imagery() {
        let mut map = Map::new();
        let mut radar = TimeRaster::new("radar", 1.0);
        radar.set_frames(&mut map, vec![(0.0, src(0))], 600.0, None).unwrap();
        radar.update(&mut map, 0.0, 0, false, &Camera::new(0.0, 0.0, 1.0), Rect::new(0, 0, 40, 20));
        assert!(!map.base_imagery_shown(1.0), "the map stays painted under radar");
    }
}
