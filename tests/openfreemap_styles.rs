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

#[test]
fn shield_text_tidies_refs() {
    use mapterm::{
        expr::{Ctx, Expr, Val},
        mvt::{GeomType, Value},
        stylejson::shield_text,
    };
    let e = Expr::parse(&shield_text()).unwrap();
    let text = |r: &str, network: &str| {
        let mut props = vec![("ref".to_string(), Value::Str(r.into()))];
        if !network.is_empty() {
            props.push(("network".to_string(), Value::Str(network.into())));
        }
        match e.eval(&Ctx { zoom: 12.0, geom: GeomType::Line, props: &props, state: &[] }) {
            Val::Str(s) => s,
            v => panic!("{v:?}"),
        }
    };
    assert_eq!(text("395", "us-interstate"), "I-395");
    assert_eq!(text("1", "us-highway"), "US 1");
    assert_eq!(text("I 270;US 40", ""), "I-270");
    assert_eq!(text("I 395", "us-interstate"), "I-395");
    assert_eq!(text("I 395 Alternate", "road"), "I-395 Alternate");
    assert_eq!(text("MD 355", "us-state"), "MD 355");
    assert_eq!(text("50", "us-state"), "50");
}
