//! How much labels flicker while the map moves: flies one camera path (zoom in, pan, zoom
//! out) and counts, frame to frame, labels that appear, vanish while still on screen, or hop
//! to another spot; once with labels placed from scratch every frame, once sticky.
//!
//!     cargo run --release --example label_churn [-- STYLE]      # default: liberty

use mapterm::{Camera, LabelPlacement, Map, Viewport};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

#[derive(Default, Debug)]
struct Churn {
    frames: usize,
    labels: usize,
    appeared: usize,
    vanished: usize,
    /// Vanished while still offered: lost the collision.
    lost: usize,
    moved: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let style = std::env::args().nth(1).unwrap_or_else(|| "liberty".into());
    let area = Rect::new(0, 0, 160, 45);
    // Georgetown: zoom 16 -> 17, pan east, zoom out to 15.5.
    let (lon, lat) = (-77.0650, 38.9045);
    let mut path = Vec::new();
    for i in 0..=40 {
        path.push(Camera::new(lon, lat, 16.0 + i as f64 / 40.0));
    }
    let mut cam = Camera::new(lon, lat, 17.0);
    for _ in 0..60 {
        cam.pan_dots(3.0, 0.0);
        path.push(cam);
    }
    for i in 0..=40 {
        path.push(Camera::new(cam.lon, cam.lat, 17.0 - 1.5 * i as f64 / 40.0));
    }
    for placement in [LabelPlacement::Fresh, LabelPlacement::Sticky] {
        let mut map = Map::from_style(&style).await?;
        map.label_placement = placement;
        let mut c = Churn::default();
        // Per phase: zoom in (frames 1-40), pan (41-100), zoom out (101-141).
        let mut phases = [Churn::default(), Churn::default(), Churn::default()];
        let mut prev: Vec<(String, f64, f64)> = Vec::new();
        for cam in &path {
            let _ = map.load_tiles(cam, area).await;
            let mut buf = Buffer::empty(area);
            map.view(cam).render(area, &mut buf);
            let now = map.placed_labels();
            let vp = Viewport::new(cam, area);
            let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
            let on = |p: (f64, f64)| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < w && p.1 < h;
            let dots = |v: &[(String, f64, f64)]| -> Vec<(String, (f64, f64))> { v.iter().map(|(k, lon, lat)| (k.clone(), vp.dot_ll(*lon, *lat))).collect() };
            let (before, after) = (dots(&prev), dots(&now));
            let (a0, v0, l0, m0) = (c.appeared, c.vanished, c.lost, c.moved);
            if c.frames > 0 {
                for (k, p) in &after {
                    let same: Vec<f64> = before.iter().filter(|(q, _)| q == k).map(|(_, b)| (p.0 - b.0).hypot(p.1 - b.1)).collect();
                    match same.iter().copied().reduce(f64::min) {
                        None => c.appeared += 1,
                        Some(d) if d > 3.0 => c.moved += 1,
                        _ => {}
                    }
                }
                let offered = map.offered_labels();
                for (k, p) in &before {
                    if on(*p) && !after.iter().any(|(q, _)| q == k) {
                        c.vanished += 1;
                        if offered.contains(k) {
                            c.lost += 1;
                        }
                    }
                }
            }
            if c.frames > 0 {
                let ph = &mut phases[if c.frames <= 40 {
                    0
                } else if c.frames <= 100 {
                    1
                } else {
                    2
                }];
                ph.frames += 1;
                ph.appeared += c.appeared - a0;
                ph.vanished += c.vanished - v0;
                ph.lost += c.lost - l0;
                ph.moved += c.moved - m0;
            }
            c.frames += 1;
            c.labels += now.len();
            prev = now;
        }
        let per = |n: usize| n as f64 / (c.frames - 1) as f64;
        println!(
            "{placement:?}: {} frames, {:.0} labels a frame; per frame: {:.2} appeared, {:.2} vanished on screen ({:.2} lost a collision), {:.2} moved",
            c.frames,
            c.labels as f64 / c.frames as f64,
            per(c.appeared),
            per(c.vanished),
            per(c.lost),
            per(c.moved)
        );
        for (name, p) in ["zoom in", "pan", "zoom out"].iter().zip(&phases) {
            println!("{}", phase_line(name, p));
        }
    }
    Ok(())
}

fn phase_line(name: &str, p: &Churn) -> String {
    let per = |n: usize| n as f64 / p.frames.max(1) as f64;
    format!("  {name:<9} appeared {:.2}  vanished {:.2} (lost {:.2})  moved {:.2}", per(p.appeared), per(p.vanished), per(p.lost), per(p.moved))
}
