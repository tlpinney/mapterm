//! What to draw for each feature, for the two common vector-tile schemas:
//! Protomaps basemap (layers earth/water/roads/places...) and OpenMapTiles (water/transportation/
//! place..., used by OpenFreeMap and MapTiler).

use ratatui::style::Color;

use crate::mvt::{Feature, GeomType, Tile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schema {
    Protomaps,
    OpenMapTiles,
}

impl Schema {
    pub fn detect(t: &Tile) -> Schema {
        if t.layers.iter().any(|l| l.name == "transportation" || l.name == "place") {
            Schema::OpenMapTiles
        } else {
            Schema::Protomaps
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// Road and line classes, smallest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Road {
    Path,
    Minor,
    Tertiary,
    Secondary,
    Primary,
    Trunk,
    Motorway,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Water,
    Park,
    Building,
    Waterway,
    Rail,
    Boundary,
    Road(Road),
    PlaceLabel,
    WaterLabel,
}

#[derive(Clone, Debug)]
pub enum Paint {
    Fill(Color),
    Line(Color),
    Label { text: String, color: Color, priority: i32, bold: bool },
}

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub theme: Theme,
}

impl Default for Style {
    fn default() -> Self {
        Self { theme: Theme::Dark }
    }
}

impl Style {
    pub fn color(&self, c: Class) -> Color {
        let dark = self.theme == Theme::Dark;
        let rgb = |d: (u8, u8, u8), l: (u8, u8, u8)| {
            let (r, g, b) = if dark { d } else { l };
            Color::Rgb(r, g, b)
        };
        match c {
            Class::Water => rgb((22, 42, 70), (168, 198, 228)),
            Class::Park => rgb((26, 50, 32), (202, 226, 192)),
            Class::Building => rgb((48, 48, 54), (218, 212, 206)),
            Class::Waterway | Class::WaterLabel => rgb((80, 130, 190), (50, 100, 170)),
            Class::Rail => rgb((115, 110, 140), (120, 110, 150)),
            Class::Boundary => rgb((150, 105, 170), (140, 90, 160)),
            Class::PlaceLabel => rgb((225, 225, 225), (25, 25, 25)),
            Class::Road(r) => match r {
                Road::Motorway => rgb((235, 110, 60), (205, 80, 40)),
                Road::Trunk => rgb((225, 160, 70), (195, 120, 30)),
                Road::Primary => rgb((210, 195, 110), (160, 130, 30)),
                Road::Secondary => rgb((170, 170, 170), (100, 100, 100)),
                Road::Tertiary => rgb((130, 130, 130), (130, 130, 130)),
                Road::Minor => rgb((88, 88, 88), (165, 165, 165)),
                Road::Path => rgb((70, 70, 70), (185, 185, 185)),
            },
        }
    }

    /// Paints for one feature, each with a draw order (fills low, roads by class, labels separate).
    pub fn paints(&self, schema: Schema, layer: &str, f: &Feature, z: u8) -> Vec<(u16, Paint)> {
        let mut out = Vec::new();
        let poly = f.geom_type == GeomType::Polygon;
        let line = f.geom_type == GeomType::Line;
        let point = f.geom_type == GeomType::Point;
        let push_class = |out: &mut Vec<(u16, Paint)>, c: Class| {
            let (order, paint) = match c {
                Class::Park => (10, Paint::Fill(self.color(c))),
                Class::Water => (20, Paint::Fill(self.color(c))),
                Class::Building => (30, Paint::Fill(self.color(c))),
                Class::Waterway => (40, Paint::Line(self.color(c))),
                Class::Boundary => (45, Paint::Line(self.color(c))),
                Class::Rail => (48, Paint::Line(self.color(c))),
                Class::Road(r) => (50 + r as u16, Paint::Line(self.color(c))),
                Class::PlaceLabel | Class::WaterLabel => return,
            };
            out.push((order, paint));
        };
        let road_label = |out: &mut Vec<(u16, Paint)>, r: Road, f: &Feature| {
            let reference = f.str("ref").filter(|s| !s.is_empty()).map(tidy_ref);
            let name = f.str("name").map(str::to_string);
            let text = if r >= Road::Primary { reference.or(name) } else { name.or(reference) };
            let min_z = match r {
                Road::Motorway | Road::Trunk => 0,
                Road::Primary => 11,
                Road::Secondary => 13,
                Road::Tertiary => 14,
                _ => 15,
            };
            if let Some(text) = text
                && z >= min_z
            {
                out.push((200, Paint::Label { text, color: self.color(Class::Road(r)), priority: 20 + r as i32 * 5, bold: r >= Road::Trunk }));
            }
        };
        match schema {
            Schema::Protomaps => match layer {
                "water" if poly => push_class(&mut out, Class::Water),
                "water" if line => push_class(&mut out, Class::Waterway),
                "water" | "physical_point" if point => water_label(self, f, &mut out),
                "landuse" | "landcover" if poly => {
                    if matches!(f.str("kind"), Some("park" | "forest" | "wood" | "nature_reserve" | "golf_course" | "cemetery" | "grass" | "meadow" | "garden" | "playground" | "protected_area" | "national_park" | "recreation_ground")) {
                        push_class(&mut out, Class::Park)
                    }
                }
                "buildings" if poly && z >= 15 => push_class(&mut out, Class::Building),
                "boundaries" if line => {
                    if matches!(f.str("kind"), Some("country" | "region")) {
                        push_class(&mut out, Class::Boundary)
                    }
                }
                "roads" if line => {
                    let r = match (f.str("kind"), f.str("kind_detail")) {
                        (Some("highway"), _) => Road::Motorway,
                        (Some("major_road"), Some("trunk")) => Road::Trunk,
                        (Some("major_road"), Some("primary")) => Road::Primary,
                        (Some("major_road"), _) => Road::Secondary,
                        (Some("medium_road"), _) => Road::Tertiary,
                        (Some("minor_road"), _) => Road::Minor,
                        (Some("path"), _) => Road::Path,
                        (Some("rail"), _) => {
                            push_class(&mut out, Class::Rail);
                            return out;
                        }
                        _ => Road::Minor,
                    };
                    if visible(r, z) {
                        push_class(&mut out, Class::Road(r));
                        road_label(&mut out, r, f);
                    }
                }
                "transit" if line && z >= 11 => push_class(&mut out, Class::Rail),
                "places" if point => {
                    if f.num("min_zoom").is_none_or(|m| m <= z as f64 + 0.5)
                        && let Some(name) = f.str("name")
                    {
                        let rank = f.num("population_rank").unwrap_or(0.0) as i32;
                        let kind = f.str("kind").unwrap_or("");
                        let base = match kind {
                            "country" => 90,
                            "region" => 70,
                            "locality" => 50,
                            _ => 30,
                        };
                        out.push((210, Paint::Label { text: name.into(), color: self.color(Class::PlaceLabel), priority: base + rank * 3, bold: kind == "locality" && rank >= 10 }));
                    }
                }
                _ => {}
            },
            Schema::OpenMapTiles => match layer {
                "water" if poly => push_class(&mut out, Class::Water),
                "waterway" if line && z >= 9 => push_class(&mut out, Class::Waterway),
                "water_name" if point => water_label(self, f, &mut out),
                "park" if poly => push_class(&mut out, Class::Park),
                "landcover" if poly => {
                    if matches!(f.str("class"), Some("wood" | "grass" | "forest")) {
                        push_class(&mut out, Class::Park)
                    }
                }
                "building" if poly && z >= 14 => push_class(&mut out, Class::Building),
                "boundary" if line => {
                    if f.num("admin_level").is_some_and(|l| l <= 4.0) && f.num("maritime").unwrap_or(0.0) == 0.0 {
                        push_class(&mut out, Class::Boundary)
                    }
                }
                "transportation" if line => {
                    let r = match f.str("class") {
                        Some("motorway") => Road::Motorway,
                        Some("trunk") => Road::Trunk,
                        Some("primary") => Road::Primary,
                        Some("secondary") => Road::Secondary,
                        Some("tertiary") => Road::Tertiary,
                        Some("minor" | "service" | "busway") => Road::Minor,
                        Some("path" | "track") => Road::Path,
                        Some("rail" | "transit") => {
                            if z >= 11 {
                                push_class(&mut out, Class::Rail);
                            }
                            return out;
                        }
                        _ => return out,
                    };
                    if visible(r, z) {
                        push_class(&mut out, Class::Road(r));
                    }
                }
                "transportation_name" if line => {
                    let r = match f.str("class") {
                        Some("motorway") => Road::Motorway,
                        Some("trunk") => Road::Trunk,
                        Some("primary") => Road::Primary,
                        Some("secondary") => Road::Secondary,
                        Some("tertiary") => Road::Tertiary,
                        _ => Road::Minor,
                    };
                    road_label(&mut out, r, f);
                }
                "place" if point => {
                    if let Some(name) = f.str("name") {
                        let class = f.str("class").unwrap_or("");
                        let rank = f.num("rank").unwrap_or(10.0) as i32;
                        let base = match class {
                            "continent" | "country" => 95,
                            "state" => 80,
                            "city" => 70,
                            "town" => 55,
                            "village" | "suburb" => 40,
                            _ => 25,
                        };
                        out.push((210, Paint::Label { text: name.into(), color: self.color(Class::PlaceLabel), priority: base - rank, bold: class == "city" }));
                    }
                }
                _ => {}
            },
        }
        out
    }
}

fn visible(r: Road, z: u8) -> bool {
    z >= match r {
        Road::Motorway | Road::Trunk => 0,
        Road::Primary => 7,
        Road::Secondary => 9,
        Road::Tertiary => 12,
        Road::Minor => 14,
        Road::Path => 16,
    }
}

fn water_label(s: &Style, f: &Feature, out: &mut Vec<(u16, Paint)>) {
    if let Some(name) = f.str("name") {
        out.push((205, Paint::Label { text: name.into(), color: s.color(Class::WaterLabel), priority: 35, bold: false }));
    }
}

/// "I 270;US 40" -> "I-270"
pub fn tidy_ref(r: &str) -> String {
    let first = r.split(';').next().unwrap_or(r).trim();
    match first.split_once(' ') {
        Some((pre @ ("I" | "US"), num)) => format!("{pre}-{num}"),
        _ => first.to_string(),
    }
}
