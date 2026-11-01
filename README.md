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

## Raster layers

`--raster URL` puts a PNG/JPEG tile layer under the vector map:

```sh
mapterm --raster 'https://tile.openstreetmap.org/{z}/{x}/{y}.png' --at 38.8895,-77.0353 -z 15
```

- URLs use `{z}`, `{x}` and `{y}`; for TMS services (rows counted from the
  bottom) use `{-y}` or add `--tms`.
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

The crate's library has the pieces mapterm is built from:

- `Source` opens a PMTiles file or URL, an XYZ template, or a TileJSON URL
- `prepare` decodes and styles one tile; `TileStore` keeps the prepared tiles
- `MapWidget` is the ratatui widget, with `Overlay` markers and lines on top
- `Controller` turns crossterm mouse and key events into camera moves, and
  reports single clicks (`take_click`) so you can act on the spot under them
- `pick` returns the features under a spot with their properties and
  measurements, for your own inspection or analysis views

`src/bin/mapterm.rs` is a complete example: about 500 lines that fetch the
tiles the view needs, feed input to the controller, and draw the widget.

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
