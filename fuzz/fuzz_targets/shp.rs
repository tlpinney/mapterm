//! Shapefiles from any path or URL: the input is a `.shp` and a `.dbf`, split at the length
//! in its first four bytes (little-endian), so both files and their pairing get fuzzed.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let n = (u32::from_le_bytes(data[..4].try_into().unwrap()) as usize).min(data.len() - 4);
    let (shp, dbf) = data[4..].split_at(n);
    let parts = mapterm::shp::Parts { shp, dbf: (!dbf.is_empty()).then_some(dbf), prj: None, cpg: None };
    if let Ok(doc) = mapterm::shp::to_geojson(&parts) {
        let _ = mapterm::geojson::features(&doc);
    }
});
