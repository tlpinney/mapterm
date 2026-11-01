//! Raster (image) tile layers: XYZ or TMS URLs of PNG/JPEG tiles, drawn underneath the vector
//! map. Tiles are fetched and cached like vector tiles, decoded once, and laid out by giving
//! every screen cell to exactly one tile, so blocks of cells can be drawn from a cropped tile
//! (kitty graphics) or sampled into cell colors (any terminal).

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use anyhow::{Context, Result, bail};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Color,
    widgets::Widget,
};

use crate::{
    render::{Camera, to_world},
    source::{cache_root, get, hash, http_client, read_fresh},
    store::TileId,
};

pub struct RasterSource {
    template: String,
    tms: bool,
    http: reqwest::Client,
    cache_dir: std::path::PathBuf,
    pub max_zoom: u8,
    pub name: String,
    /// Credit to show for the imagery (known providers are filled in automatically).
    pub attribution: String,
}

/// The credit that well-known tile providers ask for, from the tile URL's host.
pub fn known_attribution(template: &str) -> Option<&'static str> {
    let host = template.split('/').nth(2).unwrap_or("");
    if host.ends_with("arcgisonline.com") || host.ends_with("arcgis.com") {
        Some("Esri, Maxar, Earthstar Geographics")
    } else if host.ends_with("nationalmap.gov") {
        Some("USGS The National Map")
    } else if host.ends_with("openstreetmap.org") || host.ends_with("openstreetmap.fr") {
        Some("© OpenStreetMap contributors")
    } else if host.ends_with("s2maps-tiles.eu") {
        Some("Sentinel-2 cloudless by EOX")
    } else {
        None
    }
}

impl RasterSource {
    /// `template` has `{z}`, `{x}`, and `{y}` (or `{-y}` for TMS rows counted from the bottom).
    /// `tms` flips `{y}` the same way, for TMS services that use plain `{y}`.
    pub fn new(template: &str, tms: bool, max_zoom: u8) -> Result<Self> {
        if !(template.starts_with("https://") || template.starts_with("http://")) {
            bail!("{template}: raster layers need an http(s) URL template");
        }
        if !template.contains("{z}") || !template.contains("{x}") || !(template.contains("{y}") || template.contains("{-y}")) {
            bail!("{template}: the URL needs {{z}}, {{x}} and {{y}} (or {{-y}} for TMS)");
        }
        Ok(Self {
            template: template.to_string(),
            tms,
            http: http_client(),
            cache_dir: cache_root().join("raster").join(hash(template)),
            max_zoom,
            name: template.split('/').nth(2).unwrap_or(template).to_string(),
            attribution: known_attribution(template).unwrap_or_default().to_string(),
        })
    }

    fn url(&self, z: u8, x: u32, y: u32) -> String {
        let flipped = (1u32 << z) - 1 - y;
        let y_val = if self.tms { flipped } else { y };
        self.template
            .replace("{z}", &z.to_string())
            .replace("{x}", &x.to_string())
            .replace("{-y}", &flipped.to_string())
            .replace("{y}", &y_val.to_string())
    }

    /// The tile's image bytes (PNG or JPEG), from the 30-day disk cache or the network.
    pub async fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        let path = self.cache_dir.join(format!("{z}/{x}/{y}.img"));
        if let Some(b) = read_fresh(&path) {
            return Ok(if b.is_empty() { None } else { Some(b) });
        }
        let bytes = get(&self.http, &self.url(z, x, y), None).await?;
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&path, bytes.as_deref().unwrap_or(&[]));
        Ok(bytes)
    }
}

static NEXT_IMAGE_ID: AtomicU32 = AtomicU32::new(1);

/// A decoded raster tile.
pub struct RasterTile {
    /// Image id for terminal graphics protocols, unique in this process.
    pub id: u32,
    /// PNG bytes (re-encoded if the source was JPEG), for the kitty graphics protocol.
    pub png: Vec<u8>,
    pub rgba: image::RgbaImage,
}

impl RasterTile {
    pub fn size(&self) -> (u32, u32) {
        self.rgba.dimensions()
    }

    /// Average color of a source rectangle (for drawing into cell backgrounds).
    fn average(&self, x: u32, y: u32, w: u32, h: u32) -> Color {
        let (iw, ih) = self.size();
        let (x1, y1) = ((x + w).min(iw), (y + h).min(ih));
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        // Sample a few points instead of every pixel: cells are small next to tiles.
        let step = ((w.max(h)) / 4).max(1);
        let mut yy = y.min(ih.saturating_sub(1));
        while yy < y1.max(yy + 1) && yy < ih {
            let mut xx = x.min(iw.saturating_sub(1));
            while xx < x1.max(xx + 1) && xx < iw {
                let p = self.rgba.get_pixel(xx, yy);
                (r, g, b, n) = (r + p[0] as u32, g + p[1] as u32, b + p[2] as u32, n + 1);
                xx += step;
            }
            yy += step;
        }
        if n == 0 {
            return Color::Reset;
        }
        Color::Rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
    }
}

/// Decode PNG or JPEG bytes (CPU work: call from a blocking task).
pub fn decode(bytes: &[u8]) -> Result<RasterTile> {
    let img = image::load_from_memory(bytes).context("decoding raster tile")?;
    let rgba = img.to_rgba8();
    let png = if bytes.starts_with(b"\x89PNG") {
        bytes.to_vec()
    } else {
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)?;
        out.into_inner()
    };
    Ok(RasterTile { id: NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed), png, rgba })
}

/// Decoded raster tiles with a size bound. Evicted image ids are kept for the caller to free
/// from the terminal.
pub struct RasterStore {
    tiles: HashMap<TileId, Arc<RasterTile>>,
    order: VecDeque<TileId>,
    cap: usize,
    evicted: Vec<u32>,
}

impl Default for RasterStore {
    fn default() -> Self {
        Self { tiles: HashMap::new(), order: VecDeque::new(), cap: 256, evicted: Vec::new() }
    }
}

impl RasterStore {
    pub fn contains(&self, id: &TileId) -> bool {
        self.tiles.contains_key(id)
    }

    pub fn insert(&mut self, id: TileId, t: Option<RasterTile>) {
        // A missing tile is remembered as a 1x1 transparent image so it isn't fetched again.
        let t = t.unwrap_or_else(|| RasterTile {
            id: NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed),
            png: Vec::new(),
            rgba: image::RgbaImage::new(1, 1),
        });
        if let Some(old) = self.tiles.insert(id, Arc::new(t)) {
            self.evicted.push(old.id);
        } else {
            self.order.push_back(id);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front()
                && let Some(t) = self.tiles.remove(&old)
            {
                self.evicted.push(t.id);
            }
        }
    }

    /// Image ids dropped since the last call, to delete from the terminal.
    pub fn take_evicted(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.evicted)
    }

    pub fn get_or_ancestor(&self, id: TileId) -> Option<(TileId, Arc<RasterTile>)> {
        let (z, x, y) = id;
        (0..=6u8.min(z)).find_map(|k| {
            let a = (z - k, x >> k, y >> k);
            self.tiles.get(&a).filter(|t| !t.png.is_empty()).map(|t| (a, t.clone()))
        })
    }
}

/// The raster zoom to fetch so one source pixel covers about `screen_per_px` braille dots.
pub fn raster_zoom(cam: &Camera, tile_px: u32, dots_per_source_px: f64, max_zoom: u8) -> u8 {
    // At zoom z a tile spans 256 * 2^(cam.zoom - z) dots over tile_px source pixels.
    let z = cam.zoom - (dots_per_source_px * tile_px as f64 / 256.0).log2();
    (z.round() as i64).clamp(0, max_zoom as i64) as u8
}

/// The tiles a view needs at raster zoom `rz`, nearest the center first.
pub fn tiles_for(cam: &Camera, area: Rect, rz: u8) -> Vec<TileId> {
    let n = (1u64 << rz) as f64;
    let scale = 256.0 * 2f64.powf(cam.zoom);
    let (cx, cy) = to_world(cam.lon, cam.lat);
    let (hw, hh) = (area.width as f64 / scale, area.height as f64 * 2.0 / scale);
    let clamp = |v: f64| (v.floor() as i64).clamp(0, n as i64 - 1) as u32;
    let (x0, x1) = (clamp((cx - hw) * n), clamp((cx + hw) * n));
    let (y0, y1) = (clamp((cy - hh) * n), clamp((cy + hh) * n));
    let mut out: Vec<TileId> = (x0..=x1).flat_map(|x| (y0..=y1).map(move |y| (rz, x, y))).collect();
    let (ccx, ccy) = (cx * n - 0.5, cy * n - 0.5);
    out.sort_by(|a, b| {
        let d = |t: &TileId| (t.1 as f64 - ccx).powi(2) + (t.2 as f64 - ccy).powi(2);
        d(a).total_cmp(&d(b))
    });
    out
}

/// A rectangle of screen cells drawn from one tile's source rectangle.
pub struct Block {
    pub tile: Arc<RasterTile>,
    /// Absolute screen cells.
    pub cells: Rect,
    /// Source rectangle in tile pixels (x, y, w, h).
    pub src: (u32, u32, u32, u32),
}

/// Give every cell of `area` to the raster tile under its center and group the cells into
/// rectangles, one per tile, with the matching source rectangle in that tile.
pub fn layout(store: &RasterStore, cam: &Camera, area: Rect, rz: u8) -> Vec<Block> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let scale = 256.0 * 2f64.powf(cam.zoom);
    let (cx, cy) = to_world(cam.lon, cam.lat);
    let (w, h) = (area.width as f64 * 2.0, area.height as f64 * 4.0);
    // World coordinate of a dot boundary (x in dots across, y in dots down the area).
    let wx = |dot: f64| cx + (dot - w / 2.0) / scale;
    let wy = |dot: f64| cy + (dot - h / 2.0) / scale;
    let n = (1u64 << rz) as f64;
    // Runs of columns (and rows) whose centers fall in the same tile column (row).
    let runs = |count: u16, per_cell: f64, world: &dyn Fn(f64) -> f64| {
        let mut out: Vec<(u16, u16, i64)> = Vec::new();
        for c in 0..count {
            let t = (world((c as f64 + 0.5) * per_cell) * n).floor() as i64;
            match out.last_mut() {
                Some((_, end, last)) if *last == t => *end = c + 1,
                _ => out.push((c, c + 1, t)),
            }
        }
        out
    };
    let cols = runs(area.width, 2.0, &wx);
    let rows = runs(area.height, 4.0, &wy);
    let mut blocks = Vec::new();
    for &(c0, c1, tx) in &cols {
        for &(r0, r1, ty) in &rows {
            if tx < 0 || ty < 0 || tx >= n as i64 || ty >= n as i64 {
                continue;
            }
            let Some(((az, ax, ay), tile)) = store.get_or_ancestor((rz, tx as u32, ty as u32)) else { continue };
            let an = (1u64 << az) as f64;
            let (iw, ih) = tile.size();
            // The block's world edges, in source pixels of the (possibly ancestor) tile.
            let sx0 = (wx(c0 as f64 * 2.0) * an - ax as f64) * iw as f64;
            let sx1 = (wx(c1 as f64 * 2.0) * an - ax as f64) * iw as f64;
            let sy0 = (wy(r0 as f64 * 4.0) * an - ay as f64) * ih as f64;
            let sy1 = (wy(r1 as f64 * 4.0) * an - ay as f64) * ih as f64;
            let x = (sx0.round().max(0.0) as u32).min(iw - 1);
            let y = (sy0.round().max(0.0) as u32).min(ih - 1);
            let x_end = (sx1.round().max(0.0) as u32).clamp(x + 1, iw);
            let y_end = (sy1.round().max(0.0) as u32).clamp(y + 1, ih);
            blocks.push(Block {
                tile,
                cells: Rect::new(area.x + c0, area.y + r0, c1 - c0, r1 - r0),
                src: (x, y, x_end - x, y_end - y),
            });
        }
    }
    blocks
}

/// Draws raster blocks as cell background colors: works in any terminal, one color per cell.
/// Draw the vector map on top without fills to keep the colors.
pub struct RasterCells<'a> {
    pub blocks: &'a [Block],
}

impl Widget for RasterCells<'_> {
    fn render(self, _area: Rect, buf: &mut Buffer) {
        for b in self.blocks {
            let (sx, sy, sw, sh) = b.src;
            let (cw, ch) = (sw as f64 / b.cells.width as f64, sh as f64 / b.cells.height as f64);
            for r in 0..b.cells.height {
                for c in 0..b.cells.width {
                    let x = sx + (c as f64 * cw) as u32;
                    let y = sy + (r as f64 * ch) as u32;
                    let color = b.tile.average(x, y, cw.ceil() as u32, ch.ceil() as u32);
                    if let Some(cell) = buf.cell_mut((b.cells.x + c, b.cells.y + r)) {
                        cell.set_bg(color);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_xyz_and_tms() {
        let xyz = RasterSource::new("https://t.example/{z}/{x}/{y}.png", false, 19).unwrap();
        assert_eq!(xyz.url(3, 2, 1), "https://t.example/3/2/1.png");
        let tms = RasterSource::new("https://t.example/{z}/{x}/{-y}.png", false, 19).unwrap();
        assert_eq!(tms.url(3, 2, 1), "https://t.example/3/2/6.png");
        let flag = RasterSource::new("https://t.example/{z}/{x}/{y}.png", true, 19).unwrap();
        assert_eq!(flag.url(3, 2, 1), "https://t.example/3/2/6.png");
        assert!(RasterSource::new("https://t.example/{z}/{x}.png", false, 19).is_err());
        assert!(RasterSource::new("file:///x/{z}/{x}/{y}.png", false, 19).is_err());
        let esri = RasterSource::new("https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}", false, 19).unwrap();
        assert_eq!(esri.url(15, 9372, 12536), "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/15/12536/9372");
        assert_eq!(esri.attribution, "Esri, Maxar, Earthstar Geographics");
    }

    #[test]
    fn layout_covers_every_cell_once() {
        let mut store = RasterStore::default();
        let cam = Camera::new(-77.0353, 38.8895, 14.3);
        let area = Rect::new(3, 2, 90, 30);
        let rz = 14;
        for id in tiles_for(&cam, area, rz) {
            store.insert(id, Some(RasterTile { id: 0, png: vec![1], rgba: image::RgbaImage::new(256, 256) }));
        }
        let blocks = layout(&store, &cam, area, rz);
        let mut covered = vec![0u8; area.width as usize * area.height as usize];
        for b in &blocks {
            assert!(b.src.0 + b.src.2 <= 256 && b.src.1 + b.src.3 <= 256);
            for r in b.cells.y..b.cells.bottom() {
                for c in b.cells.x..b.cells.right() {
                    covered[(r - area.y) as usize * area.width as usize + (c - area.x) as usize] += 1;
                }
            }
        }
        assert!(covered.iter().all(|&n| n == 1), "every cell drawn exactly once");
    }
}
