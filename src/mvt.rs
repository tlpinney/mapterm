//! Mapbox Vector Tile (MVT 2.x) decoding.

use std::collections::HashMap;

use anyhow::Result;

use crate::pbf::{Reader, packed_u32, zigzag32, zigzag64};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Str(s) => f.write_str(s),
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => write!(f, "{}", *n as i64),
            Value::Num(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
        }
    }
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeomType {
    Unknown,
    Point,
    Line,
    Polygon,
}

#[derive(Clone, Debug)]
pub struct Feature {
    pub geom_type: GeomType,
    pub props: HashMap<String, Value>,
    /// Parts (points, lines or rings) in tile coordinates, 0..extent.
    pub geometry: Vec<Vec<[i32; 2]>>,
}

impl Feature {
    pub fn str(&self, k: &str) -> Option<&str> {
        self.props.get(k).and_then(Value::as_str)
    }

    pub fn num(&self, k: &str) -> Option<f64> {
        self.props.get(k).and_then(Value::as_f64)
    }
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub extent: u32,
    pub features: Vec<Feature>,
}

#[derive(Clone, Debug, Default)]
pub struct Tile {
    pub layers: Vec<Layer>,
}

pub fn decode(buf: &[u8]) -> Result<Tile> {
    let mut r = Reader::new(buf);
    let mut tile = Tile::default();
    while !r.done() {
        let (field, wire) = r.key()?;
        if field == 3 && wire == 2 {
            tile.layers.push(layer(r.bytes()?)?);
        } else {
            r.skip(wire)?;
        }
    }
    Ok(tile)
}

fn layer(b: &[u8]) -> Result<Layer> {
    let mut r = Reader::new(b);
    let mut name = String::new();
    let mut extent = 4096u32;
    let mut keys = Vec::new();
    let mut values = Vec::new();
    let mut raw = Vec::new();
    while !r.done() {
        match r.key()? {
            (1, 2) => name = String::from_utf8_lossy(r.bytes()?).into_owned(),
            (2, 2) => raw.push(r.bytes()?),
            (3, 2) => keys.push(String::from_utf8_lossy(r.bytes()?).into_owned()),
            (4, 2) => values.push(value(r.bytes()?)?),
            (5, 0) => extent = (r.varint()? as u32).max(1),
            (_, w) => r.skip(w)?,
        }
    }
    let features = raw.into_iter().map(|f| feature(f, &keys, &values)).collect::<Result<_>>()?;
    Ok(Layer { name, extent, features })
}

fn value(b: &[u8]) -> Result<Value> {
    let mut r = Reader::new(b);
    let mut v = Value::Bool(false);
    while !r.done() {
        v = match r.key()? {
            (1, 2) => Value::Str(String::from_utf8_lossy(r.bytes()?).into_owned()),
            (2, 5) => Value::Num(f32::from_le_bytes(r.fixed32()?) as f64),
            (3, 1) => Value::Num(f64::from_le_bytes(r.fixed64()?)),
            (4, 0) => Value::Num(r.varint()? as i64 as f64),
            (5, 0) => Value::Num(r.varint()? as f64),
            (6, 0) => Value::Num(zigzag64(r.varint()?) as f64),
            (7, 0) => Value::Bool(r.varint()? != 0),
            (_, w) => {
                r.skip(w)?;
                continue;
            }
        };
    }
    Ok(v)
}

fn feature(b: &[u8], keys: &[String], values: &[Value]) -> Result<Feature> {
    let mut r = Reader::new(b);
    let mut tags = Vec::new();
    let mut geom_type = GeomType::Unknown;
    let mut cmds = Vec::new();
    while !r.done() {
        match r.key()? {
            (2, 2) => tags = packed_u32(r.bytes()?)?,
            (3, 0) => {
                geom_type = match r.varint()? {
                    1 => GeomType::Point,
                    2 => GeomType::Line,
                    3 => GeomType::Polygon,
                    _ => GeomType::Unknown,
                }
            }
            (4, 2) => cmds = packed_u32(r.bytes()?)?,
            (_, w) => r.skip(w)?,
        }
    }
    let mut props = HashMap::new();
    for pair in tags.chunks_exact(2) {
        if let (Some(k), Some(v)) = (keys.get(pair[0] as usize), values.get(pair[1] as usize)) {
            props.insert(k.clone(), v.clone());
        }
    }
    Ok(Feature { geom_type, props, geometry: geometry(&cmds) })
}

fn geometry(cmds: &[u32]) -> Vec<Vec<[i32; 2]>> {
    let mut parts = Vec::new();
    let mut cur: Vec<[i32; 2]> = Vec::new();
    let (mut x, mut y) = (0i32, 0i32);
    let mut i = 0;
    while i < cmds.len() {
        let (cmd, count) = (cmds[i] & 7, (cmds[i] >> 3) as usize);
        i += 1;
        match cmd {
            1 | 2 => {
                for _ in 0..count {
                    if i + 1 >= cmds.len() {
                        break;
                    }
                    let (dx, dy) = (zigzag32(cmds[i]), zigzag32(cmds[i + 1]));
                    i += 2;
                    x = x.wrapping_add(dx);
                    y = y.wrapping_add(dy);
                    if cmd == 1 && !cur.is_empty() {
                        parts.push(std::mem::take(&mut cur));
                    }
                    cur.push([x, y]);
                }
            }
            7 => {
                if let Some(&first) = cur.first() {
                    cur.push(first);
                }
            }
            _ => break,
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_spec_examples() {
        // MVT spec examples: a point at (25,17); a line (2,2)->(2,10)->(10,10).
        assert_eq!(geometry(&[9, 50, 34]), vec![vec![[25, 17]]]);
        assert_eq!(geometry(&[9, 4, 4, 18, 0, 16, 16, 0]), vec![vec![[2, 2], [2, 10], [10, 10]]]);
        // Polygon (3,6)->(8,12)->(20,34), closed.
        assert_eq!(geometry(&[9, 6, 12, 18, 10, 12, 24, 44, 15]), vec![vec![[3, 6], [8, 12], [20, 34], [3, 6]]]);
    }

    #[test]
    fn truncated_geometry_is_safe() {
        assert_eq!(geometry(&[9, 4]), Vec::<Vec<[i32; 2]>>::new());
        assert_eq!(geometry(&[(1 << 3) | 2, 2, 2]), vec![vec![[1, 1]]]);
    }

    #[test]
    fn garbage_does_not_panic() {
        for seed in 0u32..200 {
            let bytes: Vec<u8> = (0..64).map(|i| (seed.wrapping_mul(2654435761).wrapping_add(i * 97) >> 7) as u8).collect();
            let _ = decode(&bytes);
        }
    }
}

/// A tiny MVT encoder for tests.
#[cfg(test)]
pub(crate) mod tests_support {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn field_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
        varint(out, ((field as u64) << 3) | 2);
        varint(out, b.len() as u64);
        out.extend_from_slice(b);
    }

    fn zz(v: i32) -> u32 {
        ((v << 1) ^ (v >> 31)) as u32
    }

    /// One tile with one layer holding one polygon feature that covers the whole tile.
    pub fn square_tile(layer: &str, extent: u32) -> Vec<u8> {
        let e = extent as i32;
        let cmds: Vec<u32> = vec![9, zz(0), zz(0), 26, zz(e), zz(0), zz(0), zz(e), zz(-e), zz(0), 15];
        let mut geom = Vec::new();
        for c in cmds {
            varint(&mut geom, c as u64);
        }
        let mut feat = Vec::new();
        varint(&mut feat, 3 << 3); // type
        varint(&mut feat, 3); // polygon
        field_bytes(&mut feat, 4, &geom);
        let mut l = Vec::new();
        varint(&mut l, 15 << 3);
        varint(&mut l, 2);
        field_bytes(&mut l, 1, layer.as_bytes());
        field_bytes(&mut l, 2, &feat);
        varint(&mut l, 5 << 3);
        varint(&mut l, extent as u64);
        let mut tile = Vec::new();
        field_bytes(&mut tile, 3, &l);
        tile
    }
}
