//! Nightly checks of the live data providers the layers use: each is asked once, through
//! mapterm's own reader, and what comes back must still parse into what the layer needs.
//! A provider that moves an endpoint, renames a field or changes its format fails the test
//! named after it. Ignored by default (they need the network); CI runs them nightly with
//! `cargo test --test providers -- --ignored`. One small request per provider, to stay well
//! within each one's fair use (aisstream.io needs a key and isn't checked).

use mapterm::outages::{FEEDS, Level};

/// One runtime for every test (mapterm shares one HTTP client; see tests/network.rs).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    static RT: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| tokio::runtime::Runtime::new().expect("tokio runtime"));
    RT.block_on(f)
}

/// Seconds since 1970.
fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

#[test]
#[ignore = "nightly provider check"]
fn usgs_earthquakes() {
    block_on(async {
        let (q, modified) = mapterm::quakes::fetch(mapterm::quakes::Period::Week, None).await.expect("USGS feed").expect("an answer");
        assert!(q.len() > 20, "only {} quakes M2.5+ in a week", q.len());
        assert!(q.iter().filter(|q| q.mag.is_some()).count() * 10 >= q.len() * 9, "magnitudes missing");
        assert!(q.iter().all(|q| q.time > now() - 8.0 * 86400.0 && q.time <= now() + 60.0), "times off");
        assert!(q.iter().any(|q| q.url.starts_with("https://earthquake.usgs.gov/")), "no USGS event pages");
        assert!(modified.is_some(), "no Last-Modified: If-Modified-Since won't work");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn nifc_wildfires() {
    block_on(async {
        let (f, _) = mapterm::fires::incidents(None).await.expect("NIFC incidents").expect("an answer");
        assert!(!f.is_empty(), "no fire incidents at all");
        assert!(f.iter().any(|f| f.kind == "WF"), "no wildfires (IncidentTypeCategory)");
        assert!(f.iter().any(|f| f.acres.is_some()), "no sizes (IncidentSize)");
        let (p, _) = mapterm::fires::perimeters(None).await.expect("NIFC perimeters").expect("an answer");
        // Perimeters can be few in winter; every one must join an incident's id format.
        assert!(p.keys().all(|k| k.len() >= 30), "perimeter ids look wrong: {:?}", p.keys().next());
    });
}

#[test]
#[ignore = "nightly provider check"]
fn nwps_river_gauges() {
    block_on(async {
        let g = mapterm::floods::gauges((-77.6, 38.6, -76.8, 39.3)).await.expect("NWPS gauges");
        assert!(g.len() > 50, "only {} gauges around DC", g.len());
        let live = g.iter().filter(|g| g.stage.is_some()).collect::<Vec<_>>();
        assert!(live.len() > 20, "only {} with a current stage", live.len());
        assert!(live.iter().any(|g| g.category != mapterm::floods::Category::Unknown), "no flood categories");
        let d = mapterm::floods::detail(&live[0].lid).await.expect("a gauge's detail");
        assert!(!d.observed.is_empty(), "no observed stages for {}", live[0].lid);
    });
}

#[test]
#[ignore = "nightly provider check"]
fn faa_flight_restrictions() {
    block_on(async {
        use mapterm::tfrs::Kind;
        let t = mapterm::tfrs::tfrs().await.expect("TFR list and outlines");
        // Security TFRs are always in force somewhere.
        assert!(t.len() >= 5, "only {} TFRs", t.len());
        assert!(t.iter().any(|r| r.kind == Kind::Security), "no security TFRs (list types)");
        let n = mapterm::tfrs::notam_detail(t[0].notam.as_deref().expect("a NOTAM number")).await.expect("a NOTAM's detail XML");
        assert!(!n.areas.is_empty(), "no areas in NOTAM {:?}", t[0].notam);
        let s = mapterm::tfrs::standing().await.expect("FAA standing airspace");
        for k in [Kind::Rules, Kind::Prohibited, Kind::Stadium] {
            assert!(s.iter().any(|r| r.kind == k), "no {k:?} in the FAA's layers");
        }
        assert!(s.iter().any(|r| r.title.starts_with("P-56")), "no P-56");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn road_closures() {
    block_on(async {
        use mapterm::closures::{FEEDS, fetch};
        // Fairfax County's service has been failing its queries (2026-11); left out until it
        // works. The others must answer and parse; the busy ones must have something.
        for feed in FEEDS.iter().filter(|f| !f.id.starts_with("ffx")) {
            let c = fetch(feed).await.unwrap_or_else(|e| panic!("{}: {e:#}", feed.id));
            if ["md", "wz-md", "wz-nj"].contains(&feed.id) {
                assert!(!c.is_empty(), "{}: nothing", feed.id);
                assert!(c.iter().any(|c| !c.road.is_empty()), "{}: no road names", feed.id);
            }
        }
    });
}

#[test]
#[ignore = "nightly provider check"]
fn nws_weather_alerts() {
    block_on(async {
        let (a, etag) = mapterm::nws::active(None).await.expect("NWS alerts").expect("an answer");
        // Somewhere in the US there's always an alert.
        assert!(!a.is_empty(), "no alerts at all");
        assert!(a.iter().any(|a| !a.zones.is_empty() || a.geometry.is_some()), "no areas");
        assert!(etag.is_some(), "no ETag");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn rainviewer_radar() {
    block_on(async {
        let s = mapterm::rainviewer::frames().await.expect("RainViewer index");
        assert!(s.past.len() >= 6, "only {} scans", s.past.len());
        assert!((s.generated as f64) > now() - 3600.0, "the index is over an hour old");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn state_power_outages() {
    block_on(async {
        for feed in &FEEDS {
            let (areas, _) =
                mapterm::outages::counts(feed, Level::County, None).await.unwrap_or_else(|e| panic!("{} counts: {e:#}", feed.state)).expect("an answer");
            assert!(areas.len() >= 20, "{}: only {} counties", feed.state, areas.len());
            assert!(areas.iter().any(|a| a.served.is_some_and(|s| s > 1000)), "{}: no customers served", feed.state);
            let outlines = mapterm::outages::outlines(feed, Level::County).await.unwrap_or_else(|e| panic!("{} outlines: {e:#}", feed.state));
            assert!(areas.iter().filter(|a| outlines.contains_key(&a.key)).count() >= areas.len() * 9 / 10, "{}: outlines don't match counties", feed.state);
            // The finer level answers (it may have nobody out).
            mapterm::outages::counts(feed, Level::Local, None).await.unwrap_or_else(|e| panic!("{} local: {e:#}", feed.state));
        }
    });
}

#[test]
#[ignore = "nightly provider check"]
fn odin_outages() {
    block_on(async {
        let s = mapterm::odin::status().await.expect("ODIN status");
        assert!(s.len() > 50, "only {} reporting utilities", s.len());
        assert!(s.values().any(|u| u.received > now() - 3600.0), "no utility reported in the last hour");
        mapterm::odin::reports(None).await.expect("ODIN outages").expect("an answer");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn celestrak_orbits() {
    block_on(async {
        // The smallest useful group, once a night (CelesTrak asks for one download per update).
        let l = mapterm::satellites::load("stations").await.expect("CelesTrak stations");
        assert!(l.sats.len() >= 5, "only {} station elements", l.sats.len());
        assert!(l.sats.iter().any(|s| s.name.contains("ISS")), "no ISS");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn satnogs_transmitters() {
    block_on(async {
        let (by_sat, ..) = mapterm::satnogs::load().await.expect("SatNOGS DB");
        assert!(by_sat.len() > 300, "only {} satellites with transmitters", by_sat.len());
        // The ISS (25544) has transmitters.
        assert!(by_sat.get(&25544).is_some_and(|t| !t.is_empty()), "no ISS transmitters");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn adsb_lol_aircraft() {
    block_on(async {
        let feed = mapterm::adsb::Feed::named("lol").expect("adsb.lol");
        let a = mapterm::adsb::fetch(&feed, -77.04, 38.85, 40.0).await.expect("adsb.lol answer");
        assert!(!a.aircraft.is_empty(), "no aircraft around DC");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn sondehub_balloons() {
    block_on(async {
        // Central Europe launches several a day; the last six hours within 1,500 km.
        let t = mapterm::sondehub::fetch_near(50.0, 10.0, 1_500_000.0, 6 * 3600).await.expect("SondeHub");
        assert!(!t.is_empty(), "no balloons over central Europe in six hours");
        assert!(t.iter().all(|t| !t.serial.is_empty()), "serials missing");
    });
}

#[test]
#[ignore = "nightly provider check"]
fn overture_addresses_catalog() {
    block_on(async {
        let url = mapterm::overture::collection_url("overture-addresses").await.expect("Overture catalog");
        assert!(url.starts_with("https://stac.overturemaps.org/") && url.ends_with("/addresses/address/collection.json"), "{url}");
        let text = mapterm::source::http_client().get(&url).send().await.expect("collection").text().await.expect("text");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(doc["type"], "Collection");
        assert!(doc["links"].as_array().is_some_and(|l| l.iter().filter(|l| l["rel"] == "item").count() > 10), "few items");
    });
}
