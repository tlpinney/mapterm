//! Positions that change over time, sampled and interpolated (Cesium's
//! `SampledPositionProperty`, in small).
//!
//! A [`SampledPosition`] keeps the recent fixes of one moving thing, each tagged with the time
//! it was true, and answers "where was it at time `t`". Between two samples it interpolates
//! linearly; after the last one it extrapolates along a velocity the caller gives (dead
//! reckoning), for a limited time; before the first it stays at the first.
//!
//! Live feeds arrive in steps (an aircraft every few seconds), so a viewer draws them a little
//! in the past, at `now - delay` with `delay` about one update interval: then nearly every
//! frame falls between two real samples, and things move smoothly through turns instead of
//! overshooting and snapping back. The samples also make trails ([`SampledPosition::trail`]).
//!
//! Times are seconds on any clock the caller keeps consistent (the viewer uses Unix time).

use std::collections::VecDeque;

/// One fix: where something was at time `t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Seconds.
    pub t: f64,
    /// Longitude, degrees.
    pub lon: f64,
    /// Latitude, degrees.
    pub lat: f64,
    /// Altitude in any unit, if known (interpolated like the position).
    pub alt: Option<f64>,
}

/// Speed and direction over the ground, for extrapolating past the last sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Velocity {
    /// Meters per second.
    pub speed: f64,
    /// Degrees clockwise from true north.
    pub bearing: f64,
}

/// The recent samples of one moving thing, oldest first.
#[derive(Clone, Debug)]
pub struct SampledPosition {
    samples: VecDeque<Sample>,
    /// Samples older than this many seconds before the newest are dropped.
    keep: f64,
    /// At most this many seconds of extrapolation past the last sample.
    extrapolate: f64,
    /// The velocity to extrapolate with, if any.
    velocity: Option<Velocity>,
}

const EARTH_M: f64 = 6_371_000.0;

impl SampledPosition {
    /// Keep `keep` seconds of samples; extrapolate up to `extrapolate` seconds past the last.
    pub fn new(keep: f64, extrapolate: f64) -> Self {
        Self { samples: VecDeque::new(), keep, extrapolate, velocity: None }
    }

    /// Add a sample. Samples may come out of order; one within 10 ms of an existing time
    /// replaces it (a feed repeating the same fix).
    pub fn push(&mut self, s: Sample) {
        if !(s.t.is_finite() && s.lon.is_finite() && s.lat.is_finite()) {
            return;
        }
        let at = self.samples.partition_point(|x| x.t < s.t - 0.01);
        match self.samples.get_mut(at) {
            Some(x) if (x.t - s.t).abs() <= 0.01 => *x = s,
            _ => self.samples.insert(at, s),
        }
        if let Some(newest) = self.samples.back().map(|x| x.t) {
            while self.samples.len() > 2 && self.samples.front().is_some_and(|x| x.t < newest - self.keep) {
                self.samples.pop_front();
            }
        }
    }

    /// Set (or clear) the velocity used past the last sample.
    pub fn set_velocity(&mut self, v: Option<Velocity>) {
        self.velocity = v.filter(|v| v.speed.is_finite() && v.bearing.is_finite());
    }

    /// The samples, oldest first.
    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }

    /// The newest sample.
    pub fn last(&self) -> Option<&Sample> {
        self.samples.back()
    }

    /// Where it was at time `t` (`None` with no samples).
    pub fn at(&self, t: f64) -> Option<Sample> {
        let first = *self.samples.front()?;
        let last = *self.samples.back()?;
        if t <= first.t {
            return Some(Sample { t, ..first });
        }
        if t >= last.t {
            let dt = (t - last.t).min(self.extrapolate);
            return Some(match self.velocity {
                Some(v) if dt > 0.0 => {
                    let (lon, lat) = travel(last.lon, last.lat, v.speed * dt, v.bearing);
                    Sample { t, lon, lat, alt: last.alt }
                }
                _ => Sample { t, ..last },
            });
        }
        let i = self.samples.partition_point(|x| x.t <= t);
        let (a, b) = (self.samples[i - 1], self.samples[i]);
        let f = (t - a.t) / (b.t - a.t);
        // The short way around, across the antimeridian too.
        let dlon = (b.lon - a.lon + 540.0).rem_euclid(360.0) - 180.0;
        let lon = (a.lon + dlon * f + 540.0).rem_euclid(360.0) - 180.0;
        let alt = match (a.alt, b.alt) {
            (Some(x), Some(y)) => Some(x + (y - x) * f),
            (x, y) => y.or(x),
        };
        Some(Sample { t, lon, lat: a.lat + (b.lat - a.lat) * f, alt })
    }

    /// The path up to time `t`, going back `seconds`: the samples in that span and the
    /// position at `t` itself, oldest first.
    pub fn trail(&self, t: f64, seconds: f64) -> Vec<(f64, f64)> {
        self.trail_samples(t, seconds).iter().map(|s| (s.lon, s.lat)).collect()
    }

    /// Like [`SampledPosition::trail`], with each point's time (to fade a trail with age).
    pub fn trail_samples(&self, t: f64, seconds: f64) -> Vec<Sample> {
        let mut out: Vec<Sample> = self.samples.iter().filter(|s| s.t >= t - seconds && s.t < t).copied().collect();
        out.extend(self.at(t));
        out
    }
}

/// Draw the trails of `paths` (each with its color) at time `t` into an RGBA image (not
/// premultiplied), `w` x `h` pixels, for a map drawn by `vp` in cells of `cell_px` pixels:
/// anti-aliased lines fading out over `seconds`, the newest end brightest. Meant to go under
/// the text as a kitty image, where braille trails would shimmer as they grow.
pub fn render_trails<'a>(
    paths: impl IntoIterator<Item = (&'a SampledPosition, (u8, u8, u8))>,
    t: f64,
    seconds: f64,
    vp: &crate::Viewport,
    w: u32,
    h: u32,
    cell_px: (f64, f64),
) -> Vec<u8> {
    use tiny_skia::{LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let Some(mut pm) = Pixmap::new(w.max(1), h.max(1)) else { return Vec::new() };
    // Braille dots are 2 per cell across and 4 down.
    let (sx, sy) = (cell_px.0 / 2.0, cell_px.1 / 4.0);
    let stroke = Stroke { width: (cell_px.0 / 5.0).clamp(1.5, 4.0) as f32, line_cap: LineCap::Round, ..Stroke::default() };
    for (path, (r, g, b)) in paths {
        let pts = path.trail_samples(t, seconds);
        for pair in pts.windows(2) {
            let (p, q) = (pair[0], pair[1]);
            let ((px, py), (qx, qy)) = (vp.dot_ll(p.lon, p.lat), vp.dot_ll(q.lon, q.lat));
            let mut pb = PathBuilder::new();
            pb.move_to((px * sx) as f32, (py * sy) as f32);
            pb.line_to((qx * sx) as f32, (qy * sy) as f32);
            let Some(line) = pb.finish() else { continue };
            let alpha = (1.0 - (t - q.t).max(0.0) / seconds).clamp(0.0, 1.0) * 0.85;
            let mut paint = Paint::default();
            paint.set_color_rgba8(r, g, b, (alpha * 255.0) as u8);
            paint.anti_alias = true;
            pm.stroke_path(&line, &paint, &stroke, Transform::identity(), None);
        }
    }
    pm.pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// The point `meters` from (`lon`, `lat`) along `bearing` (degrees from north), on a sphere
/// (flat-earth steps are fine for the few kilometers between samples).
pub fn travel(lon: f64, lat: f64, meters: f64, bearing: f64) -> (f64, f64) {
    let (b, la) = (bearing.to_radians(), lat.to_radians());
    let dlat = meters * b.cos() / EARTH_M;
    let dlon = meters * b.sin() / (EARTH_M * la.cos().max(0.01));
    ((lon + dlon.to_degrees() + 540.0).rem_euclid(360.0) - 180.0, (lat + dlat.to_degrees()).clamp(-90.0, 90.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, lon: f64, lat: f64) -> Sample {
        Sample { t, lon, lat, alt: Some(t * 100.0) }
    }

    #[test]
    fn interpolates_between_samples() {
        let mut p = SampledPosition::new(120.0, 30.0);
        p.push(s(10.0, 0.0, 0.0));
        p.push(s(20.0, 1.0, 2.0));
        let m = p.at(15.0).unwrap();
        assert_eq!((m.lon, m.lat, m.alt), (0.5, 1.0, Some(1500.0)));
        // Before the first sample it stays there; after the last, with no velocity, too.
        assert_eq!((p.at(0.0).unwrap().lon, p.at(99.0).unwrap().lat), (0.0, 2.0));
    }

    #[test]
    fn follows_a_turn_instead_of_overshooting() {
        // East, then north: at the corner's time it's at the corner, not past it.
        let mut p = SampledPosition::new(120.0, 30.0);
        for x in [s(0.0, 0.0, 0.0), s(10.0, 1.0, 0.0), s(20.0, 1.0, 1.0)] {
            p.push(x);
        }
        let c = p.at(10.0).unwrap();
        assert_eq!((c.lon, c.lat), (1.0, 0.0));
        assert_eq!(p.at(15.0).unwrap().lat, 0.5);
    }

    #[test]
    fn extrapolates_with_velocity_for_a_while() {
        let mut p = SampledPosition::new(120.0, 30.0);
        p.push(s(0.0, 0.0, 0.0));
        p.set_velocity(Some(Velocity { speed: 100.0, bearing: 0.0 }));
        let ten = p.at(10.0).unwrap();
        assert!((ten.lat - (1000.0 / EARTH_M).to_degrees()).abs() < 1e-9);
        assert_eq!(p.at(1000.0), Some(Sample { t: 1000.0, ..p.at(30.0).unwrap() }), "capped at 30 s");
    }

    #[test]
    fn keeps_order_dedupes_and_trims() {
        let mut p = SampledPosition::new(15.0, 0.0);
        for x in [s(10.0, 1.0, 0.0), s(0.0, 0.0, 0.0), s(20.0, 2.0, 0.0), s(20.005, 2.5, 0.0), s(30.0, 3.0, 0.0)] {
            p.push(x);
        }
        let ts: Vec<f64> = p.samples().map(|x| x.t).collect();
        assert_eq!(ts, vec![20.005, 30.0], "out-of-order inserted, the repeat replaced, older than 15 s dropped");
        assert_eq!(p.last().unwrap().lon, 3.0);
    }

    #[test]
    fn crosses_the_antimeridian_the_short_way() {
        let mut p = SampledPosition::new(120.0, 0.0);
        p.push(s(0.0, 179.0, 0.0));
        p.push(s(10.0, -179.0, 0.0));
        let m = p.at(5.0).unwrap().lon;
        assert!(m.abs() > 179.9, "{m}");
    }

    #[test]
    fn trail_ends_at_the_current_position() {
        let mut p = SampledPosition::new(120.0, 0.0);
        for x in [s(0.0, 0.0, 0.0), s(10.0, 1.0, 0.0), s(20.0, 2.0, 0.0)] {
            p.push(x);
        }
        assert_eq!(p.trail(15.0, 60.0), vec![(0.0, 0.0), (1.0, 0.0), (1.5, 0.0)]);
        assert_eq!(p.trail(15.0, 6.0), vec![(1.0, 0.0), (1.5, 0.0)]);
    }
}
