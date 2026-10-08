//! Specified and computed CSS value types, and their parsers.

use cssparser::{ParseError, Parser, Token};
use std::sync::Arc;

pub type PResult<T> = Result<T, ParseError<ValueError>>;

/// Why a value failed to parse, when it is more specific than "invalid value".
#[derive(Clone, Debug, PartialEq)]
pub enum ValueError {
    UnsupportedUnit(String),
    Url,
}

/// A specified length, percentage or calc() expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f32),
    Px(f32),
    Em(f32),
    Rem(f32),
    Vw(f32),
    Vh(f32),
    Pct(f32),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Min(Vec<Expr>),
    Max(Vec<Expr>),
    Clamp(Box<Expr>, Box<Expr>, Box<Expr>),
}

/// What specified lengths are computed against.
#[derive(Clone, Copy, Debug)]
pub struct LengthContext {
    pub font_size: f32,
    pub root_font_size: f32,
    pub vw: f32,
    pub vh: f32,
    /// Effective zoom: px (and rem) lengths are multiplied by it.
    pub zoom: f32,
}

/// A computed calc() tree: only px, percentages and numbers remain.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcNode {
    Num(f32),
    Px(f32),
    Pct(f32),
    Add(Box<CalcNode>, Box<CalcNode>),
    Sub(Box<CalcNode>, Box<CalcNode>),
    Mul(Box<CalcNode>, Box<CalcNode>),
    Div(Box<CalcNode>, Box<CalcNode>),
    Min(Vec<CalcNode>),
    Max(Vec<CalcNode>),
    Clamp(Box<CalcNode>, Box<CalcNode>, Box<CalcNode>),
}

impl CalcNode {
    pub fn resolve(&self, basis: f32) -> f32 {
        match self {
            CalcNode::Num(n) | CalcNode::Px(n) => *n,
            CalcNode::Pct(p) => p / 100.0 * basis,
            CalcNode::Add(a, b) => a.resolve(basis) + b.resolve(basis),
            CalcNode::Sub(a, b) => a.resolve(basis) - b.resolve(basis),
            CalcNode::Mul(a, b) => a.resolve(basis) * b.resolve(basis),
            CalcNode::Div(a, b) => {
                let d = b.resolve(basis);
                if d == 0.0 { 0.0 } else { a.resolve(basis) / d }
            }
            CalcNode::Min(v) => v
                .iter()
                .map(|e| e.resolve(basis))
                .fold(f32::INFINITY, f32::min),
            CalcNode::Max(v) => v
                .iter()
                .map(|e| e.resolve(basis))
                .fold(f32::NEG_INFINITY, f32::max),
            CalcNode::Clamp(lo, v, hi) => {
                let (lo, v, hi) = (lo.resolve(basis), v.resolve(basis), hi.resolve(basis));
                v.min(hi).max(lo)
            }
        }
    }
    fn has_pct(&self) -> bool {
        match self {
            CalcNode::Pct(_) => true,
            CalcNode::Num(_) | CalcNode::Px(_) => false,
            CalcNode::Add(a, b)
            | CalcNode::Sub(a, b)
            | CalcNode::Mul(a, b)
            | CalcNode::Div(a, b) => a.has_pct() || b.has_pct(),
            CalcNode::Min(v) | CalcNode::Max(v) => v.iter().any(|e| e.has_pct()),
            CalcNode::Clamp(a, b, c) => a.has_pct() || b.has_pct() || c.has_pct(),
        }
    }
}

/// A computed length-percentage.
#[derive(Clone, Debug, PartialEq)]
pub enum LengthPct {
    Px(f32),
    Pct(f32),
    Calc(Arc<CalcNode>),
}

impl Default for LengthPct {
    fn default() -> Self {
        LengthPct::Px(0.0)
    }
}

impl LengthPct {
    pub const ZERO: LengthPct = LengthPct::Px(0.0);
    pub fn resolve(&self, basis: f32) -> f32 {
        match self {
            LengthPct::Px(v) => *v,
            LengthPct::Pct(p) => p / 100.0 * basis,
            LengthPct::Calc(c) => c.resolve(basis),
        }
    }
    /// Resolve against an optional basis; percentages against an unknown basis are `None`.
    pub fn maybe_resolve(&self, basis: Option<f32>) -> Option<f32> {
        match (self, basis) {
            (LengthPct::Px(v), _) => Some(*v),
            (_, Some(b)) => Some(self.resolve(b)),
            _ => None,
        }
    }
    pub fn is_zero(&self) -> bool {
        matches!(self, LengthPct::Px(v) if *v == 0.0)
            || matches!(self, LengthPct::Pct(v) if *v == 0.0)
    }
    pub fn px(&self) -> Option<f32> {
        match self {
            LengthPct::Px(v) => Some(*v),
            _ => None,
        }
    }
}

impl Expr {
    pub fn is_zero_literal(&self) -> bool {
        matches!(self, Expr::Num(n) if *n == 0.0)
    }

    fn to_calc(&self, cx: &LengthContext) -> CalcNode {
        match self {
            Expr::Num(n) => CalcNode::Num(*n),
            Expr::Px(v) => CalcNode::Px(v * cx.zoom),
            Expr::Em(v) => CalcNode::Px(v * cx.font_size),
            Expr::Rem(v) => CalcNode::Px(v * cx.root_font_size * cx.zoom),
            Expr::Vw(v) => CalcNode::Px(v * cx.vw / 100.0),
            Expr::Vh(v) => CalcNode::Px(v * cx.vh / 100.0),
            Expr::Pct(v) => CalcNode::Pct(*v),
            Expr::Add(a, b) => fold(CalcNode::Add(
                Box::new(a.to_calc(cx)),
                Box::new(b.to_calc(cx)),
            )),
            Expr::Sub(a, b) => fold(CalcNode::Sub(
                Box::new(a.to_calc(cx)),
                Box::new(b.to_calc(cx)),
            )),
            Expr::Mul(a, b) => fold(CalcNode::Mul(
                Box::new(a.to_calc(cx)),
                Box::new(b.to_calc(cx)),
            )),
            Expr::Div(a, b) => fold(CalcNode::Div(
                Box::new(a.to_calc(cx)),
                Box::new(b.to_calc(cx)),
            )),
            Expr::Min(v) => fold(CalcNode::Min(v.iter().map(|e| e.to_calc(cx)).collect())),
            Expr::Max(v) => fold(CalcNode::Max(v.iter().map(|e| e.to_calc(cx)).collect())),
            Expr::Clamp(a, b, c) => fold(CalcNode::Clamp(
                Box::new(a.to_calc(cx)),
                Box::new(b.to_calc(cx)),
                Box::new(c.to_calc(cx)),
            )),
        }
    }

    /// Compute to a length-percentage.
    pub fn compute(&self, cx: &LengthContext) -> LengthPct {
        match self.to_calc(cx) {
            CalcNode::Px(v) | CalcNode::Num(v) => LengthPct::Px(v),
            CalcNode::Pct(p) => LengthPct::Pct(p),
            c if !c.has_pct() => LengthPct::Px(c.resolve(0.0)),
            c => LengthPct::Calc(Arc::new(c)),
        }
    }

    /// Compute to absolute px, with percentages resolved against `basis`.
    pub fn compute_px(&self, cx: &LengthContext, basis: f32) -> f32 {
        self.to_calc(cx).resolve(basis)
    }

    pub fn has_pct(&self) -> bool {
        match self {
            Expr::Pct(_) => true,
            Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) => {
                a.has_pct() || b.has_pct()
            }
            Expr::Min(v) | Expr::Max(v) => v.iter().any(|e| e.has_pct()),
            Expr::Clamp(a, b, c) => a.has_pct() || b.has_pct() || c.has_pct(),
            _ => false,
        }
    }
    pub fn is_unitless_number(&self) -> Option<f32> {
        match self {
            Expr::Num(n) => Some(*n),
            _ => None,
        }
    }
}

fn fold(c: CalcNode) -> CalcNode {
    use CalcNode::*;
    match c {
        Add(a, b) => match (*a, *b) {
            (Px(x), Px(y)) => Px(x + y),
            (Num(x), Num(y)) => Num(x + y),
            (Pct(x), Pct(y)) => Pct(x + y),
            (Px(x), Num(y)) | (Num(x), Px(y)) if x == 0.0 || y == 0.0 => Px(x + y),
            (a, b) => Add(Box::new(a), Box::new(b)),
        },
        Sub(a, b) => match (*a, *b) {
            (Px(x), Px(y)) => Px(x - y),
            (Num(x), Num(y)) => Num(x - y),
            (Pct(x), Pct(y)) => Pct(x - y),
            (a, b) => Sub(Box::new(a), Box::new(b)),
        },
        Mul(a, b) => match (*a, *b) {
            (Num(x), Num(y)) => Num(x * y),
            (Px(x), Num(y)) | (Num(y), Px(x)) => Px(x * y),
            (Pct(x), Num(y)) | (Num(y), Pct(x)) => Pct(x * y),
            (a, b) => Mul(Box::new(a), Box::new(b)),
        },
        Div(a, b) => match (*a, *b) {
            (Num(x), Num(y)) if y != 0.0 => Num(x / y),
            (Px(x), Num(y)) if y != 0.0 => Px(x / y),
            (Pct(x), Num(y)) if y != 0.0 => Pct(x / y),
            (a, b) => Div(Box::new(a), Box::new(b)),
        },
        Min(v) if v.iter().all(|e| matches!(e, Px(_))) => Px(v
            .iter()
            .map(|e| e.resolve(0.0))
            .fold(f32::INFINITY, f32::min)),
        Max(v) if v.iter().all(|e| matches!(e, Px(_))) => Px(v
            .iter()
            .map(|e| e.resolve(0.0))
            .fold(f32::NEG_INFINITY, f32::max)),
        Clamp(a, b, c) if matches!((&*a, &*b, &*c), (Px(_), Px(_), Px(_))) => {
            Px(Clamp(a, b, c).resolve(0.0))
        }
        other => other,
    }
}

/// Whether the parser accepts Tern's restricted unit set (program and plugin sheets) or the
/// wider set our own sheets use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitMode {
    /// px, em, rem, vw, vh, % (built-in sheets and inline styles we write).
    Full,
    /// Tern's set for author sheets: px, em, vw, vh, %; other units are "unsupported length unit".
    Tern,
}

thread_local! {
    static UNIT_MODE: std::cell::Cell<UnitMode> = const { std::cell::Cell::new(UnitMode::Full) };
}

pub(crate) fn with_unit_mode<R>(mode: UnitMode, f: impl FnOnce() -> R) -> R {
    let prev = UNIT_MODE.with(|m| m.replace(mode));
    let r = f();
    UNIT_MODE.with(|m| m.set(prev));
    r
}

/// Whether the value being parsed is in a program or plugin sheet (Tern's rules), not ours.
pub(crate) fn tern_rules() -> bool {
    UNIT_MODE.with(|m| m.get()) == UnitMode::Tern
}

fn unit_expr(value: f32, unit: &str) -> PResult<Expr> {
    let u = unit.to_ascii_lowercase();
    let tern = UNIT_MODE.with(|m| m.get()) == UnitMode::Tern;
    Ok(match u.as_str() {
        "px" => Expr::Px(value),
        "em" => Expr::Em(value),
        "rem" if !tern => Expr::Rem(value),
        "vw" => Expr::Vw(value),
        "vh" => Expr::Vh(value),
        // Physical units are accepted in our own sheets only.
        "pt" if !tern => Expr::Px(value * 4.0 / 3.0),
        _ => return Err(ParseError::custom(ValueError::UnsupportedUnit(u))),
    })
}

/// Parse a length-percentage (or a calc()/min()/max()/clamp() that yields one).
pub fn parse_length_pct<'i>(input: &mut Parser<'i>) -> PResult<Expr> {
    let e = parse_length_atom(input)?;
    match e {
        Expr::Num(n) if n != 0.0 => Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        ))),
        e => Ok(e),
    }
}

/// Parse a non-negative length-percentage.
pub fn parse_length_pct_nonneg<'i>(input: &mut Parser<'i>) -> PResult<Expr> {
    let e = parse_length_pct(input)?;
    match &e {
        Expr::Px(v) | Expr::Em(v) | Expr::Rem(v) | Expr::Vw(v) | Expr::Vh(v) | Expr::Pct(v)
            if *v < 0.0 =>
        {
            Err(ParseError::custom(ValueError::UnsupportedUnit(
                String::new(),
            )))
        }
        _ => Ok(e),
    }
}

fn parse_length_atom<'i>(input: &mut Parser<'i>) -> PResult<Expr> {
    let tok = input.next()?.clone();
    match tok {
        Token::Dimension {
            value, ref unit, ..
        } => unit_expr(value, unit),
        Token::Percentage { unit_value, .. } => Ok(Expr::Pct(unit_value * 100.0)),
        Token::Number { value, .. } => Ok(Expr::Num(value)),
        Token::Function(ref name) => {
            let name = name.to_ascii_lowercase();
            match name.as_str() {
                "calc" | "-webkit-calc" => input.parse_nested_block(parse_sum),
                "min" | "max" => input.parse_nested_block(|i| {
                    let args = i.parse_comma_separated(parse_sum)?;
                    Ok(if name == "min" {
                        Expr::Min(args)
                    } else {
                        Expr::Max(args)
                    })
                }),
                "clamp" => input.parse_nested_block(|i| {
                    let a = parse_sum(i)?;
                    i.expect_comma()?;
                    let b = parse_sum(i)?;
                    i.expect_comma()?;
                    let c = parse_sum(i)?;
                    Ok(Expr::Clamp(Box::new(a), Box::new(b), Box::new(c)))
                }),
                "url" => Err(ParseError::custom(ValueError::Url)),
                _ => Err(ParseError::unexpected_token()),
            }
        }
        Token::UnquotedUrl(_) => Err(ParseError::custom(ValueError::Url)),
        _ => Err(ParseError::unexpected_token()),
    }
}

fn parse_sum<'i>(input: &mut Parser<'i>) -> PResult<Expr> {
    let mut acc = parse_product(input)?;
    loop {
        let state = input.state();
        match input.next() {
            Ok(Token::Delim('+')) => {
                let rhs = parse_product(input)?;
                acc = Expr::Add(Box::new(acc), Box::new(rhs));
            }
            Ok(Token::Delim('-')) => {
                let rhs = parse_product(input)?;
                acc = Expr::Sub(Box::new(acc), Box::new(rhs));
            }
            _ => {
                input.reset(&state);
                return Ok(acc);
            }
        }
    }
}

fn parse_product<'i>(input: &mut Parser<'i>) -> PResult<Expr> {
    let mut acc = parse_calc_value(input)?;
    loop {
        let state = input.state();
        match input.next() {
            Ok(Token::Delim('*')) => {
                let rhs = parse_calc_value(input)?;
                acc = Expr::Mul(Box::new(acc), Box::new(rhs));
            }
            Ok(Token::Delim('/')) => {
                let rhs = parse_calc_value(input)?;
                acc = Expr::Div(Box::new(acc), Box::new(rhs));
            }
            _ => {
                input.reset(&state);
                return Ok(acc);
            }
        }
    }
}

fn parse_calc_value<'i>(input: &mut Parser<'i>) -> PResult<Expr> {
    if input.try_parse(|i| i.expect_parenthesis_block()).is_ok() {
        return input.parse_nested_block(parse_sum);
    }
    parse_length_atom(input)
}

/// An RGBA color with components in 0..=1.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    pub const BLACK: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    pub const WHITE: Color = Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    pub fn rgb8(r: u8, g: u8, b: u8) -> Color {
        Color {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        }
    }
    pub fn is_transparent(&self) -> bool {
        self.a <= 0.0
    }
    pub fn to_rgba8(&self) -> [u8; 4] {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [c(self.r), c(self.g), c(self.b), c(self.a)]
    }
}

/// A specified color.
#[derive(Clone, Debug, PartialEq)]
pub enum SColor {
    Rgba(Color),
    CurrentColor,
    LightDark(Box<SColor>, Box<SColor>),
    Mix(Box<SColor>, f32, Box<SColor>),
}

impl SColor {
    pub fn compute(&self, current: Color, dark: bool) -> Color {
        match self {
            SColor::Rgba(c) => *c,
            SColor::CurrentColor => current,
            SColor::LightDark(l, d) => {
                if dark {
                    d.compute(current, dark)
                } else {
                    l.compute(current, dark)
                }
            }
            SColor::Mix(a, p, b) => {
                let (a, b) = (a.compute(current, dark), b.compute(current, dark));
                let t = 1.0 - p;
                let alpha = a.a * p + b.a * t;
                if alpha <= 0.0 {
                    return Color::TRANSPARENT;
                }
                Color {
                    r: (a.r * a.a * p + b.r * b.a * t) / alpha,
                    g: (a.g * a.a * p + b.g * b.a * t) / alpha,
                    b: (a.b * a.a * p + b.b * b.a * t) / alpha,
                    a: alpha,
                }
            }
        }
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    let t2 = if l <= 0.5 {
        l * (s + 1.0)
    } else {
        l + s - l * s
    };
    let t1 = l * 2.0 - t2;
    let hue = |mut h: f32| {
        if h < 0.0 {
            h += 1.0
        }
        if h > 1.0 {
            h -= 1.0
        }
        if h * 6.0 < 1.0 {
            t1 + (t2 - t1) * h * 6.0
        } else if h * 2.0 < 1.0 {
            t2
        } else if h * 3.0 < 2.0 {
            t1 + (t2 - t1) * (2.0 / 3.0 - h) * 6.0
        } else {
            t1
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

fn oklab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bb = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;
    let enc = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.003_130_8 {
            12.92 * x
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        }
    };
    (enc(r), enc(g), enc(bb))
}

fn parse_alpha_or_number<'i>(input: &mut Parser<'i>, pct_scale: f32) -> PResult<f32> {
    match input.next()?.clone() {
        Token::Number { value, .. } => Ok(value),
        Token::Percentage { unit_value, .. } => Ok(unit_value * pct_scale),
        Token::Ident(ref s) if s.eq_ignore_ascii_case("none") => Ok(0.0),
        Token::Dimension {
            value, ref unit, ..
        } => match unit.to_ascii_lowercase().as_str() {
            "deg" => Ok(value),
            "rad" => Ok(value.to_degrees()),
            "grad" => Ok(value * 0.9),
            "turn" => Ok(value * 360.0),
            _ => Err(ParseError::custom(ValueError::UnsupportedUnit(
                String::new(),
            ))),
        },
        _ => Err(ParseError::unexpected_token()),
    }
}

/// Parse color function arguments: three channels then an optional alpha, comma or space separated.
fn parse_channels<'i>(input: &mut Parser<'i>, scales: [f32; 3]) -> PResult<[f32; 4]> {
    let c0 = parse_alpha_or_number(input, scales[0])?;
    let commas = input.try_parse(|i| i.expect_comma()).is_ok();
    let c1 = parse_alpha_or_number(input, scales[1])?;
    if commas {
        input.expect_comma()?;
    }
    let c2 = parse_alpha_or_number(input, scales[2])?;
    let mut a = 1.0;
    let has_alpha = if commas {
        input.try_parse(|i| i.expect_comma()).is_ok()
    } else {
        input.try_parse(|i| i.expect_delim('/')).is_ok()
    };
    if has_alpha {
        a = parse_alpha_or_number(input, 1.0)?;
    }
    Ok([c0, c1, c2, a.clamp(0.0, 1.0)])
}

pub fn parse_color<'i>(input: &mut Parser<'i>) -> PResult<SColor> {
    let tok = input.next()?.clone();
    match tok {
        Token::Hash(ref h) | Token::IDHash(ref h) => {
            match cssparser::color::parse_hash_color(h.as_bytes()) {
                Ok((r, g, b, a)) => Ok(SColor::Rgba(Color {
                    a,
                    ..Color::rgb8(r, g, b)
                })),
                Err(()) => Err(ParseError::unexpected_token()),
            }
        }
        Token::Ident(ref name) => {
            let lower = name.to_ascii_lowercase();
            match lower.as_str() {
                "currentcolor" => Ok(SColor::CurrentColor),
                "transparent" => Ok(SColor::Rgba(Color::TRANSPARENT)),
                _ => match cssparser::color::parse_named_color(&lower) {
                    Ok((r, g, b)) => Ok(SColor::Rgba(Color::rgb8(r, g, b))),
                    Err(()) => Err(ParseError::unexpected_token()),
                },
            }
        }
        Token::Function(ref name) => {
            let name = name.to_ascii_lowercase();
            input.parse_nested_block(|i| match name.as_str() {
                "rgb" | "rgba" => {
                    let [r, g, b, a] = parse_channels(i, [2.55, 2.55, 2.55])?;
                    Ok(SColor::Rgba(Color {
                        r: r / 255.0,
                        g: g / 255.0,
                        b: b / 255.0,
                        a,
                    }))
                }
                "hsl" | "hsla" => {
                    let [h, s, l, a] = parse_channels(i, [1.0, 100.0, 100.0])?;
                    let (r, g, b) =
                        hsl_to_rgb(h, (s / 100.0).clamp(0.0, 1.0), (l / 100.0).clamp(0.0, 1.0));
                    Ok(SColor::Rgba(Color { r, g, b, a }))
                }
                "oklab" => {
                    let [l, a_, b_, a] = parse_channels(i, [0.01, 0.004, 0.004])?;
                    let (r, g, b) = oklab_to_srgb(l, a_, b_);
                    Ok(SColor::Rgba(Color { r, g, b, a }))
                }
                "oklch" => {
                    let [l, c, h, a] = parse_channels(i, [0.01, 0.004, 1.0])?;
                    let (s, co) = h.to_radians().sin_cos();
                    let (r, g, b) = oklab_to_srgb(l, c * co, c * s);
                    Ok(SColor::Rgba(Color { r, g, b, a }))
                }
                "light-dark" => {
                    let l = parse_color(i)?;
                    i.expect_comma()?;
                    let d = parse_color(i)?;
                    Ok(SColor::LightDark(Box::new(l), Box::new(d)))
                }
                "color-mix" => {
                    i.expect_ident_matching("in")?;
                    let _space = i.expect_ident()?.clone();
                    // An optional hue interpolation method.
                    let _ = i.try_parse(|i| -> PResult<()> {
                        i.expect_ident()?;
                        i.expect_ident_matching("hue")?;
                        Ok(())
                    });
                    i.expect_comma()?;
                    let parse_stop = |i: &mut Parser<'i>| -> PResult<(SColor, Option<f32>)> {
                        let p1 = i.try_parse(|i| i.expect_percentage()).ok();
                        let c = parse_color(i)?;
                        let p2 = i.try_parse(|i| i.expect_percentage()).ok();
                        Ok((c, p1.or(p2)))
                    };
                    let (a, pa) = parse_stop(i)?;
                    i.expect_comma()?;
                    let (b, pb) = parse_stop(i)?;
                    let p = match (pa, pb) {
                        (Some(a), _) => a,
                        (None, Some(b)) => 1.0 - b,
                        _ => 0.5,
                    };
                    Ok(SColor::Mix(Box::new(a), p, Box::new(b)))
                }
                _ => Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                ))),
            })
        }
        _ => Err(ParseError::unexpected_token()),
    }
}

/// Parse a `<time>` in seconds.
pub fn parse_time<'i>(input: &mut Parser<'i>) -> PResult<f32> {
    match input.next()?.clone() {
        Token::Dimension {
            value, ref unit, ..
        } if unit.eq_ignore_ascii_case("s") => Ok(value),
        Token::Dimension {
            value, ref unit, ..
        } if unit.eq_ignore_ascii_case("ms") => Ok(value / 1000.0),
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => Err(ParseError::unexpected_token()),
    }
}

/// Parse an angle in degrees.
pub fn parse_angle<'i>(input: &mut Parser<'i>) -> PResult<f32> {
    match input.next()?.clone() {
        Token::Dimension {
            value, ref unit, ..
        } => match unit.to_ascii_lowercase().as_str() {
            "deg" => Ok(value),
            "rad" => Ok(value.to_degrees()),
            "grad" => Ok(value * 0.9),
            "turn" => Ok(value * 360.0),
            _ => Err(ParseError::custom(ValueError::UnsupportedUnit(
                unit.to_string(),
            ))),
        },
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => Err(ParseError::unexpected_token()),
    }
}

/// Parse an easing function, returning its text.
pub fn parse_easing<'i>(input: &mut Parser<'i>) -> PResult<Easing> {
    match input.next()?.clone() {
        Token::Ident(ref s) => Ok(match s.to_ascii_lowercase().as_str() {
            "linear" => Easing::CubicBezier(0.0, 0.0, 1.0, 1.0),
            "ease" => Easing::CubicBezier(0.25, 0.1, 0.25, 1.0),
            "ease-in" => Easing::CubicBezier(0.42, 0.0, 1.0, 1.0),
            "ease-out" => Easing::CubicBezier(0.0, 0.0, 0.58, 1.0),
            "ease-in-out" => Easing::CubicBezier(0.42, 0.0, 0.58, 1.0),
            "step-start" => Easing::Steps(1, true),
            "step-end" => Easing::Steps(1, false),
            _ => return Err(ParseError::unexpected_token()),
        }),
        Token::Function(ref f) => {
            let f = f.to_ascii_lowercase();
            input.parse_nested_block(|i| match f.as_str() {
                "cubic-bezier" => {
                    let a = i.expect_number()?;
                    i.expect_comma()?;
                    let b = i.expect_number()?;
                    i.expect_comma()?;
                    let c = i.expect_number()?;
                    i.expect_comma()?;
                    let d = i.expect_number()?;
                    Ok(Easing::CubicBezier(a, b, c, d))
                }
                "steps" => {
                    let n = i.expect_integer()?.max(1) as u32;
                    let start = if i.try_parse(|i| i.expect_comma()).is_ok() {
                        let kw = i.expect_ident()?.to_ascii_lowercase();
                        matches!(kw.as_str(), "start" | "jump-start" | "jump-both")
                    } else {
                        false
                    };
                    Ok(Easing::Steps(n, start))
                }
                "spring" => {
                    let d = parse_time(i)?;
                    let bounce = if i.try_parse(|i| i.expect_comma()).is_ok() {
                        i.expect_number()?
                    } else {
                        0.0
                    };
                    Ok(Easing::Spring(d, bounce))
                }
                _ => Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                ))),
            })
        }
        _ => Err(ParseError::unexpected_token()),
    }
}

/// A timing function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    CubicBezier(f32, f32, f32, f32),
    Steps(u32, bool),
    Spring(f32, f32),
}

impl Default for Easing {
    fn default() -> Self {
        Easing::CubicBezier(0.25, 0.1, 0.25, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len(s: &str) -> Expr {
        let mut p = Parser::new(s);
        parse_length_pct(&mut p).unwrap()
    }

    #[test]
    fn calc_folds_absolute_parts() {
        let cx = LengthContext {
            font_size: 10.0,
            root_font_size: 16.0,
            vw: 800.0,
            vh: 600.0,
            zoom: 1.0,
        };
        assert_eq!(len("calc(2em + 4px)").compute(&cx), LengthPct::Px(24.0));
        assert_eq!(len("calc(1rem * 2)").compute(&cx), LengthPct::Px(32.0));
        assert_eq!(len("10vw").compute(&cx), LengthPct::Px(80.0));
        let mixed = len("calc(50% - 10px)").compute(&cx);
        assert_eq!(mixed.resolve(200.0), 90.0);
        assert_eq!(len("min(10px, 5em)").compute(&cx), LengthPct::Px(10.0));
    }

    #[test]
    fn colors_parse() {
        let parse = |s: &str| {
            let mut p = Parser::new(s);
            parse_color(&mut p).unwrap()
        };
        assert_eq!(parse("#f00"), SColor::Rgba(Color::rgb8(255, 0, 0)));
        assert_eq!(
            parse("rgb(0 128 255 / 50%)")
                .compute(Color::BLACK, false)
                .to_rgba8(),
            [0, 128, 255, 128]
        );
        assert_eq!(
            parse("hsl(120, 100%, 50%)")
                .compute(Color::BLACK, false)
                .to_rgba8(),
            [0, 255, 0, 255]
        );
        assert_eq!(
            parse("currentColor").compute(Color::WHITE, false),
            Color::WHITE
        );
    }
}
