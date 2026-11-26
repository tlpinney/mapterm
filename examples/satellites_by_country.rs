//! Every active satellite, colored by the country that owns it, with a legend: the library's
//! satellites, coloring and legend in about 70 lines. Orbits and owners come from CelesTrak
//! (cached in ~/.cache/mapterm/celestrak: elements for 2 hours, the catalog for a day).
//! Arrows or drag pan, scroll or +/- zoom, `q` quits.
//!
//!     cargo run --release --example satellites_by_country

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode};
use mapterm::legend::Corner;
use mapterm::satellites::{self, Coloring};
use mapterm::{Camera, Controller, Map, MapSource};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The satellites and who owns them.
    let mut sats = satellites::load("active").await?.sats;
    let owners = satellites::load_catalog("active").await?;
    for s in &mut sats {
        s.catalog = owners.get(&s.norad).cloned();
    }
    // The six owners with the most satellites get a color each (Starlink on its own);
    // everyone else is gray.
    let coloring = Coloring::by_country(&sats);
    let legend = coloring.legend(&sats);

    // A dark basemap, and the satellites over it (each point carries its color).
    let mut map = Map::from_style("mapterm-dark").await?;
    let now = || SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    map.add_source("sats", MapSource::geojson(&satellites::points(&sats, now(), &coloring))?)?;
    map.add_layer(satellites::layer("sats", "sats"))?;

    let mut ctl = Controller::new(Camera::new(-40.0, 20.0, 1.5), 0.0, 12.0);
    let mut term = ratatui::init();
    crossterm::execute!(std::io::stdout(), event::EnableMouseCapture)?;
    let mut moved = Instant::now();
    loop {
        ctl.tick(Instant::now());
        let area = term.get_frame().area();
        ctl.set_area(area);
        // Satellites move: place them again twice a second.
        if moved.elapsed() >= Duration::from_millis(500) {
            moved = Instant::now();
            map.set_data("sats", &satellites::points(&sats, now(), &coloring))?;
        }
        let _ = map.load_tiles(&ctl.cam, area).await; // a real app loads in the background
        term.draw(|f| {
            f.render_widget(map.view(&ctl.cam), area);
            legend.render_in(f.buffer_mut(), area, Corner::BottomLeft);
        })?;
        if event::poll(Duration::from_millis(if ctl.animating() { 16 } else { 100 }))? {
            match event::read()? {
                Event::Key(k) if k.code == KeyCode::Char('q') => break,
                Event::Key(k) => {
                    ctl.key(k);
                }
                Event::Mouse(m) => {
                    ctl.mouse(m);
                }
                _ => {}
            }
        }
    }
    let _ = crossterm::execute!(std::io::stdout(), event::DisableMouseCapture);
    ratatui::restore();
    Ok(())
}
