# Styles and rendering

Without `--style`, mapterm draws the source with its own terminal style (OpenMapTiles
or Protomaps schema, detected from the tiles; `--light` or `t` for the light theme).
MapLibre styles draw what a terminal can show: colors, fills, lines, labels (bold and
italic fonts), filters and zoom ranges, and point icons as one-cell symbols (✚ hospital,
✈ airport, ⋔ restaurant, ◡ café, Ⓑ bus stop, ...). Wide lines draw two or three
braille dots thick and dashed lines dotted; imagery opacity blends with the
background in cell-color mode; blur, patterns and 3D are skipped. The
built-in style shows points of interest from zoom 15, more as you zoom in.

A style file can use local tiles: a source `"url"` of `mbtiles://city.mbtiles` or
`pmtiles://city.pmtiles` (or a bare path) is read from the style file's folder, and
`mbtiles:///abs/path.mbtiles` from anywhere, as in MapLibre Native. Vector files are map
sources, image files raster sources. Point a style like liberty at a downloaded region
and the whole map works offline. Styles from a URL can't name local files.

## What it reads

- **PMTiles v3**, from a local file or over HTTP range requests (own reader)
- **MBTiles** files (SQLite), vector or raster, from the command line or a style's
  `mbtiles://` source
- **XYZ** URL templates (`…/{z}/{x}/{y}.pbf`) and **TileJSON** endpoints
- Vector tiles in the **OpenMapTiles** schema (OpenFreeMap, MapTiler) or the
  **Protomaps** basemap schema, in dark and light styles

Every download has a 30 s timeout and an 8 MB cap, decompression is capped at
32 MB, and HTTP tiles are cached on disk for up to 30 days, an MBTiles file per source in
`~/.cache/mapterm` with a size limit (`mapterm --cache` lists the caches).

## Rendering

Roads, rivers and boundaries are braille lines (2x4 dots per character); water,
parks and buildings fill the cell background; labels avoid each other. While
sharper tiles load, lower-zoom tiles stand in for them.
