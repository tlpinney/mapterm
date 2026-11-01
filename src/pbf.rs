//! Minimal bounds-checked protobuf reader: just enough for vector tiles and PMTiles directories.

use anyhow::{Result, bail};

/// A protobuf wire-format reader over a byte slice (just what MVT decoding needs).
pub struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    /// Read from the start of `b`.
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

    /// Whether everything has been read.
    pub fn done(&self) -> bool {
        self.i >= self.b.len()
    }

    /// A base-128 varint.
    pub fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0u32;
        loop {
            let Some(&byte) = self.b.get(self.i) else { bail!("truncated varint") };
            self.i += 1;
            if shift >= 64 {
                bail!("varint too long");
            }
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }

    /// (field number, wire type)
    pub fn key(&mut self) -> Result<(u32, u8)> {
        let k = self.varint()?;
        Ok(((k >> 3) as u32, (k & 7) as u8))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.i.checked_add(n).filter(|e| *e <= self.b.len());
        let Some(end) = end else { bail!("truncated field") };
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }

    /// A length-delimited field's bytes.
    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.varint()?;
        self.take(usize::try_from(n)?)
    }

    /// A fixed 32-bit field.
    pub fn fixed32(&mut self) -> Result<[u8; 4]> {
        Ok(self.take(4)?.try_into()?)
    }

    /// A fixed 64-bit field.
    pub fn fixed64(&mut self) -> Result<[u8; 8]> {
        Ok(self.take(8)?.try_into()?)
    }

    /// Skip a field of wire type `wire`.
    pub fn skip(&mut self, wire: u8) -> Result<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.take(8)?;
            }
            2 => {
                self.bytes()?;
            }
            5 => {
                self.take(4)?;
            }
            w => bail!("unsupported wire type {w}"),
        }
        Ok(())
    }
}

/// A packed repeated `uint32` field.
pub fn packed_u32(b: &[u8]) -> Result<Vec<u32>> {
    let mut r = Reader::new(b);
    let mut out = Vec::with_capacity(b.len() / 2);
    while !r.done() {
        out.push(r.varint()? as u32);
    }
    Ok(out)
}

/// Decode a zigzag-encoded `sint32`.
pub fn zigzag32(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// Decode a zigzag-encoded `sint64`.
pub fn zigzag64(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_zigzag() {
        let mut r = Reader::new(&[0x96, 0x01, 0x00]);
        assert_eq!(r.varint().unwrap(), 150);
        assert_eq!(r.varint().unwrap(), 0);
        assert!(r.varint().is_err());
        assert_eq!(zigzag32(0), 0);
        assert_eq!(zigzag32(1), -1);
        assert_eq!(zigzag32(2), 1);
        assert_eq!(zigzag32(3), -2);
    }

    #[test]
    fn truncated_bytes_error() {
        let mut r = Reader::new(&[0x05, 1, 2]);
        assert!(r.bytes().is_err());
    }
}
