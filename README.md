# termtiles

Vector map tiles in the terminal: a ratatui map widget plus the `tmap` viewer.

```
tmap                                              # OpenFreeMap planet (OpenMapTiles schema)
tmap https://build.protomaps.com/20261031.pmtiles  # Protomaps planet over HTTP range requests
tmap dmv.pmtiles --at 38.8899,-77.0091 -z 13          # local extract, offline
tmap --snapshot 120x40 --at 38.8895,-77.0353           # print one frame and exit
```

- **Sources:** PMTiles v3 (local file or HTTP ranges; own reader), XYZ `{z}/{x}/{y}` templates,
  TileJSON. 30 s timeouts, 8 MB fetch cap, 32 MB inflate cap, 30-day disk cache for HTTP.
- **Decoding:** own bounds-checked MVT/protobuf decoder (no codegen).
- **Styles:** Protomaps basemap and OpenMapTiles schemas, dark and light themes.
- **Rendering:** braille lines (2x4 dots per cell), area fills as cell backgrounds, labels with
  collision avoidance, ancestor tiles as stand-ins while loading, host overlays (markers, lines).

Keys: arrows/hjkl pan · `+`/`-` zoom · scroll/swipe pan · ctrl+scroll zoom · drag · `n` labels ·
`t` theme · `q` quit.

Map data © OpenStreetMap contributors.
