//! Render a real OpenFreeMap tile (z14 over the National Mall) with several styles and compare
//! with stored snapshots: the cell text, plus how many cells use each color pair.
//! Regenerate with `UPDATE_SNAPSHOTS=1 cargo test --test snapshots` and review the diff.

use std::{collections::BTreeMap, fmt::Write};

use mapterm::{Camera, Map, MapSource, stylejson};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

const TILE: (u8, u32, u32) = (14, 4686, 6268);

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn map_for(style: &serde_json::Value, source: &str) -> Map {
    let mut map = Map::new();
    map.add_source(source, MapSource::vector_tiles(0, 14)).unwrap();
    map.add_style_layers(style).unwrap();
    map.insert_tile(source, TILE, Some(&fixture("omt-14-4686-6268.pbf"))).unwrap();
    map
}

fn snapshot(map: &Map) -> String {
    let area = Rect::new(0, 0, 100, 36);
    // The tile's center, overzoomed a little so the view stays inside it.
    let cam = Camera::new(-77.025146484375, 38.882481197550796, 14.6);
    let mut buf = Buffer::empty(area);
    map.view(&cam).render(area, &mut buf);
    let mut out = String::new();
    let mut colors: BTreeMap<String, usize> = BTreeMap::new();
    for y in 0..area.height {
        for x in 0..area.width {
            let c = &buf[(x, y)];
            out.push_str(c.symbol());
            *colors.entry(format!("fg {:?} bg {:?}", c.fg, c.bg)).or_default() += 1;
        }
        out.push('\n');
    }
    out.push_str("\n-- cells per color --\n");
    for (k, n) in colors {
        let _ = writeln!(out, "{n:5} {k}");
    }
    out
}

fn check(name: &str, got: String) {
    let path = format!("{}/tests/snapshots/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(got == want, "{name} differs from {path}; rerun with UPDATE_SNAPSHOTS=1 and review\n{got}");
}

#[test]
fn openfreemap_styles() {
    for name in ["liberty", "positron", "dark"] {
        let doc: serde_json::Value = serde_json::from_slice(&fixture(&format!("openfreemap-{name}.json"))).unwrap();
        check(name, snapshot(&map_for(&doc, "openmaptiles")));
    }
}

#[test]
fn terminal_styles() {
    for (name, dark) in [("mapterm-dark", true), ("mapterm-light", false)] {
        let doc = stylejson::terminal_style(stylejson::Schema::OpenMapTiles, dark);
        check(name, snapshot(&map_for(&doc, stylejson::BASEMAP)));
    }
}
