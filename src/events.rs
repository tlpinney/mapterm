//! Layer events, MapLibre style: `map.on("click", "layer", ...)` and `mouseenter` /
//! `mouseleave`, plus the usual hover highlight with feature state. Rust apps poll instead of
//! registering callbacks: feed [`Events`] the pointer and clicks, and act on what it returns.
//!
//! ```no_run
//! # use mapterm::{Controller, Map, StyleLayer, events::{Events, MapEvent}};
//! # use serde_json::json;
//! # fn demo(mut map: Map, mut ctl: Controller, area: ratatui::layout::Rect) -> anyhow::Result<()> {
//! let mut events = Events::new(&["sites", "route"]).hover_state("hover");
//! // A layer that lights up under the pointer:
//! map.add_layer(StyleLayer::fill("sites", "sites")
//!     .color(json!(["case", ["boolean", ["feature-state", "hover"], false], "#ffcc00", "#204060"])))?;
//! loop {
//!     // ... after handling input with the Controller:
//!     for e in events.pointer(&mut map, &ctl.cam, area, ctl.pointer()) { /* Enter / Leave */ }
//!     if let Some(click) = ctl.take_click() {
//!         for e in events.click(&map, &ctl.cam, area, &click) {
//!             if let MapEvent::Click { layer, features, .. } = e { /* open a popup */ }
//!         }
//!     }
//! #   break;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Feature state needs feature ids: GeoJSON `id`s, vector tile ids, or a source's
//! `promoteId` property ([`crate::MapSource::with_promote_id`]).

use ratatui::layout::Rect;

use crate::{interact::Click, map::Map, mvt::Value, pick::Pick, render::{Camera, Viewport}};

/// Something that happened on a layer.
#[derive(Clone, Debug)]
pub enum MapEvent {
    /// A click on the layer's features: all of them under the click, top first.
    Click {
        /// The layer clicked.
        layer: String,
        /// Its features under the click, top first.
        features: Vec<Pick>,
        /// Longitude of the click.
        lon: f64,
        /// Latitude of the click.
        lat: f64,
    },
    /// The pointer moved onto a feature of the layer (the top one under it).
    Enter {
        /// The layer.
        layer: String,
        /// The feature now under the pointer.
        feature: Pick,
    },
    /// The pointer left that feature.
    Leave {
        /// The layer.
        layer: String,
        /// The feature the pointer left.
        feature: Pick,
    },
}

/// Layer events for a set of layers. See the [module docs](self).
#[derive(Default)]
pub struct Events {
    layers: Vec<String>,
    hover_key: Option<String>,
    /// The feature under the pointer, per layer.
    hovered: Vec<(String, Pick)>,
    /// The pointer and camera last checked, to skip repeats.
    last: Option<Checked>,
}

/// A pointer position (dots), camera (lon, lat, zoom) and map area.
type Checked = ((f64, f64), (f64, f64, f64), Rect);

impl Events {
    /// Events for these layers (style or custom layer ids).
    pub fn new(layers: &[&str]) -> Self {
        Self { layers: layers.iter().map(|s| s.to_string()).collect(), ..Self::default() }
    }

    /// Set feature state `key` to true on the feature under the pointer (and remove it when the
    /// pointer leaves), for `["feature-state", key]` hover styling.
    pub fn hover_state(mut self, key: &str) -> Self {
        self.hover_key = Some(key.to_string());
        self
    }

    /// The features under the pointer now, by layer.
    pub fn hovered(&self) -> impl Iterator<Item = (&str, &Pick)> {
        self.hovered.iter().map(|(l, p)| (l.as_str(), p))
    }

    /// Where the pointer is (dots relative to `area`, e.g. [`crate::Controller::pointer`]), or
    /// `None` when it's off the map. Returns `Leave` and `Enter` events as the feature under it
    /// changes on each layer; call it every frame (repeats with nothing new are skipped).
    pub fn pointer(&mut self, map: &mut Map, cam: &Camera, area: Rect, at: Option<(f64, f64)>) -> Vec<MapEvent> {
        let now_key = at.map(|d| (d, (cam.lon, cam.lat, cam.zoom), area));
        if now_key.is_some() && now_key == self.last {
            return Vec::new();
        }
        self.last = now_key;
        let now: Vec<(String, Pick)> = match at {
            Some(d) => self
                .layers
                .iter()
                .filter_map(|l| map.query_rendered_features(cam, area, d, Some(&[l.as_str()])).into_iter().next().map(|p| (l.clone(), p)))
                .collect(),
            None => Vec::new(),
        };
        let mut events = Vec::new();
        for (layer, old) in &self.hovered {
            if !now.iter().any(|(l, p)| l == layer && same(p, old)) {
                if let (Some(key), Some(source), Some(id)) = (&self.hover_key, &old.source, &old.id) {
                    map.remove_feature_state(source, Some(id), Some(key));
                }
                events.push(MapEvent::Leave { layer: layer.clone(), feature: old.clone() });
            }
        }
        for (layer, new) in &now {
            if !self.hovered.iter().any(|(l, p)| l == layer && same(p, new)) {
                if let (Some(key), Some(source), Some(id)) = (&self.hover_key, &new.source, &new.id) {
                    let _ = map.set_feature_state(source, id, &serde_json::json!({ key.as_str(): true }));
                }
                events.push(MapEvent::Enter { layer: layer.clone(), feature: new.clone() });
            }
        }
        self.hovered = now;
        events
    }

    /// A click (from [`crate::Controller::take_click`]): one `Click` per layer with features
    /// under it.
    pub fn click(&self, map: &Map, cam: &Camera, area: Rect, click: &Click) -> Vec<MapEvent> {
        let dot = Viewport::new(cam, area).dot_ll(click.lon, click.lat);
        self.layers
            .iter()
            .filter_map(|l| {
                let features = map.query_rendered_features(cam, area, dot, Some(&[l.as_str()]));
                (!features.is_empty()).then(|| MapEvent::Click { layer: l.clone(), features, lon: click.lon, lat: click.lat })
            })
            .collect()
    }
}

/// The same feature: same source and id when both have ids, else the same geometry kind and
/// properties.
fn same(a: &Pick, b: &Pick) -> bool {
    match (&a.id, &b.id) {
        (Some(x), Some(y)) => a.source == b.source && ids_equal(x, y),
        _ => a.source == b.source && a.geom == b.geom && a.props == b.props,
    }
}

fn ids_equal(a: &Value, b: &Value) -> bool {
    a.to_string() == b.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MapSource, StyleLayer};
    use ratatui::{buffer::Buffer, style::Color, widgets::Widget};
    use serde_json::json;

    fn map() -> Map {
        let mut map = Map::new();
        map.add_source(
            "sites",
            MapSource::geojson(&json!({"type": "FeatureCollection", "features": [
                {"type": "Feature", "id": 1, "properties": {"name": "West"},
                 "geometry": {"type": "Polygon", "coordinates": [[[-0.2, -0.1], [0.0, -0.1], [0.0, 0.1], [-0.2, 0.1], [-0.2, -0.1]]]}},
                {"type": "Feature", "id": 2, "properties": {"name": "East"},
                 "geometry": {"type": "Polygon", "coordinates": [[[0.0, -0.1], [0.2, -0.1], [0.2, 0.1], [0.0, 0.1], [0.0, -0.1]]]}}
            ]}))
            .unwrap(),
        )
        .unwrap();
        map.add_layer(
            StyleLayer::fill("sites", "sites").color(json!(["case", ["boolean", ["feature-state", "hover"], false], "#ffcc00", "#204060"])),
        )
        .unwrap();
        map
    }

    fn bg_at(map: &Map, cam: &Camera, area: Rect, col: u16) -> Color {
        let mut buf = Buffer::empty(area);
        map.view(cam).render(area, &mut buf);
        buf[(col, area.height / 2)].bg
    }

    #[test]
    fn hover_enters_leaves_and_lights_up() {
        let mut map = map();
        let cam = Camera::new(0.0, 0.0, 9.0);
        let area = Rect::new(0, 0, 80, 20);
        let mut ev = Events::new(&["sites"]).hover_state("hover");
        let (west, east) = ((40.0, 40.0), (120.0, 40.0));
        let e = ev.pointer(&mut map, &cam, area, Some(west));
        assert!(matches!(&e[..], [MapEvent::Enter { feature, .. }] if feature.title() == "West"), "{e:?}");
        assert_eq!(bg_at(&map, &cam, area, 20), Color::Rgb(0xff, 0xcc, 0x00), "the hovered feature lights up");
        assert_eq!(bg_at(&map, &cam, area, 60), Color::Rgb(0x20, 0x40, 0x60));
        assert!(ev.pointer(&mut map, &cam, area, Some(west)).is_empty(), "nothing new");
        let e = ev.pointer(&mut map, &cam, area, Some(east));
        assert!(matches!(&e[..], [MapEvent::Leave { feature: a, .. }, MapEvent::Enter { feature: b, .. }] if a.title() == "West" && b.title() == "East"), "{e:?}");
        assert_eq!(bg_at(&map, &cam, area, 20), Color::Rgb(0x20, 0x40, 0x60), "back to normal");
        assert_eq!(bg_at(&map, &cam, area, 60), Color::Rgb(0xff, 0xcc, 0x00));
        let e = ev.pointer(&mut map, &cam, area, None);
        assert!(matches!(&e[..], [MapEvent::Leave { .. }]));
        assert_eq!(map.feature_state("sites", &Value::Num(2.0)), json!({}), "the hover state is removed");
    }

    #[test]
    fn clicks_report_the_layer_and_features() {
        let map = map();
        let cam = Camera::new(0.0, 0.0, 9.0);
        let area = Rect::new(0, 0, 80, 20);
        let ev = Events::new(&["sites", "missing"]);
        let (lon, lat) = Viewport::new(&cam, area).dot_to_lonlat(120.0, 40.0);
        let e = ev.click(&map, &cam, area, &Click { lon, lat, col: 60, row: 10 });
        match &e[..] {
            [MapEvent::Click { layer, features, .. }] => {
                assert_eq!(layer, "sites");
                assert_eq!(features[0].title(), "East");
                assert_eq!(features[0].id, Some(Value::Num(2.0)));
            }
            other => panic!("{other:?}"),
        }
    }
}
