# Changelog

All notable changes to mapterm are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (pre-1.0: a minor version may break the API).

## [Unreleased]

### Added

- Base map switcher: `b` opens a list of the map styles and imagery (satellite and aerial
  photos; keyed providers once their key is set) in the middle of the map. Up/down (or j/k)
  preview after a moment, typing filters, Enter keeps, Esc goes back; labels and the overlay
  over imagery toggle there too. Imagery can now be picked while the viewer runs. `m`/`M`
  step through the whole list, `'` flips back to the base map before. `mapterm::basemaps`.

- Traffic speeds (`--traffic`): Maryland CHART's speed sensors, a dot per sensor colored by
  its slowest direction, speeds labeled from zoom 10. With a route, each choice shows its
  time with traffic beside the free-flow time. `mapterm::traffic`.

- Route choices: up to three routes (Valhalla's `alternates`), the others drawn dim, the panel
  listing each one's time, distance and main road; `1`–`3` picks one.

- Directions: `--route FROM TO` (each end `here`, `LAT,LON` or a place), or `d` after a `/`
  search, draws the route, its distance and time on the status line, and the turns in a
  panel (`Esc` hides it, `x` clears it); `--route-mode car|bike|walk`. With `--closures` the
  route avoids the closed roads near the trip. From Valhalla over OpenStreetMap: FOSSGIS's
  public server (fair use: only when asked, at most once a second, `X-Client-Id: mapterm`)
  or your own (`--router`, `MAPTERM_ROUTER`). `mapterm::route`.

- A disclaimer: with a live or hazard layer on, "Not for navigation or emergencies (? more)"
  follows the credits at the bottom of the map, and the `?` key list ends with it in full
  (also in `--help`, the README and the live layers guide).
- Virginia in `--closures` with your own VDOT SmarterRoads token (`--vdot-key-cmd` or
  `VDOT_SMARTERROADS_TOKEN`): VDOT's WZDx work zones and its road closures (VaTraffic
  events: type, location, direction, jurisdiction, lanes, times, the 511 text). The token goes only
  to that feed, URL-encoded, and is taken out of any error message.

- Road closures as image tiles where the terminal shows images: smooth lines as wide as a
  road (wider for closed roads), the selected one highlighted; braille elsewhere. The shared
  area renderer (`mapterm::areas::Shape::lines`) now draws open lines too, and
  `areas::tile_source_to` takes a max zoom.

### Fixed

- A flaky tile-cache test: its scratch folder is named by process id, and a folder left by
  an earlier run with the same id could hold stale data; it's cleared first now.

- `--closures` renders the roads: unless another map is asked for, Esri's Dark Gray Canvas
  goes underneath and the vector map starts on labels only. The live overlay layers
  (closures, gauges, fires, quakes) stay up whatever `v` shows, like aircraft and ships.

- `mapterm-roads`, a basemap for looking at roads and the default with `--closures`: every
  road drawn and named in greys and cool whites by class over dim land and water, with
  county lines, and no warm colors (those are the closures'). `--style mapterm-roads` and
  the `m` cycle pick it too.

- Road closures: `--closures` draws closures and work zones from Maryland's road closure
  system (counties and cities: construction, accidents, parades, special events; planned
  ones a week ahead), Montgomery County, Prince William County, Fairfax County, and the
  WZDx work-zone feeds of MDOT SHA, DelDOT and NJDOT, as lines or points colored by impact.
  Click one for its road, reason, times and source. Every 5 minutes. `mapterm::closures`.

- River and creek levels: `--floods` draws every NOAA National Water Prediction Service
  gauge in view (from zoom 8; public, no key) in its NWS flood category, with ↑ where the
  forecast is worse. Click one for its stage and flow, flood stages, the last three days and
  the forecast as a small chart, and record crests. Gauges around the view, again when it
  leaves that area and every 10 minutes. `mapterm::floods`.
- Flight restrictions: `--tfrs` draws the FAA's temporary flight restrictions (tfr.faa.gov,
  no key) colored by kind (VIP, security, hazards, space, events), over the standing
  restricted airspace from the FAA's ArcGIS layers: special flight rules areas (DC SFRA and
  FRZ), prohibited areas, and stadiums' 3 NM event circles. Click one for its NOTAM: each
  area's rule, floor and ceiling, and when it's in force. TFRs every 5 minutes; standing
  airspace kept a week. `mapterm::tfrs`.

- Overture Maps themes by name: `--geojson overture-addresses`, `overture-buildings` and
  `overture-places` find the latest release in Overture's STAC catalog and load as the map
  moves. From zoom 17, address points are labeled with their house numbers (`number` or
  `housenumber`), and a feature's label is its name, else its number and street.
  `mapterm::overture`.
- `/` searches the data layers on the map before the geocoder: an address by number and
  street (`1600 penn`), a street by name (`vermont ave`, one result framing its addresses),
  or a feature by name, with common street abbreviations taken as the same
  (`mapterm::geocode::search_features`). They're listed first, and still found when the
  geocoder can't be reached.
- Wildfires: `--fires` draws the US's current wildland fire incidents from NIFC's WFIGS
  service (public, no key) as points (✹ wildfires colored by containment, ◇ prescribed
  burns), with the perimeters of those still burning as areas. Click one for its size,
  containment, behavior, cause, dates and people assigned. Both layers every 5 minutes with
  their ETags. `mapterm::fires`.

- Earthquakes: `--quakes [hour|day|week|month]` draws earthquakes worldwide from the USGS
  real-time feeds (public domain, no key; all magnitudes for the past hour or day, M2.5+ for
  the week, M4.5+ for the month), sized by magnitude and colored by age as on USGS's map,
  with the larger ones' magnitudes. Click one for its magnitude, place, time, depth, felt
  reports, PAGER alert, tsunami flag, status and USGS page. Asked for once a minute with
  `If-Modified-Since`. `mapterm::quakes`.

## [0.6.0] - 2026-11-26

Power outages: customers without power by county across the US from ODIN, and by town and
ZIP code from Pennsylvania's and Maryland's own feeds. Plus `--cache` naming the live layers'
caches, and a fix so `--cache-clear old` can't remove them or other programs' folders.

### Added

- Power outages: `--outages` shades counties, and from zoom 9 finer areas, by the share of
  customers without power, from the states that publish a public feed (no key): Pennsylvania
  (PEMA and PennDOT; municipalities) and Maryland (MDEM; ZIP codes, shaded by customers out,
  as they have no customer counts). Click an area for customers out and served, the county's
  total and when the figures were gathered; the status line has each state's total, and
  legends the areas in each band. The states update every 15 minutes; mapterm asks every 5,
  with the ETag; outlines once, kept 30 days. Each state is an entry in
  `mapterm::outages::FEEDS`. Elsewhere, counties come from ODIN (the Outage Data Initiative
  Nationwide, Oak Ridge National Laboratory / U.S. DOE), its public county-level feed:
  shaded by customers out, with the utilities' figures, estimated restoration times, causes
  and disclaimers on click; utilities that haven't reported for 3 hours are left out and
  counted as not reporting. `mapterm::odin`.
- `mapterm --cache` lists, and `--cache-clear` empties, the live layers' caches by name:
  `radio` (SatNOGS), `zones` (NWS alert zones), `radar` (RainViewer scans) and `outages`.

### Changed

- The alert areas' tile renderer is now `mapterm::areas` (`AreaTiles`, `Shape`), shared by
  alerts and outages; `nws::AlertTiles` is an alias, given `nws::shapes(...)` (a breaking
  change for library users of `AlertTiles::set`).
- The release workflow never touches a release that is already published (a re-run would
  have put it back to draft and replaced its signed assets), and a manual run with
  `only=publish` just publishes the crate.

### Fixed

- `--cache` counted any folder it didn't know as `old`, so `--cache-clear old` would have
  removed the SatNOGS and NWS caches, and folders other programs keep there. `old` is now
  only what earlier versions wrote: folders named by a source's hash, and the old
  `geoparquet/` folder.

## [0.5.0] - 2026-11-26

Live layers and weather: every active satellite (by orbit, country or mission, with radio
frequencies from SatNOGS), weather balloons from SondeHub, RainViewer radar you can play
back on a timeline, and US weather alerts; a clock for the map, after Cesium's. Plus a
proxy for everything mapterm fetches, a key list on `?`, times in your own time zone, and
release binaries for macOS and Linux ARM with an install script and Homebrew.

### Added

- Release binaries for macOS (one universal binary for Apple silicon and Intel, signed with
  the maintainer's Developer ID and notarized) and Linux ARM64, next to Linux x86-64; an
  install script (`install.sh`: the right build, checked against the signed checksums, into
  `~/.local/bin`); a Homebrew formula from the binaries (`release/brew-formula.sh`); and
  `mapterm --version`.
- Weather balloons: `--sondes` follows the radiosondes amateur receivers track on SondeHub
  (no key; CC BY-SA 2.0): ▲ climbing and ▼ falling, colored by altitude, with their
  flights as trails, and a panel on click or Tab (altitude, climb, drift, temperature,
  humidity, frequency, receiving station, predicted landing with its path on the map). It
  asks the API once per area and follows each nearby balloon's MQTT topic, as SondeHub
  asks (`mapterm::sondehub`, with a minimal MQTT 3.1.1 client over `mapterm::ws`).
- Radar tiles are kept on disk, in one database (`rainviewer.sqlite`), by scan: a scan never
  changes, so it's never asked for twice; scans over three hours old are removed. NWS zone
  outlines saved at full detail are rewritten simplified (the cache went from 42 to 13 MB
  here). `RasterSource::store` takes any `TileStore`.
- Weather radar: `--radar` draws RainViewer's precipitation radar over the map (no key),
  the last two hours in 10-minute scans: `space` plays them in a loop or pauses, `,` and
  `.` step, `R` hides it; the status line shows the scan's time. Playing, each scan fades
  into the next (`raster::blend`, made from both, since kitty can't draw an image half
  transparent). 512-pixel tiles, the scan
  on screen only unless playing, at most 60 tile requests a minute (RainViewer allows 100),
  and its credit "Weather data by RainViewer" (`mapterm::rainviewer`).
- mapterm's mark on the map: "mt" in braille (⠍⠞) and the name, "map" amber and "term"
  blue on a dark tile as on mapterm.com, at the end of the credits line.
- Times are shown in this computer's time zone (EST, EDT...): the radar's scan time, the
  timeline (ticks on round local times), satellite passes, and when alerts end (before, in
  the issuing office's zone). `--utc` shows UTC as before. `clock::Zone`, and
  `Timeline::zone`.
- `?` shows the keys over the map, the sections for what's on (map, look, find, live
  layers, radar and time); `?` or Esc closes it. The status line says `? keys`.
- A timeline for the map's clock (`mapterm::timeline`, after Cesium's Timeline widget): tick
  labels that fit the span, data shaded, real time and the clock's time marked; click or
  drag to go to a time, the wheel zooms, shift+wheel pans. Under the map with `--radar` or
  `--alerts` (the selected alert's period shaded). `<` and `>` play the radar backward or
  forward, `[` and `]` change speed (`Clock::reverse`, `Clock::set_speed`).
- `mapterm-weather`, a basemap for weather and the default with `--radar` and `--alerts`:
  greys only (water too, since light rain is blue), coastlines and state and country borders
  as lines over the radar, faint motorways, city names that read over rain
  (`stylejson::weather_style`, `Map::weather`); in the `m` cycle.
- The selected alert's area is highlighted: a stronger fill and a bright outline, drawn over
  the others (and a white braille outline where the terminal shows no images).
- Alert areas as image tiles drawn here where the terminal shows images: smooth outlines
  and a translucent fill, panned and zoomed like imagery, above the radar and under the
  text (`nws::AlertTiles`, `RasterSource::render` for a layer whose tiles are drawn rather
  than fetched). From the disk cache the areas go up as soon as the outlines stop
  arriving.
- US weather alerts: `--alerts` draws the NWS's warnings, watches and advisories as areas
  colored by severity, each while it's in force at the clock's time (live, or the radar's);
  click one for its text. Alerts with their own outline show at once; zone outlines come
  one a second, most severe first, kept 90 days; the list is asked for every 5 minutes with
  its ETag (`mapterm::nws`).
- Time on the map: `mapterm::clock` (a `Clock` that's live or plays over a range, looping
  or not, and `Intervals` of time with data, after Cesium's), and
  `mapterm::timelayer::TimeRaster`, image frames that follow it: the frame for the clock's
  time shown, the next loaded ahead (`Map::set_prefetch`), the last kept up until the new
  one's tiles are in (`Map::raster_ready`).
- Overlay image layers (`RasterSource::overlay`): drawn over the map instead of as its
  imagery, so its land, water and background stay; in cell mode only cells the overlay
  covers take its color. `RasterSource::require_alpha` treats opaque tiles as a service's
  error image.
- Satellite radio from the SatNOGS DB (CC BY-SA 4.0, credited with CelesTrak): the
  satellite panel lists active transmitters (downlink MHz, mode, description, uplink for
  transceivers and transponders, ⚠ for SatNOGS's frequency-violation flag), and the finder
  filters to satellites with an active downlink (`radio`), in one mode (`fm`, `lora`), or
  with an active uplink as well (`uplink`: repeaters and transponders), or carrying `voice`
  or `data` (from each transmitter's mode and description: `Transmitter::usage`), combining
  with the other filters; the radio filters hold for one and the same transmitter.
  One ~4 MB request a day, cached, with the old copy and a 2-hour pause after errors
  (`mapterm::satnogs`, `Sat::radio`). The CelesTrak cache helper now serves both.
- A proxy for everything mapterm fetches: `--proxy URL` or `MAPTERM_PROXY`, with
  `socks5h://` (the proxy looks up host names), `socks5://` or `http://`, and an
  optional user name and password in the URL (never printed). It covers the HTTP clients
  (tiles, imagery, styles, search, feeds) and the WebSocket streams (`--ais`), with
  local addresses going direct (`mapterm::proxy`). A proxy URL that doesn't parse is an
  error, never a quiet direct connection.
- Satellites: `--sats [GROUPS]` places satellites from CelesTrak's orbital elements with
  SGP4 (the `sgp4` crate), computed locally several times a second: ◇ colored by orbit,
  names, and a panel on click or Tab with altitude, speed, period, the ground track and,
  with `--at here`, its place in your sky and next pass (`mapterm::satellites`). Elements
  are cached for 2 hours per group and errors back off 2 hours, following CelesTrak's
  one-download-per-update policy; the cache shows in `mapterm --cache` as `orbits`.
  Starlink's thousands are a small unnamed ·, and the selected satellite's ground track is
  a smooth image under the text where the terminal shows kitty graphics (braille elsewhere;
  `timeseries::render_lines`). There, satellites themselves are anti-aliased dots in an
  image placed to the pixel and redrawn five times a second, so they glide instead of
  hopping a cell at a time (`satellites::render_dots`); names stay text beside them, and the
  cell glyphs return while the map moves. Their names are drawn into that image too, in the
  bundled JetBrains Mono (Ghostty's built-in font; SIL OFL 1.1, `fonts/OFL.txt`) or
  `MAPTERM_FONT`, so they glide with their dots (`mapterm::imagetext`: outlines from `skrifa`,
  drawn once per glyph with tiny-skia and copied into place). The dots redraw often enough that the fastest move
  about a pixel a frame (4 to 15 times a second, by zoom), from positions worked out by SGP4
  once a second and interpolated between (`satellites::Ephemeris`, accurate to tens of
  metres); 9 ms a frame for all 16,904, about 13% of a core at 15 a second. Orbits are
  classed by the standard definitions from their elements (LEO, MEO, GEO, GSO, HEO, beyond
  GEO; `Sat::orbit_class`, `perigee_apogee`), for the colors, the legend and the panel,
  instead of by where a satellite happens to be.
  The selected satellite (Starlink too) gets a ring and its name in that image. `--sats`
  alone is `active`: every active satellite CelesTrak publishes (~17,000), Starlink's
  unlabelled. `p` opens an fzf-style finder over satellites, aircraft and ships (by name,
  number, callsign, MMSI or operator: `digitalglobe` finds WorldView and Legion), and the
  panel names a satellite's operator for well-known ones (`mapterm::fuzzy`).
- Legends: `mapterm::legend::Legend`, a box in a corner of the map with what the colors mean
  and how many of each. The viewer shows them for satellites (by orbit, or by country with
  `o` / `--sats-color country`: the six biggest owners in a color each, Starlink apart from the rest of the USA, the
  rest as "other"), aircraft by
  altitude and ships by kind; `L` hides them, and images under the text keep clear of them.
  `examples/satellites_by_country.rs` draws every active satellite colored by owner.
- Satellites by mission: communications, Earth observation, radar imaging (SAR), weather,
  navigation, science, technology, amateur, military, space station (its modules), crew and
  cargo craft, from CelesTrak's
  purpose groups (their catalogs, kept a day, loaded after the satellites are up) and, for
  SAR and the big communications constellations, by name (`satellites::Mission`,
  `load_missions`, `Coloring::Mission`). `o` cycles orbit, country and mission colors with
  their legend; the panel names the mission; the finder offers missions, imaging (optical
  and SAR together) and operators (Planet, Vantor, ICEYE, Umbra, Spire, Airbus...) as
  filters, which combine. More operators are known by name.
- Satellites by country: CelesTrak's satellite catalog (SATCAT, kept a day) gives every
  satellite's owner and launch date, shown in the panel. In the finder, typing a country
  (`china`, `japan`, `russia`, or a code such as `PRC`) offers it with its count, and Enter
  shows only its satellites ("All countries" brings them back); `--sats-country` starts that
  way (`satellites::load_catalog`, `owner_name`, `owner_code_for`).

- When the status line doesn't fit, the credits get their own line above it (up to three,
  wrapped) instead of being cut off; the data's licenses ask for them to show.

- Live ships: `--ais` streams ships from aisstream.io (free key: `AISSTREAM_API_KEY` or
  `--ais-key-cmd`) over a WebSocket for the area in view: arrows along the course colored
  by kind, names, 10-minute trails, and a panel on click or Tab (status, speed, course,
  destination, ETA, size, MMSI). It reconnects with backoff and stops when the key is
  refused (`mapterm::ais`).
- Trail images are taken down while the map moves and drawn again once it settles, at most
  once a second (each is a full-screen image the terminal must take in).
- Nightly performance tests (`tests/perf.rs`): pans and zooms over pinned tiles, live
  layers under load, and the viewer itself panning with a local ADS-B feed, checked
  against `tests/perf-baseline.json`.

- Live aircraft: `--adsb [lol|fi|URL]` draws aircraft from a free ADS-B feed (adsb.lol by
  default) as arrows along their tracks, colored by altitude, with callsigns and 90 s
  trails, refreshed for the area in view (`mapterm::adsb`). Gentle on the free feeds:
  adsb.lol every 30 s without a key (500 requests a day per address) or 12 s with one
  (`ADSB_LOL_KEY`, `--adsb-key-cmd`, sent as `x-api-key` to adsb.lol only), slowed to fit
  the quota it reports (`adsb::Quota`, shown on the status line); a 60 nm circle at most
  (`--adsb-radius`); no requests below zoom 7; a pause of 30 s to 5 min (or the feed's
  `Retry-After`) after errors and 429s. The feed's credit (adsb.lol, ODbL) comes first.
- `mapterm-aviation`, a dark flight-tracker style (airports, runways and taxiways stand out;
  few roads and places), the default with `--adsb` (`Map::aviation`,
  `stylejson::aviation_style`).
- `--adsb`: click an aircraft, or Tab through them, for a live panel (type, registration,
  altitude and climb, speed, track, squawk, distance from you); `f` follows it. Trails are
  drawn as an image under the text with kitty graphics, fading with age
  (`adsb::render_trails`), and aircraft stay visible when `v` / `n` hide the map's labels.
- Time-series positions: `mapterm::timeseries::SampledPosition` keeps timed fixes and
  interpolates between them (extrapolating along a velocity past the last), with trails.
  `--adsb` draws a poll interval in the past so aircraft glide between real fixes.
- Your location: `--at here` starts where you are, zoomed to the fix's accuracy, and `g`
  flies there and marks it. The position comes from a command printing
  `LAT LON [ACCURACY_M]`: `--location-cmd`, `MAPTERM_LOCATION_CMD`, or on macOS
  CoreLocationCLI (Location Services) when it's installed (`mapterm::locate`).

### Changed

- mapterm asks the terminal at startup whether it reads images from shared memory (a kitty
  `a=q,t=s` query followed by a Device Attributes request, so it never waits on a terminal
  that doesn't answer) instead of guessing from its name. Through herdr, and in Ghostty,
  imagery and pixel-mode tiles now go through shared memory; about 20x fewer bytes for herdr
  to relay while panning over imagery. A terminal that doesn't answer graphics queries keeps
  the old guess; never over SSH. `MAPTERM_KITTY_MEDIUM` still overrides it
  (`kitty::probe_shm`).

### Fixed

- Smooth scrolling: image tiles no longer stretch at their edges as they shift. Tiles drawn
  here (alerts) carry bleed, pixels past their edges (`RasterTile::bleed`); fetched ones
  (radar, imagery, any tile URL) get it stitched from their neighbors once those are in, a
  few ms a frame and only while smooth scrolling is on (`Map::stitch`, `Map::stitch_some`).
- Smooth scrolling (`MAPTERM_SMOOTH=shift`, Ghostty Smooth): images now move with the text
  instead of a cell at a time. Image tiles (radar, alerts, imagery) are placed with the same
  sub-cell shift the terminal gives the text (`KittyState::shift`, `kitty::place_offset`),
  and the images mapterm draws (satellite dots, trails) use the true camera.
- Opening a panel (an alert, aircraft, ship, satellite, the finder, search results) no
  longer moves the map: the panel floats over its right side instead of taking room from
  it. Images keep clear of the panel and the legends (`KittyState::holes`,
  `raster::cut`), so their text isn't over radar or imagery.
- With imagery or radar on, satellite dots and aircraft and ship trails no longer vanish or
  flicker: each frame the image layers cleared every placement on the screen, theirs too.
  Now they take down only their own placements that aren't made again
  (`kitty::unplace_one`); one made again is moved in place.
- Outlines of polygons from vector tiles no longer draw the tile grid: the segments where a
  tile cut a polygon (its border and the margin past it) are left out, as MapLibre leaves
  them out by clipping each tile. Seen as straight lines across the sea in
  `mapterm-weather`, which outlines water for its coastline.
- `--alerts` used a lot of CPU and memory while zone outlines arrived (92% of a core and
  760 MB on a first run): the layer was rebuilt from every outline every 2 seconds. Outlines
  are now simplified as they arrive (`nws::simplify`, to about 300 m: finer than a cell
  shows), and the layer is rebuilt only when the list or the set of alerts in force changes,
  or at most every 10 seconds while outlines come in. About 150 MB and 15% now.
- Credits on a narrow screen: they take as many lines above the status as they need (up to
  half the window), broken between credits rather than mid-word, so the last one is never
  cut off; before, the line count guessed character wrapping and stopped at three.
- The credits no longer pop in and out, moving the map: once they have their own lines
  they keep them until they've needed fewer for 5 seconds (or the window is resized),
  instead of following a status line whose length changes every frame. And the
  satellites' credit (CelesTrak, SatNOGS DB) stays while they're drawn as smooth dots;
  hiding their text layer for that dropped it.

## [0.4.0] - 2026-11-24

### Added

- Rex is recognized as a kitty graphics terminal, so imagery draws as real images there.
- In a local Rex or kitty window, imagery tiles go to the terminal through POSIX shared
  memory (kitty `t=s`) instead of inline base64; anywhere else, or if the object can't be
  made, they're sent inline. `MAPTERM_KITTY_MEDIUM=shm|direct` overrides the guess
  (`kitty::shm_supported`, `kitty::transmit_shm`, `KittyState::shm`).
- `--format png` draws `--raster` imagery under the map, fetched at the image's resolution
  and sampled smoothly, with `raster-opacity` (`Map::raster_rgba`).
- `--snapshot ... --format describe`: the view described for a language model: every visible
  label's kind, cell, lon/lat and direction and distance from the center; what covers the
  view (water, parks, buildings: shares and a 25 x 10 grid); the named roads, most
  important first, with class, which way each runs, its ends (cells) and length in view
  (names from the road lines or OpenMapTiles' `transportation_name`, also with labels
  hidden); OpenMapTiles and Protomaps both (Protomaps' `kind` and
  `kind_detail` for label kinds, roads, parks and `buildings`); and each data layer's nearest features with their properties
  (`mapterm::describe`, `Map::features_in_view_list`). Tested on street-level views with
  Claude Sonnet 5.5 and Haiku 5.5: 100% on close-call questions about labelled places
  (60-87% from braille text or an image) and 83-93% on water and parks (40-87%), at ~1,700
  tokens for a busy view (image ~800, braille text ~6,600).
- `--snapshot ... --format png`: the view as an image on stdout (16 x 34 pixels per cell,
  fills and lines drawn smoothly, no labels).

### Changed

- **Breaking (library):** `KittyState` has a `shm` field (send image tiles through shared
  memory), so building one field by field needs it; `KittyState::default()` is unchanged.

### Fixed

- Pixel mode (`MAPTERM_PIXELS=1`) asks for the vector tiles under its whole grid tiles,
  not only the view's: a grid tile at the view's edge whose drawing reached into a tile
  not loaded stayed blank (and kept the viewer redrawing).
- Over imagery, flat 3D buildings (`fill-extrusion`) are hidden like other fills; they
  covered the imagery in the viewer since buildings got their own layer kind.
- `mapterm --help` lists `.mbtiles` files for the map source and `--raster`.

## [0.3.0] - 2026-11-23

### Added

- MBTiles files: `mapterm city.mbtiles` for vector MBTiles (gzipped or not) and
  `--raster ortho.mbtiles` for image MBTiles (png, jpg, webp), with the name, zoom range,
  center or bounds, and attribution from the `metadata` table; rows are flipped from TMS.
  Local files, read-only (`mapterm::mbtiles`; SQLite via `rusqlite`, bundled).
- Style files can name local tiles: `"url": "mbtiles://city.mbtiles"` (vector or image,
  as in MapLibre Native; `mbtiles:///abs/path` for an absolute path). Relative paths in a
  local style, `pmtiles://` and bare paths included, are now read from the style file's
  folder rather than the current one.
- Styles and TileJSON are cached in `~/.cache/mapterm/docs.sqlite`: used for a day without
  asking (whatever shorter cache time the server gives), then checked with `If-None-Match`;
  if the server can't be reached the last copy is used, so a known map opens offline.
  Not kept when the server says `no-store`; keyed by a hash of the URL (Mapbox URLs carry
  tokens). A warm start of the liberty style takes ~0.06 s, from ~0.3 s.
- `mapterm --cache` lists the disk caches (tiles, imagery, geoparquet, docs, buildings, stac) with
  sizes, file counts and limits; `--cache-clear NAME|all` empties one or all (only inside
  `~/.cache/mapterm`, never following links; `mapterm::cache`).
- A STAC collection read within the week is opened from its saved index, without asking
  the catalog: revisiting San Salvador in Overture's buildings takes ~1.1 s (from ~1.6 s;
  the basemap alone ~0.3 s).
- Remote GeoParquet reads are cached on disk (`~/.cache/mapterm/geoparquet`): the byte
  ranges fetched, footers and chunks, keyed by URL and ETag so a changed file is read
  afresh (files without an ETag aren't cached). `--geoparquet-cache SIZE` /
  `MAPTERM_GEOPARQUET_CACHE` sets the limit (default 2 GB, least recently used out
  first; `geoparquet::set_cache_limit`). Revisiting San Salvador in Overture's buildings
  went from ~3.6 s to ~1.6 s (the basemap alone takes ~0.3 s).
- `microsoft-buildings` follows Microsoft's newest release by itself: the
  `dataset-links.csv` with the latest date linked from their README, checked once a day,
  falling back to the last found and then a built-in one (`msbuildings::latest`). Each
  release's index is kept on disk, so later starts don't download its 6 MB again
  (3.7 s -> 0.2 s to open).
- GeoParquet loads faster: a large range is fetched as 512 KB pieces at once (one
  connection to a distant object store manages about 1 MB/s); one HTTP client is shared,
  so connections are reused; and heavy columns not drawn (over 5% of a chunk, e.g.
  Overture's `id` and `sources`; the covering `bbox` always) are skipped once a read
  passes 1 MB (`--all-columns` keeps them). A cold Overture view of San Salvador went
  from ~3.3 s (3.1 to 5.1) to ~2.3 s, and 4.3 MB to 2.7 MB per chunk.

### Changed

- **Breaking (library):** `msbuildings::index_url` is `async` and returns a `String`: it
  looks up Microsoft's newest release.
- Map tile, imagery and GeoParquet caches are SQLite now, not a file per tile: an MBTiles
  file per tile source (`~/.cache/mapterm/<source>.mbtiles`, `raster/<source>.mbtiles`)
  that opens like any MBTiles, with each tile's expiry, ETag and last use beside it, and
  `geoparquet.sqlite` for GeoParquet ranges. Map tiles and imagery get size limits
  (`--tile-cache SIZE`, `--imagery-cache SIZE`, `MAPTERM_TILE_CACHE`,
  `MAPTERM_IMAGERY_CACHE`; 2 GB each by default), least recently used out first. WAL
  mode lets several mapterm windows share them. The old file caches aren't read; they
  show in `--cache` as `old` and `--cache-clear old` removes them (`mapterm::tilecache`).

### Fixed

- Pixel mode (`MAPTERM_PIXELS=1`) now redraws after `Map::set_data`, `set_paint`,
  `set_layout` and `set_filter`; the tiles kept showing the old data or style. The old
  images stay up until the new ones are drawn.

## [0.2.0] - 2026-11-23

The library's `formats::Format` gains variants (Shapefile, GeoParquet), and `spec::Kind`
one (`FillExtrusion`).

### Added

- `--buildings-cache SIZE` / `MAPTERM_BUILDINGS_CACHE` caps the disk Microsoft building
  tiles take (default 2 GB; least recently used out first, never the tile in use;
  `msbuildings::set_cache_limit`). A tile on screen whose pieces were removed is fetched
  and split again.
- `--geojson microsoft-buildings` (or a `dataset-links.csv` URL): Microsoft's current
  Global ML Building Footprints (with heights), loading as the map moves. Each ~78 km
  gzipped tile is downloaded once and split into 256 pieces on disk; later views read only
  the pieces in view (`mapterm::msbuildings`).
- The status bar shows each data layer's count of features in view, so layers (Microsoft
  and Overture buildings, say) can be compared as the map moves (`Map::features_in_view`).
- GeoParquet files from before 1.0 (no `geometry_types`, crs as a PROJJSON string) open.
- GeoParquet URLs and STAC collections of GeoParquet files (`--geojson .../collection.json`,
  e.g. Overture's) load as the map moves: from zoom 14, only the chunks (row groups) under
  the view, nearest first, with the farthest dropped past 600,000 features; a collection's
  file index is cached on disk. Overture's worldwide buildings (512 files, 257 GB) show a
  street view of San Salvador after ~3 s and ~5 MB. Adjacent column ranges are fetched as
  one request, all at once. Library: `geoparquet::Dataset`, `Map::add_part` /
  `remove_part` (GeoJSON sources gain parts, drawn and picked with the rest).
- GeoParquet layers (`--geojson x.parquet`, a file or URL; `mapterm::geoparquet`), read
  with the `geoparquet` crate. A URL is read with HTTP range requests, and `--bbox W,S,E,N`
  (or `--at` with `-z`) limits reading to an area: chunks outside it are skipped unread
  when the file has GeoParquet 1.1 bbox columns. San Salvador's 18,000 buildings came from
  a 508 MB Overture file with 5 MB downloaded. Library: `geojson::load_in` takes the area.
- CI scans every lockfile against OSV.dev (RustSec, GitHub advisories and more) with
  OSV-Scanner, on pull requests and nightly; documented exceptions in `osv-scanner.toml`.
- Pixel mode: new tiles fade in over the low-resolution stand-in (160 ms) instead of
  swapping in at once, on a terminal on this machine; over SSH they still swap, since each
  fade step sends the tile again. Library: `Map::pixel_fade`.
- 3D buildings (`fill-extrusion` layers) are drawn flat, their footprints in their color
  with a slightly darker edge, as MapLibre shows them from straight above. Styles like
  liberty draw buildings only this way past zoom 14, so they had none there.
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

- Needs Rust 1.95 or newer (was 1.89): GeoParquet is read with `geoparquet` 0.10, on
  Arrow and Parquet 60 (zstd 0.14).
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

- `webpki-root-certs` 1.0.11: no longer trusts the distrusted emSign intermediates
  (RUSTSEC-2026-0383).
- Layers drawn only with a sprite pattern (`fill-pattern`, `line-pattern`, no color) are
  skipped instead of drawn in the default black: liberty's pedestrian areas and wetlands
  came out black. A color that doesn't evaluate to one draws nothing too.
- Zooming no longer queues downloads for views you've already left: tiles start only
  into free download slots, nearest the center first, and the rest are asked for again by
  the next frame for the view it shows. A scripted zoom from 12 to 17 over NAIP made 4
  imagery requests instead of 16, and the final view filled in ~1.3 s instead of ~4 s.
- "Zoom in for imagery" in the status bar only for raster layers shown at that zoom.
- Text that looks like a zero-padded number stays text in CSV, GPX and KML properties:
  ZIP code `02139` and route name `001` were read as the numbers 2139 and 1 (found by the
  differential tests).

### Security

- `url` 2.5.9: `Url::origin()` no longer overflows the stack on deeply nested `blob:`
  URLs (RUSTSEC-2026-0592).
- `parquet` 60.2: bloom filters are no longer written as plaintext in encrypted Parquet
  files (RUSTSEC-2026-0595; mapterm only reads Parquet).
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

[Unreleased]: https://github.com/tlpinney/mapterm/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/tlpinney/mapterm/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/tlpinney/mapterm/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/tlpinney/mapterm/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/tlpinney/mapterm/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tlpinney/mapterm/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/tlpinney/mapterm/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/tlpinney/mapterm/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tlpinney/mapterm/compare/v0.1.0-alpha.3...v0.1.0
[0.1.0-alpha.3]: https://github.com/tlpinney/mapterm/compare/v0.1.0-alpha.2...v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/tlpinney/mapterm/compare/v0.1.0-alpha.1...v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/tlpinney/mapterm/releases/tag/v0.1.0-alpha.1
