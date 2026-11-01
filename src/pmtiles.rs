//! PMTiles v3: header, Hilbert tile ids and directory lookup.
//! Spec: <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md>

use anyhow::{Result, bail};

use crate::pbf::Reader;

/// Size of the fixed PMTiles v3 header in bytes.
pub const HEADER_LEN: usize = 127;

#[derive(Clone, Debug)]
/// The PMTiles v3 header: where each section is, how tiles are compressed and typed, and the zoom range.
pub struct Header {
    /// Offset of the root directory.
    pub root_off: u64,
    /// Length of the root directory.
    pub root_len: u64,
    /// Offset of the JSON metadata.
    pub meta_off: u64,
    /// Length of the JSON metadata.
    pub meta_len: u64,
    /// Offset of the leaf directories.
    pub leaf_off: u64,
    /// Length of the leaf directories.
    pub leaf_len: u64,
    /// Offset of the tile data.
    pub data_off: u64,
    /// Length of the tile data.
    pub data_len: u64,
    /// Compression of directories and metadata (a `COMPRESSION_*` value).
    pub internal_compression: u8,
    /// Compression of tiles (a `COMPRESSION_*` value).
    pub tile_compression: u8,
    /// What the tiles are (a `TILE_TYPE_*` value).
    pub tile_type: u8,
    /// Lowest zoom with tiles.
    pub min_zoom: u8,
    /// Highest zoom with tiles.
    pub max_zoom: u8,
    /// (lon, lat, zoom)
    pub center: (f64, f64, u8),
}

/// Not compressed.
pub const COMPRESSION_NONE: u8 = 1;
/// Gzip.
pub const COMPRESSION_GZIP: u8 = 2;
/// Mapbox Vector Tiles.
pub const TILE_TYPE_MVT: u8 = 1;
/// PNG images.
pub const TILE_TYPE_PNG: u8 = 2;
/// JPEG images.
pub const TILE_TYPE_JPEG: u8 = 3;
/// WebP images.
pub const TILE_TYPE_WEBP: u8 = 4;

/// Parse the header from the first bytes of an archive.
pub fn parse_header(b: &[u8]) -> Result<Header> {
    if b.len() < HEADER_LEN || &b[0..7] != b"PMTiles" {
        bail!("not a PMTiles archive");
    }
    if b[7] != 3 {
        bail!("PMTiles version {} not supported (need 3)", b[7]);
    }
    let u = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let i = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as f64 / 1e7;
    Ok(Header {
        root_off: u(8),
        root_len: u(16),
        meta_off: u(24),
        meta_len: u(32),
        leaf_off: u(40),
        leaf_len: u(48),
        data_off: u(56),
        data_len: u(64),
        internal_compression: b[97],
        tile_compression: b[98],
        tile_type: b[99],
        min_zoom: b[100],
        max_zoom: b[101],
        center: (i(119), i(123), b[118]),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A directory entry: a run of tiles, or a pointer to a leaf directory (`run_length` 0).
pub struct Entry {
    /// The first tile's id (Hilbert order across zooms).
    pub tile_id: u64,
    /// Offset in the tile data (or the leaf directories).
    pub offset: u64,
    /// Length in bytes.
    pub length: u32,
    /// 0 means this entry points at a leaf directory.
    pub run_length: u32,
}

/// Decode a (decompressed) directory.
pub fn parse_directory(b: &[u8]) -> Result<Vec<Entry>> {
    let mut r = Reader::new(b);
    let n = r.varint()? as usize;
    // Each entry needs at least 4 bytes; a bigger count is a corrupt directory.
    if n > b.len() {
        bail!("directory claims {n} entries in {} bytes", b.len());
    }
    let mut e = vec![Entry { tile_id: 0, offset: 0, length: 0, run_length: 0 }; n];
    let mut last = 0u64;
    for x in e.iter_mut() {
        last = last.checked_add(r.varint()?).ok_or_else(|| anyhow::anyhow!("tile id overflow"))?;
        x.tile_id = last;
    }
    for x in e.iter_mut() {
        x.run_length = r.varint()? as u32;
    }
    for x in e.iter_mut() {
        x.length = r.varint()? as u32;
    }
    for i in 0..n {
        let v = r.varint()?;
        e[i].offset = if v == 0 && i > 0 { e[i - 1].offset.saturating_add(e[i - 1].length as u64) } else { v.saturating_sub(1) };
    }
    Ok(e)
}

/// The entry covering `id`: a tile run containing it, or the leaf directory that might.
pub fn find(entries: &[Entry], id: u64) -> Option<Entry> {
    let i = entries.partition_point(|e| e.tile_id <= id).checked_sub(1)?;
    let e = entries[i];
    // partition_point leaves id >= e.tile_id; subtracting can't overflow where adding could.
    if e.run_length == 0 || id - e.tile_id < e.run_length as u64 { Some(e) } else { None }
}

/// Hilbert-curve tile id: all tiles of lower zooms come first.
pub fn tile_id(z: u8, x: u32, y: u32) -> u64 {
    let acc: u64 = (0..z).map(|i| 1u64 << (2 * i as u64)).sum();
    let n = 1u64 << z;
    let (mut x, mut y) = (x as u64, y as u64);
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = n - 1 - x;
                y = n - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    acc + d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_tile_ids() {
        assert_eq!(tile_id(0, 0, 0), 0);
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
        assert_eq!(tile_id(2, 0, 0), 5);
        assert_eq!(tile_id(20, 0, 0), 366503875925);
    }

    #[test]
    fn runs_at_the_end_of_the_id_space() {
        // A run ending past u64::MAX (found by fuzzing: the end overflowed).
        let e = [Entry { tile_id: u64::MAX - 1, offset: 0, length: 1, run_length: 5 }];
        assert!(find(&e, u64::MAX).is_some());
        assert!(find(&e, 3).is_none());
    }

    #[test]
    fn directory_roundtrip() {
        // 2 entries: ids 5 (run 1, len 10, off 0) and 7 (run 2, len 20, offset follows).
        let bytes = [2, 5, 2, 1, 2, 10, 20, 1, 0];
        let d = parse_directory(&bytes).unwrap();
        assert_eq!(d[0], Entry { tile_id: 5, offset: 0, length: 10, run_length: 1 });
        assert_eq!(d[1], Entry { tile_id: 7, offset: 10, length: 20, run_length: 2 });
        assert_eq!(find(&d, 8).unwrap().tile_id, 7);
        assert!(find(&d, 6).is_none());
        assert!(find(&d, 9).is_none());
        assert!(find(&d, 4).is_none());
    }

    #[test]
    fn corrupt_directory_errors() {
        assert!(parse_directory(&[200, 1]).is_err());
        assert!(parse_directory(&[]).is_err());
    }
}
