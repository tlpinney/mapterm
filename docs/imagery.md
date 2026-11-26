# Imagery

## Raster layers

`--raster` puts an image tile layer under the vector map, by provider name or URL:

```sh
mapterm --raster esri --at 38.8895,-77.0353 -z 15        # satellite
mapterm --raster 'https://tile.openstreetmap.org/{z}/{x}/{y}.png'
mapterm --raster gibs --date 2026-10-30 -z 6                # NASA daily imagery
mapterm --raster naip --at 38.8895,-77.0353 -z 16           # US aerial photos, 60 cm
mapterm --raster planetary -z 10                            # Sentinel-2, NAIP when zoomed in
mapterm --raster mall.pmtiles                              # your own imagery, offline
mapterm --raster ortho.mbtiles                             #   (PMTiles or MBTiles)
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
| `naip` | USDA NAIP aerial photos, US only, ~60 cm, public domain; from zoom 12 (Microsoft Planetary Computer) | none |
| `sentinel2` | Sentinel-2, the latest clear scenes, global, ~10 m; from zoom 9 (Microsoft Planetary Computer) | none |
| `planetary` | Both: Sentinel-2 from zoom 9 with NAIP on top from zoom 12; Sentinel-2 shows wherever NAIP has nothing | none |
| `esri-light`, `esri-dark` | Esri Gray Canvas, quiet backgrounds for overlays | none |
| `vantor` | Vantor (Maxar) Vivid basemap | `VANTOR_API_KEY` |
| `planet` | Planet basemaps (your subscription's mosaics) | `PL_API_KEY` + a mosaic |
| `mapbox` | Mapbox Satellite, Retina tiles | `MAPBOX_ACCESS_TOKEN` |
| `maptiler` | MapTiler Satellite | `MAPTILER_KEY` |
| `google`, `google-roadmap`, `google-terrain` | Google Maps satellite, road map, terrain (Map Tiles API, billed per tile) | `GOOGLE_MAP_TILES_KEY` |

In a style file, a raster source can name any of these instead of a URL, so you can
mix providers by zoom yourself (keys come from the same environment variables):

```json
"sources": {
  "s2":   {"type": "raster", "mapterm:raster": "sentinel2", "maxzoom": 14},
  "naip": {"type": "raster", "mapterm:raster": "naip"}
},
"layers": [
  {"id": "s2",   "type": "raster", "source": "s2"},
  {"id": "naip", "type": "raster", "source": "naip", "minzoom": 12}
]
```

API keys never go on the command line: they come from the provider's
environment variable or from a command that prints the key, such as your password
manager's CLI:

```sh
mapterm --raster vantor --raster-key-cmd 'security find-generic-password -s vantor -w'  # macOS Keychain
mapterm --raster vantor --raster-key-cmd 'op read op://Private/Vantor/credential'      # 1Password
mapterm --raster vantor --raster-key-cmd 'secret-tool lookup service vantor'           # Linux keyring
```

Keys are only added to requests (Vantor's as the `maxar-api-key` header) and are
blanked out of errors. Google's tiles need a session, which mapterm asks for at start;
the status bar credits "Google Maps" and Google's imagery or map data credits for the
view (asked again when it settles somewhere new). Google tiles are kept on disk only as
long as each tile's `Cache-Control` header allows (a day, as of 2026), then checked with
its `ETag` instead of downloaded again, as Google's terms ask; `--raster-no-cache` turns
that off. Mapbox's tiles (`--raster mapbox`, and the vector tiles of `mapbox://` styles)
are cached the same way (12 hours, as of 2026); one token, from `--mapbox-token-cmd` or
`MAPBOX_ACCESS_TOKEN`, serves both. While the camera moves, paid imagery loads for where it
will stop, not for the views on the way, and `--raster-limit N|off` caps imagery requests
a minute (300 by default for keyed providers). Keyed providers don't cache tiles on disk unless you pass
`--raster-cache`, since commercial licenses often limit storing them. For any
other service, use its URL with `{key}` where the key goes (filled from
`MAPTERM_RASTER_KEY` or `--raster-key-cmd`), or for header-based keys
`--raster-header 'Name: {key}'` (e.g. `'Authorization: Bearer {key}'`). Write `{key}`
rather than the key itself: anything on the command line shows up in `ps` and in your
shell history.

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
- On terminals with the kitty graphics protocol (Ghostty, kitty, WezTerm, Rex,
  and through herdr) tiles are real images underneath the text, so vector labels,
  lines, markers and inspect draw on top. Each tile is sent once; panning and
  zooming only move it. Elsewhere the layer becomes cell background colors.
  `MAPTERM_GRAPHICS=kitty` or `cells` overrides the guess.
- When the terminal says it can (mapterm asks at startup), tiles go to it through
  POSIX shared memory instead of inline base64, which the terminal reads and frees
  itself: Ghostty, kitty, and herdr panes, among others. Over SSH, and in terminals
  that say no, they're sent inline. `MAPTERM_KITTY_MEDIUM=shm` or `direct` overrides
  the check.
- mapterm keeps at most 48 MiB of decoded images in the terminal (herdr allows
  64 MiB per pane), freeing tiles that are off screen first, so the terminal never
  drops one that is showing. `MAPTERM_KITTY_BUDGET_MB` changes it.
- **`v`** cycles the vector overlay: lines and labels, labels only, off. Area
  fills are always off over a raster layer.
- Tiles are cached on disk for 30 days and fetched at most two at a time.
  Respect each tile server's usage policy: OpenStreetMap's own tile servers
  allow only light use, with attribution.
