# mapterm

A map in your terminal. `mapterm` draws OpenStreetMap vector tiles with braille
characters, pans and zooms like a web map, and works over SSH. The same code is a
library: a [ratatui](https://ratatui.rs) map widget you can drop into your own
terminal apps.

```sh
cargo install --path .         # installs the `mapterm` command

mapterm                                                # OpenFreeMap planet, no key needed
mapterm --at 38.8895,-77.0353 -z 13                    # start somewhere (lat,lon) at zoom 13
mapterm https://build.protomaps.com/20261031.pmtiles   # Protomaps planet over HTTP range requests
mapterm dc.pmtiles --at 38.8899,-77.0091 -z 13          # a local PMTiles file, offline
mapterm --snapshot 120x40 --at 38.8895,-77.0353         # print one frame as text and exit
```

## Controls

They follow the MapLibre GL / Mapbox GL defaults:

- **Scroll / two-finger swipe up and down:** zoom toward the cursor
- **Drag:** pan, and a fling keeps gliding (inertia)
- **Double-click:** zoom in; **shift+double-click:** zoom out
- **Shift+drag:** box zoom
- **Arrows** (or `hjkl`) pan, **`+`/`-`** zoom to the next level, both eased
- Terminal extras: sideways swipes pan; `i` inspect, `v` vector overlay, `n` labels, `t` light/dark theme, `q` quit

## GeoJSON layers

`--geojson FILE|URL` draws your data over the map, as many layers as you like:

```sh
mapterm --geojson sites.geojson --geojson https://example.com/routes.geojson
```

- Points, lines and polygons (including Multi* and GeometryCollection), each
  layer in its own color with a legend in the status bar; polygon interiors are
  tinted (left clear over a raster layer).
- Features with a `name` (or `title`/`label`) property are labeled.
- Without `--at`, the map starts zoomed to fit all layers.
- Inspect mode (`i`) lists your features first, with all their properties, and
  `--snapshot ... --pick COL,ROW` prints them as JSON lines.

## Raster layers

`--raster` puts an image tile layer under the vector map, by provider name or URL:

```sh
mapterm --raster esri --at 38.8895,-77.0353 -z 15        # satellite
mapterm --raster 'https://tile.openstreetmap.org/{z}/{x}/{y}.png'
mapterm --raster gibs --date 2026-10-30 -z 6                # NASA daily imagery
mapterm --raster mall.pmtiles                              # your own imagery, offline
mapterm --raster-list                                      # providers and the keys they need
```

`--raster` also takes a PMTiles archive of PNG, JPEG or WebP tiles, as a local
file or an `https://` URL read with range requests: a single file of your own
imagery that works offline.

| Name | Imagery | Key |
|---|---|---|
| `osm` | OpenStreetMap standard map (light use only) | none |
| `esri` | Esri World Imagery, global satellite | none |
| `usgs` | USGS National Map imagery, US only, public domain | none |
| `eox` | Sentinel-2 cloudless 2024, global, ~10 m (CC BY-NC-SA) | none |
| `gibs` | NASA GIBS daily satellite imagery, ~250 m (`--date`, default yesterday) | none |
| `esri-light`, `esri-dark` | Esri Gray Canvas, quiet backgrounds for overlays | none |
| `vantor` | Vantor (Maxar) Vivid basemap | `VANTOR_API_KEY` |
| `planet` | Planet basemaps (your subscription's mosaics) | `PL_API_KEY` + a mosaic |
| `mapbox` | Mapbox Satellite, Retina tiles | `MAPBOX_ACCESS_TOKEN` |
| `maptiler` | MapTiler Satellite | `MAPTILER_KEY` |

API keys never go on the command line: they come from the provider's
environment variable or from a command, e.g.
`mapterm --raster vantor --raster-key-cmd 'pass show vantor/api-key'`. They are
only added to requests (Vantor's as the `maxar-api-key` header) and are blanked
out of errors. Keyed providers don't cache tiles on disk unless you pass
`--raster-cache`, since commercial licenses often limit storing them. For any
other service, use its URL with `{key}` where the key goes (filled from
`MAPTERM_RASTER_KEY` or `--raster-key-cmd`) and `--raster-header 'Name: value'`
for header-based keys.

Planet serves each customer's named mosaics: list the ones your key can use
with `mapterm --raster-list-mosaics`, then pick one with
`--raster planet --raster-param mosaic=NAME` (or `PLANET_MOSAIC`). Planet only
accepts the key in the tile URL; mapterm still never prints it.
`--raster-param NAME=VALUE` fills any other `{NAME}` in a custom URL; for GIBS,
`--raster-param layer=VIIRS_SNPP_CorrectedReflectance_TrueColor` picks another
layer (JPEG layers at 250 m).

- URLs use `{z}`, `{x}` and `{y}` in any order; for TMS services (rows counted
  from the bottom) use `{-y}` or add `--tms`.
- The status bar credits the imagery: Esri, USGS, OpenStreetMap and EOX are
  known; give anything else with `--raster-attribution TEXT`.
- On terminals with the kitty graphics protocol (Ghostty, kitty, WezTerm, and
  through herdr) tiles are real images underneath the text, so vector labels,
  lines, markers and inspect draw on top. Each tile is sent once; panning and
  zooming only move it. Elsewhere the layer becomes cell background colors.
  `MAPTERM_GRAPHICS=kitty` or `cells` overrides the guess.
- **`v`** cycles the vector overlay: lines and labels, labels only, off. Area
  fills are always off over a raster layer.
- Tiles are cached on disk for 30 days and fetched at most two at a time.
  Respect each tile server's usage policy: OpenStreetMap's own tile servers
  allow only light use, with attribution.

## Inspect

Press **`i`** to switch to inspect mode, and `i` (or `Esc`) again to leave it.

- **Hover** shows the top feature under the pointer in the status bar.
- **Click** lists every feature at that spot, drawn or not (roads, street names,
  buildings, parks, landcover, points of interest...), most specific first,
  and highlights the selected one on the map.
- The panel shows the feature's layer, geometry type, center, length or area,
  vertex count, source tile, and all its properties.
- **Click an entry** or press **Tab / Shift+Tab** to switch between them;
  **`y`** copies the selected feature as JSON to the clipboard (OSC 52).

For scripts, `--pick` prints the same information as JSON lines:

```sh
mapterm --at 38.8895,-77.0353 -z 16 --snapshot 100x40 --pick 50,20
```

## What it reads

- **PMTiles v3**, from a local file or over HTTP range requests (own reader)
- **XYZ** URL templates (`…/{z}/{x}/{y}.pbf`) and **TileJSON** endpoints
- Vector tiles in the **OpenMapTiles** schema (OpenFreeMap, MapTiler) or the
  **Protomaps** basemap schema, in dark and light styles

Every download has a 30 s timeout and an 8 MB cap, decompression is capped at
32 MB, and HTTP tiles are cached on disk for 30 days (`~/.cache/mapterm`).

## Rendering

Roads, rivers and boundaries are braille lines (2x4 dots per character); water,
parks and buildings fill the cell background; labels avoid each other. While
sharper tiles load, lower-zoom tiles stand in for them.

## Use it in your own app

mapterm is also a library, built like Leaflet: a map is a camera plus a stack of
layers, and anything can be a layer.

```rust
let tiles = VectorTiles::new(&store, src.min_zoom, src.max_zoom);   // the basemap
let sites = mapterm::geojson::load("sites.geojson", PALETTE[0]).await?;
let pins = Markers(&[Overlay::Marker { lon, lat, glyph: "◆".into(), .. }]);
let layers: [&dyn Layer; 3] = [&tiles, &sites, &pins];               // bottom to top

frame.render_widget(MapView { camera: &ctl.cam, layers: &layers }, area);
let under = pick_all(&layers, &ctl.cam, area, dot);                  // top layer first
```

- `Layer` is three methods: `draw` (lines, fills, markers and labels through a
  `DrawContext`, in lon/lat or world coordinates), `pick` (features under a
  spot) and `bounds` (for fitting the view). Markers and labels from every
  layer are placed together, so they never overlap.
- Built in: `VectorTiles` (PMTiles, OpenFreeMap or any MVT source), `GeoJson`,
  and `Markers`. Raster layers draw as terminal images outside the text grid
  and are handled by the app for now (see `src/bin/mapterm.rs`).
- `Controller` gives MapLibre-style mouse and keyboard handling;
  `take_click` reports clicks for your own popups and panels.

[`examples/hello_map.rs`](examples/hello_map.rs) is a complete pan-and-zoom map
with a marker in about 50 lines (`cargo run --release --example hello_map`), and
`src/bin/mapterm.rs` is the full viewer.

## Experimental: smooth scrolling

Terminals move content a whole character at a time. As an experiment, mapterm
can move the map by single pixels on terminals that support an experimental
smooth-scroll protocol. Today that is only
[Ghostty Smooth](https://github.com/tlpinney/ghostty/tree/smooth), a personal
fork of Ghostty (see its
[`SMOOTH.md`](https://github.com/tlpinney/ghostty/blob/smooth/SMOOTH.md)), and
the [herdr fork](https://github.com/tlpinney/herdr/tree/tlpinney) relays it
from herdr panes. Both are experimental and may change.

It is off unless you turn it on with an environment variable:

- `MAPTERM_SMOOTH=shift`: dragging follows the pointer pixel by pixel;
  scrolling still zooms.
- `MAPTERM_SMOOTH=1`: two-finger up/down pans the map smoothly instead of
  zooming.

With either set, `s` toggles it while mapterm runs. Terminals without the
protocol ignore it and behave as usual.

## License

MIT. Map data © OpenStreetMap contributors.
