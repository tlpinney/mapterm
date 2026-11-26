# Live layers

Things that move or change while the map is open: where you are, aircraft, ships and
satellites.

## Your location

`--at here` starts the map where you are, zoomed to how precise the fix is, and
**`g`** flies there (and marks it) at any time. mapterm gets the position from a
command that prints `LAT LON [ACCURACY_M]`:

- On macOS, [CoreLocationCLI](https://github.com/fulldecent/corelocationcli)
  (`brew install --cask corelocationcli`), found on `PATH`. It uses Location
  Services, so the first run asks for permission, and Wi-Fi must be on. (A plain
  command-line program can't ask for that permission itself.)
- Anything else with `--location-cmd CMD` or `MAPTERM_LOCATION_CMD`: gpsd
  (`gpspipe -w -n 5 | jq -r 'select(.class=="TPV") | "\(.lat) \(.lon)"' | head -1`),
  your phone, or a fixed spot (`echo 38.8895 -77.0353`).

The command runs only for `--at here` and `g`; nothing is sent anywhere.

## Live aircraft

`--adsb` draws live aircraft over the map from a free ADS-B feed that needs no key:
[adsb.lol](https://api.adsb.lol) (the default, ODbL) or `--adsb fi`
([adsb.fi](https://github.com/adsbfi/opendata)), or your own readsb-format endpoint
as a URL with `{lat}`, `{lon}` and `{nm}`. adsb.lol's data is
[ODbL](https://opendatacommons.org/licenses/odbl/summary/), credited first on the
status line.

mapterm goes easy on these free services:

- **adsb.lol's quota:** without a key it allows 500 requests a day per address, so
  mapterm asks every 30 s (about 4 hours of watching). A free
  [feeder key](https://www.adsb.lol/docs/feeders-only/re-api/) (7,200 a day), from
  `ADSB_LOL_KEY` or `--adsb-key-cmd`, makes it every 12 s; the key is only sent to
  adsb.lol. The status line shows the requests left, and as they run low mapterm
  spreads the rest over the time until the quota resets instead of running out.
- At most a 60 nm circle (`--adsb-radius`), a bit more than the view; panning away
  asks sooner, but no more than three times as often.
- Nothing below zoom 7 (the status line says to zoom in).
- After an error, 30 s doubling to 5 minutes; after "too many requests", as long as
  the feed's `Retry-After` says.

- The map switches to `mapterm-aviation`, a dark, quiet style for this: dim land
  and water, only motorways and larger places, and airports standing out (codes
  from zoom 7, runways from zoom 10, taxiways and aprons closer in). `m` cycles to
  the others; `--raster esri-dark` or `--raster esri` with `v` twice (roads and
  labels off) puts the planes over imagery alone.
- Each aircraft is an arrow along its track, colored by altitude (gray on the
  ground, green low, through yellow and orange, to pink at cruise), with its
  callsign. A trail shows the last 90 s, fading: drawn as an image under the text
  in terminals with kitty graphics (braille lines would shimmer as they grow).
- **Click** an aircraft (or **Tab** / **shift+Tab** through them from the middle of
  the map out) for a live panel: callsign, type, registration, altitude and climb
  rate, speed, track, squawk, distance from you with `--at here`. **`f`** follows
  it; **Esc** closes.
- Aircraft move smoothly: mapterm keeps every fix with the time it was true and
  draws the aircraft a poll interval in the past, interpolating between real fixes (Cesium's
  sampled positions), so planes glide through turns instead of overshooting and
  jumping back. Past the last fix they continue along their track for up to 30 s.

The library side is `mapterm::timeseries` (`SampledPosition`: samples in, position
at any time out, with trails) and `mapterm::adsb` (`Feed`, `Tracks`), for any
moving data in your own app: update a GeoJSON source with `set_data` a few times a
second.

## Live ships

`--ais` draws live ships from [aisstream.io](https://aisstream.io)'s free AIS
stream. It needs a free key (sign in with GitHub, then create one under API keys),
from `AISSTREAM_API_KEY` or a command (`--ais-key-cmd 'pass show aisstream/api-key'`).

- Ships stream in over a WebSocket for a box around the view, from zoom 7; there's
  no polling and no quota. Panning away reconnects with a new box (at most every
  3 s); after a dropped connection mapterm waits 5 s, doubling to 5 minutes, and
  three drops right after subscribing (aisstream.io's answer to a bad key) stop it.
- Each ship is an arrow along its course (a dot when still), colored by kind:
  cargo, tanker, passenger, fishing, pleasure, tug and service, high speed,
  military, other. Names show beside them; trails cover the last 10 minutes.
- Positions arrive every few seconds under way and every few minutes at anchor;
  ships are drawn 10 s in the past and glide between fixes like aircraft.
- **Click** a ship (or **Tab** through them when there are no aircraft) for its
  status (under way, at anchor, moored), speed, course, destination, ETA, size,
  draught, call sign, IMO and MMSI. Names and voyage details come in their own
  messages every few minutes, so a ship can show only its MMSI at first.
- With `--adsb` too, ships draw under the aircraft; a click picks an aircraft
  before a ship.

aisstream.io is free and in beta; it can take several seconds (sometimes tens of
seconds) to accept a connection.

## Satellites

`--sats` shows satellites, placed from [CelesTrak](https://celestrak.org)'s orbital
elements with SGP4. No key is needed, and once the elements are in nothing more goes over
the network: positions are worked out locally, several times a second, so satellites move
smoothly (mapterm's ISS agrees with live trackers to within a kilometer).

- Groups are CelesTrak's, comma-separated: `--sats` alone is `active`, every active
  satellite CelesTrak publishes (~17,000: the ISS, Earth-imaging ones such as WorldView and
  Legion, and Starlink's ~11,000, of which a couple of hundred are always above the
  horizon). Smaller groups: `visual` (the ~150 brightest), `stations`, `starlink`,
  `resource`, `amateur`, `weather`, `gnss` and more. Dead satellites and debris aren't
  included.
- **`p`** opens a finder over everything live (satellites, aircraft, ships), fzf-style:
  type any part of a name, NORAD number, callsign or MMSI, or an operator (`digitalglobe`,
  `planet`, `spacex`), ↑/↓ to choose, Enter to select it and fly there.
- **By country:** the panel shows each satellite's owner and launch date, from CelesTrak's
  satellite catalog (refreshed daily). Type a country in the finder (`china`, `japan`,
  `russia`, `india`, or a catalog code such as `PRC`, `CIS`, `ESA`) and choose it to show
  only its satellites; "All countries" shows them all again. `--sats-country PRC,JPN`
  starts that way.
- **`o`** cycles how satellites are colored: by orbit class, by owner (the six with the
  most satellites get a color each, Starlink on its own apart from the rest of the USA,
  everyone else "other"), or by mission (`--sats-color orbit|country|mission` starts with
  one).
- **Missions:** communications, Earth observation, radar imaging (SAR), weather,
  navigation, science, technology, amateur and education, military, space station (the
  ISS's and Tiangong's modules), crew and cargo craft (Soyuz, Progress, Dragon, Cygnus,
  Shenzhou, Tianzhou, by name). CelesTrak's station group also holds the small satellites
  deployed from the ISS; those count as amateur and education.
  CelesTrak groups satellites by purpose (weather, resource, gnss, science, military...),
  so mapterm reads those groups' catalogs (kept a day; a satellite in several takes the most
  specific), and knows radar imaging by name (Umbra, ICEYE, Capella, Sentinel-1, RADARSAT,
  TerraSAR-X, COSMO-SkyMed, SAOCOM, NISAR, ALOS, Gaofen-3, StriX, QPS-SAR and more, each
  name checked against CelesTrak's) and the big communications constellations by name.
  Well-known constellations no group covers (Hulianwang, Yaogan, IRIDE, Jilin, Satellogic,
  Spire...) are named by their constellation when no group claims them, and every
  geosynchronous satellite left is counted as communications. What's still "other" is
  mostly objects not yet identified (named only by launch) and rideshare objects.
- **Radio:** the panel lists what the satellite transmits on, from the
  [SatNOGS DB](https://db.satnogs.org) (Libre Space Foundation's community database, CC
  BY-SA 4.0): each active transmitter's downlink frequency in MHz, its mode (FM, APRS,
  SSTV, BPSK, LoRa...) and what it is, the uplink for transceivers and transponders, and
  ⚠ where SatNOGS flags one as off its coordinated frequency; then how many more and how
  many inactive. With the next pass over you, that's when to listen and what to tune to.
  The finder filters to satellites with an active downlink (`radio`), one mode (`fm`,
  `lora`, `sstv`, `cw`...), or an active uplink too (`uplink`, `repeater`): the ones you
  can transmit through, with a licence; or `voice` (FM, SSB or AM that's a repeater,
  transponder or labelled voice) or `data` (digital modes, and FM packet or telemetry).
  They combine, and the radio ones must hold for the same transmitter: `amateur`, then
  `uplink`, then `voice` leaves the voice repeaters and transponders you can work (SO-50,
  AO-27, AO-123, the TEVEL2s, the ISS...); `data` instead of `voice`, the APRS digipeaters
  and other packet satellites. The labels come from each transmitter's mode and
  description, so they're a good guide, not a guarantee. The whole list is one request (about 4 MB) kept a day; after an
  error mapterm uses the old copy and doesn't ask again for 2 hours, and without one the
  panel just has no radio section. If your network can't reach db.satnogs.org,
  [`--proxy`](network.md) can.
- **Filter by mission or operator** from the finder too: `sar`, `weather`, `navigation`,
  `imaging` (Earth observation and SAR together), or an operator: `planet`, `vantor`,
  `iceye`, `umbra`, `spire`, `airbus`... Filters combine (Chinese imaging satellites:
  choose China, then imaging); "Show all satellites" clears them.

## Weather radar

```
mapterm --radar --at here -z 6
```

Rain and snow worldwide from [RainViewer](https://www.rainviewer.com) (no key), drawn over
the map. With `--radar` or `--alerts` the map switches to `mapterm-weather` (unless you give
`--style`, a source or `--light`; `m` cycles styles): greys only, so the radar and alert
colors are the only color on it, with water a slightly different grey rather than blue
(light rain is blue), coastlines and state and country borders as lines that stay readable
over the radar, motorways faintly, and city names light enough to read over rain. The radar
itself: where the terminal shows images, the radar's own colors over the land and water,
under the roads and labels; elsewhere, cell colors where it rains. RainViewer keeps the
last two hours in 10-minute scans:

| Key | Does |
| --- | --- |
| `space` | play the two hours in a loop (about six seconds), each scan fading into the next; again to pause |
| `<` `>` | play backward or forward |
| `[` `]` | slower or faster (60× to 4800×; 1200× plays two hours in six seconds) |
| `,` `.` | step back or forward one scan; `.` past the latest goes back to now |
| `R` | hide or show the radar |

A **timeline** sits under the map (after Cesium's): tick labels over a bar, the radar's two
hours shaded, real time marked ╂ and the clock's time as the needle ●. Click or drag on it
to go to that time (at or past now goes back to live); the wheel zooms it about the
pointer, shift+wheel (or a sideways swipe) pans it. With `--alerts`, the selected alert's
period is shaded too.

The status line shows the scan's time (☂ 22:40 UTC · 20 min ago; ▶ playing, ⏸ paused). It
looks for new scans every 5 minutes. RainViewer serves detail to zoom 7; closer in, the
radar is the zoom 7 image enlarged.

**Being a good client.** RainViewer's free API is for personal, educational and small
community projects, allows 100 requests a minute per address and asks clients to cache.
mapterm asks for the scan list every 5 minutes, uses 512-pixel tiles (a quarter of the
requests), loads only the scan on screen unless playing (then the rest, a few at a time),
keeps them in memory, and holds itself to 60 tile requests a minute. Its terms ask for the
credit "Weather data by RainViewer" with a link, which mapterm shows while the radar is on.

**How it works.** The map has a clock (`mapterm::clock`, after Cesium's): live, it's real
time; playing, it loops over the scans. The radar is a time layer
(`mapterm::timelayer::TimeRaster`): a transparent image layer per scan, the one for the
clock's time shown, the next loaded ahead, and the one before kept up until a new one's
tiles are in, so it never flashes blank. Playing, each scan fades into the next through two
blends made from both (kitty can't draw an image half transparent, so a blend is a new
image); the last doesn't fade into the first. Aircraft, ships and satellites stay live whatever
the clock says.

## Weather alerts (US)

```
mapterm --alerts --radar --at here -z 6
```

The National Weather Service's warnings, watches and advisories (api.weather.gov, public
domain, no key) as areas outlined and faintly filled by severity: purple extreme, red
severe, orange moderate, yellow minor. Click one for its text: what it is, until when, who
issued it, the area, the headline, description and what to do. The status line counts the
alerts in force (⚠ 431).

Thunderstorm, tornado and flood warnings come with their own outline and show at once. Most
alerts (small craft, gale, wind, winter weather...) name forecast or marine zones instead,
and the NWS gives a zone's outline one zone a request: mapterm asks for them one a second,
the most severe alerts' first, and keeps them for 90 days, so the first time there are a lot
of alerts the areas fill in over some minutes (⚠ 431 (areas 340/1459)), and after that
almost all come from the cache. The list itself is asked for every 5 minutes with its ETag,
so an unchanged list is a tiny reply.

Each alert is shown only while it's in force (from its onset to its end, like a Cesium
entity's availability): live, that's now; with `--radar` paused or playing, the radar's
time. The NWS lists only alerts still active, so ones that ended within the radar's two
hours don't come back.

## Legends

Whatever is colored gets a legend in the map's lower left, with how many of each:
satellites by orbit or by country, aircraft by altitude, ships by kind. **`L`** hides or
shows them. Images under the text (satellite dots, trails, tracks) keep clear of the
legend boxes.
- Each satellite is ◇ (the ISS ◆), colored by its orbit's standard class, from its
  elements: **LEO** (apogee up to 2,000 km), **MEO** (up to geosynchronous), **GEO**
  (geostationary: one orbit a sidereal day, circular, over the equator), **GSO**
  (geosynchronous but inclined or eccentric), **HEO** (highly elliptical, eccentricity over
  0.25, such as Molniya), or beyond GEO. The panel shows the class and the orbit's perigee
  and apogee. Starlink
  satellites are a small · without names, since there are thousands; select one to see
  its name.
- Where the terminal shows kitty graphics, satellites are dots in an image placed to the
  pixel, redrawn five times a second, so they glide instead of hopping a cell at a time;
  their names are drawn with them in JetBrains Mono (bundled; `MAPTERM_FONT=/path/to/font`
  for another), and the text markers come back while you pan.
- **Click** one (or **Tab** through them when there are no aircraft or ships) for its
  altitude, speed, period and inclination, and its ground track for the next orbit on the
  map. With `--at here` the panel shows where it is in your sky when it's up, and its next
  pass over you that rises at least 10° (rise, highest point, set, in UTC).
- CelesTrak updates elements every 2 hours and asks for one download per update, blocking
  repeats. mapterm keeps each group in `~/.cache/mapterm/celestrak` (`mapterm --cache`
  shows it as `orbits`) and downloads only when that copy is over 2 hours old; after any
  error it uses the old copy and doesn't ask again for 2 hours. It asks for CSV and gzip,
  as CelesTrak prefers, and refuses `active` together with `starlink` (the first holds the
  second).
