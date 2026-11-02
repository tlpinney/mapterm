//! A map in your terminal in about 50 lines: OpenFreeMap vector tiles drawn by a few style
//! layers, a GeoJSON pin that lights up under the pointer, and MapLibre-style pan and zoom
//! (drag, scroll, double-click, arrows, +/-). Click the pin or a place name. `q` quits.
//!
//!     cargo run --release --example hello_map

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use mapterm::{
    Camera, Controller, Map, MapSource, Source, StyleLayer,
    events::{Events, MapEvent},
};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut map = Map::new();
    map.add_source("omt", MapSource::vector(Source::open(mapterm::source::OPENFREEMAP).await?))?;
    map.add_source(
        "pin",
        MapSource::geojson(&json!({"type": "Feature", "id": 1, "properties": {"name": "Washington Monument"},
        "geometry": {"type": "Point", "coordinates": [-77.0353, 38.8895]}}))?,
    )?;
    map.add_layer(StyleLayer::background("bg").color("#101418"))?;
    map.add_layer(StyleLayer::fill("park", "omt").source_layer("park").color("#1a3220"))?;
    map.add_layer(StyleLayer::fill("water", "omt").source_layer("water").color("#16304a"))?;
    map.add_layer(StyleLayer::line("roads", "omt").source_layer("transportation").color(json!([
        "match",
        ["get", "class"],
        "motorway",
        "#eb6e3c",
        "trunk",
        "#e1a046",
        "primary",
        "#d2c36e",
        "#5a5a5a"
    ])))?;
    map.add_layer(StyleLayer::symbol("places", "omt").source_layer("place").text(json!(["get", "name"])).sort_key(json!(["get", "rank"])).color("#e1e1e1"))?;
    map.add_layer(StyleLayer::symbol("pin", "pin").glyph("◆").text("{name}").color(json!([
        "case",
        ["boolean", ["feature-state", "hover"], false],
        "yellow",
        "cyan"
    ])))?;
    let mut events = Events::new(&["pin", "places"]).hover_state("hover");

    let mut ctl = Controller::new(Camera::new(-77.0353, 38.8895, 13.0), 0.0, 17.0);
    let mut status = String::new();
    let mut term = ratatui::init();
    crossterm::execute!(std::io::stdout(), event::EnableMouseCapture)?;
    loop {
        ctl.tick(Instant::now());
        let area = term.get_frame().area();
        ctl.set_area(area);
        let _ = map.load_tiles(&ctl.cam, area).await; // a real app loads in the background
        term.draw(|f| {
            f.render_widget(map.view(&ctl.cam), area);
            f.render_widget(ratatui::text::Line::from(status.as_str()), area);
        })?;
        events.pointer(&mut map, &ctl.cam, area, ctl.pointer()); // hover: the pin turns yellow
        if let Some(click) = ctl.take_click() {
            for e in events.click(&map, &ctl.cam, area, &click) {
                if let MapEvent::Click { layer, features, .. } = e {
                    status = format!("{layer}: {}", features[0].title());
                }
            }
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
