# Changelog

All notable changes to mapterm are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (pre-1.0: a minor version may break the API).

## [0.1.0-alpha.3] - unreleased

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

[0.1.0-alpha.3]: https://github.com/tlpinney/mapterm/compare/v0.1.0-alpha.2...v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/tlpinney/mapterm/compare/v0.1.0-alpha.1...v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/tlpinney/mapterm/releases/tag/v0.1.0-alpha.1
