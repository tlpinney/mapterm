//! --geojson data: the first byte picks the format, the rest is the file.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else { return };
    let name = ["a.geojson", "a.csv", "a.gpx", "a.kml", "a.fgb", "a.json"][pick as usize % 6];
    if let Ok(doc) = mapterm::formats::to_geojson(name, body) {
        let _ = mapterm::geojson::features(&doc);
    }
    if let Ok(s) = std::str::from_utf8(body) {
        let _ = mapterm::formats::wkt_geometry(s);
    }
});
