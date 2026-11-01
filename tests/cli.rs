//! The `mapterm` command, end to end and offline: a one-tile PMTiles archive is built from the
//! fixture tile (z14 over the National Mall), and the binary draws, picks and exports from it.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
};

const TILE: (u8, u32, u32) = (14, 4686, 6268);
/// The tile's center, a little overzoomed so the view stays inside it.
const AT: &str = "38.88248,-77.02515";

fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A PMTiles v3 archive holding one uncompressed MVT tile, with an attribution in its metadata.
fn pmtiles() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let tile = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/omt-14-4686-6268.pbf")).unwrap();
        let mut dir = Vec::new();
        varint(&mut dir, 1); // one entry
        varint(&mut dir, mapterm::pmtiles::tile_id(TILE.0, TILE.1, TILE.2));
        varint(&mut dir, 1); // run length
        varint(&mut dir, tile.len() as u64);
        varint(&mut dir, 1); // offset 0, stored plus one
        let meta = br#"{"attribution":"<a href=\"https://example.org\">&copy; Test Tiles</a>"}"#;
        let (root_off, meta_off) = (127u64, 127 + dir.len() as u64);
        let data_off = meta_off + meta.len() as u64;
        let mut h = vec![0u8; 127];
        h[..7].copy_from_slice(b"PMTiles");
        h[7] = 3;
        let mut put = |o: usize, v: u64| h[o..o + 8].copy_from_slice(&v.to_le_bytes());
        put(8, root_off);
        put(16, dir.len() as u64);
        put(24, meta_off);
        put(32, meta.len() as u64);
        put(40, data_off);
        put(48, 0);
        put(56, data_off);
        put(64, tile.len() as u64);
        put(72, 1);
        put(80, 1);
        put(88, 1);
        h[96] = 1; // clustered
        h[97] = 1; // no compression (directories)
        h[98] = 1; // no compression (tiles)
        h[99] = 1; // MVT
        h[100] = 14;
        h[101] = 14;
        let e7 = |v: f64| ((v * 1e7) as i32).to_le_bytes();
        h[118] = 14;
        h[119..123].copy_from_slice(&e7(-77.0251));
        h[123..127].copy_from_slice(&e7(38.8825));
        let mut file = h;
        file.extend_from_slice(&dir);
        file.extend_from_slice(meta);
        file.extend_from_slice(&tile);
        let path = tmp("mall.pmtiles");
        std::fs::write(&path, file).unwrap();
        path
    })
}

fn mapterm(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mapterm")).args(args).env("HOME", env!("CARGO_TARGET_TMPDIR")).output().unwrap()
}

fn ok(args: &[&str]) -> String {
    let out = mapterm(args);
    assert!(out.status.success(), "mapterm {args:?} failed:\n{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn snapshot(extra: &[&str]) -> String {
    let pm = pmtiles().to_str().unwrap().to_string();
    let mut args = vec![pm.as_str(), "--at", AT, "-z", "14.6", "--snapshot", "100x30"];
    args.extend_from_slice(extra);
    ok(&args)
}

#[test]
fn help_and_bad_arguments() {
    let help = ok(&["--help"]);
    assert!(help.contains("usage: mapterm") && help.contains("--geojson") && help.contains("--style"));
    let out = mapterm(&["--nope"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown option --nope"));
    let out = mapterm(&["--style", "x.json", "y.pmtiles"]);
    assert!(!out.status.success(), "SOURCE and --style together");
    let out = mapterm(&[tmp("missing.pmtiles").to_str().unwrap(), "--snapshot", "10x5"]);
    assert!(!out.status.success(), "a missing file fails");
}

#[test]
fn raster_provider_list() {
    let list = ok(&["--raster-list"]);
    for name in ["osm", "esri", "gibs", "vantor", "planet"] {
        assert!(list.contains(name), "{name} missing from:\n{list}");
    }
    assert!(list.contains("VANTOR_API_KEY"), "keyed providers name their key variable");
}

#[test]
fn snapshot_draws_the_tile_with_the_builtin_style() {
    let text = snapshot(&[]);
    assert_eq!(text.lines().count(), 30);
    for name in ["L'Enfant Plaza Southwest", "7th Street Southwest", "I-395"] {
        assert!(text.contains(name), "{name} missing:\n{text}");
    }
    assert!(text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)), "braille lines");
}

#[test]
fn snapshot_formats() {
    let ansi = snapshot(&["--format", "ansi"]);
    assert!(ansi.contains(";38;2;"), "24-bit foreground colors");
    assert!(ansi.lines().all(|l| l.ends_with("\x1b[0m")), "each line resets its colors");
    let html = snapshot(&["--format", "html"]);
    assert!(html.starts_with("<pre") && html.trim_end().ends_with("</pre>"));
    // The visible text, without tags (a label can span several color runs) and entities.
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text.replace("&#39;", "'").replace("&apos;", "'").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"");
    assert!(text.contains("L'Enfant Plaza Southwest"), "{text}");
    let out = mapterm(&[pmtiles().to_str().unwrap(), "--snapshot", "10x5", "--format", "svg"]);
    assert!(!out.status.success());
}

#[test]
fn pick_prints_features_as_json() {
    let out = snapshot(&["--pick", "50,15"]);
    let picks: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(!picks.is_empty());
    for p in &picks {
        assert!(p["_layer"].is_string() && p["_geometry"].is_string() && p["_center"].is_array(), "{p}");
        assert_eq!(p["_source"], "basemap");
    }
    assert!(picks.iter().any(|p| p["_style_layer"].is_string()), "some are drawn, and say by which layer");
}

#[test]
fn data_layers_over_the_map() {
    let csv = tmp("sites.csv");
    std::fs::write(&csv, "name,lat,lon\nTest Site,38.8829,-77.0270\n").unwrap();
    let gpx = tmp("walk.gpx");
    std::fs::write(&gpx, r#"<gpx version="1.1"><trk><name>Test Walk</name><trkseg><trkpt lat="38.8800" lon="-77.0330"/><trkpt lat="38.8850" lon="-77.0180"/></trkseg></trk></gpx>"#).unwrap();
    let text = snapshot(&["--geojson", csv.to_str().unwrap(), "--geojson", gpx.to_str().unwrap()]);
    assert!(text.contains("Test Site"), "{text}");
    assert!(text.contains("Test Walk"), "{text}");
    // Pick where the site's ● is drawn.
    let alone = snapshot(&["--geojson", csv.to_str().unwrap()]);
    let (row, line) = alone.lines().enumerate().find(|(_, l)| l.contains("Test Site")).unwrap();
    let col = line.chars().position(|c| c == '●').unwrap();
    let at = format!("{col},{row}");
    let picks = snapshot(&["--geojson", csv.to_str().unwrap(), "--pick", &at]);
    assert!(picks.lines().next().is_some_and(|l| l.contains("\"Test Site\"") && l.contains("\"_source\":\"sites\"")), "your data is listed first:\n{picks}");
}

#[test]
fn local_maplibre_style() {
    // A style pointing at the archive with pmtiles://, drawing water, roads and their names.
    let style = serde_json::json!({
        "version": 8,
        "center": [-77.02515, 38.88248], "zoom": 14.6,
        "sources": {"omt": {"type": "vector", "url": format!("pmtiles://{}", pmtiles().display()), "attribution": "Style Credit"}},
        "layers": [
            {"id": "bg", "type": "background", "paint": {"background-color": "#102030"}},
            {"id": "water", "type": "fill", "source": "omt", "source-layer": "water", "paint": {"fill-color": "#0000ff"}},
            {"id": "roads", "type": "line", "source": "omt", "source-layer": "transportation", "paint": {"line-color": "#ffffff"}},
            {"id": "names", "type": "symbol", "source": "omt", "source-layer": "transportation_name",
             "layout": {"text-field": "{name}", "symbol-placement": "line"}, "paint": {"text-color": "#ffff00"}}
        ]
    });
    let path = tmp("style.json");
    std::fs::write(&path, style.to_string()).unwrap();
    let text = ok(&["--style", path.to_str().unwrap(), "--snapshot", "100x30"]);
    assert!(text.contains("L'Enfant Plaza Southwest"), "{text}");
    let picks = ok(&["--style", path.to_str().unwrap(), "--snapshot", "100x30", "--pick", "50,15"]);
    assert!(picks.contains("\"_source\":\"omt\""), "{picks}");
}
