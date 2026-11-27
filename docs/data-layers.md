# Your data and inspecting the map

## GeoJSON layers

`--geojson FILE|URL` draws your data over the map, as many layers as you like:

```sh
mapterm --geojson sites.geojson --geojson https://example.com/routes.geojson
mapterm --geojson stores.csv --geojson run.gpx --geojson places.kml
mapterm --geojson parcels.shp
# Overture's buildings, worldwide (512 files, 257 GB), loading only what's on screen:
mapterm --geojson https://stac.overturemaps.org/2026-10-21.0/buildings/building/collection.json \
  --at 13.70,-89.20 -z 17
# Overture's addresses, by name for the latest release, labeled with house numbers:
mapterm --geojson overture-addresses --at 38.9005,-77.035 -z 17
# Microsoft's current footprints (with heights) and Overture's, side by side:
mapterm --geojson microsoft-buildings \
  --geojson https://stac.overturemaps.org/2026-10-21.0/buildings/building/collection.json \
  --at 38.8880,-76.9950 -z 17
```

Besides GeoJSON it reads CSV/TSV with latitude/longitude columns (`lat`,
`latitude`, `lon`, `lng`, `longitude`, any case) or a WKT geometry column, GPX
(waypoints, tracks, routes), KML (placemarks with their names, descriptions
and extended data; unzip KMZ first), FlatGeobuf (lon/lat) and Shapefiles
(lon/lat; the `.dbf` attributes, `.prj` and `.cpg` code page beside the `.shp`
are read too), and GeoParquet (lon/lat). The format
comes from the extension, else the content.

GeoParquet is read only where needed. A GeoParquet URL, or a STAC collection of
GeoParquet files (its `collection.json`, as Overture publishes), loads as the map
moves: from zoom 14, the chunks under the view are fetched with HTTP range
requests, nearest the center first, and the farthest are dropped past 600,000
features. A collection's file index is read once and kept in `~/.cache/mapterm` (used
without asking the catalog again for a week), and the
parts of files already read (footers and chunks) are kept in
`~/.cache/mapterm/geoparquet`, keyed by each file's ETag so a changed file is read
afresh: revisiting an area needs only a quick check per file. That cache takes at most
2 GB, least recently used out first; set it with `--geoparquet-cache SIZE` or
`MAPTERM_GEOPARQUET_CACHE` (`0` turns it off).

`mapterm --cache` lists the caches (map tiles, imagery, GeoParquet, styles and TileJSON,
Microsoft buildings, STAC indexes) with their sizes and limits; `--cache-clear NAME` empties one, `all` every
one. Map tiles and imagery are kept in SQLite, an MBTiles file per source in
`~/.cache/mapterm` (so a cache opens like any MBTiles file), each kind within 2 GB by
default (`--tile-cache SIZE`, `--imagery-cache SIZE`), least recently used out first.
Styles and TileJSON are kept a day in `docs.sqlite` (keyed by a hash of the URL, so access
tokens aren't stored) and then checked with their ETag, so a map you've opened starts
without waiting on its servers, and opens offline from the last copy. Caches from earlier
versions (a file per tile) show as `old`: `--cache-clear old` removes them.
Over San Salvador that's one footer and one 4 MB chunk, about 3 s, out of
Overture's 257 GB. With `--bbox W,S,E,N` a layer is instead read once for that area
(local files are read once, in place).

`overture-addresses`, `overture-buildings` and `overture-places` are those Overture Maps
themes in its latest release, found from its STAC catalog when the layer opens, so a
command keeps working as Overture publishes a new release each month. They load as the
map moves like any collection. Overture's addresses come largely from OpenAddresses and
national address files (each feature carries its source and license; `i` shows them).
From zoom 17, address points are labeled with their house numbers (further out they're
plain dots), and `/` searches what's loaded: a
number and street (`1600 penn`) finds the address, a street (`vermont ave`) frames all of
its addresses, with `Avenue`/`Ave` and `Northwest`/`NW` taken as the same.

`microsoft-buildings` is Microsoft's Global ML Building Footprints as Microsoft keeps
them current (refreshed every few weeks, with height estimates; the Planetary
Computer's copy stopped in 2023). It always uses the newest release: the one Microsoft's
README links with the latest date, checked once a day (a release's `dataset-links.csv`
URL also works). They come as one gzipped file per ~78 km tile,
which can't be read in parts: the first view of a tile downloads it once (DC's is
78 MB, about 15 s) and splits it into 256 pieces under `~/.cache/mapterm/msbuildings`,
so later views there load in a second or two. Those tiles take at most 2 GB of disk
(about twenty dense cities), least recently used out first; set it with
`--buildings-cache SIZE` or `MAPTERM_BUILDINGS_CACHE` (e.g. `500MB`, `10GB`; `0` keeps
none between uses). The status bar shows each data layer's
count of features in view, for comparing layers. Files and
URLs up to 512 MB. The readers are checked against GDAL on a corpus of real files
(`tests/differential.rs`).

- Points, lines and polygons (including Multi* and GeometryCollection), each
  layer in its own color with a legend in the status bar; polygon interiors are
  tinted (left clear over a raster layer).
- Features with a `name` (or `title`/`label`) property are labeled; address points
  without one show their house number (`number` or `housenumber`) from zoom 17.
- Without `--at`, the map starts zoomed to fit all layers.
- Inspect mode (`i`) lists your features first, with all their properties, and
  `--snapshot ... --pick COL,ROW` prints them as JSON lines.
- Big files stay fast: features are spatially indexed, and a layer with more
  than 1,000 points clusters them (`◉ 152`) until you zoom in, as MapLibre's
  `cluster: true` does.

## Inspect

Press **`i`** to switch to inspect mode, and `i` (or `Esc`) again to leave it.

- **Hover** shows the top feature under the pointer in the status bar.
- **Click** lists every feature at that spot, drawn or not (roads, street names,
  buildings, parks, landcover, points of interest...), most specific first,
  and highlights the selected one on the map.
- The panel shows the style layer that draws the feature (or that none does),
  its source layer, geometry type, center, length or area, vertex count, source
  tile, and all its properties.
- **Click an entry** or press **Tab / Shift+Tab** to switch between them;
  **`y`** copies the selected feature as JSON to the clipboard (OSC 52).

For scripts, `--pick` prints the same information as JSON lines:

```sh
mapterm --at 38.8895,-77.0353 -z 16 --snapshot 100x40 --pick 50,20
```
