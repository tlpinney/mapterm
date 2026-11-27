//! Areas (polygons) drawn as map tiles here, for terminals that show images: weather alerts
//! and power outages. Each area is a fill and a smooth outline in its own color, drawn in
//! the order given (the last on top), with one optionally highlighted. Shown and moved like
//! imagery, so panning and zooming stay images; a tile draws only the areas it touches.

use std::sync::{Arc, RwLock};

use anyhow::Result;
use serde_json::Value;

/// Pixels area tiles are drawn past their right and bottom edges, so smooth scrolling can
/// move into them without stretching (more than a cell at any zoom a tile is shown at).
pub const BLEED: u32 = 64;

/// An area to draw: its color, how strongly it's filled and outlined, and its rings.
pub struct Shape {
    /// The color of its fill and outline.
    pub rgb: (u8, u8, u8),
    /// The fill's alpha (0 for an outline only).
    pub fill: u8,
    /// The outline's alpha (0 for a fill only).
    pub line: u8,
    /// Highlighted: a stronger fill and a wide outline under a white one.
    pub selected: bool,
    /// Lines (roads), not areas: not closed, never filled.
    pub open: bool,
    /// The line width, pixels.
    pub width: f32,
    /// The rings (or lines), in world coordinates (0 to 1, Web Mercator).
    rings: Vec<Vec<(f64, f64)>>,
}

impl Shape {
    /// An area in color `rgb`, with a faint fill and a solid outline; add its outlines with
    /// [`Shape::add`].
    pub fn new(rgb: (u8, u8, u8), selected: bool) -> Shape {
        Shape { rgb, fill: 38, line: 230, selected, open: false, width: 2.0, rings: Vec::new() }
    }

    /// Lines in color `rgb` (a road closure), `width` pixels wide; add them with [`Shape::add`].
    pub fn lines(rgb: (u8, u8, u8), width: f32, selected: bool) -> Shape {
        Shape { rgb, fill: 0, line: 255, selected, open: true, width, rings: Vec::new() }
    }

    /// Add a GeoJSON Polygon or MultiPolygon geometry (lon/lat) to the area; anything else is
    /// ignored.
    pub fn add(&mut self, g: &Value) {
        let ring = |r: &Value| -> Vec<(f64, f64)> {
            r.as_array().into_iter().flatten().filter_map(|p| Some(world(p.get(0)?.as_f64()?, p.get(1)?.as_f64()?))).collect()
        };
        match g.get("type").and_then(Value::as_str) {
            Some("Polygon") => self.rings.extend(g["coordinates"].as_array().into_iter().flatten().map(ring)),
            Some("MultiPolygon") => {
                for p in g["coordinates"].as_array().into_iter().flatten() {
                    self.rings.extend(p.as_array().into_iter().flatten().map(ring));
                }
            }
            Some("LineString") if self.open => self.rings.push(ring(&g["coordinates"])),
            Some("MultiLineString") if self.open => self.rings.extend(g["coordinates"].as_array().into_iter().flatten().map(ring)),
            _ => {}
        }
    }
}

/// A shape ready to draw, with its bounds.
struct Area {
    shape: Shape,
    bounds: (f64, f64, f64, f64),
}

/// Lon/lat to world coordinates (0 to 1, Web Mercator).
pub fn world(lon: f64, lat: f64) -> (f64, f64) {
    let lat = lat.clamp(-85.05, 85.05).to_radians();
    ((lon + 180.0) / 360.0, (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0)
}

/// Areas as map tiles: [`AreaTiles::set`] gives it the areas, and a tile is drawn on demand
/// ([`crate::raster::TileRender`]).
#[derive(Default)]
pub struct AreaTiles {
    areas: RwLock<Vec<Area>>,
}

impl AreaTiles {
    /// Draw these areas from now on, in this order (the last on top). The caller drops tiles
    /// already drawn ([`crate::Map::put_raster_tile`]).
    pub fn set(&self, shapes: Vec<Shape>) {
        let areas = shapes
            .into_iter()
            .filter_map(|mut shape| {
                let min = if shape.open { 2 } else { 3 };
                shape.rings.retain(|r| r.len() >= min);
                let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for &(x, y) in shape.rings.iter().flatten() {
                    b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
                }
                (!shape.rings.is_empty()).then_some(Area { shape, bounds: b })
            })
            .collect();
        *self.areas.write().unwrap_or_else(|e| e.into_inner()) = areas;
    }

    /// Tile `z/x/y` as RGBA, `size` pixels square plus [`BLEED`] past its right and bottom
    /// edges, or None when no area touches it.
    pub fn draw(&self, z: u8, x: u32, y: u32, size: u32) -> Option<Vec<u8>> {
        use tiny_skia::{FillRule, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};
        let n = (1u64 << z) as f64;
        // The tile in world coordinates, with a little room for outlines just outside it.
        let pad = 4.0 / size as f64 / n;
        let far = (BLEED as f64 + 4.0) / size as f64 / n;
        let (x0, y0, x1, y1) = (x as f64 / n - pad, y as f64 / n - pad, (x + 1) as f64 / n + far, (y + 1) as f64 / n + far);
        let areas = self.areas.read().unwrap_or_else(|e| e.into_inner());
        let mut pm: Option<Pixmap> = None;
        let stroke = Stroke { width: 2.0, line_join: LineJoin::Round, line_cap: tiny_skia::LineCap::Round, ..Stroke::default() };
        let px = |(wx, wy): (f64, f64)| (((wx * n - x as f64) * size as f64) as f32, ((wy * n - y as f64) * size as f64) as f32);
        for a in areas.iter().filter(|a| a.bounds.0 <= x1 && a.bounds.2 >= x0 && a.bounds.1 <= y1 && a.bounds.3 >= y0) {
            let mut pb = PathBuilder::new();
            for r in &a.shape.rings {
                let (sx, sy) = px(r[0]);
                pb.move_to(sx, sy);
                for &p in &r[1..] {
                    let (qx, qy) = px(p);
                    pb.line_to(qx, qy);
                }
                if !a.shape.open {
                    pb.close();
                }
            }
            let Some(path) = pb.finish() else { continue };
            let pm = pm.get_or_insert_with(|| Pixmap::new(size + BLEED, size + BLEED).expect("tile size"));
            let (r, g, b) = a.shape.rgb;
            let mut paint = Paint { anti_alias: true, ..Paint::default() };
            let stroke = Stroke { width: a.shape.width, ..stroke.clone() };
            if a.shape.selected {
                // Highlighted: a stronger fill, its color's outline wide under a white one.
                if !a.shape.open {
                    paint.set_color_rgba8(r, g, b, a.shape.fill.saturating_add(52).max(90));
                    pm.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
                }
                paint.set_color_rgba8(r, g, b, 255);
                pm.stroke_path(&path, &paint, &Stroke { width: a.shape.width + 4.0, ..stroke.clone() }, Transform::identity(), None);
                paint.set_color_rgba8(255, 255, 255, 255);
                pm.stroke_path(&path, &paint, &Stroke { width: (a.shape.width * 0.8).max(1.5), ..stroke.clone() }, Transform::identity(), None);
                continue;
            }
            if a.shape.fill > 0 && !a.shape.open {
                paint.set_color_rgba8(r, g, b, a.shape.fill);
                pm.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
            }
            if a.shape.line > 0 {
                paint.set_color_rgba8(r, g, b, a.shape.line);
                pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        // Areas whose bounds touch the tile but that miss it draw nothing: no tile.
        let rgba = crate::timeseries::to_rgba(pm?);
        rgba.chunks(4).any(|p| p[3] > 0).then_some(rgba)
    }
}

impl crate::raster::TileRender for AreaTiles {
    fn render(&self, z: u8, x: u32, y: u32, size: u32) -> Option<Vec<u8>> {
        crate::raster::encode_png(&self.draw(z, x, y, size)?, size + BLEED, size + BLEED).ok()
    }
}

/// Area tiles as an overlay image source named `name`: 512-pixel tiles, drawn to zoom 14
/// (enlarged past it), kept in memory, credited `credit`.
pub fn tile_source(tiles: Arc<AreaTiles>, name: &str, credit: &str) -> Result<Arc<crate::raster::RasterSource>> {
    tile_source_to(tiles, name, credit, 14)
}

/// [`tile_source`], drawn to zoom `max_zoom`: higher for lines, which would thicken enlarged.
pub fn tile_source_to(tiles: Arc<AreaTiles>, name: &str, credit: &str, max_zoom: u8) -> Result<Arc<crate::raster::RasterSource>> {
    // Never fetched (the renderer comes first); .invalid never resolves, should anything try.
    let mut s = crate::raster::RasterSource::new(&format!("https://{name}.mapterm.invalid/{{z}}/{{x}}/{{y}}.png"), false, max_zoom)?;
    s.tile_px = 512;
    s.overlay = true;
    s.cache = false;
    s.render = Some(tiles);
    s.bleed = BLEED;
    s.name = name.into();
    s.attribution = credit.into();
    Ok(Arc::new(s))
}
