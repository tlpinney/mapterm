//! Any MapLibre style in your terminal: an OpenFreeMap name (liberty, positron, bright, dark,
//! fiord), a style URL or a file. Click a feature to see its layer. `q` quits.
//!
//!     cargo run --release --example style -- dark

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use mapterm::{Camera, Controller, Map, Viewport};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let spec = std::env::args().nth(1).unwrap_or_else(|| "liberty".into());
    let mut map = Map::from_style(&spec).await?;
    for w in map.warnings() {
        eprintln!("warning: {w}");
    }
    let mut ctl = Controller::new(Camera::new(-77.0353, 38.8895, 13.0), 0.0, 17.0);
    let mut status = format!("{spec}: {} layers", map.layer_ids().len());
    let mut term = ratatui::init();
    crossterm::execute!(std::io::stdout(), event::EnableMouseCapture)?;
    loop {
        ctl.tick(Instant::now());
        let area = term.get_frame().area();
        ctl.set_area(area);
        let _ = map.load_tiles(&ctl.cam, area).await;
        term.draw(|f| {
            f.render_widget(map.view(&ctl.cam), area);
            f.render_widget(ratatui::text::Line::from(status.as_str()), area);
        })?;
        if let Some(click) = ctl.take_click() {
            let dot = Viewport::new(&ctl.cam, area).dot_ll(click.lon, click.lat);
            let hits = map.query_rendered_features(&ctl.cam, area, dot, None);
            let names: Vec<String> = hits.iter().take(4).map(|p| format!("{}: {}", p.style_layer.as_deref().unwrap_or("?"), p.title())).collect();
            status = names.join(" | ");
        }
        if event::poll(Duration::from_millis(if ctl.animating() { 16 } else { 250 }))? {
            match event::read()? {
                Event::Key(k) if k.code == KeyCode::Char('q') => break,
                Event::Key(k) => _ = ctl.key(k),
                Event::Mouse(m) => _ = ctl.mouse(m),
                _ => {}
            }
        }
    }
    crossterm::execute!(std::io::stdout(), event::DisableMouseCapture)?;
    ratatui::restore();
    Ok(())
}
