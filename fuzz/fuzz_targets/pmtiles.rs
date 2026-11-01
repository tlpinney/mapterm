//! PMTiles headers and directories from any file or server.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Ok(h) = mapterm::pmtiles::parse_header(data) {
        let _ = (h.min_zoom, h.max_zoom);
    }
    if let Ok(entries) = mapterm::pmtiles::parse_directory(data) {
        for id in [0, 1, 5, u64::MAX] {
            let _ = mapterm::pmtiles::find(&entries, id);
        }
    }
});
