//! Vector tiles from any server.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = mapterm::mvt::decode(data);
});
