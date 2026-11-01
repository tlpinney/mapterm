//! Point clustering for GeoJSON sources, like MapLibre's `cluster: true` (supercluster): for
//! each zoom level up to `max_zoom`, points closer than `radius` merge into cluster features
//! with `cluster`, `cluster_id`, `point_count` and `point_count_abbreviated` properties, built
//! level by level from the one above so clusters nest. Other geometry passes through.

use std::collections::HashMap;

use crate::{
    geojson::Feature,
    index::{BBox, Index},
    mvt::{GeomType, Value},
    render::TILE_DOTS,
};

/// Clustering settings (MapLibre's `clusterRadius` and `clusterMaxZoom`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClusterOptions {
    /// How close points merge, in CSS pixels (about 4 per braille dot). MapLibre's default: 50.
    pub radius_px: f64,
    /// The last zoom with clusters; past it every point shows. MapLibre's default: 14.
    pub max_zoom: u8,
}

impl Default for ClusterOptions {
    fn default() -> Self {
        Self { radius_px: 50.0, max_zoom: 14 }
    }
}

/// Features and their index at one zoom level.
#[derive(Clone, Debug)]
pub struct Level {
    pub features: Vec<Feature>,
    pub index: Index,
}

impl Level {
    pub fn new(features: Vec<Feature>) -> Self {
        let boxes: Vec<BBox> = features.iter().map(|f| f.bbox).collect();
        Self { index: Index::new(&boxes), features }
    }
}

/// The clustered levels of a source, zoom 0 to `max_zoom`.
#[derive(Clone, Debug)]
pub struct Clusters {
    pub options: ClusterOptions,
    levels: Vec<Level>,
    /// The zoom each cluster was formed at, by cluster id.
    formed: HashMap<u64, u8>,
}

/// A point being clustered: where, how many original points, and what it is.
struct Item {
    at: (f64, f64),
    count: u64,
    /// The original feature (a single point), or the cluster id.
    what: Either,
}

enum Either {
    Point(usize),
    Cluster(u64),
}

impl Clusters {
    /// Cluster the single-point features of `features`.
    pub fn build(features: &[Feature], options: ClusterOptions) -> Self {
        let is_point = |f: &Feature| f.geom == GeomType::Point && f.parts.len() == 1 && f.parts[0].len() == 1;
        let others: Vec<Feature> = features.iter().filter(|f| !is_point(f)).cloned().collect();
        let mut items: Vec<Item> =
            features.iter().enumerate().filter(|(_, f)| is_point(f)).map(|(i, f)| Item { at: f.parts[0][0], count: 1, what: Either::Point(i) }).collect();
        let mut formed = HashMap::new();
        let mut next_id = 1u64;
        let mut levels = Vec::with_capacity(options.max_zoom as usize + 1);
        for z in (0..=options.max_zoom).rev() {
            // The radius in world units at this zoom (TILE_DOTS dots per world at zoom 0).
            let r = options.radius_px / 4.0 / (TILE_DOTS * 2f64.powi(z as i32));
            items = merge(items, r, z, &mut next_id, &mut formed);
            let mut level: Vec<Feature> = items.iter().map(|it| to_feature(it, features)).collect();
            level.extend(others.iter().cloned());
            levels.push(Level::new(level));
        }
        levels.reverse();
        Self { options, levels, formed }
    }

    /// The level to draw at `zoom`, or `None` past `max_zoom` (draw the original features).
    pub fn at(&self, zoom: f64) -> Option<&Level> {
        let z = zoom.floor();
        if z > self.options.max_zoom as f64 {
            return None;
        }
        self.levels.get(z.max(0.0) as usize)
    }

    /// The zoom at which a cluster splits into its parts (MapLibre's
    /// `getClusterExpansionZoom`).
    pub fn expansion_zoom(&self, cluster_id: u64) -> Option<u8> {
        self.formed.get(&cluster_id).map(|z| z + 1)
    }
}

/// Merge items closer than `r` (greedily, in order, using a grid of `r`-sized cells).
fn merge(items: Vec<Item>, r: f64, z: u8, next_id: &mut u64, formed: &mut HashMap<u64, u8>) -> Vec<Item> {
    let cell = |p: (f64, f64)| ((p.0 / r).floor() as i64, (p.1 / r).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        grid.entry(cell(it.at)).or_default().push(i);
    }
    let mut taken = vec![false; items.len()];
    let mut out = Vec::new();
    let mut items: Vec<Option<Item>> = items.into_iter().map(Some).collect();
    for i in 0..items.len() {
        if taken[i] {
            continue;
        }
        taken[i] = true;
        let at = items[i].as_ref().map(|it| it.at).unwrap_or_default();
        let (cx, cy) = cell(at);
        let mut group = vec![i];
        for gx in cx - 1..=cx + 1 {
            for gy in cy - 1..=cy + 1 {
                for &j in grid.get(&(gx, gy)).into_iter().flatten() {
                    if !taken[j] && items[j].as_ref().is_some_and(|o| (o.at.0 - at.0).hypot(o.at.1 - at.1) <= r) {
                        taken[j] = true;
                        group.push(j);
                    }
                }
            }
        }
        if group.len() == 1 {
            out.extend(items[i].take());
            continue;
        }
        // A cluster at the weighted center of its parts.
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0u64);
        for &j in &group {
            if let Some(it) = items[j].take() {
                sx += it.at.0 * it.count as f64;
                sy += it.at.1 * it.count as f64;
                n += it.count;
            }
        }
        let id = *next_id;
        *next_id += 1;
        formed.insert(id, z);
        out.push(Item { at: (sx / n as f64, sy / n as f64), count: n, what: Either::Cluster(id) });
    }
    out
}

fn to_feature(it: &Item, originals: &[Feature]) -> Feature {
    match it.what {
        Either::Point(i) => originals[i].clone(),
        Either::Cluster(id) => {
            let n = it.count;
            let abbreviated = if n >= 10_000 {
                format!("{}k", (n as f64 / 1000.0).round())
            } else if n >= 1000 {
                format!("{}k", (n as f64 / 100.0).round() / 10.0)
            } else {
                n.to_string()
            };
            Feature {
                id: Some(Value::Num(id as f64)),
                geom: GeomType::Point,
                parts: vec![vec![it.at]],
                bbox: [it.at.0, it.at.1, it.at.0, it.at.1],
                props: vec![
                    ("cluster".into(), Value::Bool(true)),
                    ("cluster_id".into(), Value::Num(id as f64)),
                    ("point_count".into(), Value::Num(n as f64)),
                    ("point_count_abbreviated".into(), Value::Str(abbreviated)),
                ],
                label: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::to_world;

    fn point(lon: f64, lat: f64, name: &str) -> Feature {
        let p = to_world(lon, lat);
        Feature { id: None, geom: GeomType::Point, parts: vec![vec![p]], bbox: [p.0, p.1, p.0, p.1], props: vec![("name".into(), Value::Str(name.into()))], label: Some(name.into()) }
    }

    #[test]
    fn nearby_points_cluster_by_zoom() {
        // Two tight groups 1 km apart within DC, and one point in Baltimore.
        let mut fs = Vec::new();
        for i in 0..30 {
            fs.push(point(-77.03 + i as f64 * 1e-5, 38.89, "a"));
            fs.push(point(-77.02 + i as f64 * 1e-5, 38.89, "b"));
        }
        fs.push(point(-76.61, 39.29, "baltimore"));
        let c = Clusters::build(&fs, ClusterOptions::default());
        let count = |z: f64| c.at(z).map(|l| l.features.iter().map(|f| match f.props.iter().find(|(k, _)| k == "point_count") {
            Some((_, Value::Num(n))) => *n as u64,
            _ => 1,
        }).collect::<Vec<_>>());
        let total = |z: f64| count(z).unwrap().iter().sum::<u64>();
        for z in [2.0, 8.0, 12.0, 14.0] {
            assert_eq!(total(z), 61, "every point counted once at z{z}");
        }
        assert_eq!(count(8.0).unwrap().len(), 2, "DC (60) and Baltimore at z8: {:?}", count(8.0));
        assert!(count(14.0).unwrap().contains(&30), "two groups of 30 at z14: {:?}", count(14.0));
        assert!(c.at(15.0).is_none(), "past max zoom: the original points");
        // Abbreviated counts and expansion zooms.
        let dc = c.at(8.0).unwrap().features.iter().find(|f| f.props.iter().any(|(k, v)| k == "point_count" && *v == Value::Num(60.0))).unwrap();
        assert!(dc.props.iter().any(|(k, v)| k == "point_count_abbreviated" && *v == Value::Str("60".into())));
        let Some(Value::Num(id)) = dc.id.clone() else { panic!() };
        let z = c.expansion_zoom(id as u64).unwrap();
        assert!(z > 8 && z <= 15, "{z}");
    }
}
