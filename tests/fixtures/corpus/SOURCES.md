# Differential test corpus

Real files that `tests/differential.rs` reads with mapterm and compares with GDAL's
reading of them (`<file>.gdal.json`, written by `make-reference.py`). Each comes unchanged
from its source, at a pinned commit or release:

| Files | Source | License |
|---|---|---|
| `shp/countries.*`, `fgb/*.fgb` | [flatgeobuf/flatgeobuf](https://github.com/flatgeobuf/flatgeobuf) `test/data/`, commit `07138176d2726dabb72a2c93d7ddc33ebf6e0f57` | BSD-2-Clause |
| `shp/ne_110m_populated_places_simple.*`, `shp/ne_110m_rivers_lake_centerlines.*` | [Natural Earth](https://www.naturalearthdata.com) 1:110m, version 5.1.2 | Public domain |
| `gpx/*.gpx` | [georust/gpx](https://github.com/georust/gpx) `tests/fixtures/`, commit `7ad83e33e64350c6a4893243b77bb3e474db56b5` | MIT |
| `kml/*.kml` | [georust/kml](https://github.com/georust/kml) `tests/fixtures/`, commit `4cfba50d568a2f4b43d467ad6897790f5d4e3e6b` | MIT OR Apache-2.0 |

`kml/polygon.kml` is a bare `<Polygon>` without a `<kml>` root: GDAL and mapterm both
decline it, so it has no reference and only checks that mapterm doesn't panic.

To add a file: put it in the folder for its format (a shapefile with its `.shx`, `.dbf`,
`.prj` and `.cpg`), note its source here, run `uv run tests/fixtures/corpus/make-reference.py`
and commit both. Differences that are expected go in `KNOWN` or `REFUSED` in
`tests/differential.rs`, with the reason.
