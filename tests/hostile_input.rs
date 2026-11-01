//! Map data is untrusted: tiles, styles, search results and data files come from anywhere.
//! These inputs must neither reach the terminal as escape sequences nor crash mapterm.

use mapterm::{Camera, Map, MapSource, StyleLayer};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use serde_json::json;

/// A clipboard write (OSC 52), a title change (OSC 0) and a color change (CSI).
const EVIL: &str = "\u{1b}]52;c;cHduZWQ=\u{7}\u{1b}]0;owned\u{7}\u{9b}31mHi";

fn render_evil_label() -> Buffer {
    let mut map = Map::new();
    let point = |lon: f64| json!({"type": "Feature", "properties": {"name": EVIL}, "geometry": {"type": "Point", "coordinates": [lon, 0]}});
    map.add_source("label", MapSource::geojson(&point(-60.0)).unwrap()).unwrap();
    map.add_source("glyph", MapSource::geojson(&point(60.0)).unwrap()).unwrap();
    map.add_layer(StyleLayer::symbol("t", "label").text("{name}").color("#fff")).unwrap();
    map.add_layer(StyleLayer::symbol("m", "glyph").glyph("\u{1b}[31m*").color("#fff")).unwrap();
    let area = Rect::new(0, 0, 120, 10);
    let mut buf = Buffer::empty(area);
    map.view(&Camera::new(0.0, 0.0, 1.0)).render(area, &mut buf);
    buf
}

#[test]
fn labels_and_glyphs_drop_control_characters() {
    let buf = render_evil_label();
    let text = mapterm::export::to_text(&buf);
    assert!(text.contains("Hi") && text.contains("*"), "the label and glyph are still drawn:\n{text}");
    for cell in buf.content() {
        assert!(!cell.symbol().chars().any(char::is_control), "control character in cell {:?}", cell.symbol());
    }
}

#[test]
fn exports_carry_no_control_characters() {
    let buf = render_evil_label();
    let html = mapterm::export::to_html(&buf);
    assert!(!html.chars().any(|c| c.is_control() && c != '\n'), "control character in HTML export");
    let ansi = mapterm::export::to_ansi(&buf);
    // Only mapterm's own SGR color sequences: ESC [ ... m.
    for part in ansi.split('\u{1b}').skip(1) {
        assert!(part.starts_with('['), "foreign escape sequence in ANSI export: {part:?}");
    }
    assert!(!ansi.contains('\u{7}') && !ansi.contains('\u{9b}'), "{ansi:?}");
}

/// Runs `f` on a thread with a small stack, so recursion on hostile input shows up as a
/// crash here rather than only on deeper inputs.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(1 << 20).spawn(f).unwrap().join().expect("no stack overflow or panic")
}

#[test]
fn deeply_nested_kml_is_an_error() {
    let n = 200_000;
    let kml = format!(
        "<kml><Placemark>{}<Point><coordinates>1,2</coordinates></Point>{}</Placemark></kml>",
        "<MultiGeometry>".repeat(n),
        "</MultiGeometry>".repeat(n)
    );
    assert!(on_small_stack(move || mapterm::formats::kml(&kml).is_err()));
}

#[test]
fn normally_nested_kml_still_reads() {
    let kml = "<kml><Placemark><name>a</name><MultiGeometry><MultiGeometry><Point><coordinates>1,2</coordinates></Point></MultiGeometry></MultiGeometry></Placemark></kml>";
    let fc = mapterm::formats::kml(kml).unwrap();
    assert_eq!(fc["features"].as_array().unwrap().len(), 1, "{fc}");
}

#[test]
fn deeply_nested_wkt_is_rejected() {
    let n = 1_000_000;
    let wkt = format!("POLYGON {}0 0{}", "(".repeat(n), ")".repeat(n));
    assert!(on_small_stack(move || mapterm::formats::wkt_geometry(&wkt).is_none()));
    assert!(mapterm::formats::wkt_geometry("MULTIPOLYGON (((0 0, 1 0, 1 1, 0 0)))").is_some());
}

#[test]
fn source_zoom_ranges_from_headers_are_tamed() {
    let cam = Camera::new(0.0, 0.0, 14.0);
    // min > max (a crafted PMTiles header or TileJSON) used to panic in clamp.
    assert_eq!(cam.tile_zoom(200, 10), 10);
    // Zooms past 30 would overflow the tile-count shifts.
    assert_eq!(cam.tile_zoom(100, 100), 30);
    assert_eq!(cam.tile_zoom(0, 14), 14);
}
