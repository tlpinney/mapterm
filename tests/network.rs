//! Smoke tests against the live services mapterm uses: OpenFreeMap tiles and styles, Photon
//! place search, and a FlatGeobuf file by URL. They need the network, so they are ignored by
//! default; run them with `cargo test --test network -- --ignored` (CI does nightly). A
//! handful of requests in all, to stay within the services' fair use.

use mapterm::{Map, Source, source::OPENFREEMAP};

#[tokio::test]
#[ignore = "needs the network"]
async fn openfreemap_tiles_decode() {
    let src = Source::open(OPENFREEMAP).await.expect("OpenFreeMap TileJSON");
    assert!(src.max_zoom >= 14, "max zoom {}", src.max_zoom);
    assert!(src.attribution.as_deref().is_some_and(|a| a.contains("OpenStreetMap")), "{:?}", src.attribution);
    let tile = src.tile(14, 4686, 6268).await.expect("a tile").expect("tile over DC");
    let features = mapterm::store::decode_features(&tile).expect("valid MVT");
    for layer in ["transportation", "water", "place"] {
        assert!(features.iter().any(|f| &*f.layer == layer), "no {layer} features");
    }
}

#[tokio::test]
#[ignore = "needs the network"]
async fn openfreemap_liberty_loads() {
    let map = Map::from_style("liberty").await.expect("liberty");
    assert!(map.layer_ids().len() > 100);
    assert!(map.warnings().iter().all(|w| !w.contains("filter")), "{:?}", map.warnings());
}

#[tokio::test]
#[ignore = "needs the network"]
async fn photon_finds_the_washington_monument() {
    let geo = mapterm::geocode::Geocoder::new(mapterm::geocode::Api::Photon, mapterm::geocode::PHOTON);
    let places = geo.search("Washington Monument", Some((-77.03, 38.89, 12.0)), 5).await.expect("search");
    let p = places.first().expect("a result");
    assert!((p.lon - -77.0353).abs() < 0.01 && (p.lat - 38.8895).abs() < 0.01, "{p:?}");
}

#[tokio::test]
#[ignore = "needs the network"]
async fn flatgeobuf_by_url() {
    let layer = mapterm::geojson::load("https://flatgeobuf.org/test/data/countries.fgb", ratatui::style::Color::White).await.expect("countries.fgb");
    assert_eq!(layer.features.len(), 179);
    assert_eq!(layer.name, "countries");
}
