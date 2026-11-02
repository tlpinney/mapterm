//! Decoded vector tile features, kept for styling and inspection.

use std::sync::Arc;

use anyhow::Result;

use crate::mvt::{self, GeomType, Value};

/// A tile address: (zoom, x, y).
pub type TileId = (u8, u32, u32);

/// Feature geometry in tile-relative coordinates 0..1.
pub type Parts = Arc<Vec<Vec<[f32; 2]>>>;

/// Everything a tile says about one feature, kept for inspection: drawn or not.
pub struct Feature {
    /// The source-layer it came from.
    pub layer: Arc<str>,
    /// The tile's id for the feature, if any.
    pub id: Option<Value>,
    /// Points, lines or polygons.
    pub geom: GeomType,
    /// Properties, with the identifying ones (name, class, kind, ...) first.
    pub props: Vec<(String, Value)>,
    /// Parts in tile-relative coordinates 0..1.
    pub parts: Parts,
    /// Unused; kept for compatibility (see [`crate::Map::query_features`]).
    pub drawn: bool,
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
            let parts: Parts = Arc::new(f.geometry.iter().map(|p| p.iter().map(|[x, y]| [*x as f32 / ext, *y as f32 / ext]).collect()).collect());
            let id = f.id.map(|v| Value::Num(v as f64));
            features.push(Feature { layer: name.clone(), id, geom: f.geom_type, props: ordered_props(f), parts, drawn: false });
        }
    }
    Ok(features)
}
