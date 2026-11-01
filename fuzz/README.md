# Fuzzing

Coverage-guided fuzz targets for the parsers that read data from the network or from
files: tiles, archives, data formats and styles. They need nightly Rust and
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz):

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
cd fuzz
cargo +nightly fuzz run mvt -- -max_total_time=300
```

| target | input |
|---|---|
| `mvt` | a vector tile |
| `pmtiles` | a PMTiles header or directory |
| `fgb` | a FlatGeobuf file |
| `formats` | a `--geojson` file: the first byte picks GeoJSON, CSV, GPX, KML or FlatGeobuf |
| `raster` | a PNG, JPEG or WebP tile |
| `style` | a MapLibre style, drawn over a few features; fails if a control character reaches a cell |

Seed `corpus/<target>/` with real inputs first (the files in `tests/fixtures/`, tiles from
`~/.cache/mapterm/`); libFuzzer adds what it finds there. Crashes land in
`artifacts/<target>/`; replay one with `cargo +nightly fuzz run <target> <file>`. The
corpus and artifacts stay out of git.
