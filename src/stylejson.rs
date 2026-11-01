//! Loading MapLibre style documents (style spec v8) into a [`Map`]: vector sources (TileJSON
//! `url`, `tiles` templates, PMTiles), GeoJSON sources (inline or by URL) and the layers.
//! Sprites, glyphs, terrain and light don't apply in a terminal and are ignored.
//!
//! ```ignore
//! let map = Map::from_style("liberty").await?;            // OpenFreeMap by name
//! let map = Map::from_style("https://example.com/style.json").await?;
//! let map = Map::from_style("./my-style.json").await?;
//! ```

use anyhow::{Context, Result, bail};
use serde_json::Value as Json;

use crate::{
    map::{Map, MapSource},
    source::Source,
    spec::StyleLayer,
};

/// OpenFreeMap styles, loadable by name.
pub const OPENFREEMAP_STYLES: [&str; 5] = ["liberty", "positron", "bright", "dark", "fiord"];

/// Largest style document read.
const MAX_STYLE_BYTES: usize = 16 << 20;

/// The URL or path for a style name, URL or path.
pub fn style_location(spec: &str) -> String {
    if OPENFREEMAP_STYLES.contains(&spec) {
        format!("https://tiles.openfreemap.org/styles/{spec}")
    } else {
        spec.to_string()
    }
}

/// Read a style document from a URL, a file, or an OpenFreeMap style name.
pub async fn read_style(spec: &str) -> Result<Json> {
    let loc = style_location(spec);
    let bytes = if loc.starts_with("http://") || loc.starts_with("https://") {
        let http = crate::source::http_client();
        crate::source::get_with(&http, &loc, None, &[]).await?.with_context(|| format!("{loc}: not found"))?
    } else {
        let len = std::fs::metadata(&loc).with_context(|| format!("reading {loc}"))?.len();
        if len as usize > MAX_STYLE_BYTES {
            bail!("{loc}: larger than {} MB", MAX_STYLE_BYTES >> 20);
        }
        std::fs::read(&loc).with_context(|| format!("reading {loc}"))?
    };
    serde_json::from_slice(&bytes).with_context(|| format!("{loc}: not a JSON style"))
}

/// The layers of a style document, plus warnings about what mapterm can't draw.
pub fn style_layers(doc: &Json) -> Result<(Vec<StyleLayer>, Vec<String>)> {
    let Some(layers) = doc["layers"].as_array() else { bail!("style has no \"layers\" array") };
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for j in layers {
        let (l, w) = StyleLayer::from_json(j)?;
        warnings.extend(w);
        out.push(l);
    }
    Ok((out, warnings))
}

impl Map {
    /// A map from a style: `mapterm-dark` or `mapterm-light` (the built-in terminal styles
    /// over OpenFreeMap), an OpenFreeMap name ([`OPENFREEMAP_STYLES`]), a URL or a file.
    pub async fn from_style(spec: &str) -> Result<Map> {
        if let Some(theme) = spec.strip_prefix("mapterm-") {
            let dark = match theme {
                "dark" => true,
                "light" => false,
                _ => bail!("built-in styles are mapterm-dark and mapterm-light"),
            };
            return Map::terminal(Source::open(crate::source::OPENFREEMAP).await?, Schema::OpenMapTiles, dark);
        }
        Map::from_style_json(&read_style(spec).await?).await
    }

    /// A map drawing `src` with the built-in terminal style for its schema.
    pub fn terminal(src: Source, schema: Schema, dark: bool) -> Result<Map> {
        let mut map = Map::new();
        map.add_source(BASEMAP, MapSource::vector(src))?;
        map.add_style_layers(&terminal_style(schema, dark))?;
        Ok(map)
    }

    /// A map from a parsed style document. Vector sources that fail to open are errors;
    /// source types mapterm can't draw yet (raster, raster-dem, image, video) become warnings.
    pub async fn from_style_json(doc: &Json) -> Result<Map> {
        let mut map = Map::new();
        for (id, s) in doc["sources"].as_object().into_iter().flatten() {
            match s["type"].as_str() {
                Some("vector") => {
                    let src = open_vector(s).await.with_context(|| format!("source \"{id}\""))?;
                    let mut src = MapSource::Vector(src);
                    if let Some(p) = s["promoteId"].as_str() {
                        src = src.with_promote_id(p);
                    }
                    map.add_source(id, src)?;
                }
                Some("geojson") => {
                    let data = match &s["data"] {
                        Json::String(url) => {
                            let http = crate::source::http_client();
                            let bytes = crate::source::get_with(&http, url, None, &[]).await?.with_context(|| format!("{url}: not found"))?;
                            serde_json::from_slice(&bytes).with_context(|| format!("{url}: not JSON"))?
                        }
                        d => d.clone(),
                    };
                    let mut src = MapSource::geojson(&data).with_context(|| format!("source \"{id}\""))?;
                    if let Some(p) = s["promoteId"].as_str() {
                        src = src.with_promote_id(p);
                    }
                    if s["cluster"].as_bool() == Some(true) {
                        let d = crate::cluster::ClusterOptions::default();
                        src = src.with_cluster(crate::cluster::ClusterOptions {
                            radius_px: s["clusterRadius"].as_f64().unwrap_or(d.radius_px),
                            max_zoom: s["clusterMaxZoom"].as_u64().map_or(d.max_zoom, |z| z.min(24) as u8),
                        });
                    }
                    map.add_source(id, src)?;
                }
                Some("raster") => match open_raster(s).await {
                    Ok(r) => map.add_source(id, MapSource::raster(std::sync::Arc::new(r)))?,
                    Err(e) => map.warn(format!("source \"{id}\": {e:#}")),
                },
                t => {
                    map.warn(format!("source \"{id}\": type {} is not drawn", t.unwrap_or("?")));
                    continue;
                }
            }
            // The style's own credit for a source wins over the tiles' metadata.
            if let Some(a) = s["attribution"].as_str()
                && map.source(id).is_some()
            {
                map.set_attribution(id, a)?;
            }
        }
        if let (Some(lon), Some(lat)) = (doc["center"][0].as_f64(), doc["center"][1].as_f64()) {
            map.center = Some((lon, lat, doc["zoom"].as_f64().unwrap_or(2.0)));
        }
        map.add_style_layers(doc)?;
        Ok(map)
    }

    /// Add a style document's layers on top of the current ones.
    pub fn add_style_layers(&mut self, doc: &Json) -> Result<()> {
        let (layers, warnings) = style_layers(doc)?;
        for w in warnings {
            self.warn(w);
        }
        for l in layers {
            self.add_layer(l)?;
        }
        Ok(())
    }
}

/// Open a style's raster source: `tiles` templates, a TileJSON `url`, or `pmtiles://`.
async fn open_raster(s: &Json) -> Result<crate::raster::RasterSource> {
    use crate::raster::RasterSource;
    let max_zoom = s["maxzoom"].as_u64().unwrap_or(22).min(30) as u8;
    let tms = s["scheme"].as_str() == Some("tms");
    let mut src = match (s["url"].as_str(), s["tiles"].get(0).and_then(Json::as_str)) {
        (_, Some(t)) => RasterSource::new(t, tms, max_zoom)?,
        (Some(url), None) if url.starts_with("pmtiles://") || url.ends_with(".pmtiles") => {
            RasterSource::from_pmtiles(url.strip_prefix("pmtiles://").unwrap_or(url)).await?
        }
        (Some(url), None) => {
            let http = crate::source::http_client();
            let bytes = crate::source::get_with(&http, url, None, &[]).await?.with_context(|| format!("{url}: not found"))?;
            let tj: Json = serde_json::from_slice(&bytes).with_context(|| format!("{url}: not TileJSON"))?;
            let t = tj["tiles"].get(0).and_then(Json::as_str).with_context(|| format!("{url}: no tiles"))?;
            let tms = tms || tj["scheme"].as_str() == Some("tms");
            RasterSource::new(t, tms, tj["maxzoom"].as_u64().map_or(max_zoom, |z| z.min(30) as u8))?
        }
        _ => bail!("raster source without \"url\" or \"tiles\""),
    };
    // The style spec's default raster tile size is 512.
    src.tile_px = s["tileSize"].as_u64().unwrap_or(512).clamp(64, 4096) as u32;
    if let Some(a) = s["attribution"].as_str() {
        src.attribution = a.to_string();
    }
    Ok(src)
}

/// Open a style's vector source: TileJSON `url`, `tiles` templates, or `pmtiles://` URLs.
async fn open_vector(s: &Json) -> Result<crate::map::VectorSource> {
    let spec = match (s["url"].as_str(), s["tiles"].get(0).and_then(Json::as_str)) {
        (Some(url), _) => url.strip_prefix("pmtiles://").unwrap_or(url).to_string(),
        (None, Some(t)) => t.to_string(),
        _ => bail!("vector source without \"url\" or \"tiles\""),
    };
    if spec.starts_with("mapbox://") {
        bail!("mapbox:// URLs need Mapbox's API; use an https URL");
    }
    let mut src = Source::open(&spec).await?;
    if let Some(z) = s["minzoom"].as_u64() {
        src.min_zoom = z.min(30) as u8;
    }
    if let Some(z) = s["maxzoom"].as_u64() {
        src.max_zoom = z.min(30) as u8;
    }
    match MapSource::vector(src) {
        MapSource::Vector(v) => Ok(v),
        _ => unreachable!("MapSource::vector makes a vector source"),
    }
}

/// The display text of a road `ref`, as an expression: the first of several (`I 270;US 40`
/// -> `I 270`), `I 66` -> `I-66`, and bare numbers prefixed from the `network` OpenMapTiles
/// gives them (`395` on `us-interstate` -> `I-395`, `1` on `us-highway` -> `US 1`).
pub fn shield_text() -> Json {
    use serde_json::json;
    let r = json!(["to-string", ["get", "ref"]]);
    let semi = json!(["index-of", ";", r]);
    let first = json!(["case", [">=", semi, 0], ["slice", r, 0, semi], r]);
    let numeric = json!([">=", ["to-number", first, -1], 0]);
    json!(["case",
        ["==", ["slice", first, 0, 2], "I "], ["concat", "I-", ["slice", first, 2]],
        ["all", numeric, ["==", ["get", "network"], "us-interstate"]], ["concat", "I-", first],
        ["all", numeric, ["==", ["get", "network"], "us-highway"]], ["concat", "US ", first],
        first])
}

/// Road label text: major roads by their ref (falling back to the name), others by name.
fn road_text(major: bool) -> Json {
    use serde_json::json;
    let shield = json!(["case", ["has", "ref"], shield_text(), ["get", "name"]]);
    if major {
        shield
    } else {
        json!(["case", ["has", "name"], ["get", "name"], shield])
    }
}

/// Vector tile schemas the built-in terminal styles know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schema {
    /// OpenMapTiles: OpenFreeMap, MapTiler (water, transportation, place, ...).
    OpenMapTiles,
    /// Protomaps basemap (earth, water, roads, places, ...).
    Protomaps,
}

impl Schema {
    /// Guess the schema from a tile's source-layer names.
    pub fn detect<'a>(layers: impl IntoIterator<Item = &'a str>) -> Schema {
        if layers.into_iter().any(|l| l == "transportation" || l == "place") { Schema::OpenMapTiles } else { Schema::Protomaps }
    }
}

/// The source id the built-in styles draw from.
pub const BASEMAP: &str = "basemap";

/// mapterm's own styles, tuned for terminal cells: few layers, roads drawn smallest first so
/// major roads stay on top, labels ranked by importance. `dark` picks colors for a dark
/// terminal background (the background itself is left to the terminal). Pair with a source
/// named [`BASEMAP`].
pub fn terminal_style(schema: Schema, dark: bool) -> Json {
    use serde_json::json;
    let c = |d: &str, l: &str| -> Json { json!(if dark { d } else { l }) };
    let water = c("#162a46", "#a8c6e4");
    let park = c("#1a3220", "#cae2c0");
    let building = c("#303036", "#dad4ce");
    let waterway = c("#5082be", "#3264aa");
    let rail = c("#736e8c", "#786e96");
    let boundary = c("#9669aa", "#8c5aa0");
    let place = c("#e1e1e1", "#191919");
    let poi = c("#c8a2e0", "#7b3fa0");
    // (name, color, first zoom drawn, first zoom labelled, label sort key)
    let roads = [
        ("path", c("#464646", "#b9b9b9"), 16, 15, 0),
        ("minor", c("#585858", "#a5a5a5"), 14, 15, -5),
        ("tertiary", c("#828282", "#828282"), 12, 14, -10),
        ("secondary", c("#aaaaaa", "#646464"), 9, 13, -15),
        ("primary", c("#d2c36e", "#a0821e"), 7, 11, -20),
        ("trunk", c("#e1a046", "#c3781e"), 0, 0, -25),
        ("motorway", c("#eb6e3c", "#cd5028"), 0, 0, -30),
    ];
    // Road labels come from lines only: OpenMapTiles also has motorway junction points
    // (exit numbers like "2A; 2B") in transportation_name.
    let label = |id: &str, src_layer: &str, filter: Json, text: Json, color: &Json, sort: Json, bold: Json| {
        let filter = json!(["all", ["==", ["geometry-type"], "LineString"], filter]);
        json!({"id": id, "type": "symbol", "source": BASEMAP, "source-layer": src_layer, "filter": filter,
               "layout": {"text-field": text, "symbol-sort-key": sort, "symbol-placement": "line",
                          "text-font": ["case", bold, ["literal", ["Bold"]], ["literal", ["Regular"]]]},
               "paint": {"text-color": color}})
    };
    let mut layers = Vec::new();
    let fill = |id: &str, src_layer: &str, filter: Json, color: &Json, min: u8| {
        json!({"id": id, "type": "fill", "source": BASEMAP, "source-layer": src_layer, "filter": filter, "minzoom": min, "paint": {"fill-color": color}})
    };
    let line = |id: &str, src_layer: &str, filter: Json, color: &Json, min: u8| {
        json!({"id": id, "type": "line", "source": BASEMAP, "source-layer": src_layer, "filter": filter, "minzoom": min, "paint": {"line-color": color}})
    };
    let poly = json!(["==", ["geometry-type"], "Polygon"]);
    let lines = json!(["==", ["geometry-type"], "LineString"]);
    match schema {
        Schema::OpenMapTiles => {
            layers.push(fill("park", "park", poly.clone(), &park, 0));
            layers.push(fill("landcover", "landcover", json!(["in", ["get", "class"], ["literal", ["wood", "grass", "forest"]]]), &park, 0));
            layers.push(fill("water", "water", poly.clone(), &water, 0));
            layers.push(fill("building", "building", poly.clone(), &building, 14));
            layers.push(line("waterway", "waterway", lines.clone(), &waterway, 9));
            layers.push(line("boundary", "boundary", json!(["all", ["<=", ["get", "admin_level"], 4], ["!=", ["get", "maritime"], 1]]), &boundary, 0));
            layers.push(line("rail", "transportation", json!(["in", ["get", "class"], ["literal", ["rail", "transit"]]]), &rail, 11));
            for (name, color, min, _, _) in &roads {
                let classes = match *name {
                    "minor" => json!(["minor", "service", "busway"]),
                    "path" => json!(["path", "track"]),
                    n => json!([n]),
                };
                layers.push(line(&format!("road-{name}"), "transportation", json!(["in", ["get", "class"], ["literal", classes]]), color, *min));
            }
            layers.push(json!({"id": "water-name", "type": "symbol", "source": BASEMAP, "source-layer": "water_name",
                               "filter": ["==", ["geometry-type"], "Point"],
                               "layout": {"text-field": ["get", "name"], "text-font": ["Italic"]}, "paint": {"text-color": waterway}}));
            for (name, color, _, min_label, sort) in &roads {
                let class = if *name == "minor" { json!(["!", ["in", ["get", "class"], ["literal", ["motorway", "trunk", "primary", "secondary", "tertiary"]]]]) } else { json!(["==", ["get", "class"], name]) };
                // Major roads are labelled by their ref (I 66), others by name.
                let text = road_text(*sort <= -20);
                let mut l = label(&format!("road-{name}-label"), "transportation_name", class, text, color, json!(sort), json!(*sort <= -25));
                l["minzoom"] = json!(min_label);
                layers.push(l);
            }
            // Points of interest as icon glyphs with names: the most important from z15
            // (OpenMapTiles ranks them per tile, 1 first), more as you zoom in.
            layers.push(json!({"id": "poi", "type": "symbol", "source": BASEMAP, "source-layer": "poi", "minzoom": 15,
                "filter": ["all", ["==", ["geometry-type"], "Point"], ["<=", ["coalesce", ["get", "rank"], 99], ["step", ["zoom"], 4, 16, 14, 17, 99]]],
                "layout": {"icon-image": ["get", "class"], "text-field": ["get", "name"], "symbol-sort-key": ["coalesce", ["get", "rank"], 99]},
                "paint": {"text-color": poi}}));
            layers.push(json!({"id": "airport", "type": "symbol", "source": BASEMAP, "source-layer": "aerodrome_label", "minzoom": 10,
                "layout": {"icon-image": "airport", "text-field": ["coalesce", ["get", "iata"], ["get", "name"]]},
                "paint": {"text-color": poi}}));
            layers.push(json!({"id": "place", "type": "symbol", "source": BASEMAP, "source-layer": "place",
                "layout": {"text-field": ["get", "name"],
                           "symbol-sort-key": ["-", ["coalesce", ["get", "rank"], 10],
                               ["match", ["get", "class"], ["continent", "country"], 95, "state", 80, "city", 70, "town", 55, ["village", "suburb"], 40, 25]],
                           "text-font": ["case", ["==", ["get", "class"], "city"], ["literal", ["Bold"]], ["literal", ["Regular"]]]},
                "paint": {"text-color": place}}));
        }
        Schema::Protomaps => {
            layers.push(fill("park", "landuse", json!(["in", ["get", "kind"], ["literal",
                ["park", "forest", "wood", "nature_reserve", "golf_course", "cemetery", "grass", "meadow", "garden", "playground", "protected_area", "national_park", "recreation_ground"]]]), &park, 0));
            layers.push(fill("landcover", "landcover", json!(["in", ["get", "kind"], ["literal", ["forest", "wood", "grass", "meadow"]]]), &park, 0));
            layers.push(fill("water", "water", poly.clone(), &water, 0));
            layers.push(fill("building", "buildings", poly.clone(), &building, 15));
            layers.push(line("waterway", "water", lines.clone(), &waterway, 0));
            layers.push(line("boundary", "boundaries", json!(["in", ["get", "kind"], ["literal", ["country", "region"]]]), &boundary, 0));
            layers.push(line("rail", "roads", json!(["==", ["get", "kind"], "rail"]), &rail, 0));
            layers.push(line("transit", "transit", lines.clone(), &rail, 11));
            let filter = |name: &str| match name {
                "motorway" => json!(["==", ["get", "kind"], "highway"]),
                "trunk" => json!(["all", ["==", ["get", "kind"], "major_road"], ["==", ["get", "kind_detail"], "trunk"]]),
                "primary" => json!(["all", ["==", ["get", "kind"], "major_road"], ["==", ["get", "kind_detail"], "primary"]]),
                "secondary" => json!(["all", ["==", ["get", "kind"], "major_road"], ["!", ["in", ["get", "kind_detail"], ["literal", ["trunk", "primary"]]]]]),
                "tertiary" => json!(["==", ["get", "kind"], "medium_road"]),
                "minor" => json!(["!", ["in", ["get", "kind"], ["literal", ["highway", "major_road", "medium_road", "path", "rail"]]]]),
                _ => json!(["==", ["get", "kind"], "path"]),
            };
            for (name, color, min, _, _) in &roads {
                layers.push(line(&format!("road-{name}"), "roads", filter(name), color, *min));
            }
            layers.push(json!({"id": "water-name", "type": "symbol", "source": BASEMAP, "source-layer": "water",
                               "filter": ["==", ["geometry-type"], "Point"],
                               "layout": {"text-field": ["get", "name"], "text-font": ["Italic"]}, "paint": {"text-color": waterway}}));
            for (name, color, _, min_label, sort) in &roads {
                let text = road_text(*sort <= -20);
                let mut l = label(&format!("road-{name}-label"), "roads", filter(name), text, color, json!(sort), json!(*sort <= -25));
                l["minzoom"] = json!(min_label);
                layers.push(l);
            }
            // Points of interest: Protomaps gives each a zoom to appear from.
            layers.push(json!({"id": "poi", "type": "symbol", "source": BASEMAP, "source-layer": "pois", "minzoom": 14,
                "filter": ["all", ["==", ["geometry-type"], "Point"], ["<=", ["coalesce", ["get", "min_zoom"], 16], ["+", ["zoom"], 0.5]]],
                "layout": {"icon-image": ["get", "kind"], "text-field": ["get", "name"], "symbol-sort-key": ["coalesce", ["get", "min_zoom"], 16]},
                "paint": {"text-color": poi}}));
            layers.push(json!({"id": "place", "type": "symbol", "source": BASEMAP, "source-layer": "places",
                "filter": ["<=", ["coalesce", ["get", "min_zoom"], 0], ["+", ["zoom"], 0.5]],
                "layout": {"text-field": ["get", "name"],
                           "symbol-sort-key": ["-", 0, ["+", ["match", ["get", "kind"], "country", 90, "region", 70, "locality", 50, 30],
                               ["*", 3, ["coalesce", ["get", "population_rank"], 0]]]],
                           "text-font": ["case", ["all", ["==", ["get", "kind"], "locality"], [">=", ["coalesce", ["get", "population_rank"], 0], 10]],
                               ["literal", ["Bold"]], ["literal", ["Regular"]]]},
                "paint": {"text-color": place}}));
        }
    }
    json!({"version": 8, "name": format!("mapterm {}", if dark { "dark" } else { "light" }), "sources": {}, "layers": layers})
}
