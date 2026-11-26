# Use it in your own app

mapterm is also a library that works like MapLibre GL: a `Map` holds named
sources and an ordered list of style layers that say how to draw them.

```rust
let mut map = Map::from_style("liberty").await?;          // any MapLibre style
map.add_source("sites", MapSource::geojson(&sites)?)?;
map.add_layer(StyleLayer::fill("sites", "sites").color("#204060"))?;
map.add_layer(StyleLayer::symbol("pins", "sites")
    .filter(json!(["==", ["geometry-type"], "Point"]))
    .glyph("◆").text(json!(["get", "name"])).color("cyan"))?;

map.load_tiles(&ctl.cam, area).await?;
frame.render_widget(map.view(&ctl.cam), area);
let under = map.query_rendered_features(&ctl.cam, area, dot, Some(&["pins"]));
```

- Styles: `Map::from_style` takes an OpenFreeMap name (`liberty`, `positron`,
  `bright`, `dark`, `fiord`), a style URL or file, a Mapbox `mapbox://styles/...` URL
  (after `mapbox::set_access_token`), or `mapterm-dark` /
  `mapterm-light`, built-in styles tuned for terminal cells. Style layers can
  also be built in Rust with `StyleLayer`.
- Change the map with `set_data`, `set_paint`, `set_layout`, `set_filter`,
  `set_visibility` and `move_layer`. Filters and paint values are MapLibre
  expressions (`get`, `match`, `case`, `interpolate`, `step`, ... and the legacy
  filter syntax).
- A terminal cell can't show line widths, blur or fonts, so only colors,
  opacity (blended with the background), filters, zoom ranges, `text-field`,
  bold and italic fonts, `text-anchor` and `symbol-sort-key` are used. The
  `mapterm:glyph` layout property sets the character drawn at points.
- GeoJSON sources are spatially indexed, and `with_cluster` (or `cluster: true`
  in a style) clusters points; clusters carry `point_count` and
  `point_count_abbreviated`, and `cluster_expansion_zoom` says where they split.
- `events::Events` turns the pointer and clicks into MapLibre-style layer events
  (`Click`, `Enter`, `Leave` for the layers you name) and can set a `hover` feature
  state, so a layer styled with `["feature-state", "hover"]` lights up under the
  pointer. `set_feature_state` works for any feature with an id (GeoJSON `id`,
  vector tile ids, or a source's `promoteId` property).
- Anything else is a `CustomLayer`: `draw` (lines, fills, markers and labels
  through a `DrawContext`) and `pick`. Markers and labels from every layer are
  placed together, so they never overlap.
- `legend::Legend` draws a legend box in a corner of the map: a title and entries (a
  symbol in a color, a label, a count). `satellites::Coloring` makes one for satellites by
  orbit or country, and `adsb::legend` and `ais::legend` for aircraft and ships.
- `Controller` gives MapLibre-style mouse and keyboard handling and camera
  moves (`fly_to`, `ease_to`, `fit_bounds`), and `take_click` reports clicks for
  your own popups and panels. `geocode::Geocoder` searches places.

[`examples/satellites_by_country.rs`](../examples/satellites_by_country.rs) shows every
active satellite colored by owner, with a legend, in about 70 lines.
[`examples/hello_map.rs`](examples/hello_map.rs) builds a basemap from a few
style layers and adds a pin in about 50 lines, and
[`examples/style.rs`](examples/style.rs) shows any style
(`cargo run --release --example style -- dark`). `src/bin/mapterm.rs` is the
full viewer built on the same `Map` (background tile loading with
`tile_requests` / `insert_loaded`, kitty images with `kitty_commands`, and
`query_features` for inspect).
