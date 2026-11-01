//! Style layers, as in the MapLibre style spec: a layer draws features of one source (and one
//! source-layer of a vector source) as fills, lines, circles or symbols, chosen by a filter and
//! colored by paint properties that may be expressions.
//!
//! Build them in Rust:
//!
//! ```ignore
//! StyleLayer::line("route", "osrm").color("#00e5ff")
//! StyleLayer::fill("parks", "basemap").source_layer("park").color("#1a3220")
//! StyleLayer::symbol("towns", "basemap").source_layer("place")
//!     .filter(json!(["==", ["get", "class"], "town"]))
//!     .text(json!(["get", "name"]))
//!     .color("#ddd")
//! ```
//!
//! or parse them from style JSON with [`StyleLayer::from_json`].
//!
//! Terminal cells can't show widths, blur or fonts, so only these properties are used:
//!
//! | layer | paint | layout |
//! |---|---|---|
//! | background | `background-color`, `background-opacity` | `visibility` |
//! | fill | `fill-color`, `fill-opacity`, `fill-outline-color` | `visibility` |
//! | line | `line-color`, `line-opacity` | `visibility` |
//! | circle | `circle-color`, `circle-opacity` | `visibility`, `mapterm:glyph` |
//! | symbol | `text-color`, `text-opacity`, `icon-color`, `icon-opacity` | `visibility`, `text-field`, `text-font` (bold, italic), `text-transform`, `text-anchor`, `text-variable-anchor`, `text-offset`, `symbol-placement`, `symbol-spacing`, `symbol-sort-key`, `icon-image` (as a glyph), `mapterm:glyph` |
//!
//! `mapterm:glyph` is a mapterm extension: the character drawn for a point (`●` by default
//! for circles; symbols draw it before their text, like a marker). Without it, a point's
//! `icon-image` becomes a one-cell symbol when [`crate::icons`] knows the icon (✚ hospital,
//! ✈ airport, ⋔ restaurant, ...).

use anyhow::{Result, anyhow, bail};
use serde_json::Value as Json;

use crate::expr::Expr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Background,
    Fill,
    Line,
    Circle,
    Symbol,
    Raster,
    /// A layer type mapterm doesn't draw (hillshade, heatmap, fill-extrusion, ...).
    Other(String),
}

impl Kind {
    fn parse(s: &str) -> Kind {
        match s {
            "background" => Kind::Background,
            "fill" => Kind::Fill,
            "line" => Kind::Line,
            "circle" => Kind::Circle,
            "symbol" => Kind::Symbol,
            "raster" => Kind::Raster,
            o => Kind::Other(o.to_string()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Kind::Background => "background",
            Kind::Fill => "fill",
            Kind::Line => "line",
            Kind::Circle => "circle",
            Kind::Symbol => "symbol",
            Kind::Raster => "raster",
            Kind::Other(o) => o,
        }
    }

    /// The paint property `.color()` sets.
    fn color_prop(&self) -> &'static str {
        match self {
            Kind::Background => "background-color",
            Kind::Fill => "fill-color",
            Kind::Line => "line-color",
            Kind::Circle => "circle-color",
            Kind::Symbol => "text-color",
            Kind::Raster | Kind::Other(_) => "color",
        }
    }
}

/// Properties mapterm reads; others are kept out so lookups stay short.
const USED_PAINT: [&str; 13] = [
    "background-color",
    "background-opacity",
    "fill-color",
    "fill-opacity",
    "fill-outline-color",
    "line-color",
    "line-opacity",
    "circle-color",
    "circle-opacity",
    "text-color",
    "text-opacity",
    "icon-color",
    "icon-opacity",
];
const USED_LAYOUT: [&str; 12] = [
    "text-field",
    "text-font",
    "text-transform",
    "text-anchor",
    "text-variable-anchor",
    "text-offset",
    "symbol-placement",
    "symbol-spacing",
    "symbol-sort-key",
    "mapterm:glyph",
    "icon-image",
    "visibility",
];

/// One style layer.
#[derive(Clone, Debug)]
pub struct StyleLayer {
    pub id: String,
    pub kind: Kind,
    pub source: Option<String>,
    pub source_layer: Option<String>,
    pub min_zoom: f64,
    pub max_zoom: f64,
    pub filter: Option<Expr>,
    pub visible: bool,
    paint: Vec<(String, Expr)>,
    layout: Vec<(String, Expr)>,
}

impl StyleLayer {
    pub fn new(id: impl Into<String>, kind: Kind, source: Option<String>) -> Self {
        Self { id: id.into(), kind, source, source_layer: None, min_zoom: 0.0, max_zoom: 24.0, filter: None, visible: true, paint: Vec::new(), layout: Vec::new() }
    }

    pub fn background(id: impl Into<String>) -> Self {
        Self::new(id, Kind::Background, None)
    }

    pub fn fill(id: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(id, Kind::Fill, Some(source.into()))
    }

    pub fn line(id: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(id, Kind::Line, Some(source.into()))
    }

    pub fn circle(id: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(id, Kind::Circle, Some(source.into()))
    }

    pub fn symbol(id: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(id, Kind::Symbol, Some(source.into()))
    }

    /// Image tiles from a raster source.
    pub fn raster(id: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(id, Kind::Raster, Some(source.into()))
    }

    /// The source-layer of a vector source to draw.
    pub fn source_layer(mut self, name: impl Into<String>) -> Self {
        self.source_layer = Some(name.into());
        self
    }

    /// Only draw from this zoom on.
    pub fn min_zoom(mut self, z: f64) -> Self {
        self.min_zoom = z;
        self
    }

    /// Only draw below this zoom.
    pub fn max_zoom(mut self, z: f64) -> Self {
        self.max_zoom = z;
        self
    }

    /// Draw only features where the expression (or legacy filter) is true.
    ///
    /// # Panics
    /// If the filter isn't a supported expression; use [`StyleLayer::try_filter`] for input you
    /// don't control.
    pub fn filter(self, f: Json) -> Self {
        self.try_filter(f).unwrap_or_else(|e| panic!("bad filter: {e}"))
    }

    pub fn try_filter(mut self, f: Json) -> Result<Self> {
        self.filter = Some(Expr::parse(&f)?);
        Ok(self)
    }

    /// The layer's color: `fill-color`, `line-color`, `circle-color`, `text-color` or
    /// `background-color`. A CSS color string or an expression.
    pub fn color(self, c: impl Into<Json>) -> Self {
        let prop = self.kind.color_prop();
        self.paint(prop, c)
    }

    /// `fill-opacity`, `line-opacity`, ... (blended with the background color in a terminal).
    pub fn opacity(self, o: impl Into<Json>) -> Self {
        let prop = self.kind.color_prop().replace("-color", "-opacity");
        self.paint(&prop, o)
    }

    /// A symbol's `text-field`: an expression, or a string with `{property}` tokens.
    pub fn text(self, t: impl Into<Json>) -> Self {
        self.layout("text-field", t)
    }

    /// The character drawn at points (`mapterm:glyph`).
    pub fn glyph(self, g: impl Into<Json>) -> Self {
        self.layout("mapterm:glyph", g)
    }

    /// `symbol-sort-key`: lower keys are placed first when labels collide.
    pub fn sort_key(self, k: impl Into<Json>) -> Self {
        self.layout("symbol-sort-key", k)
    }

    /// Set any paint property.
    ///
    /// # Panics
    /// If the value isn't a supported expression; use [`StyleLayer::set_paint`] for input
    /// you don't control.
    pub fn paint(mut self, name: &str, v: impl Into<Json>) -> Self {
        self.set_paint(name, &v.into()).unwrap_or_else(|e| panic!("{name}: {e}"));
        self
    }

    /// Set any layout property (see [`StyleLayer::paint`]).
    pub fn layout(mut self, name: &str, v: impl Into<Json>) -> Self {
        self.set_layout(name, &v.into()).unwrap_or_else(|e| panic!("{name}: {e}"));
        self
    }

    pub fn set_paint(&mut self, name: &str, v: &Json) -> Result<()> {
        set(&mut self.paint, name, v)
    }

    pub fn set_layout(&mut self, name: &str, v: &Json) -> Result<()> {
        if name == "visibility" {
            self.visible = v.as_str() != Some("none");
            return Ok(());
        }
        set(&mut self.layout, name, v)
    }

    pub fn paint_expr(&self, name: &str) -> Option<&Expr> {
        self.paint.iter().find(|(k, _)| k == name).map(|(_, e)| e)
    }

    pub fn layout_expr(&self, name: &str) -> Option<&Expr> {
        self.layout.iter().find(|(k, _)| k == name).map(|(_, e)| e)
    }

    /// Whether the filter or any property used depends on the zoom level.
    pub fn uses_zoom(&self) -> bool {
        self.filter.as_ref().is_some_and(Expr::uses_zoom)
            || self.paint.iter().chain(&self.layout).any(|(_, e)| e.uses_zoom())
    }

    /// Whether the layer draws at this zoom.
    pub fn shown_at(&self, zoom: f64) -> bool {
        self.visible && zoom >= self.min_zoom && zoom < self.max_zoom
    }

    /// Parse one layer of a style document. Properties mapterm can't use are skipped; ones
    /// with unsupported expressions are dropped with a warning (falling back to defaults),
    /// and a filter it can't evaluate hides the layer, also with a warning.
    pub fn from_json(j: &Json) -> Result<(StyleLayer, Vec<String>)> {
        let id = j["id"].as_str().ok_or_else(|| anyhow!("layer without an id"))?;
        let kind = Kind::parse(j["type"].as_str().ok_or_else(|| anyhow!("{id}: layer without a type"))?);
        let mut l = StyleLayer::new(id, kind, j["source"].as_str().map(str::to_string));
        l.source_layer = j["source-layer"].as_str().map(str::to_string);
        l.min_zoom = j["minzoom"].as_f64().unwrap_or(0.0);
        l.max_zoom = j["maxzoom"].as_f64().unwrap_or(24.0);
        let mut warnings = Vec::new();
        if let Some(f) = j.get("filter") {
            match Expr::parse(f) {
                Ok(e) => l.filter = Some(e),
                Err(e) => {
                    warnings.push(format!("{id}: filter: {e}; layer hidden"));
                    l.visible = false;
                }
            }
        }
        for (section, used) in [("paint", &USED_PAINT[..]), ("layout", &USED_LAYOUT[..])] {
            for (k, v) in j[section].as_object().into_iter().flatten() {
                if !used.contains(&k.as_str()) {
                    continue;
                }
                let r = if section == "paint" { l.set_paint(k, v) } else { l.set_layout(k, v) };
                if let Err(e) = r {
                    warnings.push(format!("{id}: {k}: {e}"));
                }
            }
        }
        Ok((l, warnings))
    }
}

fn set(props: &mut Vec<(String, Expr)>, name: &str, v: &Json) -> Result<()> {
    let e = match (name, v) {
        ("text-field", _) => Expr::parse_text(v)?,
        // An array of font names is a literal, not an expression.
        ("text-font" | "text-variable-anchor", Json::Array(a)) if a.iter().all(Json::is_string) => Expr::Lit(crate::expr::Val::Array(
            a.iter().filter_map(Json::as_str).map(|s| crate::expr::Val::Str(s.to_string())).collect(),
        )),
        _ => Expr::parse(v)?,
    };
    if name.ends_with("-color") && let Expr::Lit(crate::expr::Val::Str(s)) = &e && crate::expr::parse_color(s).is_none() {
        bail!("not a color: {s}");
    }
    match props.iter_mut().find(|(k, _)| k == name) {
        Some(slot) => slot.1 = e,
        None => props.push((name.to_string(), e)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builders() {
        let l = StyleLayer::fill("parks", "osm").source_layer("park").color("#2a4").opacity(0.5).min_zoom(8.0);
        assert_eq!(l.kind, Kind::Fill);
        assert!(l.paint_expr("fill-color").is_some() && l.paint_expr("fill-opacity").is_some());
        assert!(!l.shown_at(7.0) && l.shown_at(8.0));
        let s = StyleLayer::symbol("t", "osm").text("{name}").glyph("◆").color("white").layout("visibility", "none");
        assert!(s.layout_expr("text-field").is_some() && s.paint_expr("text-color").is_some());
        assert!(!s.visible);
    }

    #[test]
    #[should_panic(expected = "not a color")]
    fn bad_color_panics_in_builder() {
        let _ = StyleLayer::line("x", "s").color("#zz");
    }

    #[test]
    fn from_json_skips_and_warns() {
        let (l, w) = StyleLayer::from_json(&json!({
            "id": "roads", "type": "line", "source": "openmaptiles", "source-layer": "transportation",
            "minzoom": 5, "filter": ["==", "class", "motorway"],
            "layout": {"line-cap": "round", "visibility": "visible"},
            "paint": {"line-color": "#e96", "line-width": {"stops": [[5, 1], [14, 4]]}, "line-opacity": ["distance", 1]}
        }))
        .unwrap();
        assert_eq!(l.source_layer.as_deref(), Some("transportation"));
        assert!(l.filter.is_some() && l.visible && l.min_zoom == 5.0);
        assert!(l.paint_expr("line-color").is_some());
        assert!(l.paint_expr("line-width").is_none(), "unused properties are skipped");
        assert_eq!(w.len(), 1, "{w:?}");
        let (hidden, w) = StyleLayer::from_json(&json!({"id": "x", "type": "fill", "source": "s", "filter": ["within", {}]})).unwrap();
        assert!(!hidden.visible && w[0].contains("hidden"));
    }
}
