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
- Terminal extras: sideways swipes pan; `n` labels, `t` light/dark theme, `q` quit

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

## Smooth scrolling (experimental, opt-in)

Terminals move content a whole character at a time. With the experimental
smooth-scroll protocol of [Ghostty Smooth](https://github.com/tlpinney/ghostty)
(relayed through the [herdr](https://github.com/tlpinney/herdr) multiplexer),
mapterm can move by single pixels:

- `--smooth-shift` (or `MAPTERM_SMOOTH=shift`): dragging follows the pointer
  pixel by pixel; scrolling still zooms.
- `--smooth` (or `MAPTERM_SMOOTH=1`): two-finger up/down pans smoothly instead
  of zooming.
- `s` toggles it at runtime. Other terminals ignore the protocol and behave as
  usual.

## Use it in your own app

The crate's library has the pieces mapterm is built from:

- `Source` opens a PMTiles file or URL, an XYZ template, or a TileJSON URL
- `prepare` decodes and styles one tile; `TileStore` keeps the prepared tiles
- `MapWidget` is the ratatui widget, with `Overlay` markers and lines on top
- `Controller` turns crossterm mouse and key events into camera moves, and
  reports single clicks (`take_click`) so you can act on the spot under them

`src/bin/mapterm.rs` is a complete example: about 300 lines that fetch the
tiles the view needs, feed input to the controller, and draw the widget.

## License

MIT. Map data © OpenStreetMap contributors.
