//! The OpenFreeMap styles parse, and every filter and drawn property is understood.

use mapterm::stylejson::style_layers;

#[test]
fn openfreemap_styles_parse_without_warnings() {
    for name in ["liberty", "positron", "bright", "dark", "fiord"] {
        let path = format!("{}/tests/fixtures/openfreemap-{name}.json", env!("CARGO_MANIFEST_DIR"));
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let (layers, warnings) = style_layers(&doc).unwrap();
        assert!(layers.len() > 40, "{name}: {} layers", layers.len());
        assert!(warnings.is_empty(), "{name}:\n{}", warnings.join("\n"));
    }
}

#[test]
fn terminal_styles_parse_without_warnings() {
    use mapterm::stylejson::{Schema, terminal_style};
    for schema in [Schema::OpenMapTiles, Schema::Protomaps] {
        for dark in [true, false] {
            let (layers, warnings) = style_layers(&terminal_style(schema, dark)).unwrap();
            assert!(layers.len() > 15, "{schema:?}");
            assert!(warnings.is_empty(), "{schema:?} dark={dark}:\n{}", warnings.join("\n"));
        }
    }
}
