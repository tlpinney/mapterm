//! Smoke tests against the live services mapterm uses: OpenFreeMap tiles and styles, Photon
//! place search, and a FlatGeobuf file by URL. They need the network, so they are ignored by
//! default; run them with `cargo test --test network -- --ignored` (CI does nightly). A
//! handful of requests in all, to stay within the services' fair use.

use mapterm::{Map, Source, source::OPENFREEMAP};

/// One runtime for every test here. mapterm shares one HTTP client across the process, and a
/// pooled connection is driven by the runtime that opened it; with a runtime per test (what
/// `#[tokio::test]` gives), a test that reuses a connection opened by a test that has
/// finished fails with "dispatch task is gone".
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    static RT: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| tokio::runtime::Runtime::new().expect("tokio runtime"));
    RT.block_on(f)
}

#[test]
#[ignore = "needs the network"]
fn openfreemap_tiles_decode() {
    block_on(async {
        let src = Source::open(OPENFREEMAP).await.expect("OpenFreeMap TileJSON");
        assert!(src.max_zoom >= 14, "max zoom {}", src.max_zoom);
        assert!(src.attribution.as_deref().is_some_and(|a| a.contains("OpenStreetMap")), "{:?}", src.attribution);
        let tile = src.tile(14, 4686, 6268).await.expect("a tile").expect("tile over DC");
        let features = mapterm::store::decode_features(&tile).expect("valid MVT");
        for layer in ["transportation", "water", "place"] {
            assert!(features.iter().any(|f| &*f.layer == layer), "no {layer} features");
        }
    });
}

#[test]
#[ignore = "needs the network"]
fn openfreemap_liberty_loads() {
    block_on(async {
        let map = Map::from_style("liberty").await.expect("liberty");
        assert!(map.layer_ids().len() > 100);
        assert!(map.warnings().iter().all(|w| !w.contains("filter")), "{:?}", map.warnings());
    });
}

#[test]
#[ignore = "needs the network"]
fn photon_finds_the_washington_monument() {
    block_on(async {
        let geo = mapterm::geocode::Geocoder::new(mapterm::geocode::Api::Photon, mapterm::geocode::PHOTON);
        let places = geo.search("Washington Monument", Some((-77.03, 38.89, 12.0)), 5).await.expect("search");
        let p = places.first().expect("a result");
        assert!((p.lon - -77.0353).abs() < 0.01 && (p.lat - 38.8895).abs() < 0.01, "{p:?}");
    });
}

#[test]
#[ignore = "needs the network"]
fn flatgeobuf_by_url() {
    block_on(async {
        let layer = mapterm::geojson::load("https://flatgeobuf.org/test/data/countries.fgb", ratatui::style::Color::White).await.expect("countries.fgb");
        assert_eq!(layer.features.len(), 179);
        assert_eq!(layer.name, "countries");
    });
}

#[test]
#[ignore = "needs the network"]
fn planetary_computer_naip_tile() {
    block_on(async {
        use mapterm::raster::{RasterSource, preset};
        let mut s = RasterSource::from_preset(preset("naip").unwrap(), None).expect("no key needed");
        s.start_session().await.expect("registering the NAIP mosaic");
        assert!(s.missing_params().is_empty(), "{:?}", s.missing_params());
        // The Washington Monument at zoom 15: 60 cm aerial photography.
        let tile = s.tile(15, 9372, 12536).await.expect("a tile").expect("NAIP covers DC");
        let img = image::load_from_memory(&tile).expect("a PNG");
        assert_eq!((img.width(), img.height()), (256, 256));
    });
}
