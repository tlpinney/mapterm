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
- Each satellite is ◇ (the ISS ◆), colored by orbit: low, medium, geostationary. Starlink
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
