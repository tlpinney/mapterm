//! A style's layers, drawn over a few features: parses and evaluates expressions, filters
//! and colors, places labels, and checks no control character reaches a terminal cell.
#![no_main]
use mapterm::{Camera, Map, MapSource};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use serde_json::{Value, json};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Ok(doc) = serde_json::from_slice::<Value>(data) else { return };
    let Ok((layers, _)) = mapterm::stylejson::style_layers(&doc) else { return };
    let mut map = Map::new();
    // Every layer's source is "s", whatever the style said.
    let features = json!({"type": "FeatureCollection", "features": [
        {"type": "Feature", "id": 1, "properties": {"name": "a\u{1b}]0;x\u{7}b", "class": "primary", "rank": 3},
         "geometry": {"type": "Point", "coordinates": [0.5, 0.5]}},
        {"type": "Feature", "properties": {"name": "road", "ref": "I 66"},
         "geometry": {"type": "LineString", "coordinates": [[-1, -1], [1, 1], [2, 0]]}},
        {"type": "Feature", "properties": {"name": "park"},
         "geometry": {"type": "Polygon", "coordinates": [[[-2, -2], [2, -2], [2, 2], [-2, -2]]]}}
    ]});
    map.add_source("s", MapSource::geojson(&features).unwrap()).unwrap();
    for mut l in layers.into_iter().take(16) {
        if l.source.is_some() {
            l.source = Some("s".into());
        }
        let _ = map.add_layer(l);
    }
    let zoom = doc["zoom"].as_f64().filter(|z| z.is_finite()).unwrap_or(5.0).clamp(0.0, 24.0);
    let area = Rect::new(0, 0, 60, 16);
    let mut buf = Buffer::empty(area);
    map.view(&Camera::new(0.0, 0.0, zoom)).render(area, &mut buf);
    for cell in buf.content() {
        assert!(!cell.symbol().chars().any(char::is_control), "control character in a cell: {:?}", cell.symbol());
    }
});
