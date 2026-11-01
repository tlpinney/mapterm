//! Decoded, styled tiles ready to draw, keyed by (z, x, y), with a bounded size.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use anyhow::Result;

use crate::{
    mvt::{self, GeomType, Value},
    style::{Paint, Schema, Style},
};

pub type TileId = (u8, u32, u32);

/// Feature geometry in tile-relative coordinates 0..1, shared by a feature's draw items.
pub type Parts = Arc<Vec<Vec<[f32; 2]>>>;

pub struct Item {
    pub order: u16,
    pub paint: Paint,
    pub parts: Parts,
}

/// Everything a tile says about one feature, kept for inspection: drawn or not.
pub struct Feature {
    pub layer: Arc<str>,
    pub geom: GeomType,
    /// Properties, with the identifying ones (name, class, kind, ...) first.
    pub props: Vec<(String, Value)>,
    pub parts: Parts,
    /// Whether the style draws it.
    pub drawn: bool,
}

#[derive(Default)]
pub struct Prepared {
    /// Sorted by draw order.
    pub items: Vec<Item>,
    pub features: Vec<Feature>,
}

/// Properties shown first when inspecting, in this order; the rest follow alphabetically.
const KEY_PROPS: [&str; 8] = ["name", "class", "subclass", "kind", "kind_detail", "ref", "type", "admin_level"];

fn ordered_props(f: &mvt::Feature) -> Vec<(String, Value)> {
    let mut props: Vec<(String, Value)> = f.props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let rank = |k: &str| KEY_PROPS.iter().position(|p| *p == k).unwrap_or(KEY_PROPS.len());
    props.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0)));
    props
}

/// Decode MVT bytes into features (tile-relative geometry, ordered properties), unstyled.
pub fn decode_features(bytes: &[u8]) -> Result<Vec<Feature>> {
    let tile = mvt::decode(bytes)?;
    let mut features = Vec::new();
    for layer in &tile.layers {
        let ext = layer.extent as f32;
        let name: Arc<str> = Arc::from(layer.name.as_str());
        for f in &layer.features {
            if f.geometry.is_empty() || f.geom_type == GeomType::Unknown {
                continue;
            }
            let parts: Parts = Arc::new(
                f.geometry.iter().map(|p| p.iter().map(|[x, y]| [*x as f32 / ext, *y as f32 / ext]).collect()).collect(),
            );
            features.push(Feature { layer: name.clone(), geom: f.geom_type, props: ordered_props(f), parts, drawn: false });
        }
    }
    Ok(features)
}

/// Decode MVT bytes and turn features into draw items.
pub fn prepare(bytes: &[u8], style: &Style, z: u8) -> Result<Prepared> {
    let tile = mvt::decode(bytes)?;
    let schema = Schema::detect(&tile);
    let mut items = Vec::new();
    let mut features = Vec::new();
    for layer in &tile.layers {
        let ext = layer.extent as f32;
        let name: Arc<str> = Arc::from(layer.name.as_str());
        for f in &layer.features {
            if f.geometry.is_empty() || f.geom_type == GeomType::Unknown {
                continue;
            }
            let parts: Parts = Arc::new(
                f.geometry.iter().map(|p| p.iter().map(|[x, y]| [*x as f32 / ext, *y as f32 / ext]).collect()).collect(),
            );
            let paints = style.paints(schema, &layer.name, f, z);
            let drawn = !paints.is_empty();
            for (order, paint) in paints {
                items.push(Item { order, paint, parts: parts.clone() });
            }
            features.push(Feature { layer: name.clone(), geom: f.geom_type, props: ordered_props(f), parts, drawn });
        }
    }
    items.sort_by_key(|i| i.order);
    Ok(Prepared { items, features })
}

pub struct TileStore {
    tiles: HashMap<TileId, Arc<Prepared>>,
    order: VecDeque<TileId>,
    cap: usize,
}

impl Default for TileStore {
    fn default() -> Self {
        Self { tiles: HashMap::new(), order: VecDeque::new(), cap: 512 }
    }
}

impl TileStore {
    pub fn contains(&self, id: &TileId) -> bool {
        self.tiles.contains_key(id)
    }

    pub fn insert(&mut self, id: TileId, t: Prepared) {
        if self.tiles.insert(id, Arc::new(t)).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.tiles.remove(&old);
            }
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.order.clear();
    }

    /// The tile, or the nearest loaded ancestor (up to 4 levels up) to stand in while it loads.
    pub fn get_or_ancestor(&self, id: TileId) -> Option<(TileId, Arc<Prepared>)> {
        let (z, x, y) = id;
        (0..=4u8.min(z)).find_map(|k| {
            let a = (z - k, x >> k, y >> k);
            self.tiles.get(&a).map(|t| (a, t.clone()))
        })
    }
}
