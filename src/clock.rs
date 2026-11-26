//! Time for the map, in the shape of Cesium's: one [`Clock`] says what time it is, and
//! anything that changes over time asks it. Layers that follow the clock (radar frames, dated
//! imagery) show that moment; live feeds keep to real time whatever it says.
//!
//! The clock is **live** (its time is real time) until it's moved: played over a range (the
//! last two hours of radar), paused, or stepped. [`Clock::go_live`] returns it to real time.
//!
//! [`Intervals`] are spans of time each carrying data (Cesium's `TimeIntervalCollection`):
//! "from 22:00 to 22:10 the radar frame is /v2/radar/123". They answer what's in force at a
//! time, and where the next and previous ones start, for stepping.
//!
//! Times are seconds on one clock the caller keeps consistent: Unix seconds in the viewer.

/// What a playing clock does when it reaches the end of its range (Cesium's `ClockRange`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    /// Back to the start and on: a loop.
    #[default]
    Loop,
    /// Stop at the end (and pause).
    Clamped,
    /// No end: time runs on past it.
    Unbounded,
}

/// The map's time.
#[derive(Clone, Debug, PartialEq)]
pub struct Clock {
    /// The start of the range it plays over.
    pub start: f64,
    /// The end of that range.
    pub stop: f64,
    /// What happens at the end.
    pub range: Range,
    /// How many seconds of map time pass per second of real time while playing; negative
    /// plays backward.
    pub multiplier: f64,
    current: f64,
    playing: bool,
    live: bool,
}

impl Clock {
    /// A live clock: its time is real time (`now`) until it's moved.
    pub fn live(now: f64) -> Clock {
        Clock { start: now, stop: now, range: Range::Loop, multiplier: 1.0, current: now, playing: false, live: true }
    }

    /// The time it says.
    pub fn time(&self) -> f64 {
        self.current
    }

    /// Whether its time is real time.
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Whether it's playing (moving at [`Clock::multiplier`]).
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Play over `start..stop` at `multiplier`, from where it is if that's inside, else from
    /// the start.
    pub fn play(&mut self, start: f64, stop: f64, range: Range, multiplier: f64) {
        (self.start, self.stop, self.range, self.multiplier) = (start, stop.max(start), range, multiplier);
        if !(start..=stop).contains(&self.current) || self.current >= stop {
            self.current = start;
        }
        (self.playing, self.live) = (true, false);
    }

    /// Stop where it is.
    pub fn pause(&mut self) {
        self.playing = false;
        self.live = false;
    }

    /// Carry on playing over the range it had (from the other end when it stopped at one).
    pub fn resume(&mut self) {
        if self.stop > self.start {
            if self.range != Range::Unbounded {
                if self.multiplier >= 0.0 && self.current >= self.stop {
                    self.current = self.start;
                } else if self.multiplier < 0.0 && self.current <= self.start {
                    self.current = self.stop;
                }
            }
            (self.playing, self.live) = (true, false);
        }
    }

    /// Go to `t` and stay there (paused).
    pub fn seek(&mut self, t: f64) {
        self.current = t;
        self.playing = false;
        self.live = false;
    }

    /// Back to real time.
    pub fn go_live(&mut self, now: f64) {
        (self.current, self.playing, self.live) = (now, false, true);
    }

    /// Play backward if it was playing forward, and forward if backward (Cesium's reverse
    /// button), keeping the speed; playing either way afterwards.
    pub fn reverse(&mut self) {
        self.multiplier = -self.multiplier;
        self.resume();
    }

    /// Play at `speed` (map seconds per real second, the sign kept: backward stays backward).
    pub fn set_speed(&mut self, speed: f64) {
        self.multiplier = speed.abs() * if self.multiplier < 0.0 { -1.0 } else { 1.0 };
    }

    /// Move on: to `now` when live; by `elapsed` real seconds times the multiplier when
    /// playing, then wrapped, held or let run at either end. Returns the time it now says.
    pub fn tick(&mut self, now: f64, elapsed: f64) -> f64 {
        if self.live {
            self.current = now;
        } else if self.playing {
            self.current += elapsed.max(0.0) * self.multiplier;
            let span = self.stop - self.start;
            let past = if self.multiplier >= 0.0 { self.current >= self.stop } else { self.current <= self.start };
            if past {
                match self.range {
                    Range::Loop if span > 0.0 => self.current = self.start + (self.current - self.start).rem_euclid(span),
                    Range::Loop | Range::Clamped => {
                        self.current = if self.multiplier >= 0.0 { self.stop } else { self.start };
                        self.playing = false;
                    }
                    Range::Unbounded => {}
                }
            }
        }
        self.current
    }
}

/// A span of time, `start..stop`, with data.
#[derive(Clone, Debug, PartialEq)]
pub struct Interval<T> {
    /// When it starts (in force from here).
    pub start: f64,
    /// When it ends (not in force from here).
    pub stop: f64,
    /// What it carries.
    pub data: T,
}

/// Spans of time in order, not overlapping, each with data (Cesium's
/// `TimeIntervalCollection`).
#[derive(Clone, Debug, PartialEq)]
pub struct Intervals<T> {
    items: Vec<Interval<T>>,
}

impl<T> Default for Intervals<T> {
    fn default() -> Self {
        Intervals { items: Vec::new() }
    }
}

impl<T> Intervals<T> {
    /// From moments each in force until the next: `(time, data)` in any order, the last
    /// lasting `last_for` seconds. Frames of a loop, scans of a radar.
    pub fn from_times(mut times: Vec<(f64, T)>, last_for: f64) -> Intervals<T> {
        times.sort_by(|a, b| a.0.total_cmp(&b.0));
        times.dedup_by(|b, a| a.0 == b.0);
        let stops: Vec<f64> = times.iter().skip(1).map(|t| t.0).chain(times.last().map(|t| t.0 + last_for.max(0.0))).collect();
        let items = times.into_iter().zip(stops).map(|((start, data), stop)| Interval { start, stop, data }).collect();
        Intervals { items }
    }

    /// All of them, in order.
    pub fn all(&self) -> &[Interval<T>] {
        &self.items
    }

    /// How many there are.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Where the first starts and the last stops.
    pub fn span(&self) -> Option<(f64, f64)> {
        Some((self.items.first()?.start, self.items.last()?.stop))
    }

    /// The index of the one in force at `t`.
    pub fn index_at(&self, t: f64) -> Option<usize> {
        let i = self.items.partition_point(|iv| iv.start <= t).checked_sub(1)?;
        (t < self.items[i].stop).then_some(i)
    }

    /// The one in force at `t`.
    pub fn at(&self, t: f64) -> Option<&Interval<T>> {
        self.index_at(t).map(|i| &self.items[i])
    }

    /// The one in force at `t`, else the nearest before it, else the first: what to show
    /// when the clock is just past the end or in a gap.
    pub fn at_or_before(&self, t: f64) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        Some(self.items.partition_point(|iv| iv.start <= t).saturating_sub(1))
    }

    /// How far `t` is through interval `i`, 0 to 1: for blending one into the next.
    pub fn fraction(&self, i: usize, t: f64) -> f64 {
        self.items.get(i).map_or(0.0, |iv| if iv.stop > iv.start { ((t - iv.start) / (iv.stop - iv.start)).clamp(0.0, 1.0) } else { 0.0 })
    }

    /// The start of the interval `steps` away from the one in force at (or just before) `t`,
    /// held at the first and last: stepping frame by frame.
    pub fn step(&self, t: f64, steps: i64) -> Option<f64> {
        let i = self.at_or_before(t)? as i64;
        let j = (i + steps).clamp(0, self.items.len() as i64 - 1) as usize;
        Some(self.items[j].start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_until_moved() {
        let mut c = Clock::live(1000.0);
        assert!(c.is_live());
        assert_eq!(c.tick(1005.0, 5.0), 1005.0, "live: real time");
        c.seek(900.0);
        assert!(!c.is_live() && !c.is_playing());
        assert_eq!(c.tick(1010.0, 5.0), 900.0, "paused: stays");
        c.go_live(1020.0);
        assert_eq!(c.tick(1021.0, 1.0), 1021.0);
    }

    #[test]
    fn plays_and_loops() {
        let mut c = Clock::live(0.0);
        // Two hours in a loop at 600x: ten minutes of radar a second.
        c.play(0.0, 7200.0, Range::Loop, 600.0);
        assert_eq!(c.time(), 0.0, "now is past the range: from its start");
        assert_eq!(c.tick(99.0, 1.0), 600.0);
        assert_eq!(c.tick(99.0, 11.0), 0.0, "600 + 6600 = 7200 wraps to the start");
        assert_eq!(c.tick(99.0, 1.5), 900.0);
        assert!(c.is_playing(), "a loop keeps playing");
        let mut c = Clock::live(0.0);
        c.play(0.0, 100.0, Range::Clamped, 10.0);
        c.tick(0.0, 20.0);
        assert_eq!(c.time(), 100.0);
        assert!(!c.is_playing(), "clamped: stops at the end");
        c.resume();
        assert_eq!(c.time(), 0.0, "and starts over when resumed");
        let mut c = Clock::live(0.0);
        c.play(0.0, 100.0, Range::Unbounded, 10.0);
        assert_eq!(c.tick(0.0, 20.0), 200.0);
    }

    #[test]
    fn plays_backward_and_changes_speed() {
        let mut c = Clock::live(0.0);
        c.play(0.0, 100.0, Range::Loop, 10.0);
        c.tick(0.0, 3.0);
        c.reverse();
        assert!(c.is_playing() && c.multiplier < 0.0);
        assert_eq!(c.tick(0.0, 1.0), 20.0, "30 back by 10");
        assert_eq!(c.tick(0.0, 3.0), 90.0, "past the start: wraps to the end");
        c.set_speed(40.0);
        assert_eq!(c.multiplier, -40.0, "still backward");
        let mut c = Clock::live(0.0);
        c.play(0.0, 100.0, Range::Clamped, -50.0);
        assert_eq!(c.time(), 0.0);
        c.resume();
        assert_eq!(c.time(), 100.0, "backward from the start: from the end");
        c.tick(0.0, 3.0);
        assert_eq!(c.time(), 0.0);
        assert!(!c.is_playing(), "clamped: stops at the start");
    }

    #[test]
    fn plays_from_where_it_is() {
        let mut c = Clock::live(0.0);
        c.seek(50.0);
        c.play(0.0, 100.0, Range::Loop, 1.0);
        assert_eq!(c.time(), 50.0);
    }

    fn frames() -> Intervals<&'static str> {
        // Out of order, with a repeat: sorted and deduplicated.
        Intervals::from_times(vec![(600.0, "b"), (0.0, "a"), (1200.0, "c"), (600.0, "b again")], 600.0)
    }

    #[test]
    fn intervals_answer_for_a_time() {
        let f = frames();
        assert_eq!(f.len(), 3);
        assert_eq!(f.span(), Some((0.0, 1800.0)));
        assert_eq!(f.at(0.0).map(|i| i.data), Some("a"));
        assert_eq!(f.at(599.9).map(|i| i.data), Some("a"));
        assert_eq!(f.at(600.0).map(|i| i.data), Some("b"), "starts are in, stops are out");
        assert_eq!(f.at(1799.0).map(|i| i.data), Some("c"));
        assert_eq!(f.at(1800.0), None, "past the end");
        assert_eq!(f.at(-1.0), None, "before the start");
        assert_eq!(f.at_or_before(5000.0), Some(2), "past the end: the last");
        assert_eq!(f.at_or_before(-1.0), Some(0), "before: the first");
        assert_eq!(f.fraction(1, 900.0), 0.5);
    }

    #[test]
    fn steps_frame_by_frame() {
        let f = frames();
        assert_eq!(f.step(700.0, 1), Some(1200.0));
        assert_eq!(f.step(700.0, -1), Some(0.0));
        assert_eq!(f.step(700.0, 5), Some(1200.0), "held at the last");
        assert_eq!(f.step(0.0, -1), Some(0.0), "and the first");
        assert_eq!(Intervals::<()>::default().step(0.0, 1), None);
    }
}
