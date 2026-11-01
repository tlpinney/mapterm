//! MapLibre / Mapbox style expressions, the subset that makes sense in a terminal:
//! `get`, `has`, `zoom`, `geometry-type`, comparisons, `all`/`any`/`!`, `in`, `match`, `case`,
//! `coalesce`, `interpolate` (linear and exponential), `step`, string and number helpers, plus
//! the legacy filter syntax (`["==", "class", "park"]`, `["in", "class", ...]`, `["has", k]`)
//! and legacy zoom functions (`{"stops": [[5, 1], [10, 4]], "base": 1.2}`).
//!
//! Anything else is rejected with an error, so a style can fall back to the property's default
//! (or hide a layer whose filter it can't understand) instead of drawing the wrong thing.
//! Spec: <https://maplibre.org/maplibre-style-spec/expressions/>

use anyhow::{Result, anyhow, bail};
use serde_json::Value as Json;

use crate::mvt::{GeomType, Value};

/// An RGBA color, channels 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    /// Red.
    pub r: f64,
    /// Green.
    pub g: f64,
    /// Blue.
    pub b: f64,
    /// Alpha (opacity).
    pub a: f64,
}

impl Rgba {
    /// An opaque color from 8-bit channels.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r: r as f64 / 255.0, g: g as f64 / 255.0, b: b as f64 / 255.0, a: 1.0 }
    }

    /// The color as 8-bit RGB (alpha ignored).
    pub fn to_u8(self) -> (u8, u8, u8) {
        let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        (c(self.r), c(self.g), c(self.b))
    }

    /// Mix `self` over `under` with the given alpha (0 = `under`, 1 = `self`).
    pub fn over(self, under: Rgba, alpha: f64) -> Rgba {
        let a = alpha.clamp(0.0, 1.0);
        let m = |x: f64, y: f64| x * a + y * (1.0 - a);
        Rgba { r: m(self.r, under.r), g: m(self.g, under.g), b: m(self.b, under.b), a: 1.0 }
    }

    fn lerp(self, o: Rgba, t: f64) -> Rgba {
        let m = |x: f64, y: f64| x + (y - x) * t;
        Rgba { r: m(self.r, o.r), g: m(self.g, o.g), b: m(self.b, o.b), a: m(self.a, o.a) }
    }
}

/// A CSS color: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()`, `hsl()`, `hsla()`
/// or a common name.
pub fn parse_color(s: &str) -> Option<Rgba> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(hex) = s.strip_prefix('#') {
        let d: Vec<u8> = hex.chars().map(|c| c.to_digit(16).map(|v| v as u8)).collect::<Option<_>>()?;
        let (r, g, b, a) = match d.len() {
            3 => (d[0] * 17, d[1] * 17, d[2] * 17, 255),
            4 => (d[0] * 17, d[1] * 17, d[2] * 17, d[3] * 17),
            6 => (d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5], 255),
            8 => (d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5], d[6] * 16 + d[7]),
            _ => return None,
        };
        let mut c = Rgba::rgb(r, g, b);
        c.a = a as f64 / 255.0;
        return Some(c);
    }
    if let Some((func, rest)) = s.split_once('(') {
        let args: Vec<&str> = rest.trim_end_matches(')').split([',', ' ', '/']).filter(|a| !a.is_empty()).collect();
        let num = |a: &str| -> Option<f64> {
            match a.strip_suffix('%') {
                Some(p) => p.parse::<f64>().ok().map(|v| v / 100.0),
                None => a.trim_end_matches("deg").parse().ok(),
            }
        };
        let alpha = args.get(3).map_or(Some(1.0), |a| num(a))?;
        return match func.trim() {
            "rgb" | "rgba" if args.len() >= 3 => {
                let ch = |a: &str| -> Option<f64> {
                    if a.ends_with('%') { num(a) } else { num(a).map(|v| v / 255.0) }
                };
                Some(Rgba { r: ch(args[0])?, g: ch(args[1])?, b: ch(args[2])?, a: alpha })
            }
            "hsl" | "hsla" if args.len() >= 3 => {
                let (h, sat, l) = (num(args[0])?.rem_euclid(360.0) / 360.0, num(args[1])?, num(args[2])?);
                let q = if l < 0.5 { l * (1.0 + sat) } else { l + sat - l * sat };
                let p = 2.0 * l - q;
                let hue = |mut t: f64| {
                    t = t.rem_euclid(1.0);
                    if t < 1.0 / 6.0 {
                        p + (q - p) * 6.0 * t
                    } else if t < 0.5 {
                        q
                    } else if t < 2.0 / 3.0 {
                        p + (q - p) * (2.0 / 3.0 - t) * 6.0
                    } else {
                        p
                    }
                };
                Some(Rgba { r: hue(h + 1.0 / 3.0), g: hue(h), b: hue(h - 1.0 / 3.0), a: alpha })
            }
            _ => None,
        };
    }
    let (r, g, b) = match s.as_str() {
        "transparent" => return Some(Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "orange" => (255, 165, 0),
        "purple" => (128, 0, 128),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        "navy" => (0, 0, 128),
        "teal" => (0, 128, 128),
        "maroon" => (128, 0, 0),
        "olive" => (128, 128, 0),
        "brown" => (165, 42, 42),
        "pink" => (255, 192, 203),
        _ => return None,
    };
    Some(Rgba::rgb(r, g, b))
}

/// A value an expression produces.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    /// No value (a missing property, say).
    Null,
    /// true or false.
    Bool(bool),
    /// A number.
    Num(f64),
    /// Text.
    Str(String),
    /// A color.
    Color(Rgba),
    /// A list.
    Array(Vec<Val>),
}

impl Val {
    fn from_json(j: &Json) -> Val {
        match j {
            Json::Null => Val::Null,
            Json::Bool(b) => Val::Bool(*b),
            Json::Number(n) => n.as_f64().map_or(Val::Null, Val::Num),
            Json::String(s) => Val::Str(s.clone()),
            Json::Array(a) => Val::Array(a.iter().map(Val::from_json).collect()),
            Json::Object(_) => Val::Null,
        }
    }

    fn from_prop(v: &Value) -> Val {
        match v {
            Value::Str(s) => Val::Str(s.clone()),
            Value::Num(n) => Val::Num(*n),
            Value::Bool(b) => Val::Bool(*b),
        }
    }

    /// The number, if it is one.
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// Whether it is `true`.
    pub fn as_bool(&self) -> bool {
        matches!(self, Val::Bool(true))
    }

    /// Colors, and strings that parse as colors.
    pub fn as_color(&self) -> Option<Rgba> {
        match self {
            Val::Color(c) => Some(*c),
            Val::Str(s) => parse_color(s),
            _ => None,
        }
    }

    /// The value as display text (`to-string` rules).
    pub fn to_text(&self) -> String {
        match self {
            Val::Null => String::new(),
            Val::Bool(b) => b.to_string(),
            Val::Num(n) => Value::Num(*n).to_string(),
            Val::Str(s) => s.clone(),
            Val::Color(c) => {
                let (r, g, b) = c.to_u8();
                format!("rgba({r},{g},{b},{})", c.a)
            }
            Val::Array(a) => format!("[{}]", a.iter().map(Val::to_text).collect::<Vec<_>>().join(",")),
        }
    }

    fn truthy(&self) -> bool {
        !matches!(self, Val::Null | Val::Bool(false))
    }
}

/// What an expression is evaluated against: one feature at one zoom, with its feature state
/// (see [`crate::Map::set_feature_state`]).
pub struct Ctx<'a> {
    /// The zoom level.
    pub zoom: f64,
    /// The feature's geometry type.
    pub geom: GeomType,
    /// The feature's properties.
    pub props: &'a [(String, Value)],
    /// The feature's state (empty when none is set).
    pub state: &'a [(String, Val)],
}

impl Ctx<'_> {
    fn prop(&self, k: &str) -> Option<&Value> {
        self.props.iter().find(|(key, _)| key == k).map(|(_, v)| v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// A comparison operator.
pub enum Cmp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// An arithmetic operator.
pub enum Math {
    /// `+` (sum of all)
    Add,
    /// `-` (negation with one argument)
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `min`
    Min,
    /// `max`
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// The type `string`, `number` and `boolean` assert.
pub enum Assert {
    /// `string`
    Str,
    /// `number`
    Num,
    /// `boolean`
    Bool,
}

/// A compiled expression. Make one with [`Expr::parse`] (or [`Expr::parse_text`] for
/// `text-field`); each variant is the expression of the same name in the MapLibre spec.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// A constant (`["literal", ...]` or a plain value).
    Lit(Val),
    /// `["get", key]`: a property.
    Get(String),
    /// `["has", key]`: whether the property exists.
    Has(String),
    /// `["zoom"]`.
    Zoom,
    /// `["geometry-type"]`: `Point`, `LineString` or `Polygon`.
    GeomType,
    /// A comparison (`==`, `!=`, `<`, `<=`, `>`, `>=`).
    Cmp(Cmp, Box<Expr>, Box<Expr>),
    /// `["all", ...]`: every condition holds.
    All(Vec<Expr>),
    /// `["any", ...]`: some condition holds.
    Any(Vec<Expr>),
    /// `["!", x]` (or legacy `none`).
    Not(Box<Expr>),
    /// Needle in a list of values (legacy `in`, or modern `in` with a literal list).
    InList(Box<Expr>, Vec<Val>),
    /// Modern `in` with a computed haystack (string or array).
    In(Box<Expr>, Box<Expr>),
    /// `["match", input, labels, output, ..., default]`.
    Match {
        /// The value matched.
        input: Box<Expr>,
        /// Labels (one or a list) and the output for them.
        arms: Vec<(Vec<Val>, Expr)>,
        /// The output when no label matches.
        default: Box<Expr>,
    },
    /// `["case", condition, output, ..., default]`.
    Case {
        /// Conditions and their outputs, tried in order.
        arms: Vec<(Expr, Expr)>,
        /// The output when no condition holds.
        default: Box<Expr>,
    },
    /// `["coalesce", ...]`: the first value that isn't null.
    Coalesce(Vec<Expr>),
    /// `["interpolate", curve, input, stop, output, ...]` (numbers and colors).
    Interpolate {
        /// Exponential base; 1 is linear.
        base: f64,
        /// The value interpolated over (usually `["zoom"]`).
        input: Box<Expr>,
        /// Stop inputs and their outputs, ascending.
        stops: Vec<(f64, Expr)>,
    },
    /// `["step", input, default, stop, output, ...]`.
    Step {
        /// The value stepped over.
        input: Box<Expr>,
        /// The output below the first stop.
        default: Box<Expr>,
        /// Stop inputs and the outputs from them on, ascending.
        stops: Vec<(f64, Expr)>,
    },
    /// Arithmetic (`+`, `-`, `*`, `/`, `%`, `min`, `max`).
    Math(Math, Vec<Expr>),
    /// `["concat", ...]` (and `format`, as plain text).
    Concat(Vec<Expr>),
    /// `["upcase", x]`.
    Upcase(Box<Expr>),
    /// `["downcase", x]`.
    Downcase(Box<Expr>),
    /// `["to-string", x]`.
    ToStr(Box<Expr>),
    /// `["to-number", x, fallback...]`.
    ToNum(Vec<Expr>),
    /// `["to-boolean", x]`.
    ToBool(Box<Expr>),
    /// `["to-color", x, fallback...]`.
    ToColor(Vec<Expr>),
    /// `["length", x]`: characters of a string or items of an array.
    Length(Box<Expr>),
    /// `["typeof", x]`.
    TypeOf(Box<Expr>),
    /// `["index-of", keyword, input, from?]`: character position, or -1.
    IndexOf(Vec<Expr>),
    /// `["slice", input, start, end?]`: characters (or array items) from start to end.
    Slice(Vec<Expr>),
    /// `["string", ...]`, `["number", ...]`, `["boolean", ...]`: the first value of that type.
    Assert(Assert, Vec<Expr>),
    /// `["feature-state", key]`: a value the app set on the feature.
    FeatureState(String),
    /// A legacy `"{name}"` text template.
    Template(Vec<Part>),
}

/// A piece of a legacy text template.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// Literal text.
    Text(String),
    /// A property's value (`{name}`).
    Prop(String),
}

fn key_expr(k: &str) -> Expr {
    match k {
        "$type" => Expr::GeomType,
        _ => Expr::Get(k.to_string()),
    }
}

fn args(a: &[Json]) -> Result<Vec<Expr>> {
    a.iter().map(Expr::parse).collect()
}

fn one(a: &[Json], op: &str) -> Result<Box<Expr>> {
    match a {
        [x] => Ok(Box::new(Expr::parse(x)?)),
        _ => bail!("{op}: expected 1 argument"),
    }
}

/// `["literal", [..]]` or a bare array, as values.
fn literal_list(j: &Json) -> Option<Vec<Val>> {
    match j {
        Json::Array(a) if a.first().and_then(Json::as_str) == Some("literal") => {
            a.get(1).and_then(Json::as_array).map(|l| l.iter().map(Val::from_json).collect())
        }
        _ => None,
    }
}

fn stops(a: &[Json], op: &str) -> Result<Vec<(f64, Expr)>> {
    if !a.len().is_multiple_of(2) {
        bail!("{op}: stops must come in pairs");
    }
    let mut out = Vec::new();
    for pair in a.chunks(2) {
        let x = pair[0].as_f64().ok_or_else(|| anyhow!("{op}: stop input must be a number"))?;
        out.push((x, Expr::parse(&pair[1])?));
    }
    Ok(out)
}

impl Expr {
    /// A constant.
    pub fn lit(v: Val) -> Expr {
        Expr::Lit(v)
    }

    /// Compile a JSON value: an expression array, a legacy filter, a legacy function object or
    /// a literal.
    pub fn parse(j: &Json) -> Result<Expr> {
        match j {
            Json::Array(a) => match a.first() {
                Some(Json::String(op)) => Self::parse_op(op, &a[1..]),
                _ => Ok(Expr::Lit(Val::from_json(j))),
            },
            Json::Object(o) if o.contains_key("stops") => Self::parse_function(j),
            Json::Object(_) => bail!("objects are not expressions"),
            _ => Ok(Expr::Lit(Val::from_json(j))),
        }
    }

    /// Like [`Expr::parse`] for `text-field`, where plain strings may hold `{prop}` tokens.
    pub fn parse_text(j: &Json) -> Result<Expr> {
        match j.as_str() {
            Some(s) if s.contains('{') => {
                let mut parts = Vec::new();
                let mut rest = s;
                while let Some(open) = rest.find('{') {
                    let Some(close) = rest[open..].find('}') else { break };
                    if open > 0 {
                        parts.push(Part::Text(rest[..open].to_string()));
                    }
                    parts.push(Part::Prop(rest[open + 1..open + close].to_string()));
                    rest = &rest[open + close + 1..];
                }
                if !rest.is_empty() {
                    parts.push(Part::Text(rest.to_string()));
                }
                Ok(Expr::Template(parts))
            }
            _ => Self::parse(j),
        }
    }

    /// Legacy `{"stops": ..}` functions: zoom functions, property functions, categorical.
    fn parse_function(j: &Json) -> Result<Expr> {
        let input = match j.get("property").and_then(Json::as_str) {
            Some(p) => Box::new(key_expr(p)),
            None => Box::new(Expr::Zoom),
        };
        let raw = j["stops"].as_array().ok_or_else(|| anyhow!("stops must be an array"))?;
        let mut st = Vec::new();
        for s in raw {
            let (Some(k), Some(v)) = (s.get(0), s.get(1)) else { bail!("a stop must be [input, output]") };
            if k.is_object() {
                bail!("zoom-and-property functions are not supported");
            }
            st.push((k.clone(), Val::from_json(v)));
        }
        let kind = j.get("type").and_then(Json::as_str);
        let default = Box::new(Expr::Lit(j.get("default").map_or(Val::Null, Val::from_json)));
        let interpolable = st.first().is_some_and(|(_, v)| matches!(v, Val::Num(_)) || v.as_color().is_some());
        match kind {
            Some("categorical") => {
                Ok(Expr::Match { input, arms: st.into_iter().map(|(k, v)| (vec![Val::from_json(&k)], Expr::Lit(v))).collect(), default })
            }
            Some("identity") => Ok(*input),
            _ => {
                let nums: Vec<(f64, Expr)> = st
                    .into_iter()
                    .map(|(k, v)| k.as_f64().map(|k| (k, Expr::Lit(v))).ok_or_else(|| anyhow!("stop inputs must be numbers")))
                    .collect::<Result<_>>()?;
                if nums.is_empty() {
                    bail!("no stops");
                }
                if kind == Some("interval") || (kind.is_none() && !interpolable) {
                    let first = nums[0].1.clone();
                    Ok(Expr::Step { input, default: Box::new(first), stops: nums })
                } else {
                    let base = j.get("base").and_then(Json::as_f64).unwrap_or(1.0);
                    Ok(Expr::Interpolate { base, input, stops: nums })
                }
            }
        }
    }

    fn parse_op(op: &str, a: &[Json]) -> Result<Expr> {
        let legacy_key = a.first().and_then(Json::as_str);
        // Legacy filters name the property as a bare string and use only literal operands.
        let legacy = legacy_key.is_some() && a[1..].iter().all(|v| !v.is_array());
        let cmp = |c: Cmp| -> Result<Expr> {
            match a {
                [_, r] if legacy => Ok(Expr::Cmp(c, Box::new(key_expr(legacy_key.unwrap_or_default())), Box::new(Expr::Lit(Val::from_json(r))))),
                [l, r] => Ok(Expr::Cmp(c, Box::new(Expr::parse(l)?), Box::new(Expr::parse(r)?))),
                [l, r, _collator] => Ok(Expr::Cmp(c, Box::new(Expr::parse(l)?), Box::new(Expr::parse(r)?))),
                _ => bail!("{op}: expected 2 arguments"),
            }
        };
        Ok(match op {
            "literal" => Expr::Lit(a.first().map_or(Val::Null, Val::from_json)),
            "get" => match a {
                [Json::String(k)] => Expr::Get(k.clone()),
                _ => bail!("get: only [\"get\", \"name\"] is supported"),
            },
            "has" => match a {
                [Json::String(k)] => Expr::Has(k.clone()),
                _ => bail!("has: only [\"has\", \"name\"] is supported"),
            },
            "!has" => match a {
                [Json::String(k)] => Expr::Not(Box::new(Expr::Has(k.clone()))),
                _ => bail!("!has: expected a property name"),
            },
            "zoom" => Expr::Zoom,
            "feature-state" => match a {
                [Json::String(k)] => Expr::FeatureState(k.clone()),
                _ => bail!("feature-state: only [\"feature-state\", \"key\"] is supported"),
            },
            "geometry-type" => Expr::GeomType,
            "==" => cmp(Cmp::Eq)?,
            "!=" => cmp(Cmp::Ne)?,
            "<" => cmp(Cmp::Lt)?,
            "<=" => cmp(Cmp::Le)?,
            ">" => cmp(Cmp::Gt)?,
            ">=" => cmp(Cmp::Ge)?,
            "all" => Expr::All(args(a)?),
            "any" => Expr::Any(args(a)?),
            "none" => Expr::Not(Box::new(Expr::Any(args(a)?))),
            "!" => Expr::Not(one(a, op)?),
            "in" | "!in" => {
                let e = if legacy {
                    Expr::InList(Box::new(key_expr(legacy_key.unwrap_or_default())), a[1..].iter().map(Val::from_json).collect())
                } else {
                    match a {
                        [needle, hay] => match literal_list(hay) {
                            Some(list) => Expr::InList(Box::new(Expr::parse(needle)?), list),
                            None => Expr::In(Box::new(Expr::parse(needle)?), Box::new(Expr::parse(hay)?)),
                        },
                        _ => bail!("in: expected 2 arguments"),
                    }
                };
                if op == "!in" { Expr::Not(Box::new(e)) } else { e }
            }
            "match" => {
                if a.len() < 2 || !a.len().is_multiple_of(2) {
                    bail!("match: expected input, label/output pairs and a default");
                }
                let input = Box::new(Expr::parse(&a[0])?);
                let mut arms = Vec::new();
                for pair in a[1..a.len() - 1].chunks(2) {
                    let labels = match &pair[0] {
                        Json::Array(l) => l.iter().map(Val::from_json).collect(),
                        v => vec![Val::from_json(v)],
                    };
                    arms.push((labels, Expr::parse(&pair[1])?));
                }
                Expr::Match { input, arms, default: Box::new(Expr::parse(&a[a.len() - 1])?) }
            }
            "case" => {
                if a.len() % 2 != 1 {
                    bail!("case: expected condition/output pairs and a default");
                }
                let mut arms = Vec::new();
                for pair in a[..a.len() - 1].chunks(2) {
                    arms.push((Expr::parse(&pair[0])?, Expr::parse(&pair[1])?));
                }
                Expr::Case { arms, default: Box::new(Expr::parse(&a[a.len() - 1])?) }
            }
            "coalesce" => Expr::Coalesce(args(a)?),
            "interpolate" | "interpolate-hcl" | "interpolate-lab" => {
                if a.len() < 4 {
                    bail!("interpolate: expected a curve, an input and stops");
                }
                let base = match a[0].get(0).and_then(Json::as_str) {
                    Some("linear") => 1.0,
                    Some("exponential") => a[0].get(1).and_then(Json::as_f64).unwrap_or(1.0),
                    // Close enough in a terminal: a cubic bezier between the same stops.
                    Some("cubic-bezier") => 1.0,
                    _ => bail!("interpolate: unknown curve"),
                };
                Expr::Interpolate { base, input: Box::new(Expr::parse(&a[1])?), stops: stops(&a[2..], op)? }
            }
            "step" => {
                if a.len() < 2 {
                    bail!("step: expected an input and a default");
                }
                Expr::Step { input: Box::new(Expr::parse(&a[0])?), default: Box::new(Expr::parse(&a[1])?), stops: stops(&a[2..], op)? }
            }
            "+" => Expr::Math(Math::Add, args(a)?),
            "-" => Expr::Math(Math::Sub, args(a)?),
            "*" => Expr::Math(Math::Mul, args(a)?),
            "/" => Expr::Math(Math::Div, args(a)?),
            "%" => Expr::Math(Math::Rem, args(a)?),
            "min" => Expr::Math(Math::Min, args(a)?),
            "max" => Expr::Math(Math::Max, args(a)?),
            "concat" => Expr::Concat(args(a)?),
            // `format` sections become plain text; their style options don't apply here.
            "format" => Expr::Concat(a.iter().filter(|v| !v.is_object()).map(Expr::parse).collect::<Result<_>>()?),
            "upcase" => Expr::Upcase(one(a, op)?),
            "downcase" => Expr::Downcase(one(a, op)?),
            "to-string" => Expr::ToStr(one(a, op)?),
            "to-number" => Expr::ToNum(args(a)?),
            "to-boolean" => Expr::ToBool(one(a, op)?),
            "to-color" => Expr::ToColor(args(a)?),
            "length" => Expr::Length(one(a, op)?),
            "typeof" => Expr::TypeOf(one(a, op)?),
            "index-of" if (2..=3).contains(&a.len()) => Expr::IndexOf(args(a)?),
            "slice" if (2..=3).contains(&a.len()) => Expr::Slice(args(a)?),
            "string" => Expr::Assert(Assert::Str, args(a)?),
            "number" => Expr::Assert(Assert::Num, args(a)?),
            "boolean" => Expr::Assert(Assert::Bool, args(a)?),
            other => bail!("unsupported expression \"{other}\""),
        })
    }

    /// Whether the value is the same for every feature and zoom.
    pub fn is_constant(&self) -> bool {
        match self {
            Expr::Lit(_) => true,
            Expr::Get(_) | Expr::Has(_) | Expr::Zoom | Expr::GeomType | Expr::Template(_) | Expr::FeatureState(_) => false,
            Expr::Cmp(_, a, b) | Expr::In(a, b) => a.is_constant() && b.is_constant(),
            Expr::Not(a) | Expr::Upcase(a) | Expr::Downcase(a) | Expr::ToStr(a) | Expr::ToBool(a) | Expr::Length(a) | Expr::TypeOf(a) => a.is_constant(),
            Expr::InList(a, _) => a.is_constant(),
            Expr::All(v) | Expr::Any(v) | Expr::Coalesce(v) | Expr::Math(_, v) | Expr::Concat(v) | Expr::ToNum(v) | Expr::ToColor(v) | Expr::Assert(_, v) | Expr::IndexOf(v) | Expr::Slice(v) => {
                v.iter().all(Expr::is_constant)
            }
            Expr::Match { input, arms, default } => input.is_constant() && default.is_constant() && arms.iter().all(|(_, e)| e.is_constant()),
            Expr::Case { arms, default } => default.is_constant() && arms.iter().all(|(c, e)| c.is_constant() && e.is_constant()),
            Expr::Interpolate { input, stops, .. } => input.is_constant() && stops.iter().all(|(_, e)| e.is_constant()),
            Expr::Step { input, default, stops } => input.is_constant() && default.is_constant() && stops.iter().all(|(_, e)| e.is_constant()),
        }
    }

    /// Whether the value can change with the zoom level.
    pub fn uses_zoom(&self) -> bool {
        self.any(&|e| matches!(e, Expr::Zoom))
    }

    /// Whether the value reads feature state.
    pub fn uses_state(&self) -> bool {
        self.any(&|e| matches!(e, Expr::FeatureState(_)))
    }

    /// Whether this expression or any inside it matches.
    fn any(&self, f: &dyn Fn(&Expr) -> bool) -> bool {
        if f(self) {
            return true;
        }
        match self {
            Expr::Lit(_) | Expr::Get(_) | Expr::Has(_) | Expr::Zoom | Expr::GeomType | Expr::Template(_) | Expr::FeatureState(_) => false,
            Expr::Cmp(_, a, b) | Expr::In(a, b) => a.any(f) || b.any(f),
            Expr::Not(a) | Expr::Upcase(a) | Expr::Downcase(a) | Expr::ToStr(a) | Expr::ToBool(a) | Expr::Length(a) | Expr::TypeOf(a) | Expr::InList(a, _) => a.any(f),
            Expr::All(v) | Expr::Any(v) | Expr::Coalesce(v) | Expr::Math(_, v) | Expr::Concat(v) | Expr::ToNum(v) | Expr::ToColor(v) | Expr::Assert(_, v) | Expr::IndexOf(v) | Expr::Slice(v) => {
                v.iter().any(|e| e.any(f))
            }
            Expr::Match { input, arms, default } => input.any(f) || default.any(f) || arms.iter().any(|(_, e)| e.any(f)),
            Expr::Case { arms, default } => default.any(f) || arms.iter().any(|(c, e)| c.any(f) || e.any(f)),
            Expr::Interpolate { input, stops, .. } => input.any(f) || stops.iter().any(|(_, e)| e.any(f)),
            Expr::Step { input, default, stops } => input.any(f) || default.any(f) || stops.iter().any(|(_, e)| e.any(f)),
        }
    }

    /// Evaluate for one feature at one zoom.
    pub fn eval(&self, c: &Ctx) -> Val {
        match self {
            Expr::Lit(v) => v.clone(),
            Expr::Get(k) => c.prop(k).map_or(Val::Null, Val::from_prop),
            Expr::Has(k) => Val::Bool(c.prop(k).is_some()),
            Expr::FeatureState(k) => c.state.iter().find(|(key, _)| key == k).map_or(Val::Null, |(_, v)| v.clone()),
            Expr::Zoom => Val::Num(c.zoom),
            Expr::GeomType => Val::Str(
                match c.geom {
                    GeomType::Point => "Point",
                    GeomType::Line => "LineString",
                    GeomType::Polygon => "Polygon",
                    GeomType::Unknown => "Unknown",
                }
                .into(),
            ),
            Expr::Cmp(op, l, r) => Val::Bool(compare(*op, &l.eval(c), &r.eval(c))),
            Expr::All(v) => Val::Bool(v.iter().all(|e| e.eval(c).as_bool())),
            Expr::Any(v) => Val::Bool(v.iter().any(|e| e.eval(c).as_bool())),
            Expr::Not(e) => Val::Bool(!e.eval(c).as_bool()),
            Expr::InList(needle, list) => {
                let n = needle.eval(c);
                Val::Bool(list.iter().any(|v| loose_eq(v, &n)))
            }
            Expr::In(needle, hay) => {
                let n = needle.eval(c);
                Val::Bool(match hay.eval(c) {
                    Val::Array(a) => a.iter().any(|v| loose_eq(v, &n)),
                    Val::Str(s) => s.contains(&n.to_text()),
                    _ => false,
                })
            }
            Expr::Match { input, arms, default } => {
                let v = input.eval(c);
                arms.iter().find(|(labels, _)| labels.iter().any(|l| loose_eq(l, &v))).map_or_else(|| default.eval(c), |(_, out)| out.eval(c))
            }
            Expr::Case { arms, default } => arms.iter().find(|(cond, _)| cond.eval(c).as_bool()).map_or_else(|| default.eval(c), |(_, out)| out.eval(c)),
            Expr::Coalesce(v) => v.iter().map(|e| e.eval(c)).find(Val::truthy).unwrap_or(Val::Null),
            Expr::Interpolate { base, input, stops } => {
                let Some(x) = input.eval(c).as_num() else { return Val::Null };
                interpolate(*base, x, stops, c)
            }
            Expr::Step { input, default, stops } => {
                let Some(x) = input.eval(c).as_num() else { return default.eval(c) };
                stops.iter().rev().find(|(s, _)| x >= *s).map_or_else(|| default.eval(c), |(_, e)| e.eval(c))
            }
            Expr::Math(op, v) => {
                let n: Vec<f64> = v.iter().filter_map(|e| e.eval(c).as_num()).collect();
                if n.len() != v.len() || n.is_empty() {
                    return Val::Null;
                }
                Val::Num(match op {
                    Math::Add => n.iter().sum(),
                    Math::Mul => n.iter().product(),
                    Math::Sub if n.len() == 1 => -n[0],
                    Math::Sub => n[0] - n[1..].iter().sum::<f64>(),
                    Math::Div => n[1..].iter().fold(n[0], |a, b| a / b),
                    Math::Rem => n[1..].iter().fold(n[0], |a, b| a % b),
                    Math::Min => n.iter().copied().fold(f64::MAX, f64::min),
                    Math::Max => n.iter().copied().fold(f64::MIN, f64::max),
                })
            }
            Expr::Concat(v) => Val::Str(v.iter().map(|e| e.eval(c).to_text()).collect()),
            Expr::Upcase(e) => Val::Str(e.eval(c).to_text().to_uppercase()),
            Expr::Downcase(e) => Val::Str(e.eval(c).to_text().to_lowercase()),
            Expr::ToStr(e) => Val::Str(e.eval(c).to_text()),
            Expr::ToNum(v) => v
                .iter()
                .find_map(|e| match e.eval(c) {
                    Val::Num(n) => Some(n),
                    Val::Str(s) => s.trim().parse().ok(),
                    Val::Bool(b) => Some(b as u8 as f64),
                    _ => None,
                })
                .map_or(Val::Null, Val::Num),
            Expr::ToBool(e) => Val::Bool(match e.eval(c) {
                Val::Num(n) => n != 0.0 && !n.is_nan(),
                Val::Str(s) => !s.is_empty(),
                v => v.truthy(),
            }),
            Expr::ToColor(v) => v.iter().find_map(|e| e.eval(c).as_color()).map_or(Val::Null, Val::Color),
            Expr::Length(e) => match e.eval(c) {
                Val::Str(s) => Val::Num(s.chars().count() as f64),
                Val::Array(a) => Val::Num(a.len() as f64),
                _ => Val::Null,
            },
            Expr::TypeOf(e) => Val::Str(
                match e.eval(c) {
                    Val::Null => "null",
                    Val::Bool(_) => "boolean",
                    Val::Num(_) => "number",
                    Val::Str(_) => "string",
                    Val::Color(_) => "color",
                    Val::Array(_) => "array",
                }
                .into(),
            ),
            Expr::IndexOf(v) => {
                let (needle, hay) = (v[0].eval(c), v[1].eval(c));
                let from = v.get(2).and_then(|e| e.eval(c).as_num()).unwrap_or(0.0).max(0.0) as usize;
                Val::Num(match hay {
                    Val::Str(h) => {
                        let (h, n): (Vec<char>, Vec<char>) = (h.chars().collect(), needle.to_text().chars().collect());
                        (from..=h.len().saturating_sub(n.len())).find(|&i| h.len() >= n.len() && h[i..i + n.len()] == n[..]).map_or(-1.0, |i| i as f64)
                    }
                    Val::Array(a) => a.iter().skip(from).position(|x| loose_eq(x, &needle)).map_or(-1.0, |i| (i + from) as f64),
                    _ => -1.0,
                })
            }
            Expr::Slice(v) => {
                let input = v[0].eval(c);
                let len = match &input {
                    Val::Str(s) => s.chars().count(),
                    Val::Array(a) => a.len(),
                    _ => return Val::Null,
                } as f64;
                // Negative positions count from the end, as in JavaScript.
                let pos = |e: Option<&Expr>, default: f64| {
                    let p = e.and_then(|e| e.eval(c).as_num()).unwrap_or(default);
                    (if p < 0.0 { len + p } else { p }).clamp(0.0, len) as usize
                };
                let (start, end) = (pos(v.get(1), 0.0), pos(v.get(2), len));
                let end = end.max(start);
                match input {
                    Val::Str(s) => Val::Str(s.chars().skip(start).take(end - start).collect()),
                    Val::Array(a) => Val::Array(a[start..end].to_vec()),
                    _ => Val::Null,
                }
            }
            Expr::Assert(kind, v) => v
                .iter()
                .map(|e| e.eval(c))
                .find(|x| matches!((kind, x), (Assert::Str, Val::Str(_)) | (Assert::Num, Val::Num(_)) | (Assert::Bool, Val::Bool(_))))
                .unwrap_or(Val::Null),
            Expr::Template(parts) => Val::Str(
                parts
                    .iter()
                    .map(|p| match p {
                        Part::Text(t) => t.clone(),
                        Part::Prop(k) => c.prop(k).map(|v| v.to_string()).unwrap_or_default(),
                    })
                    .collect(),
            ),
        }
    }
}

fn loose_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => x == y,
        _ => a == b,
    }
}

fn compare(op: Cmp, l: &Val, r: &Val) -> bool {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (l, r) {
        (Val::Num(a), Val::Num(b)) => a.partial_cmp(b),
        (Val::Str(a), Val::Str(b)) => Some(a.cmp(b)),
        (Val::Bool(a), Val::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    };
    match op {
        Cmp::Eq => ord == Some(Ordering::Equal) || (l == &Val::Null && r == &Val::Null),
        Cmp::Ne => !(ord == Some(Ordering::Equal) || (l == &Val::Null && r == &Val::Null)),
        Cmp::Lt => ord == Some(Ordering::Less),
        Cmp::Le => matches!(ord, Some(Ordering::Less | Ordering::Equal)),
        Cmp::Gt => ord == Some(Ordering::Greater),
        Cmp::Ge => matches!(ord, Some(Ordering::Greater | Ordering::Equal)),
    }
}

fn interpolate(base: f64, x: f64, stops: &[(f64, Expr)], c: &Ctx) -> Val {
    let Some(first) = stops.first() else { return Val::Null };
    if x <= first.0 {
        return first.1.eval(c);
    }
    let Some(i) = stops.iter().position(|(s, _)| *s > x) else {
        return stops[stops.len() - 1].1.eval(c);
    };
    let ((x0, lo), (x1, hi)) = (&stops[i - 1], &stops[i]);
    let span = x1 - x0;
    let t = if span <= 0.0 {
        0.0
    } else if (base - 1.0).abs() < 1e-9 {
        (x - x0) / span
    } else {
        (base.powf(x - x0) - 1.0) / (base.powf(span) - 1.0)
    };
    match (lo.eval(c), hi.eval(c)) {
        (Val::Num(a), Val::Num(b)) => Val::Num(a + (b - a) * t),
        (a, b) => match (a.as_color(), b.as_color()) {
            (Some(ca), Some(cb)) => Val::Color(ca.lerp(cb, t)),
            _ => a,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn props(p: &[(&str, Value)]) -> Vec<(String, Value)> {
        p.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn eval(e: Json, z: f64, geom: GeomType, p: &[(String, Value)]) -> Val {
        Expr::parse(&e).unwrap().eval(&Ctx { zoom: z, geom, props: p, state: &[] })
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff").unwrap().to_u8(), (255, 255, 255));
        assert_eq!(parse_color("#336699").unwrap().to_u8(), (0x33, 0x66, 0x99));
        assert!((parse_color("#33669980").unwrap().a - 0.502).abs() < 0.01);
        assert_eq!(parse_color("rgb(10, 20, 30)").unwrap().to_u8(), (10, 20, 30));
        assert_eq!(parse_color("rgba(10,20,30,0.5)").unwrap().a, 0.5);
        assert_eq!(parse_color("hsl(0, 100%, 50%)").unwrap().to_u8(), (255, 0, 0));
        assert_eq!(parse_color("hsla(120,100%,25%,1)").unwrap().to_u8(), (0, 128, 0));
        assert_eq!(parse_color("Navy").unwrap().to_u8(), (0, 0, 128));
        assert!(parse_color("nonsense").is_none());
    }

    #[test]
    fn legacy_filters() {
        let p = props(&[("class", Value::Str("motorway".into())), ("admin_level", Value::Num(2.0))]);
        let t = |e: Json| eval(e, 10.0, GeomType::Line, &p).as_bool();
        assert!(t(json!(["==", "class", "motorway"])));
        assert!(t(json!(["!=", "class", "primary"])));
        assert!(t(json!(["!=", "missing", "x"])), "a missing property is not equal");
        assert!(!t(json!(["==", "missing", "x"])));
        assert!(t(json!(["in", "class", "trunk", "motorway"])));
        assert!(t(json!(["!in", "class", "path", "track"])));
        assert!(t(json!(["<=", "admin_level", 4])));
        assert!(t(json!(["has", "class"])));
        assert!(t(json!(["!has", "name"])));
        assert!(t(json!(["==", "$type", "LineString"])));
        assert!(t(json!(["all", ["==", "class", "motorway"], ["none", ["==", "admin_level", 4]]])));
    }

    #[test]
    fn modern_expressions() {
        let p = props(&[("class", Value::Str("park".into())), ("rank", Value::Num(3.0)), ("name", Value::Str("Mall".into()))]);
        let e = |j: Json| eval(j, 12.0, GeomType::Polygon, &p);
        assert!(e(json!(["==", ["get", "class"], "park"])).as_bool());
        assert!(e(json!(["in", ["get", "class"], ["literal", ["park", "wood"]]])).as_bool());
        assert!(e(json!(["in", "ar", ["get", "class"]])).as_bool(), "substring");
        assert_eq!(e(json!(["match", ["get", "class"], ["wood", "park"], "green", "gray"])), Val::Str("green".into()));
        assert_eq!(e(json!(["case", [">", ["get", "rank"], 5], "big", "small"])), Val::Str("small".into()));
        assert_eq!(e(json!(["coalesce", ["get", "name:en"], ["get", "name"]])), Val::Str("Mall".into()));
        assert_eq!(e(json!(["step", ["zoom"], 1, 10, 2, 14, 3])), Val::Num(2.0));
        assert_eq!(e(json!(["interpolate", ["linear"], ["zoom"], 10, 0, 14, 8])), Val::Num(4.0));
        assert_eq!(e(json!(["+", ["get", "rank"], 2])), Val::Num(5.0));
        assert_eq!(e(json!(["concat", ["upcase", ["get", "name"]], "!"])), Val::Str("MALL!".into()));
        assert_eq!(e(json!(["format", ["get", "name"], {"font-scale": 1.2}])), Val::Str("Mall".into()));
        assert_eq!(e(json!(["to-number", "x", "7"])), Val::Num(7.0));
        assert_eq!(e(json!(["typeof", ["get", "rank"]])), Val::Str("number".into()));
        assert_eq!(e(json!(["index-of", ";", "I 270;US 40"])), Val::Num(5.0));
        assert_eq!(e(json!(["index-of", "x", "abc"])), Val::Num(-1.0));
        assert_eq!(e(json!(["index-of", "a", "banana", 2])), Val::Num(3.0));
        assert_eq!(e(json!(["slice", "I 270;US 40", 0, 5])), Val::Str("I 270".into()));
        assert_eq!(e(json!(["slice", "I 270", 2])), Val::Str("270".into()));
        assert_eq!(e(json!(["slice", "abc", -2])), Val::Str("bc".into()));
        assert_eq!(e(json!(["slice", ["literal", [1, 2, 3]], 1, 2])), Val::Array(vec![Val::Num(2.0)]));
        let c = e(json!(["interpolate", ["linear"], ["zoom"], 10, "#000000", 14, "#ffffff"])).as_color().unwrap();
        assert_eq!(c.to_u8(), (128, 128, 128));
        assert!(Expr::parse(&json!(["distance", 1])).is_err());
        assert!(Expr::parse(&json!(["interpolate", ["linear"], ["zoom"], 1, 2])).is_ok());
    }

    #[test]
    fn exponential_and_functions() {
        let p = props(&[("kind", Value::Str("a".into()))]);
        let e = |j: Json, z: f64| eval(j, z, GeomType::Point, &p);
        // Base 2 between 10 and 12 at 11: (2^1 - 1) / (2^2 - 1) = 1/3 of the way.
        assert!((e(json!(["interpolate", ["exponential", 2], ["zoom"], 10, 0, 12, 3]), 11.0).as_num().unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(e(json!({"stops": [[5, 1], [10, 6]]}), 7.0), Val::Num(3.0));
        assert_eq!(e(json!({"stops": [[5, "none"], [10, "visible"]]}), 11.0), Val::Str("visible".into()));
        assert_eq!(e(json!({"property": "kind", "type": "categorical", "stops": [["a", 1], ["b", 2]]}), 0.0), Val::Num(1.0));
        let c = e(json!({"base": 1, "stops": [[0, "#000"], [10, "#fff"]]}), 5.0).as_color().unwrap();
        assert_eq!(c.to_u8(), (128, 128, 128));
    }

    #[test]
    fn text_templates() {
        let p = props(&[("name", Value::Str("Georgetown".into())), ("ref", Value::Num(7.0))]);
        let e = Expr::parse_text(&json!("{name} ({ref}) {missing}")).unwrap();
        assert_eq!(e.eval(&Ctx { zoom: 0.0, geom: GeomType::Point, props: &p, state: &[] }), Val::Str("Georgetown (7) ".into()));
        assert_eq!(Expr::parse_text(&json!("plain")).unwrap(), Expr::Lit(Val::Str("plain".into())));
        assert!(!Expr::parse(&json!(["get", "name"])).unwrap().is_constant());
        assert!(Expr::parse(&json!(["concat", "a", "b"])).unwrap().is_constant());
        let st = vec![("hover".to_string(), Val::Bool(true))];
        let fs = Expr::parse(&json!(["case", ["boolean", ["feature-state", "hover"], false], "on", "off"])).unwrap();
        assert!(fs.uses_state() && !fs.is_constant());
        assert_eq!(fs.eval(&Ctx { zoom: 0.0, geom: GeomType::Point, props: &p, state: &st }), Val::Str("on".into()));
        assert_eq!(fs.eval(&Ctx { zoom: 0.0, geom: GeomType::Point, props: &p, state: &[] }), Val::Str("off".into()));
        assert!(Expr::parse(&json!(["interpolate", ["linear"], ["zoom"], 5, 1, 10, 2])).unwrap().uses_zoom());
        assert!(Expr::parse(&json!({"stops": [[5, "#000"], [10, "#fff"]]})).unwrap().uses_zoom());
        assert!(Expr::parse(&json!(["all", ["==", "class", "x"], [">=", ["zoom"], 14]])).unwrap().uses_zoom());
        assert!(!Expr::parse(&json!(["match", ["get", "class"], "a", "#fff", "#000"])).unwrap().uses_zoom());
    }
}
