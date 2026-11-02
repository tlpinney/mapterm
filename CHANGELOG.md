# Changelog

All notable changes to mapterm are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (pre-1.0: a minor version may break the API).

## [Unreleased]

The next release is 0.2.0: the library's `formats::Format` gains a variant (Shapefile).

### Added

- `--raster planetary`: Sentinel-2 with NAIP over it from zoom 12, so the imagery stays
  sharp from regional zooms down to streets, and Sentinel-2 fills in wherever NAIP has
  nothing (outside the US). Library: `raster::STACKS` / `raster::stack`.
- Style files can name a built-in imagery provider in a raster source,
  `{"type": "raster", "mapterm:raster": "naip"}`, with `maxzoom` to cap it; mixing
  providers by zoom is then plain `minzoom`/`maxzoom` on the layers.
- Microsoft Planetary Computer imagery, free and without a key: `--raster naip` (USDA NAIP
  aerial photos of the US at ~60 cm, public domain) and `--raster sentinel2` (Sentinel-2's
  latest clear scenes, global at ~10 m). The mosaic is registered when the layer starts
  (`RasterSource::start_session`, module `mapterm::planetary`), and tiles are kept as long
  as their `Cache-Control` says (an hour). The service only renders these mosaics zoomed in,
  so mapterm requests nothing below zoom 12 (NAIP) or 9 (Sentinel-2) and the status bar says
  to zoom in: `RasterSource::min_zoom`, `Map::rasters_below_min_zoom`.
- Experimental pixel mode (`MAPTERM_PIXELS=1`, kitty-graphics terminals): fills and lines
  as an image at the screen's resolution, drawn once per tile and cached, with labels as
  text. Library: `Map::pixel_mode` and `Map::take_pixels`.
- Release archives include `THIRD_PARTY_LICENSES.txt`: the copyright notices and
  license terms of the crates built into the binary, made with cargo-about.
- Shapefiles with `--geojson roads.shp` (a file or URL): the geometry, the `.dbf`
  attributes beside it with their text decoded by the `.cpg` code page, and the `.prj`.
  Lon/lat only for now: a projected `.prj` is refused with its name, as is a file without
  one whose coordinates aren't lon/lat. Points, lines, polygons with holes and multipoints,
  with Z and M dropped; multipatches are skipped. Read with the `shapefile` crate
  (`mapterm::shp`).
- Differential tests (`tests/differential.rs`): the Shapefile, FlatGeobuf, GPX and KML
  readers against GDAL 3.9 on 29 real files (`tests/fixtures/corpus/`, from FlatGeobuf,
  Natural Earth and the georust `gpx` and `kml` crates), geometry and attributes. GDAL's
  output is stored beside each file, so the tests need neither GDAL nor network.
- A fuzz target for Shapefiles (`shp`).
- Google Maps 2D tiles as raster providers: `--raster google` (satellite),
  `google-roadmap` and `google-terrain`, with a Map Tiles API key from
  `GOOGLE_MAP_TILES_KEY` or `--raster-key-cmd`. `RasterSource::start_session` gets the
  session Google needs before the first tile (a no-op for other sources), and
  `RasterSource::google_credits` gives the credits ("Google Maps" plus Google's
  imagery or map data credits) for a view; the viewer asks again whenever the view
  settles somewhere new. Google tiles are cached by their HTTP headers, as Google's terms
  ask: kept for the `Cache-Control` `max-age` (a day, at most 30), then revalidated with
  `If-None-Match`, so an unchanged tile costs a 304 instead of a download; `no-store`
  responses aren't kept. `--raster-no-cache` turns it off.

### Changed

- **Breaking (library):** `formats::Format` has a `Shapefile` variant and is now
  `#[non_exhaustive]`, so later formats won't break matches again.
- `Map::set_attribution` works on raster sources that are shared (it used to fail once
  the source had another owner), so credits can follow the view.
- While the camera moves, the viewer loads paid imagery (keyed providers, billed per tile)
  for where it will stop: a jump's destination starts loading as the jump begins, and
  eased zooms and drag glides keep their imagery, without the views on the way.
- Tiles refused with HTTP 429 (too many requests, or a used-up quota) are retried without
  limit instead of given up, and the viewer pauses imagery requests for a minute and says
  so, instead of counting them as failed.

- Mapbox styles and tilesets: `--style mapbox://styles/mapbox/streets-v12` (and
  `Map::from_style` with a `mapbox://` URL) once a token is set with
  `mapbox::set_access_token`, `--mapbox-token-cmd` or `MAPBOX_ACCESS_TOKEN`. `mapbox://`
  style and tileset URLs become Mapbox API URLs; the token is kept out of errors and
  source names. Mapbox's raster and vector tiles are cached by their HTTP headers (12
  hours, as of 2026) like Google's, and `--raster mapbox` uses the same token.

### Added (viewer)

- `--raster-limit N|off` (or `MAPTERM_RASTER_LIMIT`): at most N imagery tile requests a
  minute. Default 300 for keyed, billed providers and none for free ones; `off` lifts it.
- `Controller::target`: where the running animation ends. `map::is_rate_limited` tells
  an HTTP 429 tile error apart from a failure.

### Fixed

- Zooming no longer queues downloads for views you've already left: tiles start only
  into free download slots, nearest the center first, and the rest are asked for again by
  the next frame for the view it shows. A scripted zoom from 12 to 17 over NAIP made 4
  imagery requests instead of 16, and the final view filled in ~1.3 s instead of ~4 s.
- "Zoom in for imagery" in the status bar only for raster layers shown at that zoom.
- Text that looks like a zero-padded number stays text in CSV, GPX and KML properties:
  ZIP code `02139` and route name `001` were read as the numbers 2139 and 1 (found by the
  differential tests).

### Security

- A Shapefile record that claims more parts or points than it holds, or whose parts start
  outside its points, is refused before the `shapefile` crate reads it: the crate reserves
  memory for the claimed counts and part sizes before checking them, so one record
  claiming two billion parts reserved 7 GB, and a part claiming 200 million points in a
  record of none 3.5 GB (found by the new fuzz target).

## [0.1.2] - 2026-11-01

Cleaner rivers in the built-in style, and label-free snapshots.

### Added

- `--no-labels` starts the viewer with labels and symbols hidden, as if `n` had been
  pressed (`n` shows them). With `--snapshot` it prints a frame of map lines and fills
  only, e.g. for images: labels otherwise take cells where the map would be drawn.

### Changed

- The built-in style draws rivers in the water color from zoom 11, as MapLibre styles do,
  so a river's centerline disappears into its water polygon instead of showing as a
  stray line down the river, or as a stub where a creek flows into a wider river.
  Streams, canals and ditches stay bright, and rivers stay bright below zoom 11.

## [0.1.1] - 2026-11-01

Security fixes for memory use and API keys.

### Security

- A small vector tile can no longer decode to gigabytes: each feature gets its own copy of
  the dictionary strings it references, so one long string shared by many features (a
  1 MiB tile, 5 KiB gzipped) took 2 GiB. A tile's feature properties are now limited to
  64 MB decoded (`mvt::MAX_PROP_BYTES`); real tiles stay far below it.
- API keys no longer follow redirects to other hosts. Requests that carry a key (in a
  header such as `maxar-api-key`, or in the URL) follow a redirect only to the same scheme,
  host and port; reqwest dropped `Authorization` on a cross-origin redirect but kept custom
  key headers. No request sends a `Referer` any more, which carried the previous URL,
  including a key in its query, to the next host.
- The raster tile store is limited to 512 MB (`raster::RASTER_STORE_BUDGET`) as well as 256
  tiles: 4096 px tiles that compress to a few hundred KB each could fill about 16 GB.
  Decoding also moves the pixels instead of copying them, halving the peak per tile.

## [0.1.0] - 2026-11-01

The first stable-numbered release, so plain Cargo commands find it (they skip
pre-releases):

```sh
cargo install mapterm   # the viewer
cargo add mapterm       # the library
```

Pre-1.0, a minor version (0.2.0) may still change the API. No code changes since
0.1.0-alpha.3; this is also the first version published by the release workflow with
crates.io trusted publishing.

## [0.1.0-alpha.3] - 2026-11-01

The first release on crates.io:

```sh
cargo install mapterm --version 0.1.0-alpha.3   # the viewer
cargo add mapterm@0.1.0-alpha.3                 # the library
```

### Releases

- Each version tag also publishes the crate to crates.io once the maintainer approves the
  release workflow's `crates-io` environment, with crates.io trusted publishing (no
  stored token).

## [0.1.0-alpha.2] - 2026-11-01

Hardening from a security review and fuzzing, plus signed release binaries.

### Security

- Control characters in map text (tile labels, style text and glyphs, search results,
  data file properties) no longer reach the terminal, where they could set the clipboard
  or window title; `--snapshot` exports drop them too.
- Deeply nested KML, WKT and FlatGeobuf geometry is rejected instead of overflowing the
  stack.
- A source with a bad zoom range (min above max, or past zoom 30) in a PMTiles header,
  TileJSON or style no longer panics.
- Raster tiles larger than 4096 pixels a side are refused before decoding, so a small
  image declaring huge dimensions can't take hundreds of megabytes.
- The docs show header-based API keys as `--raster-header 'Name: {key}'`, keeping the key
  itself out of `ps` and shell history.
- A style loaded from a URL can only use web sources: a `pmtiles:///path`, file path or
  local GeoJSON in it is an error, so a remote style can't make mapterm read local files.
- A FlatGeobuf file with part ends out of order no longer crashes mapterm, and huge
  feature counts or offsets in FlatGeobuf and PMTiles no longer overflow (found by the new
  fuzz targets in `fuzz/`).
- `--raster-list-mosaics` only follows Planet's next-page links back to
  `https://api.planet.com/`, since each request carries the key; place search and the
  mosaic list read at most 4 and 16 MB per response.
- PMTiles directory entries must lie inside their section of the archive, and zero-length
  or wrapping byte ranges are refused before any request.

### Releases

- Signed binaries: each version tag builds `mapterm` for macOS (Apple silicon) and Linux
  (x86-64) in CI, attached to the GitHub release with `SHA256SUMS`, which the maintainer
  signs with an SSH key (`SHA256SUMS.sig`, namespace `mapterm-release`; trusted keys in
  `signing/allowed_signers`). The release notes say how to verify, taking the key from
  the signing keys on the maintainer's GitHub account rather than from the repository.
- Commits and tags are signed with the same key (shown as Verified on GitHub).
- Install with Cargo from git (not published to crates.io):

  ```sh
  cargo install --git https://github.com/tlpinney/mapterm --tag v0.1.0-alpha.2
  ```

## [0.1.0-alpha.1] - 2026-11-01

The first alpha, for private testing. Not published to crates.io; install from git:

```sh
cargo install --git https://github.com/tlpinney/mapterm --tag v0.1.0-alpha.1
```

### The viewer (`mapterm`)

- OpenStreetMap vector tiles in the terminal: braille lines, cell-color fills, labels that
  don't overlap, and MapLibre-style pan and zoom (wheel at the cursor, drag with inertia,
  double-click and box zoom, eased keys).
- Sources: PMTiles (local or over HTTP range requests), XYZ URLs and TileJSON; OpenFreeMap
  by default, no key needed.
- A built-in terminal style for the OpenMapTiles and Protomaps schemas (dark and light),
  or any MapLibre style with `--style` (OpenFreeMap's liberty, positron, bright, dark,
  fiord, a URL or a file); `m` switches styles, `t` the theme.
- Imagery under the map with `--raster` (OSM, Esri, USGS, EOX, NASA GIBS, and keyed
  providers such as Vantor, Planet, Mapbox and MapTiler), as kitty graphics or cell colors.
- Your data over the map with `--geojson`: GeoJSON, CSV with lat/lon or WKT columns, GPX,
  KML and FlatGeobuf, by file or URL; large point sets cluster.
- Place search with `/` (Photon or Nominatim), with a flyTo-style flight.
- Inspect mode (`i`): every feature under a spot, drawn or not, with its properties,
  size and location; `y` copies it as JSON.
- `--snapshot` prints a frame as text, ANSI color or HTML; `--pick` prints features as JSON.
- Experimental pixel-smooth scrolling on the Ghostty smooth-scroll fork (`MAPTERM_SMOOTH`).

### The library

- `Map`: named sources (vector tiles, GeoJSON, raster) and ordered style layers
  (background, fill, line, circle, symbol, raster) with MapLibre expressions and filters,
  plus custom layers; drawn as a ratatui widget.
- MapLibre style JSON loading, and the style properties a terminal can show: colors and
  opacity, line widths and dashes, text fields and fonts, anchors and offsets, labels
  repeated along roads, sort keys, icons as one-cell symbols.
- `query_rendered_features` and `query_features` (drawn or not), layer events and hover
  state (`events::Events`, feature state), `Controller` with `fly_to` and `fit_bounds`.
- GeoJSON sources with a spatial index and point clustering; tiles loaded in the
  background with retries; kitty image memory kept within a budget; source attribution.

[Unreleased]: https://github.com/tlpinney/mapterm/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/tlpinney/mapterm/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/tlpinney/mapterm/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tlpinney/mapterm/compare/v0.1.0-alpha.3...v0.1.0
[0.1.0-alpha.3]: https://github.com/tlpinney/mapterm/compare/v0.1.0-alpha.2...v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/tlpinney/mapterm/compare/v0.1.0-alpha.1...v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/tlpinney/mapterm/releases/tag/v0.1.0-alpha.1
