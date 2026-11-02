//! GeoParquet: Parquet files whose `geo` metadata names a geometry column, as the Planetary
//! Computer, Overture and GDAL write them. Read with the `geoparquet` crate (Arrow, column by
//! column): the primary geometry column becomes GeoJSON geometry, the other columns its
//! properties.
//!
//! Only what's needed is read. A local file is read in place. A URL is read with HTTP range
//! requests: the footer first, then only the chunks (row groups) kept. With a bounding box,
//! chunks whose bbox statistics (GeoParquet 1.1's `covering`) lie outside it are skipped
//! without being fetched, and rows outside it are dropped.
//!
//! Coordinates must be lon/lat: GeoParquet's default (OGC:CRS84), or a `crs` of EPSG:4326 or
//! OGC:CRS84 (GeoParquet always stores x then y, so EPSG:4326 is lon/lat too). Any other
//! `crs` is refused with its name; reprojection is for later.

use std::ops::Range;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use arrow_array::{Array, RecordBatch, cast::AsArray};
use arrow_schema::{DataType, SchemaRef};
use bytes::Bytes;
use futures::{FutureExt, TryStreamExt, future::BoxFuture};
use geo_traits::{
    CoordTrait, GeometryCollectionTrait, GeometryTrait, GeometryType, LineStringTrait, MultiLineStringTrait, MultiPointTrait, MultiPolygonTrait, PointTrait,
    PolygonTrait,
};
use geoarrow_array::{GeoArrowArray, GeoArrowArrayAccessor};
use geoparquet::reader::{GeoParquetReaderBuilder, GeoParquetRecordBatchReader, GeoParquetRecordBatchStream};
use parquet::arrow::ParquetRecordBatchStreamBuilder;
use parquet::arrow::arrow_reader::{ArrowReaderBuilder, ParquetRecordBatchReaderBuilder};
use parquet::arrow::async_reader::AsyncFileReader;
use parquet::file::metadata::{ParquetMetaData, ParquetMetaDataReader};
use serde_json::{Map, Value as Json, json};

/// An area to read, in lon/lat: west, south, east, north.
pub type Bbox = [f64; 4];

/// Whether `bytes` look like a Parquet file (`PAR1` at both ends).
pub fn is_parquet(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && bytes.starts_with(b"PAR1") && bytes.ends_with(b"PAR1")
}

/// A GeoParquet file already in memory, as a GeoJSON FeatureCollection.
pub fn to_geojson(bytes: &[u8]) -> Result<Json> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(bytes)).context("not a Parquet file")?;
    read_sync(builder, None)
}

/// A local GeoParquet file, read in place (only the chunks inside `bbox`, if given).
pub fn read_file(path: &str, bbox: Option<Bbox>) -> Result<Json> {
    let file = std::fs::File::open(path).with_context(|| format!("{path}: can't open"))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).context("not a Parquet file")?;
    read_sync(builder, bbox)
}

/// A GeoParquet file at an http(s) URL, read with range requests: its footer, then only the
/// chunks inside `bbox` (all of them without one).
pub async fn read_url(url: &str, bbox: Option<Bbox>) -> Result<Json> {
    let reader = HttpReader::open(url).await?;
    let builder = ParquetRecordBatchStreamBuilder::new(reader).await.context("not a Parquet file")?;
    let (builder, read, schema) = prepare(builder, bbox)?;
    let stream = GeoParquetRecordBatchStream::try_new(builder.build()?, schema)?;
    let batches: Vec<RecordBatch> = stream.try_collect().await?;
    collect(&batches, &read)
}

fn read_sync<T: parquet::file::reader::ChunkReader + 'static>(builder: ParquetRecordBatchReaderBuilder<T>, bbox: Option<Bbox>) -> Result<Json> {
    let (builder, read, schema) = prepare(builder, bbox)?;
    let reader = GeoParquetRecordBatchReader::try_new(builder.build()?, schema)?;
    let batches = reader.collect::<Result<Vec<_>, _>>()?;
    collect(&batches, &read)
}

/// What [`collect`] needs to know about a read.
struct Read {
    /// The geometry column.
    column: String,
    /// Keep only rows touching this box (when the file can't skip by itself).
    keep: Option<Bbox>,
    /// The column holding each row's bbox (GeoParquet 1.1 covering), left out of properties.
    covering: Option<String>,
}

/// Check the metadata and coordinate system, give the geometry column GeoArrow metadata, and
/// narrow the read to `bbox`. With GeoParquet 1.1 bbox columns, chunks outside it are skipped
/// and rows filtered as they're read; without them, every row is read and the returned box is
/// for filtering rows afterwards.
fn prepare<T>(builder: ArrowReaderBuilder<T>, bbox: Option<Bbox>) -> Result<(ArrowReaderBuilder<T>, Read, SchemaRef)> {
    let geo = builder
        .geoparquet_metadata()
        .context("a Parquet file without GeoParquet's \"geo\" metadata (no geometry column named)")?
        .map_err(|e| anyhow!("GeoParquet metadata: {e}"))?;
    let raw: Json = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .and_then(|kv| kv.iter().find(|k| k.key == "geo"))
        .and_then(|k| k.value.as_deref())
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_default();
    let col = &raw["columns"][&geo.primary_column];
    check_crs(&col["crs"], col.get("crs").is_some())?;
    let schema = builder.geoarrow_schema(&geo, false, Default::default()).map_err(|e| anyhow!("GeoParquet schema: {e}"))?;
    let mut builder = builder;
    let mut keep = None;
    if let Some([w, s, e, n]) = bbox {
        let rect = geo_types::Rect::new(geo_types::coord! { x: w, y: s }, geo_types::coord! { x: e, y: n });
        match builder.intersecting_row_groups(rect, &geo, None) {
            Ok(groups) => {
                builder = builder.with_row_groups(groups);
                builder = builder.with_intersecting_row_filter(rect, &geo, None).map_err(|e| anyhow!("GeoParquet bbox filter: {e}"))?;
            }
            // No bbox columns: read everything, keep the rows inside.
            Err(_) => keep = bbox,
        }
    }
    // The covering's bbox column is bookkeeping for skipping, not a property.
    let covering = col["covering"]["bbox"]["xmin"][0].as_str().map(str::to_string);
    Ok((builder, Read { column: geo.primary_column.clone(), keep, covering }, schema))
}

/// Record batches as a GeoJSON FeatureCollection, keeping only geometries that touch `keep`.
fn collect(batches: &[RecordBatch], read: &Read) -> Result<Json> {
    let (column, keep) = (read.column.as_str(), read.keep);
    let mut features = Vec::new();
    for batch in batches {
        let schema = batch.schema();
        let gi = schema.index_of(column).with_context(|| format!("no geometry column {column:?}"))?;
        let geoms =
            geoarrow_array::array::from_arrow_array(batch.column(gi).as_ref(), schema.field(gi)).map_err(|e| anyhow!("geometry column {column:?}: {e}"))?;
        // Any encoding (WKB, or GeoArrow's native ones) read through WKB.
        let wkb = geoarrow_array::cast::to_wkb::<i32>(geoms.as_ref()).map_err(|e| anyhow!("geometry column {column:?}: {e}"))?;
        let opts = arrow_cast::display::FormatOptions::default();
        let props: Vec<(String, &dyn Array, arrow_cast::display::ArrayFormatter)> = schema
            .fields()
            .iter()
            .enumerate()
            .filter(|(i, f)| *i != gi && Some(f.name()) != read.covering.as_ref())
            .map(|(i, f)| {
                let a = batch.column(i).as_ref();
                Ok((f.name().clone(), a, arrow_cast::display::ArrayFormatter::try_new(a, &opts)?))
            })
            .collect::<Result<_, arrow_schema::ArrowError>>()?;
        for row in 0..batch.num_rows() {
            let geometry = if wkb.is_null(row) {
                Json::Null
            } else {
                let g = wkb.value(row).map_err(|e| anyhow!("row {}: geometry: {e}", features.len()))?;
                geometry(&g, 0).with_context(|| format!("row {}: geometry", features.len()))?
            };
            if let Some(b) = keep
                && !extent(&geometry).is_some_and(|e| e[0] <= b[2] && e[2] >= b[0] && e[1] <= b[3] && e[3] >= b[1])
            {
                continue;
            }
            let mut p = Map::new();
            for (name, a, fmt) in &props {
                p.insert(name.clone(), property(*a, fmt, row));
            }
            features.push(json!({"type": "Feature", "properties": p, "geometry": geometry}));
        }
    }
    Ok(json!({"type": "FeatureCollection", "features": features}))
}

/// A column's value as a property: numbers, text and booleans as themselves; binary as
/// nothing (a property can't show bytes); anything else (dates, decimals, lists, ...) as
/// Arrow writes it as text.
fn property(a: &dyn Array, fmt: &arrow_cast::display::ArrayFormatter, row: usize) -> Json {
    if a.is_null(row) {
        return Json::Null;
    }
    use arrow_array::types::*;
    match a.data_type() {
        DataType::Boolean => json!(a.as_boolean().value(row)),
        DataType::Int8 => json!(a.as_primitive::<Int8Type>().value(row)),
        DataType::Int16 => json!(a.as_primitive::<Int16Type>().value(row)),
        DataType::Int32 => json!(a.as_primitive::<Int32Type>().value(row)),
        DataType::Int64 => json!(a.as_primitive::<Int64Type>().value(row)),
        DataType::UInt8 => json!(a.as_primitive::<UInt8Type>().value(row)),
        DataType::UInt16 => json!(a.as_primitive::<UInt16Type>().value(row)),
        DataType::UInt32 => json!(a.as_primitive::<UInt32Type>().value(row)),
        DataType::UInt64 => json!(a.as_primitive::<UInt64Type>().value(row)),
        DataType::Float32 => json!(a.as_primitive::<Float32Type>().value(row)),
        DataType::Float64 => json!(a.as_primitive::<Float64Type>().value(row)),
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView | DataType::FixedSizeBinary(_) => Json::Null,
        _ => Json::String(fmt.value(row).to_string()),
    }
}

/// Refuse coordinates that aren't lon/lat. A missing `crs` is GeoParquet's default, OGC:CRS84;
/// an explicit null means "unknown".
fn check_crs(crs: &Json, given: bool) -> Result<()> {
    if !given {
        return Ok(());
    }
    let id = |c: &Json| -> Option<(String, String)> {
        let id = &c["id"];
        Some((
            id["authority"].as_str()?.to_ascii_uppercase(),
            match &id["code"] {
                Json::String(s) => s.to_ascii_uppercase(),
                n => n.to_string(),
            },
        ))
    };
    match crs {
        Json::Null => bail!("GeoParquet geometry with an unknown coordinate system (crs: null); only lon/lat is read"),
        Json::String(s) if matches!(s.to_ascii_uppercase().as_str(), "OGC:CRS84" | "EPSG:4326") => Ok(()),
        c => match id(c) {
            Some((a, code)) if (a == "EPSG" && code == "4326") || (a == "OGC" && code == "CRS84") => Ok(()),
            _ => {
                let name = c["name"].as_str().map(str::to_string).or_else(|| id(c).map(|(a, n)| format!("{a}:{n}"))).unwrap_or_else(|| c.to_string());
                bail!("GeoParquet geometry in {name}; only lon/lat (OGC:CRS84 / EPSG:4326) is read for now")
            }
        },
    }
}

// ---- geometry ----

/// A geometry (WKB, as the `wkb` crate reads it) as GeoJSON. Z and M are dropped; an empty
/// point is null.
fn geometry(g: &impl GeometryTrait<T = f64>, depth: u32) -> Result<Json> {
    if depth > 32 {
        bail!("geometry nested too deeply");
    }
    Ok(match g.as_type() {
        GeometryType::Point(p) => match p.coord() {
            Some(c) if !c.x().is_nan() => json!({"type": "Point", "coordinates": xy(&c)}),
            _ => Json::Null,
        },
        GeometryType::LineString(l) => json!({"type": "LineString", "coordinates": line(l)}),
        GeometryType::Polygon(p) => json!({"type": "Polygon", "coordinates": rings(p)}),
        GeometryType::MultiPoint(m) => json!({"type": "MultiPoint", "coordinates": m.points().filter_map(|p| p.coord().map(|c| xy(&c))).collect::<Vec<_>>()}),
        GeometryType::MultiLineString(m) => json!({"type": "MultiLineString", "coordinates": m.line_strings().map(|l| line(&l)).collect::<Vec<_>>()}),
        GeometryType::MultiPolygon(m) => json!({"type": "MultiPolygon", "coordinates": m.polygons().map(|p| rings(&p)).collect::<Vec<_>>()}),
        GeometryType::GeometryCollection(c) => {
            json!({"type": "GeometryCollection", "geometries": c.geometries().map(|g| geometry(&g, depth + 1)).collect::<Result<Vec<_>>>()?})
        }
        _ => bail!("geometry type isn't read (rectangles, triangles and lines aren't)"),
    })
}

/// A GeoJSON geometry's extent: west, south, east, north (None when empty).
fn extent(g: &Json) -> Option<Bbox> {
    fn walk(v: &Json, b: &mut Option<Bbox>) {
        match v.as_array() {
            Some(a) if a.len() >= 2 && a[0].is_number() => {
                let (x, y) = (a[0].as_f64().unwrap_or(f64::NAN), a[1].as_f64().unwrap_or(f64::NAN));
                let e = b.get_or_insert([x, y, x, y]);
                *e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
            }
            Some(a) => a.iter().for_each(|x| walk(x, b)),
            None => {}
        }
    }
    let mut b = None;
    walk(&g["coordinates"], &mut b);
    for c in g["geometries"].as_array().into_iter().flatten() {
        if let Some(e) = extent(c) {
            let x = b.get_or_insert(e);
            *x = [x[0].min(e[0]), x[1].min(e[1]), x[2].max(e[2]), x[3].max(e[3])];
        }
    }
    b
}

fn xy(c: &impl CoordTrait<T = f64>) -> Json {
    json!([c.x(), c.y()])
}

fn line(l: &impl LineStringTrait<T = f64>) -> Json {
    l.coords().map(|c| xy(&c)).collect()
}

fn rings(p: &impl PolygonTrait<T = f64>) -> Json {
    p.exterior().into_iter().map(|r| line(&r)).chain(p.interiors().map(|r| line(&r))).collect()
}

// ---- HTTP range reads ----

/// Bytes of remote GeoParquet downloaded so far (all reads), for measuring.
static FETCHED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bytes of remote GeoParquet downloaded so far by this process.
#[doc(hidden)]
pub fn bytes_fetched() -> u64 {
    FETCHED.load(std::sync::atomic::Ordering::Relaxed)
}

/// A remote file read with HTTP range requests.
struct HttpReader {
    client: reqwest::Client,
    url: String,
    len: u64,
}

impl HttpReader {
    async fn open(url: &str) -> Result<HttpReader> {
        let client = crate::source::http_client();
        let r = client.head(url).send().await.with_context(|| format!("{url}: HEAD"))?;
        if !r.status().is_success() {
            bail!("{url}: HTTP {}", r.status());
        }
        let len = r
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()?.parse().ok())
            .with_context(|| format!("{url}: no Content-Length, so it can't be read in parts"))?;
        Ok(HttpReader { client, url: url.to_string(), len })
    }

    async fn range(&self, range: Range<u64>) -> parquet::errors::Result<Bytes> {
        let err = |e: String| parquet::errors::ParquetError::External(e.into());
        if range.start >= range.end {
            return Ok(Bytes::new());
        }
        let r = self
            .client
            .get(&self.url)
            .header(reqwest::header::RANGE, format!("bytes={}-{}", range.start, range.end - 1))
            .send()
            .await
            .map_err(|e| err(format!("{}: {e}", self.url)))?;
        if r.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(err(format!("{}: range request answered HTTP {} (the server must support ranges)", self.url, r.status())));
        }
        let b = r.bytes().await.map_err(|e| err(format!("{}: {e}", self.url)))?;
        if b.len() as u64 != range.end - range.start {
            return Err(err(format!("{}: asked for {} bytes, got {}", self.url, range.end - range.start, b.len())));
        }
        FETCHED.fetch_add(b.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(b)
    }
}

impl AsyncFileReader for HttpReader {
    fn get_bytes(&mut self, range: Range<u64>) -> BoxFuture<'_, parquet::errors::Result<Bytes>> {
        async move { self.range(range).await }.boxed()
    }

    fn get_metadata<'a>(
        &'a mut self,
        _options: Option<&'a parquet::arrow::arrow_reader::ArrowReaderOptions>,
    ) -> BoxFuture<'a, parquet::errors::Result<Arc<ParquetMetaData>>> {
        async move {
            let len = self.len;
            // The footer and usually the whole metadata in one request.
            let meta = ParquetMetaDataReader::new().with_prefetch_hint(Some(64 << 10)).load_and_finish(&mut *self, len).await?;
            Ok(Arc::new(meta))
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parquet::basic::{Repetition, Type as PhysicalType};
    use parquet::data_type::{ByteArray, ByteArrayType, DoubleType};
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use parquet::file::writer::SerializedFileWriter;
    use parquet::schema::types::Type;

    fn le_point(x: f64, y: f64) -> Vec<u8> {
        let mut v = vec![1, 1, 0, 0, 0];
        v.extend(x.to_le_bytes());
        v.extend(y.to_le_bytes());
        v
    }

    /// A small GeoParquet file: a WKB `geometry` column, a `name` and a `height`.
    fn sample(geo: &str) -> Vec<u8> {
        let schema = Type::group_type_builder("schema")
            .with_fields(vec![
                Arc::new(Type::primitive_type_builder("geometry", PhysicalType::BYTE_ARRAY).with_repetition(Repetition::REQUIRED).build().unwrap()),
                Arc::new(
                    Type::primitive_type_builder("name", PhysicalType::BYTE_ARRAY)
                        .with_repetition(Repetition::REQUIRED)
                        .with_logical_type(Some(parquet::basic::LogicalType::String))
                        .build()
                        .unwrap(),
                ),
                Arc::new(Type::primitive_type_builder("height", PhysicalType::DOUBLE).with_repetition(Repetition::REQUIRED).build().unwrap()),
            ])
            .build()
            .unwrap();
        let props = WriterProperties::builder().set_key_value_metadata(Some(vec![KeyValue::new("geo".into(), geo.to_string())])).build();
        let mut out = Vec::new();
        let mut w = SerializedFileWriter::new(&mut out, Arc::new(schema), Arc::new(props)).unwrap();
        let mut rg = w.next_row_group().unwrap();
        let geoms: Vec<ByteArray> = vec![le_point(-77.03, 38.89).into(), le_point(2.29, 48.86).into()];
        let mut c = rg.next_column().unwrap().unwrap();
        c.typed::<ByteArrayType>().write_batch(&geoms, None, None).unwrap();
        c.close().unwrap();
        let mut c = rg.next_column().unwrap().unwrap();
        c.typed::<ByteArrayType>().write_batch(&[ByteArray::from("Mall"), ByteArray::from("Tour")], None, None).unwrap();
        c.close().unwrap();
        let mut c = rg.next_column().unwrap().unwrap();
        c.typed::<DoubleType>().write_batch(&[0.0, 330.0], None, None).unwrap();
        c.close().unwrap();
        rg.close().unwrap();
        w.close().unwrap();
        out
    }

    const WKB_GEO: &str = r#"{"version": "1.1.0", "primary_column": "geometry", "columns": {"geometry": {"encoding": "WKB", "geometry_types": ["Point"]}}}"#;

    #[test]
    fn reads_geoparquet() {
        let bytes = sample(WKB_GEO);
        assert!(is_parquet(&bytes));
        assert_eq!(crate::formats::Format::detect("x.bin", &bytes), crate::formats::Format::GeoParquet);
        let doc = to_geojson(&bytes).unwrap();
        assert_eq!(extent(&json!({"type": "LineString", "coordinates": [[1.0, 5.0], [-2.0, 3.0]]})), Some([-2.0, 3.0, 1.0, 5.0]));
        let f = &doc["features"];
        assert_eq!(f.as_array().unwrap().len(), 2);
        assert_eq!(f[1]["geometry"], json!({"type": "Point", "coordinates": [2.29, 48.86]}));
        assert_eq!(f[1]["properties"], json!({"name": "Tour", "height": 330.0}));
        // Lon/lat by EPSG code is fine; a projected crs is refused by name.
        let epsg = r#"{"version": "1.1.0", "primary_column": "geometry", "columns": {"geometry": {"encoding": "WKB", "geometry_types": [], "crs": {"id": {"authority": "EPSG", "code": 4326}}}}}"#;
        assert!(to_geojson(&sample(epsg)).is_ok());
        let utm = r#"{"version": "1.1.0", "primary_column": "geometry", "columns": {"geometry": {"encoding": "WKB", "geometry_types": [], "crs": {"name": "WGS 84 / UTM zone 18N", "id": {"authority": "EPSG", "code": 32618}}}}}"#;
        assert!(format!("{:#}", to_geojson(&sample(utm)).unwrap_err()).contains("UTM zone 18N"));
        assert!(to_geojson(b"PAR1 not really PAR1").is_err());
    }

    #[test]
    fn bbox_keeps_rows_inside() {
        // Rows outside the box are dropped (this file has no bbox statistics, so only rows).
        let dir = std::env::temp_dir().join(format!("mapterm-gpq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two.parquet");
        std::fs::write(&path, sample(WKB_GEO)).unwrap();
        let doc = read_file(path.to_str().unwrap(), Some([-80.0, 35.0, -70.0, 42.0])).unwrap();
        let names: Vec<&str> = doc["features"].as_array().unwrap().iter().map(|f| f["properties"]["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Mall"]);
        assert_eq!(read_file(path.to_str().unwrap(), None).unwrap()["features"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
