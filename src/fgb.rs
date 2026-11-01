//! FlatGeobuf (`.fgb`) reading, as GeoJSON: the magic bytes, a FlatBuffers header (columns,
//! geometry type, feature count, index node size, CRS), an optional packed Hilbert R-tree
//! index (skipped here), then size-prefixed FlatBuffers features. Coordinates must be lon/lat
//! (EPSG:4326, or a CRS that is lon/lat like NAD83).
//! Spec: <https://flatgeobuf.org> (header.fbs, feature.fbs).

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value as Json, json};

const MAGIC: &[u8; 3] = b"fgb";

/// Whether the bytes start like a FlatGeobuf file.
pub fn is_fgb(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[0..3] == MAGIC && &bytes[4..7] == MAGIC
}

/// A FlatBuffers table inside a buffer.
#[derive(Clone, Copy)]
struct Table<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn u16_at(b: &[u8], p: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(b.get(p..p + 2).ok_or_else(|| anyhow!("FlatGeobuf: truncated"))?.try_into()?))
}
fn u32_at(b: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(b.get(p..p + 4).ok_or_else(|| anyhow!("FlatGeobuf: truncated"))?.try_into()?))
}
fn u64_at(b: &[u8], p: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(b.get(p..p + 8).ok_or_else(|| anyhow!("FlatGeobuf: truncated"))?.try_into()?))
}
fn f64_at(b: &[u8], p: usize) -> Result<f64> {
    Ok(f64::from_bits(u64_at(b, p)?))
}

impl<'a> Table<'a> {
    /// The root table of a FlatBuffer.
    fn root(buf: &'a [u8]) -> Result<Self> {
        Ok(Table { buf, pos: u32_at(buf, 0)? as usize })
    }

    /// Where field `i` is stored, if present.
    fn field(&self, i: usize) -> Result<Option<usize>> {
        let soff = i32::from_le_bytes(self.buf.get(self.pos..self.pos + 4).ok_or_else(|| anyhow!("FlatGeobuf: truncated"))?.try_into()?);
        let vt = (self.pos as i64 - soff as i64) as usize;
        let vt_len = u16_at(self.buf, vt)? as usize;
        if 4 + 2 * i + 2 > vt_len {
            return Ok(None);
        }
        let off = u16_at(self.buf, vt + 4 + 2 * i)? as usize;
        Ok((off != 0).then_some(self.pos + off))
    }

    fn u8(&self, i: usize, default: u8) -> Result<u8> {
        Ok(match self.field(i)? {
            Some(p) => *self.buf.get(p).ok_or_else(|| anyhow!("FlatGeobuf: truncated"))?,
            None => default,
        })
    }

    fn u16(&self, i: usize, default: u16) -> Result<u16> {
        self.field(i)?.map_or(Ok(default), |p| u16_at(self.buf, p))
    }

    fn u64(&self, i: usize) -> Result<u64> {
        self.field(i)?.map_or(Ok(0), |p| u64_at(self.buf, p))
    }

    fn i32(&self, i: usize) -> Result<i32> {
        Ok(self.field(i)?.map_or(Ok(0), |p| u32_at(self.buf, p))? as i32)
    }

    /// Where an offset field points.
    fn target(&self, i: usize) -> Result<Option<usize>> {
        match self.field(i)? {
            Some(p) => Ok(Some(p + u32_at(self.buf, p)? as usize)),
            None => Ok(None),
        }
    }

    fn table(&self, i: usize) -> Result<Option<Table<'a>>> {
        Ok(self.target(i)?.map(|pos| Table { buf: self.buf, pos }))
    }

    fn string(&self, i: usize) -> Result<Option<String>> {
        let Some(p) = self.target(i)? else { return Ok(None) };
        let n = u32_at(self.buf, p)? as usize;
        let b = self.buf.get(p + 4..p + 4 + n).ok_or_else(|| anyhow!("FlatGeobuf: truncated string"))?;
        Ok(Some(String::from_utf8_lossy(b).into_owned()))
    }

    /// A vector field: its first element's position and length.
    fn vector(&self, i: usize) -> Result<Option<(usize, usize)>> {
        let Some(p) = self.target(i)? else { return Ok(None) };
        Ok(Some((p + 4, u32_at(self.buf, p)? as usize)))
    }

    fn bytes(&self, i: usize) -> Result<&'a [u8]> {
        match self.vector(i)? {
            Some((p, n)) => self.buf.get(p..p + n).ok_or_else(|| anyhow!("FlatGeobuf: truncated bytes")),
            None => Ok(&[]),
        }
    }

    fn f64s(&self, i: usize) -> Result<Vec<f64>> {
        match self.vector(i)? {
            Some((p, n)) => (0..n).map(|k| f64_at(self.buf, p + 8 * k)).collect(),
            None => Ok(Vec::new()),
        }
    }

    fn u32s(&self, i: usize) -> Result<Vec<u32>> {
        match self.vector(i)? {
            Some((p, n)) => (0..n).map(|k| u32_at(self.buf, p + 4 * k)).collect(),
            None => Ok(Vec::new()),
        }
    }

    fn tables(&self, i: usize) -> Result<Vec<Table<'a>>> {
        match self.vector(i)? {
            Some((p, n)) => (0..n)
                .map(|k| {
                    let e = p + 4 * k;
                    Ok(Table { buf: self.buf, pos: e + u32_at(self.buf, e)? as usize })
                })
                .collect(),
            None => Ok(Vec::new()),
        }
    }
}

#[derive(Clone, Debug)]
struct Column {
    name: String,
    kind: u8,
}

/// Size of a packed R-tree of `items` with `node_size` children per node, in bytes (40 per
/// node), as the spec computes it.
fn index_size(items: u64, node_size: u16) -> u64 {
    if node_size < 2 || items == 0 {
        return 0;
    }
    let ns = node_size as u64;
    let (mut n, mut nodes) = (items, items);
    loop {
        n = n.div_ceil(ns);
        nodes += n;
        if n == 1 {
            break;
        }
    }
    nodes * 40
}

/// A whole FlatGeobuf file as a GeoJSON FeatureCollection.
pub fn to_geojson(bytes: &[u8]) -> Result<Json> {
    if !is_fgb(bytes) {
        bail!("not FlatGeobuf (no fgb magic)");
    }
    if bytes[3] != 3 {
        bail!("FlatGeobuf version {} (only 3 is read)", bytes[3]);
    }
    let hlen = u32_at(bytes, 8)? as usize;
    let hbuf = bytes.get(12..12 + hlen).ok_or_else(|| anyhow!("FlatGeobuf: truncated header"))?;
    let h = Table::root(hbuf)?;
    let header_type = h.u8(2, 0)?;
    let columns: Vec<Column> = h.tables(7)?.iter().map(|c| Ok(Column { name: c.string(0)?.unwrap_or_default(), kind: c.u8(1, 0)? })).collect::<Result<_>>()?;
    let count = h.u64(8)?;
    let node_size = h.u16(9, 16)?;
    if let Some(crs) = h.table(10)? {
        let code = crs.i32(1)?;
        if code != 0 && code != 4326 && code != 4269 && code != 4258 {
            bail!("FlatGeobuf in EPSG:{code}; mapterm reads lon/lat (EPSG:4326) only");
        }
    }
    let mut pos = 12 + hlen + index_size(count, node_size) as usize;
    let mut features = Vec::new();
    while pos + 4 <= bytes.len() {
        let len = u32_at(bytes, pos)? as usize;
        let fbuf = bytes.get(pos + 4..pos + 4 + len).ok_or_else(|| anyhow!("FlatGeobuf: truncated feature"))?;
        pos += 4 + len;
        let f = Table::root(fbuf)?;
        let geometry = match f.table(0)? {
            Some(g) => geometry(&g, header_type)?,
            None => Json::Null,
        };
        // A feature may carry its own columns.
        let own = f.tables(2)?;
        let cols: Vec<Column> = if own.is_empty() {
            columns.clone()
        } else {
            own.iter().map(|c| Ok(Column { name: c.string(0)?.unwrap_or_default(), kind: c.u8(1, 0)? })).collect::<Result<_>>()?
        };
        let props = properties(f.bytes(1)?, &cols)?;
        features.push(json!({"type": "Feature", "properties": props, "geometry": geometry}));
    }
    Ok(json!({"type": "FeatureCollection", "features": features}))
}

/// Pairs from flat x,y doubles.
fn pairs(xy: &[f64]) -> Vec<Json> {
    xy.chunks_exact(2).map(|p| json!([p[0], p[1]])).collect()
}

/// Split points by `ends` (point counts where each part ends); one part when empty.
fn split(xy: &[f64], ends: &[u32]) -> Vec<Json> {
    if ends.is_empty() {
        return vec![Json::Array(pairs(xy))];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    for &e in ends {
        let e = (e as usize).min(xy.len() / 2);
        out.push(Json::Array(pairs(&xy[start * 2..e * 2])));
        start = e;
    }
    out
}

fn geometry(g: &Table, header_type: u8) -> Result<Json> {
    let kind = match header_type {
        0 => g.u8(6, 0)?,
        t => t,
    };
    let xy = g.f64s(1)?;
    let ends = g.u32s(0)?;
    Ok(match kind {
        1 => json!({"type": "Point", "coordinates": pairs(&xy).into_iter().next().unwrap_or(Json::Null)}),
        2 => json!({"type": "LineString", "coordinates": pairs(&xy)}),
        3 => json!({"type": "Polygon", "coordinates": split(&xy, &ends)}),
        4 => json!({"type": "MultiPoint", "coordinates": pairs(&xy)}),
        5 => json!({"type": "MultiLineString", "coordinates": split(&xy, &ends)}),
        6 => {
            let polys: Vec<Json> = g.tables(7)?.iter().map(|p| Ok(Json::Array(split(&p.f64s(1)?, &p.u32s(0)?)))).collect::<Result<_>>()?;
            json!({"type": "MultiPolygon", "coordinates": polys})
        }
        7 => {
            let parts: Vec<Json> = g.tables(7)?.iter().map(|p| geometry(p, 0)).collect::<Result<_>>()?;
            json!({"type": "GeometryCollection", "geometries": parts})
        }
        _ => Json::Null,
    })
}

/// Decode the property bytes: (u16 column index, value) pairs, typed by the column.
fn properties(b: &[u8], cols: &[Column]) -> Result<Map<String, Json>> {
    let mut m = Map::new();
    let mut p = 0usize;
    let take = |p: &mut usize, n: usize| -> Result<&[u8]> {
        let s = b.get(*p..*p + n).ok_or_else(|| anyhow!("FlatGeobuf: truncated properties"))?;
        *p += n;
        Ok(s)
    };
    while p + 2 <= b.len() {
        let i = u16::from_le_bytes(take(&mut p, 2)?.try_into()?) as usize;
        let col = cols.get(i).ok_or_else(|| anyhow!("FlatGeobuf: property for missing column {i}"))?;
        let v = match col.kind {
            0 => json!(take(&mut p, 1)?[0] as i8),
            1 => json!(take(&mut p, 1)?[0]),
            2 => json!(take(&mut p, 1)?[0] != 0),
            3 => json!(i16::from_le_bytes(take(&mut p, 2)?.try_into()?)),
            4 => json!(u16::from_le_bytes(take(&mut p, 2)?.try_into()?)),
            5 => json!(i32::from_le_bytes(take(&mut p, 4)?.try_into()?)),
            6 => json!(u32::from_le_bytes(take(&mut p, 4)?.try_into()?)),
            7 => json!(i64::from_le_bytes(take(&mut p, 8)?.try_into()?)),
            8 => json!(u64::from_le_bytes(take(&mut p, 8)?.try_into()?)),
            9 => json!(f32::from_le_bytes(take(&mut p, 4)?.try_into()?)),
            10 => json!(f64::from_le_bytes(take(&mut p, 8)?.try_into()?)),
            // String, Json, DateTime, Binary: a u32 length and the bytes.
            11..=14 => {
                let n = u32::from_le_bytes(take(&mut p, 4)?.try_into()?) as usize;
                let s = take(&mut p, n)?;
                match col.kind {
                    12 => serde_json::from_slice(s).unwrap_or_else(|_| json!(String::from_utf8_lossy(s))),
                    14 => json!(format!("<{} bytes>", s.len())),
                    _ => json!(String::from_utf8_lossy(s)),
                }
            }
            k => bail!("FlatGeobuf: unknown column type {k}"),
        };
        m.insert(col.name.clone(), v);
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_sizes() {
        assert_eq!(index_size(0, 16), 0);
        assert_eq!(index_size(10, 0), 0);
        // 179 items, 16 per node: 12 leaves' parents, then 1 root -> 179 + 12 + 1 nodes.
        assert_eq!(index_size(179, 16), (179 + 12 + 1) * 40);
    }

    #[test]
    fn reads_the_natural_earth_countries_sample() {
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/countries.fgb")).unwrap();
        assert!(is_fgb(&bytes));
        let doc = to_geojson(&bytes).unwrap();
        let f = doc["features"].as_array().unwrap();
        assert_eq!(f.len(), 179);
        let usa = f.iter().find(|x| x["properties"].as_object().unwrap().values().any(|v| v == "United States of America")).expect("USA");
        assert_eq!(usa["geometry"]["type"], "MultiPolygon");
        // The USA's outline is inside a plausible lon/lat box.
        let pt = &usa["geometry"]["coordinates"][0][0][0];
        let (lon, lat) = (pt[0].as_f64().unwrap(), pt[1].as_f64().unwrap());
        assert!((-180.0..=-60.0).contains(&lon) && (15.0..=75.0).contains(&lat), "{lon},{lat}");
        // And it reads as GeoJSON.
        let (features, _) = crate::geojson::features(&doc).unwrap();
        assert_eq!(features.len(), 179);
        assert!(to_geojson(b"not fgb").is_err());
    }
}
