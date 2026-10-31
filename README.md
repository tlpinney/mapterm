# termtiles

Vector map tiles in the terminal: a ratatui map widget plus the `mapterm` viewer.

```
mapterm                                              # OpenFreeMap planet (OpenMapTiles schema)
mapterm https://build.protomaps.com/20261031.pmtiles  # Protomaps planet over HTTP range requests
mapterm dmv.pmtiles --at 38.8899,-77.0091 -z 13          # local extract, offline
mapterm --snapshot 120x40 --at 38.8895,-77.0353           # print one frame and exit
```

- **Sources:** PMTiles v3 (local file or HTTP ranges; own reader), XYZ `{z}/{x}/{y}` templates,
  TileJSON. 30 s timeouts, 8 MB fetch cap, 32 MB inflate cap, 30-day disk cache for HTTP.
- **Decoding:** own bounds-checked MVT/protobuf decoder (no codegen).
- **Styles:** Protomaps basemap and OpenMapTiles schemas, dark and light themes.
- **Rendering:** braille lines (2x4 dots per cell), area fills as cell backgrounds, labels with
  collision avoidance, ancestor tiles as stand-ins while loading, host overlays (markers, lines).

Keys follow MapLibre GL conventions: wheel zooms toward the cursor · drag pans with inertia ·
double-click zooms in, shift+double-click out · shift+drag box zoom · arrows/hjkl pan · `+`/`-` zoom
to the next level · sideways swipe pans · `n` labels · `t` theme · `q` quit. Clicks are reported via
`Controller::take_click` for feature inspection.

**Smooth panning (opt-in):** `--smooth`, `MAPTERM_SMOOTH=1`, or `s` at runtime (it deliberately ignores
a terminal-wide `SMOOTH_SCROLL`, since smooth mode changes what the wheel does). On terminals with the
experimental smooth-scroll region protocol ([tlpinney/ghostty](https://github.com/tlpinney/ghostty),
relayed by tlpinney/herdr), two-finger up/down pans the map pixel-smoothly; zoom then uses `+`/`-`,
double-click and right-click. Other terminals ignore the declaration and pan by rows.

Map data © OpenStreetMap contributors.
