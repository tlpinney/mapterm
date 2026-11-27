//! Mobile network cells from [OpenCelliD](https://opencellid.org) (CC BY-SA 4.0): where each
//! cell (a radio on a tower, one of several per mast) is estimated to be, from crowd-sourced
//! measurements, with its network, radio (GSM, UMTS, LTE, 5G NR) and range.
//!
//! OpenCelliD's live API is too tight for a map (4 km², 50 cells a request, 1,000 requests a
//! day), so mapterm uses its per-country files instead: `mapterm --cells-download us` fetches
//! them with your own key (from `--opencellid-key-cmd`, else `OPENCELLID_KEY`), at most once
//! in 12 hours each (OpenCelliD allows two downloads of a file a day), into the cache; `--cells`
//! reads them. Positions are estimates, not surveyed masts.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The credit OpenCelliD asks for (CC BY-SA 4.0).
pub const CREDIT: &str = "Cell towers: OpenCelliD, CC BY-SA 4.0";
/// A file isn't downloaded again sooner than this (OpenCelliD allows two a day).
const MIN_AGAIN: std::time::Duration = std::time::Duration::from_secs(12 * 3600);
/// The US's mobile country codes (316 has no file).
pub const US: &[u16] = &[310, 311, 312, 313, 314, 315];

/// A cell's radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Radio {
    /// 2G.
    Gsm,
    /// 3G.
    Umts,
    /// 3G (CDMA networks).
    Cdma,
    /// 4G.
    Lte,
    /// 5G.
    Nr,
    /// Narrowband IoT.
    NbIot,
}

impl Radio {
    fn parse(s: &str) -> Option<Radio> {
        Some(match s {
            "GSM" => Radio::Gsm,
            "UMTS" => Radio::Umts,
            "CDMA" => Radio::Cdma,
            "LTE" => Radio::Lte,
            "NR" => Radio::Nr,
            "NBIOT" => Radio::NbIot,
            _ => return None,
        })
    }

    /// Its code in OpenCelliD's files (`LTE`, `NR`, ...).
    pub fn code(self) -> &'static str {
        match self {
            Radio::Gsm => "GSM",
            Radio::Umts => "UMTS",
            Radio::Cdma => "CDMA",
            Radio::Lte => "LTE",
            Radio::Nr => "NR",
            Radio::NbIot => "NBIOT",
        }
    }

    /// Its name.
    pub fn name(self) -> &'static str {
        match self {
            Radio::Gsm => "GSM (2G)",
            Radio::Umts => "UMTS (3G)",
            Radio::Cdma => "CDMA (3G)",
            Radio::Lte => "LTE (4G)",
            Radio::Nr => "5G NR",
            Radio::NbIot => "NB-IoT",
        }
    }

    /// Its color: 5G magenta, LTE blue, 3G green, 2G and the rest grey.
    pub fn color(self) -> &'static str {
        match self {
            Radio::Nr => "#e879f9",
            Radio::Lte => "#60a5fa",
            Radio::Umts | Radio::Cdma => "#4ade80",
            Radio::Gsm | Radio::NbIot => "#9ca3af",
        }
    }
}

/// A cell, as OpenCelliD estimates it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Its radio.
    pub radio: Radio,
    /// Mobile country code.
    pub mcc: u16,
    /// Mobile network code (the carrier).
    pub mnc: u16,
    /// Location (or tracking) area code.
    pub area: u32,
    /// Cell id.
    pub cell: u64,
    /// Degrees (estimated).
    pub lon: f32,
    /// Degrees (estimated).
    pub lat: f32,
    /// How far it's heard, meters (estimated).
    pub range: u32,
    /// How many measurements the estimate rests on.
    pub samples: u32,
    /// When it was last seen (Unix seconds).
    pub updated: i64,
}

/// The carrier of a US network code, for the big ones (about 95% of US cells); None for the
/// rest, shown by their code rather than guessed.
pub fn carrier(mcc: u16, mnc: u16) -> Option<&'static str> {
    Some(match (mcc, mnc) {
        (310, 260 | 240) => "T-Mobile",
        (310, 410) => "AT&T",
        (313, 100) => "AT&T FirstNet",
        (311, 480) => "Verizon",
        (311, 580) => "UScellular",
        (313, 340) => "Dish (Boost)",
        _ => return None,
    })
}

/// The cells in an OpenCelliD CSV (`radio,mcc,net,area,cell,unit,lon,lat,range,samples,
/// changeable,created,updated,averageSignal`), sorted by longitude for area lookups.
pub fn parse(csv: &str) -> Vec<Cell> {
    let mut v: Vec<Cell> = csv
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            if f.len() < 13 {
                return None;
            }
            let (lon, lat): (f32, f32) = (f[6].parse().ok()?, f[7].parse().ok()?);
            if lat.abs() > 90.0 || lon.abs() > 180.0 || (lat == 0.0 && lon == 0.0) {
                return None;
            }
            Some(Cell {
                radio: Radio::parse(f[0])?,
                mcc: f[1].parse().ok()?,
                mnc: f[2].parse().ok()?,
                area: f[3].parse().unwrap_or(0),
                cell: f[4].parse().unwrap_or(0),
                lon,
                lat,
                range: f[8].parse().unwrap_or(0),
                samples: f[9].parse().unwrap_or(0),
                updated: f[12].parse().unwrap_or(0),
            })
        })
        .collect();
    v.sort_by(|a, b| a.lon.total_cmp(&b.lon));
    v
}

/// Where the files are kept.
pub fn dir() -> PathBuf {
    crate::source::cache_root().join("opencellid")
}

/// What to download: a mobile country code's file, or the whole world's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    /// One mobile country code (the US has several: [`US`]).
    Mcc(u16),
    /// Every cell OpenCelliD has.
    World,
}

impl Area {
    /// Its name, and its files' (`310`, `world`).
    pub fn name(self) -> String {
        match self {
            Area::Mcc(m) => m.to_string(),
            Area::World => "world".into(),
        }
    }

    fn url(self, key: &str) -> String {
        match self {
            Area::Mcc(m) => format!("https://opencellid.org/ocid/downloads?token={key}&type=mcc&file={m}.csv.gz"),
            Area::World => format!("https://opencellid.org/ocid/downloads?token={key}&type=full&file=cell_towers.csv.gz"),
        }
    }
}

/// Download `areas` with `key` and turn each into GeoParquet in [`dir`], each at most once in
/// 12 hours. Returns, for each, what happened.
pub async fn download(areas: &[Area], key: &str) -> Vec<(String, Result<String>)> {
    let mut out = Vec::new();
    for &a in areas {
        out.push((a.name(), download_one(a, key).await));
    }
    out
}

async fn download_one(area: Area, key: &str) -> Result<String> {
    let name = area.name();
    let stamp = dir().join(format!("{name}.downloaded"));
    if let Some(age) = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok())
        && age < MIN_AGAIN
    {
        return Ok(format!("kept (downloaded {} ago; OpenCelliD allows two a day)", crate::sources::span(age.as_secs_f64())));
    }
    let got = crate::source::get_limited(&crate::source::http_client(), &area.url(key), &[], 8 << 30).await;
    // The URL holds the key: errors are reported without it.
    let bytes = match got {
        Ok(Some(b)) => b,
        Ok(None) => bail!("no file for {name}"),
        Err(e) => bail!("{}", format!("{e:#}").replace(key, "<key>")),
    };
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        // OpenCelliD answers errors (a bad key, too many downloads) in JSON or text.
        bail!("OpenCelliD: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).replace(key, "<key>").trim());
    }
    std::fs::create_dir_all(dir())?;
    let gz = dir().join(format!("{name}.csv.gz.part"));
    std::fs::write(&gz, &bytes)?;
    std::fs::write(&stamp, b"")?;
    let size = bytes.len();
    drop(bytes);
    let out = dir().join(format!("{name}.parquet"));
    let (gz2, out2) = (gz.clone(), out.clone());
    let n = tokio::task::spawn_blocking(move || convert(&gz2, &out2)).await??;
    let _ = std::fs::remove_file(&gz);
    let kept = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    Ok(format!("{:.1} MB, {} cells, kept as GeoParquet ({:.1} MB)", size as f64 / 1e6, n, kept as f64 / 1e6))
}

/// A cell packed for sorting on disk.
const PACKED: usize = 40;

fn pack(c: &Cell) -> [u8; PACKED] {
    let mut b = [0u8; PACKED];
    b[0..4].copy_from_slice(&c.lon.to_le_bytes());
    b[4..8].copy_from_slice(&c.lat.to_le_bytes());
    b[8] = c.radio as u8;
    b[9..11].copy_from_slice(&c.mcc.to_le_bytes());
    b[11..13].copy_from_slice(&c.mnc.to_le_bytes());
    b[13..17].copy_from_slice(&c.area.to_le_bytes());
    b[17..25].copy_from_slice(&c.cell.to_le_bytes());
    b[25..29].copy_from_slice(&c.range.to_le_bytes());
    b[29..33].copy_from_slice(&c.samples.to_le_bytes());
    b[33..37].copy_from_slice(&(c.updated.clamp(0, u32::MAX as i64) as u32).to_le_bytes());
    b
}

fn unpack(b: &[u8]) -> Cell {
    let f32_at = |i: usize| f32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"));
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"));
    let u16_at = |i: usize| u16::from_le_bytes(b[i..i + 2].try_into().expect("2 bytes"));
    let radio = [Radio::Gsm, Radio::Umts, Radio::Cdma, Radio::Lte, Radio::Nr, Radio::NbIot][(b[8] as usize).min(5)];
    Cell {
        radio,
        mcc: u16_at(9),
        mnc: u16_at(11),
        area: u32_at(13),
        cell: u64::from_le_bytes(b[17..25].try_into().expect("8 bytes")),
        lon: f32_at(0),
        lat: f32_at(4),
        range: u32_at(25),
        samples: u32_at(29),
        updated: u32::from_le_bytes([b[33], b[34], b[35], b[36]]) as i64,
    }
}

/// Where a point falls on a Z-order curve (16 bits of longitude, 16 of latitude, interleaved):
/// nearby points sort near each other, so a chunk of rows covers a small area.
fn zorder(lon: f32, lat: f32) -> u32 {
    let x = (((lon as f64 + 180.0) / 360.0 * 65535.0) as u32).min(65535);
    let y = (((lat as f64 + 90.0) / 180.0 * 65535.0) as u32).min(65535);
    let spread = |mut v: u32| {
        v = (v | (v << 8)) & 0x00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333;
        (v | (v << 1)) & 0x5555_5555
    };
    spread(x) | (spread(y) << 1)
}

/// Rows in each chunk (row group) of the GeoParquet file.
const CHUNK: usize = 20_000;

/// Turn an OpenCelliD CSV (gzipped) into GeoParquet at `out`, sorted along a Z-order curve
/// with GeoParquet 1.1 bbox columns, so a view reads only the few chunks it covers. The world
/// is too big to sort in memory: rows go first into 30° squares on disk, then each square is
/// sorted and written in turn. Returns how many cells there were.
pub fn convert(gz: &std::path::Path, out: &std::path::Path) -> Result<usize> {
    use std::io::{BufRead, Write};
    let tmp = out.with_extension("buckets");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    // Pass 1: each cell into its 30° square's file (6 x 12 of them).
    let square =
        |c: &Cell| (((c.lat as f64 + 90.0) / 30.0).floor().clamp(0.0, 5.0) as usize) * 12 + (((c.lon as f64 + 180.0) / 30.0).floor().clamp(0.0, 11.0) as usize);
    let mut files: Vec<Option<std::io::BufWriter<std::fs::File>>> = (0..72).map(|_| None).collect();
    let mut n = 0usize;
    let reader = std::io::BufReader::new(flate2::read::GzDecoder::new(std::fs::File::open(gz).with_context(|| format!("opening {}", gz.display()))?));
    for line in reader.lines() {
        let line = line.context("reading the cells")?;
        let Some(c) = parse(&line).pop() else { continue };
        let i = square(&c);
        if files[i].is_none() {
            files[i] = Some(std::io::BufWriter::new(std::fs::File::create(tmp.join(format!("{i}")))?));
        }
        files[i].as_mut().expect("opened").write_all(&pack(&c))?;
        n += 1;
    }
    for f in files.iter_mut().flatten() {
        f.flush()?;
    }
    drop(files);
    // Pass 2: each square sorted along the curve, written in chunks.
    let schema = schema();
    let geo = serde_json::json!({
        "version": "1.1.0",
        "primary_column": "geometry",
        "columns": {"geometry": {"encoding": "WKB", "geometry_types": ["Point"],
            "covering": {"bbox": {"xmin": ["bbox", "xmin"], "ymin": ["bbox", "ymin"], "xmax": ["bbox", "xmax"], "ymax": ["bbox", "ymax"]}}}}
    });
    let props = parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(CHUNK))
        .set_compression(parquet::basic::Compression::ZSTD(parquet::basic::ZstdLevel::try_new(3)?))
        .set_key_value_metadata(Some(vec![parquet::file::metadata::KeyValue::new("geo".into(), geo.to_string())]))
        .build();
    let part = out.with_extension("parquet.part");
    let mut w = parquet::arrow::ArrowWriter::try_new(std::fs::File::create(&part)?, schema.clone(), Some(props))?;
    for i in 0..72 {
        let Ok(raw) = std::fs::read(tmp.join(format!("{i}"))) else { continue };
        let mut cells: Vec<Cell> = raw.as_chunks::<PACKED>().0.iter().map(|b| unpack(b)).collect();
        drop(raw);
        cells.sort_by_key(|c| zorder(c.lon, c.lat));
        for chunk in cells.chunks(CHUNK) {
            w.write(&batch(&schema, chunk)?)?;
            // Each square's last chunk ends its row group, so none spans two squares.
        }
        w.flush()?;
    }
    w.close()?;
    std::fs::rename(&part, out)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(n)
}

fn schema() -> std::sync::Arc<arrow_schema::Schema> {
    use arrow_schema::{DataType, Field, Fields, Schema};
    let bbox = Fields::from(vec![
        Field::new("xmin", DataType::Float64, false),
        Field::new("ymin", DataType::Float64, false),
        Field::new("xmax", DataType::Float64, false),
        Field::new("ymax", DataType::Float64, false),
    ]);
    std::sync::Arc::new(Schema::new(vec![
        Field::new("geometry", DataType::Binary, false),
        Field::new("bbox", DataType::Struct(bbox), false),
        Field::new("radio", DataType::Utf8, false),
        Field::new("network", DataType::Utf8, false),
        Field::new("mcc", DataType::Int32, false),
        Field::new("mnc", DataType::Int32, false),
        Field::new("area", DataType::Int64, false),
        Field::new("cell", DataType::Int64, false),
        Field::new("range_m", DataType::Int32, false),
        Field::new("samples", DataType::Int32, false),
        Field::new("updated", DataType::Int64, false),
    ]))
}

fn batch(schema: &std::sync::Arc<arrow_schema::Schema>, cells: &[Cell]) -> Result<arrow_array::RecordBatch> {
    use arrow_array::{ArrayRef, BinaryArray, Float64Array, Int32Array, Int64Array, StringArray, StructArray};
    use std::sync::Arc;
    let wkb: Vec<Vec<u8>> = cells
        .iter()
        .map(|c| {
            let mut b = Vec::with_capacity(21);
            b.push(1u8);
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&(c.lon as f64).to_le_bytes());
            b.extend_from_slice(&(c.lat as f64).to_le_bytes());
            b
        })
        .collect();
    let xs: Vec<f64> = cells.iter().map(|c| c.lon as f64).collect();
    let ys: Vec<f64> = cells.iter().map(|c| c.lat as f64).collect();
    let arrow_schema::DataType::Struct(bbox_fields) = schema.field(1).data_type().clone() else { unreachable!("bbox is a struct") };
    let bbox = StructArray::new(
        bbox_fields,
        vec![
            Arc::new(Float64Array::from(xs.clone())) as ArrayRef,
            Arc::new(Float64Array::from(ys.clone())),
            Arc::new(Float64Array::from(xs)),
            Arc::new(Float64Array::from(ys)),
        ],
        None,
    );
    let radio = StringArray::from_iter_values(cells.iter().map(|c| c.radio.code()));
    let network =
        StringArray::from_iter_values(cells.iter().map(|c| carrier(c.mcc, c.mnc).map(str::to_string).unwrap_or_else(|| format!("{}-{}", c.mcc, c.mnc))));
    let cols: Vec<ArrayRef> = vec![
        Arc::new(BinaryArray::from_iter_values(wkb.iter())),
        Arc::new(bbox),
        Arc::new(radio),
        Arc::new(network),
        Arc::new(Int32Array::from_iter_values(cells.iter().map(|c| c.mcc as i32))),
        Arc::new(Int32Array::from_iter_values(cells.iter().map(|c| c.mnc as i32))),
        Arc::new(Int64Array::from_iter_values(cells.iter().map(|c| c.area as i64))),
        Arc::new(Int64Array::from_iter_values(cells.iter().map(|c| c.cell as i64))),
        Arc::new(Int32Array::from_iter_values(cells.iter().map(|c| c.range.min(i32::MAX as u32) as i32))),
        Arc::new(Int32Array::from_iter_values(cells.iter().map(|c| c.samples.min(i32::MAX as u32) as i32))),
        Arc::new(Int64Array::from_iter_values(cells.iter().map(|c| c.updated))),
    ];
    Ok(arrow_array::RecordBatch::try_new(schema.clone(), cols)?)
}

/// The GeoParquet files to read: the world's when there is one, else the countries'.
pub fn files() -> Vec<PathBuf> {
    let world = dir().join("world.parquet");
    if world.exists() {
        return vec![world];
    }
    let mut v: Vec<PathBuf> =
        std::fs::read_dir(dir()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "parquet")).collect();
    v.sort();
    v
}

/// The cells in (west, south, east, north) from `files`, as GeoJSON points, at most `max`
/// (the most measured first). Reads only the chunks the box covers (CPU and disk work: call
/// from a blocking task).
pub fn read_view(files: &[PathBuf], bounds: (f64, f64, f64, f64), max: usize) -> Result<Value> {
    let (w, s, e, n) = bounds;
    let mut features: Vec<Value> = Vec::new();
    for f in files {
        let fc = crate::geoparquet::read_file_all(&f.to_string_lossy(), Some([w, s, e, n])).with_context(|| format!("reading {}", f.display()))?;
        if let Some(Value::Array(v)) = fc.get("features").cloned() {
            features.extend(v);
        }
    }
    if features.len() > max {
        features.sort_by_key(|f| std::cmp::Reverse(f["properties"]["samples"].as_i64().unwrap_or(0)));
        features.truncate(max);
    }
    Ok(json!({"type": "FeatureCollection", "features": features}))
}

/// The style layer: ▴ colored by radio (5G NR magenta, LTE blue, 3G green, the rest grey),
/// the carrier from zoom 15.
pub fn layer(id: &str, source: &str) -> crate::StyleLayer {
    crate::StyleLayer::symbol(id, source).glyph(json!("▴")).text(json!(["step", ["zoom"], "", 15, ["get", "network"]])).color("#9ca3af").paint(
        "icon-color",
        json!([
            "match",
            ["get", "radio"],
            "NR",
            Radio::Nr.color(),
            "LTE",
            Radio::Lte.color(),
            "UMTS",
            Radio::Umts.color(),
            "CDMA",
            Radio::Cdma.color(),
            Radio::Gsm.color()
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "\
GSM,310,260,307,17022,0,-120.2461,36.1916,11049,9,1,1363987473,1751301121,0
LTE,310,410,21250,45008913,0,-83.6967,41.6888,4374,34,1,1372103025,1794679019,0
NR,311,480,1,99,0,-77.41,39.41,500,120,1,1700000000,1794000000,0
WIFI,310,1,1,1,0,-77.0,39.0,1,1,1,1,1,0
LTE,310,410,1,1,0,0,0,1,1,1,1,1,0
";

    #[test]
    fn reads_cells() {
        let c = parse(CSV);
        assert_eq!(c.len(), 3, "an unknown radio and 0,0 left out");
        assert!(c.windows(2).all(|w| w[0].lon <= w[1].lon), "by longitude");
        let nr = c.iter().find(|x| x.radio == Radio::Nr).unwrap();
        assert_eq!((nr.mcc, nr.mnc, nr.range, nr.samples), (311, 480, 500, 120));
        assert_eq!(carrier(311, 480), Some("Verizon"));
        assert_eq!(carrier(310, 410), Some("AT&T"));
        assert_eq!(carrier(310, 260), Some("T-Mobile"));
        assert_eq!(carrier(310, 999), None);
    }

    #[test]
    fn packs_and_orders() {
        let c = parse(CSV);
        for x in &c {
            assert_eq!(unpack(&pack(x)), Cell { updated: x.updated & 0xffff_ffff, ..*x });
        }
        // Near points sort near each other.
        assert!(zorder(-77.41, 39.41).abs_diff(zorder(-77.40, 39.42)) < zorder(-77.41, 39.41).abs_diff(zorder(10.0, 50.0)));
    }

    #[test]
    fn converts_to_geoparquet_and_reads_a_view() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("mapterm-cells-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let gz = dir.join("t.csv.gz");
        let mut enc = flate2::write::GzEncoder::new(std::fs::File::create(&gz).unwrap(), flate2::Compression::fast());
        enc.write_all(b"radio,mcc,net,area,cell,unit,lon,lat,range,samples,changeable,created,updated,averageSignal\n").unwrap();
        enc.write_all(CSV.as_bytes()).unwrap();
        enc.finish().unwrap();
        let out = dir.join("t.parquet");
        assert_eq!(convert(&gz, &out).unwrap(), 3, "the header and bad rows left out");
        let v = read_view(std::slice::from_ref(&out), (-78.0, 39.0, -77.0, 40.0), 100).unwrap();
        let f = v["features"].as_array().unwrap();
        assert_eq!(f.len(), 1, "only the cell in the box");
        assert_eq!(f[0]["properties"]["network"], "Verizon");
        assert_eq!(f[0]["properties"]["radio"], "NR");
        let all = read_view(std::slice::from_ref(&out), (-180.0, -90.0, 180.0, 90.0), 2).unwrap();
        assert_eq!(all["features"].as_array().unwrap().len(), 2, "capped, the most measured kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
