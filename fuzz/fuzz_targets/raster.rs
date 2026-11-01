//! Raster tiles (PNG, JPEG, WebP) from any server.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = mapterm::raster::decode(data);
});
