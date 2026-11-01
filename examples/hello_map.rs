//! A map in your terminal in about 50 lines: OpenFreeMap tiles, a marker, MapLibre-style pan
//! and zoom (drag, scroll, double-click, arrows, +/-). `q` quits.
//!
//!     cargo run --release --example hello_map

use std::{collections::HashSet, time::Duration};

use crossterm::event::{self, Event, KeyCode};
use mapterm::{Camera, Controller, Layer, MapView, Markers, Overlay, Source, Style, TileStore, VectorTiles, Viewport, prepare};
use ratatui::style::{Color, Style as TStyle};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let src = Source::open(mapterm::source::OPENFREEMAP).await?;
    let mut store = TileStore::default();
    let mut tried = HashSet::new();
    let mut ctl = Controller::new(Camera::new(-77.0353, 38.8895, 13.0), 0.0, 17.0);
    let pin = [Overlay::Marker {
        lon: -77.0353,
        lat: 38.8895,
        glyph: "◆".into(),
        style: TStyle::new().fg(Color::Cyan),
        label: Some(("Washington Monument".into(), TStyle::new().fg(Color::Cyan))),
        priority: 100,
    }];

    let mut term = ratatui::init();
    crossterm::execute!(std::io::stdout(), event::EnableMouseCapture)?;
    loop {
        ctl.tick(std::time::Instant::now());
        let area = term.get_frame().area();
        ctl.set_area(area);
        // Fetch the tiles this view needs (a real app does this in the background).
        for id in Viewport::new(&ctl.cam, area).tiles(ctl.cam.tile_zoom(src.min_zoom, src.max_zoom)) {
            if tried.insert(id) && let Ok(Some(bytes)) = src.tile(id.0, id.1, id.2).await {
                store.insert(id, prepare(&bytes, &Style::default(), id.0)?);
            }
        }
        let tiles = VectorTiles::new(&store, src.min_zoom, src.max_zoom);
        let marks = Markers(&pin);
        let layers: [&dyn Layer; 2] = [&tiles, &marks];
        term.draw(|f| f.render_widget(MapView { camera: &ctl.cam, layers: &layers }, f.area()))?;

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
