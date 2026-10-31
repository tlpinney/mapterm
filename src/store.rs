//! Decoded, styled tiles ready to draw, keyed by (z, x, y), with a bounded size.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use anyhow::Result;

use crate::{
    mvt,
    style::{Paint, Schema, Style},
};

pub type TileId = (u8, u32, u32);

pub struct Item {
    pub order: u16,
    pub paint: Paint,
    /// Parts in tile-relative coordinates 0..1.
    pub parts: Vec<Vec<[f32; 2]>>,
}

#[derive(Default)]
pub struct Prepared {
    /// Sorted by draw order.
    pub items: Vec<Item>,
}

/// Decode MVT bytes and turn features into draw items.
pub fn prepare(bytes: &[u8], style: &Style, z: u8) -> Result<Prepared> {
    let tile = mvt::decode(bytes)?;
    let schema = Schema::detect(&tile);
    let mut items = Vec::new();
    for layer in &tile.layers {
        let ext = layer.extent as f32;
        for f in &layer.features {
            let paints = style.paints(schema, &layer.name, f, z);
            if paints.is_empty() {
                continue;
            }
            let parts: Vec<Vec<[f32; 2]>> =
                f.geometry.iter().map(|p| p.iter().map(|[x, y]| [*x as f32 / ext, *y as f32 / ext]).collect()).collect();
            for (order, paint) in paints {
                items.push(Item { order, paint, parts: parts.clone() });
            }
        }
    }
    items.sort_by_key(|i| i.order);
    Ok(Prepared { items })
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
