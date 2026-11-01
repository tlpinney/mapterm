//! FlatGeobuf files from any path or URL.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Ok(doc) = mapterm::fgb::to_geojson(data) {
        let _ = mapterm::geojson::features(&doc);
    }
});
