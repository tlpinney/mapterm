# Test fixtures

- `omt-14-4686-6268.pbf`: one vector tile (z14, over the National Mall) from
  [OpenFreeMap](https://openfreemap.org), OpenMapTiles schema.
  Map data © [OpenStreetMap contributors](https://www.openstreetmap.org/copyright) (ODbL),
  schema © [OpenMapTiles](https://openmaptiles.org).
- `openfreemap-*.json`: the OpenFreeMap styles (liberty, positron, bright, dark,
  fiord) as published at `https://tiles.openfreemap.org/styles/<name>`; see
  OpenFreeMap for their licenses and upstream credits.
- `countries.fgb`: the FlatGeobuf project's sample of Natural Earth countries
  (`https://flatgeobuf.org/test/data/countries.fgb`); Natural Earth data is in
  the public domain.
- `perf-dc.mbtiles`: OpenFreeMap tiles over central Washington, DC (zooms 10 to 12, and 13
  to 14 around the National Mall), pinned for the nightly performance tests
  (`tests/perf.rs`) so their numbers compare from night to night. Rebuilt with
  `cargo test --release --test tools make_perf_fixture -- --ignored` (the baseline in
  `tests/perf-baseline.json` then needs `MAPTERM_PERF_UPDATE=1`).
  Map data © [OpenStreetMap contributors](https://www.openstreetmap.org/copyright) (ODbL),
  schema © [OpenMapTiles](https://openmaptiles.org).
