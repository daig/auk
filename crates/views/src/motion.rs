//! Motion clocked by the view at display rate: `spinner`,
//! `shimmer`, `elapsed`, `rate`, `progress`, `meter` and span effects. The UI engine parses CSS
//! animations but doesn't run them, so the view drives every moving part by writing the DOM on
//! each tick. Reduce Motion gives still equivalents; timers still tick.

use crate::common::{fmt_num, num_prop, str_prop};
use crate::kinds::{begin, part, spans_into};
use crate::spans;
use crate::view::{Cx, Extra, Look, Mount};
use auk_tsp::NodeRef;
use auk_ui::NodeId;
use serde_json::Value;

/// An `elapsed`-style timer anchored at `start = receive − age`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Timer {
    /// When the count started (view clock, ms). For a countdown, when it ends.
    pub anchor: f64,
    pub countdown: bool,
    pub stopped: Option<f64>,
    pub clock_format: bool,
    pub text: NodeId,
    pub last: String,
}

impl Timer {
    pub fn shown_ms(&self, now: f64) -> f64 {
        if let Some(s) = self.stopped {
            return s.max(0.0);
        }
        if self.countdown {
            (self.anchor - now).max(0.0)
        } else {
            (now - self.anchor).max(0.0)
        }
    }

    /// The next time the text changes.
    pub fn next(&self, now: f64) -> Option<f64> {
        if self.stopped.is_some() {
            return None;
        }
        let ms = self.shown_ms(now);
        if self.countdown && ms <= 0.0 {
            return None;
        }
        let step = if !self.clock_format && ms < 60_000.0 {
            100.0
        } else {
            1000.0
        };
        let into = if self.countdown {
            ms % step
        } else {
            step - ms % step
        };
        Some(now + into.max(1.0))
    }
}

/// `short`: `12.3s`, `4m 05s`, `1h 02m`; `clock`: `0:12`, `4:05`, `1:02:03`.
pub fn format_duration(ms: f64, clock: bool) -> String {
    let ms = ms.max(0.0);
    let total_s = (ms / 1000.0).floor() as u64;
    let (h, m, s) = (total_s / 3600, (total_s / 60) % 60, total_s % 60);
    if clock {
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{}:{s:02}", total_s / 60)
        }
    } else if ms < 60_000.0 {
        // Tenths, rounded down so it never runs ahead.
        let tenths = (ms / 100.0).floor() as u64;
        format!("{}.{}s", tenths / 10, tenths % 10)
    } else if h == 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{h}h {m:02}m")
    }
}

/// The age props timers anchor, by path: top-level `age` (`elapsed`, `tool`) and an agent's
/// nested ones.
pub(crate) const AGE_PATHS: [&str; 4] = ["age", "stats.age", "tool.age", "retry.age"];

/// The [`SurfaceState::anchors`](crate::state::SurfaceState) key of node `id`'s age at `path`.
pub(crate) fn anchor_key(id: &str, path: &str) -> String {
    format!("{id}/{path}")
}

/// Anchor an age prop: a new value re-anchors, the same value keeps running. A top-level `age`
/// also re-anchors whenever it arrives, even unchanged: the view does that from the drain's
/// `ages`. Nested agent ages (`stats.age`, `tool.age`, `retry.age`) restart only on a new
/// value.
pub(crate) fn anchor(cx: &mut Cx, key: String, age: Option<&Value>) -> Option<f64> {
    let age_v = age?.clone();
    let age_ms = age_v.as_f64()?;
    let now = cx.now;
    let entry = cx.st.anchors.get(&key);
    let start = match entry {
        Some((v, start)) if *v == age_v => *start,
        _ => {
            let start = now - age_ms;
            cx.st.anchors.insert(key, (age_v, start));
            start
        }
    };
    Some(start)
}

pub(crate) fn elapsed(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    look.attr("role", "timer");
    let p = node.props();
    let clock = str_prop(p, "format") == Some("clock");
    let start = anchor(
        cx,
        anchor_key(node.id(), "age"),
        p.get("age").or(Some(&Value::from(0))),
    )
    .unwrap_or(cx.now);
    let age = num_prop(p, "age").unwrap_or(0.0);
    let countdown = age < 0.0;
    // `start = receive − age`: for a countdown (negative age) that is when it ends.
    let anchor_ms = start;
    let text = cx.dom.text(m.el, "");
    m.generated.push(text);
    let t = Timer {
        anchor: anchor_ms,
        countdown,
        stopped: num_prop(p, "stopped"),
        clock_format: clock,
        text,
        last: String::new(),
    };
    m.extra = Extra::Timer(t);
    m.clock = num_prop(p, "stopped").is_none();
    tick(cx, node, m);
}

const BRAILLE: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const STARBURST: [&str; 10] = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpinState {
    pub style: String,
    /// The glyph text node (braille, starburst) or the dot/orbit elements.
    pub parts: Vec<NodeId>,
    pub frame: i64,
}

pub(crate) fn spinner(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let style = str_prop(p, "style").unwrap_or("braille");
    let label = spans::parse_spans(p.get("label"));
    let label_text = spans::spans_text(&label);
    if str_prop(p, "aria").is_none() {
        look.attr("role", "img");
        look.attr(
            "aria-label",
            if label_text.is_empty() {
                "Working".to_owned()
            } else {
                label_text.clone()
            },
        );
    }
    // A starburst in omp's working row draws the Spindle mark instead: painted
    // by the view on the clock, not by CSS keyframes.
    let spindle = style == "starburst"
        && node
            .parent()
            .and_then(|p| str_prop(p.props(), "role"))
            .is_some_and(|r| r == "omp.working");
    let style = if spindle { "spindle" } else { style };
    let mut parts = Vec::new();
    match style {
        "spindle" => {
            let s = part(cx, m, "span", "sf-spindle");
            cx.dom.attr(s, "aria-hidden", "true");
            parts.push(s);
        }
        "dots" => {
            let d = part(cx, m, "span", "sf-dots");
            cx.dom.attr(d, "aria-hidden", "true");
            for _ in 0..3 {
                parts.push(cx.dom.child(d, "i", ""));
            }
        }
        "orbit" => {
            let o = part(cx, m, "span", "sf-orbit");
            cx.dom.attr(o, "aria-hidden", "true");
            parts.push(cx.dom.child(o, "i", ""));
        }
        _ => {
            let s = part(cx, m, "span", &format!("sf-spin {style}"));
            cx.dom.attr(s, "aria-hidden", "true");
            let strip = cx.dom.child(s, "span", "strip");
            parts.push(cx.dom.text(strip, BRAILLE[0]));
        }
    }
    if !label.is_empty() {
        let l = part(cx, m, "span", "lbl");
        spans::render(&mut cx.dom, l, &label);
    }
    m.extra = Extra::Spin(SpinState {
        style: style.to_owned(),
        parts,
        frame: -1,
    });
    m.clock = true;
    tick(cx, node, m);
}

/// The Spindle mark's turn: one revolution per 1.2 s (Tern's starburst loop), in
/// [`SPINDLE_FRAMES`] steps.
const SPINDLE_MS: f64 = 1200.0;
pub const SPINDLE_FRAMES: usize = 24;

/// The Spindle mark's inline style at `frame` (negative: the rest frame). The mark turns about
/// its upright axis: its width follows the turn's cosine, never thinner than a fifth.
pub fn spindle_style(frame: i64) -> String {
    if frame < 0 {
        return String::new();
    }
    let a = frame as f64 / SPINDLE_FRAMES as f64 * std::f64::consts::TAU;
    let k = 0.2 + 0.8 * a.cos().abs();
    format!(
        "transform: scaleX({})",
        fmt_num((k * 100.0).round() / 100.0)
    )
}

/// Whether the cascade gives the mark `--sf-spindle: still`.
fn spindle_still(dom: &crate::dom::Dom, el: NodeId) -> bool {
    dom.doc
        .computed_style(el)
        .and_then(|s| s.custom.get("--sf-spindle").map(|v| v.trim() == "still"))
        .unwrap_or(false)
}

/// A Spindle mark the clock holds (under a sheet, `.sf-covered`): its rest frame.
pub(crate) fn spindle_rest(cx: &mut Cx, m: &mut Mount) {
    if let Extra::Spin(s) = &mut m.extra
        && s.style == "spindle"
        && s.frame != -2
    {
        s.frame = -2;
        cx.dom.set_style(s.parts[0], "");
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShimmerState {
    pub chars: Vec<NodeId>,
    pub kitt: bool,
    pub levels: Vec<u8>,
}

/// A shimmer color token: a semantic one naming a color, else a palette token (`strong`, `em`
/// and the other non-color semantic tokens included) over the text's own color.
fn token_color(t: &str) -> String {
    if spans::COLOR_TOKENS.contains(&t) {
        format!("var(--sf-c-{t})")
    } else {
        format!("var(--sf-p-{t}, currentColor)")
    }
}

pub(crate) fn shimmer(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let kitt = str_prop(p, "mode") == Some("kitt");
    look.class_if(kitt, "kitt");
    let sp = spans::parse_spans(match p.get("spans") {
        Some(a @ Value::Array(_)) => Some(a),
        _ => p.get("text"),
    });
    let text = spans::spans_text(&sp);
    if str_prop(p, "aria").is_none() {
        look.attr("aria-label", text.clone());
    }
    if let Some(Value::Object(pal)) = p.get("palette") {
        for (k, var) in [("low", "--sh-lo"), ("mid", "--sh-mid"), ("high", "--sh-hi")] {
            if let Some(t) = pal.get(k).and_then(Value::as_str) {
                look.style(&format!("{var}: {};", token_color(t)));
            }
        }
    }
    let mut chars = Vec::new();
    for s in &sp {
        let st = spans::SpanStyle::parse(&s.s);
        let holder = part(cx, m, "span", &st.class());
        if let Some(c) = &st.color {
            cx.dom.set_style(holder, &format!("color: {c}"));
        }
        for ch in s.t.chars() {
            let c = cx.dom.child(holder, "span", "sf-sh");
            cx.dom.set_style(c, &format!("--i: {}", chars.len()));
            cx.dom.text(c, &ch.to_string());
            chars.push(c);
        }
    }
    let n = chars.len();
    m.extra = Extra::Shimmer(ShimmerState {
        chars,
        kitt,
        levels: vec![u8::MAX; n],
    });
    m.clock = true;
    tick(cx, node, m);
}

/// Per-character shimmer levels at time `t`: 2.25 s sweep, 45 ms stagger, `steps(7)`; `kitt`
/// alternates direction. 0 = low, 1 = mid, 2 = high.
pub fn shimmer_levels(n: usize, t: f64, kitt: bool) -> Vec<u8> {
    let period = 2250.0;
    (0..n)
        .map(|i| {
            let mut ph = ((t - i as f64 * 45.0).rem_euclid(period)) / period;
            if kitt {
                let cycle = ((t - i as f64 * 45.0) / period).floor() as i64;
                if cycle.rem_euclid(2) == 1 {
                    ph = 1.0 - ph;
                }
            }
            let step = (ph * 7.0).floor() as i32;
            match step {
                3 => 2,
                2 | 4 => 1,
                _ => 0,
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RateState {
    pub from: f64,
    pub to: f64,
    pub start: f64,
    pub unit: String,
    pub text: NodeId,
    pub last: String,
}

pub fn format_rate(v: f64) -> String {
    if v.abs() >= 100.0 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

pub(crate) fn rate(cx: &mut Cx, node: NodeRef, m: &mut Mount, _look: &mut Look) {
    let p = node.props();
    let value = num_prop(p, "value").unwrap_or(0.0);
    let unit = str_prop(p, "unit").unwrap_or("").to_owned();
    let now = cx.now;
    let (from, start) = match &m.extra {
        Extra::Rate(r) if r.to != value => {
            // Ease from where it is now to the new value.
            (shown_rate(r, now), now)
        }
        Extra::Rate(r) => (r.from, r.start),
        // The first value shows at once.
        _ => (value, now - 1000.0),
    };
    begin(cx, m);
    let text = cx.dom.text(m.el, "");
    m.generated.push(text);
    m.extra = Extra::Rate(RateState {
        from,
        to: value,
        start,
        unit,
        text,
        last: String::new(),
    });
    m.clock = true;
    tick(cx, node, m);
}

fn shown_rate(r: &RateState, now: f64) -> f64 {
    let t = ((now - r.start) / 420.0).clamp(0.0, 1.0);
    let e = 1.0 - (1.0 - t).powi(3);
    r.from + (r.to - r.from) * e
}

/// A bar that transitions to its value (`progress`, `meter`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BarState {
    pub fill: Vec<NodeId>,
    /// Target values (fractions), per fill.
    pub to: Vec<f64>,
    pub from: Vec<f64>,
    pub start: f64,
    pub dur: f64,
    pub indeterminate: bool,
    pub last: Vec<String>,
}

pub(crate) fn progress(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let p = node.props();
    let value = num_prop(p, "value").map(|v| v.clamp(0.0, 1.0));
    let now = cx.now;
    let prev = match &m.extra {
        Extra::Bar(b) if !b.indeterminate => Some(bar_now(b, now)[0]),
        _ => None,
    };
    begin(cx, m);
    look.attr("role", "progressbar");
    look.attr("aria-valuemin", "0");
    look.attr("aria-valuemax", "100");
    look.class_if(value.is_none(), "indeterminate");
    if let Some(v) = value {
        look.attr("aria-valuenow", fmt_num((v * 100.0).round()));
    }
    let track = part(cx, m, "div", "track");
    let fill = cx.dom.child(track, "div", "fill");
    let label = p.get("label");
    if label.is_some_and(|l| !l.is_null() && l != "") {
        let l = part(cx, m, "span", "lbl");
        spans_into(cx, l, label);
    }
    let v = value.unwrap_or(0.0);
    m.extra = Extra::Bar(BarState {
        fill: vec![fill],
        to: vec![v],
        from: vec![prev.unwrap_or(v)],
        start: now,
        dur: 300.0,
        indeterminate: value.is_none(),
        last: vec![String::new()],
    });
    m.clock = true;
    tick(cx, node, m);
}

fn bar_now(b: &BarState, now: f64) -> Vec<f64> {
    let t = if b.dur <= 0.0 {
        1.0
    } else {
        ((now - b.start) / b.dur).clamp(0.0, 1.0)
    };
    // ease
    let e = 1.0 - (1.0 - t).powi(3);
    b.from
        .iter()
        .zip(&b.to)
        .map(|(f, to)| f + (to - f) * e)
        .collect()
}

/// `data-level` from thresholds `{warn, bad}`.
pub(crate) fn meter_level(v: f64, th: Option<&Value>) -> Option<&'static str> {
    let th = th?.as_object()?;
    let bad = th.get("bad").and_then(Value::as_f64);
    let warn = th.get("warn").and_then(Value::as_f64);
    if bad.is_some_and(|b| v >= b) {
        Some("bad")
    } else if warn.is_some_and(|w| v >= w) {
        Some("warn")
    } else {
        None
    }
}

pub(crate) fn meter(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let p = node.props();
    let now = cx.now;
    let parts: Vec<serde_json::Map<String, Value>> = p
        .get("parts")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect())
        .unwrap_or_default();
    let values: Vec<f64> = if parts.is_empty() {
        vec![num_prop(p, "value").unwrap_or(0.0).clamp(0.0, 1.0)]
    } else {
        parts
            .iter()
            .map(|x| {
                x.get("value")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0)
            })
            .collect()
    };
    let total: f64 = values.iter().sum::<f64>().min(1.0);
    // The first show grows from 0 over 0.6 s; later values transition over 0.4 s.
    let (from, dur) = match &m.extra {
        Extra::Bar(b) if b.fill.len() == values.len() => (bar_now(b, now), 400.0),
        Extra::Bar(_) => (vec![0.0; values.len()], 400.0),
        _ => (vec![0.0; values.len()], 600.0),
    };
    begin(cx, m);
    let style = str_prop(p, "style").unwrap_or("bar");
    let size = str_prop(p, "size").unwrap_or("md");
    look.attr("data-style", style);
    look.attr("data-size", size);
    if let Some(l) = meter_level(total, p.get("thresholds")) {
        look.attr("data-level", l);
    }
    look.attr("role", "meter");
    look.attr("aria-valuemin", "0");
    look.attr("aria-valuemax", "100");
    look.attr("aria-valuenow", fmt_num((total * 100.0).round()));
    look.style(&format!("--sf-meter: {};", fmt_num(total)));
    let mut fills = Vec::new();
    match style {
        "blocks" => {
            let steps = num_prop(p, "steps").unwrap_or(10.0).clamp(1.0, 20.0) as usize;
            let b = part(cx, m, "div", "sf-mt-blocks");
            let on = (total * steps as f64).round() as usize;
            for i in 0..steps {
                cx.dom.child(b, "i", if i < on { "on" } else { "" });
            }
        }
        "ring" => {
            let r = part(cx, m, "span", "sf-mt-ring");
            cx.dom.attr(r, "aria-hidden", "true");
            let arc = cx.dom.child(r, "span", "arc");
            fills.push(arc);
        }
        _ => {
            let track = part(cx, m, "div", "sf-mt-track");
            let marks: Vec<serde_json::Map<String, Value>> = p
                .get("marks")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect())
                .unwrap_or_default();
            if marks.iter().any(|mk| mk.contains_key("icon")) {
                cx.dom.child(track, "div", "sf-mt-rail");
            }
            if parts.is_empty() {
                fills.push(cx.dom.child(track, "div", "sf-mt-fill"));
            } else {
                let mut at = 0.0;
                for (i, pt) in parts.iter().enumerate() {
                    let mut class = String::from("sf-mt-part");
                    if i == 0 {
                        class.push_str(" first");
                    }
                    if i + 1 == parts.len() {
                        class.push_str(" last");
                    }
                    if pt.get("hatch").and_then(Value::as_bool) == Some(true) {
                        class.push_str(" hatch");
                    }
                    let f = cx.dom.child(track, "div", &class);
                    if let Some(l) = pt.get("label").and_then(Value::as_str) {
                        cx.dom.attr(f, "title", l);
                    }
                    let color = pt
                        .get("token")
                        .and_then(Value::as_str)
                        .map(token_color)
                        .unwrap_or_else(|| "var(--tc, var(--accent))".to_owned());
                    cx.dom.attr(f, "data-at", &fmt_num(at));
                    at += values[i];
                    cx.dom.set_style(f, &format!("background-color: {color}"));
                    fills.push(f);
                }
            }
            for mk in &marks {
                let at = mk
                    .get("at")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                let class = if mk.contains_key("icon") {
                    "sf-mt-mark icon"
                } else {
                    "sf-mt-mark"
                };
                let e = cx.dom.child(track, "div", class);
                if let Some(t) = mk.get("tone").and_then(Value::as_str) {
                    cx.dom.attr(e, "data-tone", t);
                }
                if let Some(t) = mk.get("title").and_then(Value::as_str) {
                    cx.dom.attr(e, "title", t);
                }
                cx.dom
                    .set_style(e, &format!("left: {}%", fmt_num(at * 100.0)));
                if let Some(ic) = mk.get("icon").and_then(Value::as_str) {
                    crate::kinds::icon(cx, e, ic);
                }
            }
        }
    }
    let label = p.get("label");
    if label.is_some_and(|l| !l.is_null() && l != "") {
        let l = part(cx, m, "span", "sf-mt-lbl");
        spans_into(cx, l, label);
    }
    if let Some(t) = p.get("total").filter(|t| !t.is_null()) {
        let l = part(cx, m, "span", "sf-mt-total");
        spans_into(cx, l, Some(t));
    }
    let n = fills.len();
    let mut to = values.clone();
    to.truncate(n.max(1));
    let mut fr = from;
    fr.resize(to.len(), 0.0);
    m.extra = Extra::Bar(BarState {
        fill: fills,
        to,
        from: fr,
        start: now,
        dur: if cx.set.reduce_motion { 0.0 } else { dur },
        indeterminate: false,
        last: vec![String::new(); n],
    });
    m.clock = n > 0;
    tick(cx, node, m);
}

/// One clock step for the motion kinds. Returns whether the node still needs the clock.
pub(crate) fn tick(cx: &mut Cx, node: NodeRef, m: &mut Mount) -> bool {
    let now = cx.now;
    let still = cx.set.reduce_motion;
    match &mut m.extra {
        Extra::Timer(t) => {
            let text = format_duration(t.shown_ms(now), t.clock_format);
            if text != t.last {
                cx.dom.doc.set_text(t.text, &text);
                t.last = text;
            }
            t.next(now).is_some()
        }
        Extra::Spin(s) if s.style == "spindle" => {
            let el = s.parts[0];
            // `--sf-spindle: still` (Reduce Motion, `.sf-still`, `.sf-paused`, `.sf-covered`, or a
            // program's sheet) holds the rest frame. Only Reduce Motion stops the clock: the
            // cascade's value can lag a region class by a frame, and the mark turns again when
            // it goes.
            let rest = still || spindle_still(&cx.dom, el);
            let frame = if rest {
                -2
            } else {
                ((now.rem_euclid(SPINDLE_MS)) / SPINDLE_MS * SPINDLE_FRAMES as f64).floor() as i64
            };
            if frame != s.frame {
                s.frame = frame;
                cx.dom.set_style(el, &spindle_style(frame));
            }
            !still
        }
        Extra::Spin(s) => {
            let (period, frames) = match s.style.as_str() {
                "starburst" => (1200.0, 10),
                "dots" => (1200.0, 12),
                "orbit" => (900.0, 12),
                _ => (1000.0, 10),
            };
            let frame = if still {
                0
            } else {
                ((now.rem_euclid(period)) / period * frames as f64).floor() as i64
            };
            if frame != s.frame {
                s.frame = frame;
                match s.style.as_str() {
                    "dots" => {
                        for (i, d) in s.parts.iter().enumerate() {
                            // 1.2 s, delays 0/.16/.32 s: bright near the start of each one's cycle.
                            let t = if still {
                                0.5
                            } else {
                                ((now - i as f64 * 160.0).rem_euclid(1200.0)) / 1200.0
                            };
                            let o = if t < 0.4 { 1.0 - t * 1.5 } else { 0.4 };
                            cx.dom.set_style(*d, &format!("opacity: {}", fmt_num(o)));
                        }
                    }
                    "orbit" => {
                        cx.dom.set_style(
                            s.parts[0],
                            &format!("transform: rotate({}deg)", frame * 30),
                        );
                    }
                    style => {
                        let glyphs = if style == "starburst" {
                            &STARBURST
                        } else {
                            &BRAILLE
                        };
                        cx.dom
                            .doc
                            .set_text(s.parts[0], glyphs[frame as usize % glyphs.len()]);
                    }
                }
            }
            !still
        }
        Extra::Shimmer(s) => {
            let levels = if still {
                vec![0; s.chars.len()]
            } else {
                shimmer_levels(s.chars.len(), now, s.kitt)
            };
            for (i, (&c, &l)) in s.chars.iter().zip(&levels).enumerate() {
                if s.levels[i] != l {
                    let class = match l {
                        2 => "sf-sh hi",
                        1 => "sf-sh mid",
                        _ => "sf-sh",
                    };
                    cx.dom.set_class(c, class);
                    s.levels[i] = l;
                }
            }
            !still
        }
        Extra::Rate(r) => {
            let v = shown_rate(r, now);
            let mut text = format_rate(v);
            if !r.unit.is_empty() {
                text.push(' ');
                text.push_str(&r.unit);
            }
            if text != r.last {
                cx.dom.doc.set_text(r.text, &text);
                r.last = text;
            }
            now - r.start < 420.0
        }
        Extra::Bar(b) => {
            if b.indeterminate {
                let css = if still {
                    "left: 0%; width: 100%; opacity: 0.35".to_owned()
                } else {
                    let step = ((now.rem_euclid(1300.0)) / 1300.0 * 16.0).floor();
                    let left = step / 16.0 * 135.0 - 35.0;
                    format!("left: {}%; width: 35%", fmt_num(left))
                };
                if b.last[0] != css {
                    cx.dom.set_style(b.fill[0], &css);
                    b.last[0] = css;
                }
                return !still;
            }
            let vals = if still { b.to.clone() } else { bar_now(b, now) };
            let ring = node.kind() == "meter" && str_prop(node.props(), "style") == Some("ring");
            let mut at = 0.0;
            for (i, (&f, v)) in b.fill.iter().zip(&vals).enumerate() {
                let css = if ring {
                    format!("--sf-meter: {}", fmt_num(*v))
                } else if b.fill.len() > 1 {
                    let s = format!(
                        "left: {}%; width: {}%",
                        fmt_num(at * 100.0),
                        fmt_num(v * 100.0)
                    );
                    at += v;
                    s
                } else {
                    format!("width: {}%", fmt_num(v * 100.0))
                };
                // Keep the part's color.
                let css = match cx.dom.w.style(f) {
                    Some(old)
                        if old.starts_with("background-color")
                            || old.contains("; background-color") =>
                    {
                        let color = old
                            .split(';')
                            .find(|d| d.trim_start().starts_with("background-color"))
                            .unwrap_or("")
                            .trim()
                            .to_owned();
                        format!("{css}; {color}")
                    }
                    _ => css,
                };
                if b.last[i] != css {
                    cx.dom.set_style(f, &css);
                    b.last[i] = css;
                }
            }
            !still && now - b.start < b.dur
        }
        _ => false,
    }
}

/// Span effects (`fx`) inside a node: shimmer chars and pulses.
pub(crate) fn tick_fx(cx: &mut Cx, m: &mut Mount) -> bool {
    if cx.set.reduce_motion {
        return false;
    }
    let now = cx.now;
    let mut stack = vec![m.el];
    let mut chars: Vec<NodeId> = Vec::new();
    let mut pulses: Vec<NodeId> = Vec::new();
    while let Some(n) = stack.pop() {
        let class = cx.dom.class_of(n).to_owned();
        if class.split(' ').any(|c| c == "sf-fx-shimmer") {
            chars.extend(
                cx.dom
                    .doc
                    .children(n)
                    .iter()
                    .copied()
                    .filter(|c| cx.dom.doc.node(*c).is_element()),
            );
            continue;
        }
        if class.split(' ').any(|c| c == "sf-fx-pulse") {
            pulses.push(n);
        }
        for &c in cx.dom.doc.children(n) {
            if cx.dom.doc.node(c).is_element() {
                stack.push(c);
            }
        }
    }
    let levels = shimmer_levels(chars.len(), now, false);
    for (c, l) in chars.iter().zip(levels) {
        cx.dom.set_class(
            *c,
            match l {
                2 => "sf-sh hi",
                1 => "sf-sh mid",
                _ => "sf-sh",
            },
        );
    }
    for p in pulses {
        // 1 → .45 → 1 over 1.6 s.
        let t = (now.rem_euclid(1600.0)) / 1600.0;
        let o = 1.0 - 0.55 * (1.0 - (2.0 * t - 1.0).abs());
        cx.dom.set_style(
            p,
            &format!("opacity: {}", fmt_num((o * 20.0).round() / 20.0)),
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(format_duration(12_345.0, false), "12.3s");
        assert_eq!(format_duration(245_000.0, false), "4m 05s");
        assert_eq!(format_duration(3_720_000.0, false), "1h 02m");
        assert_eq!(format_duration(12_345.0, true), "0:12");
        assert_eq!(format_duration(245_000.0, true), "4:05");
        assert_eq!(format_duration(3_723_000.0, true), "1:02:03");
        assert_eq!(format_duration(-5.0, false), "0.0s");
    }

    #[test]
    fn shimmer_tokens_without_a_color_fall_back_to_the_palette_then_the_text() {
        assert_eq!(token_color("warning"), "var(--sf-c-warning)");
        for t in ["strong", "em", "mono", "mark", "typo", "icon", "hide"] {
            assert_eq!(
                token_color(t),
                format!("var(--sf-p-{t}, currentColor)"),
                "{t}"
            );
        }
    }

    #[test]
    fn shimmer_sweeps() {
        let a = shimmer_levels(8, 0.0, false);
        let b = shimmer_levels(8, 1000.0, false);
        assert_ne!(a, b);
        assert!(shimmer_levels(8, 1100.0, false).contains(&2));
    }
}
