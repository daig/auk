//! CSS transitions and keyframe animations (§8.4 "Effects: transitions, keyframes", §7.13), run on
//! the document's clock ([`crate::Document::set_time`]), which the view drives at display rate.
//!
//! On top of each element's cascaded style the engine paints running transitions, then CSS
//! animations (research/elements.md §4.3). Transitions start when a listed property changes value
//! through a style change, from the value shown at that moment. Animations follow CSS Animations
//! (delay, iterations, direction, fill mode, play state), their keyframes resolved first in the
//! element's scope (private to program sheets). Interpolated properties are the ones Auk paints
//! without a new layout: `opacity`, the colors (`color`, `background-color`, the border colors,
//! `outline-color`, `text-decoration-color`), `transform` and `box-shadow`. Other properties in a keyframe
//! switch discretely in CSS; here they keep their cascaded value. Reduce Motion (the
//! `prefers-reduced-motion` media feature) gives still equivalents: transitions jump and
//! animations don't run.

use crate::css::properties::Prop;
use crate::css::values::{Color, Easing, LengthPct};
use crate::style::{Animation, ComputedStyle, Shadow, TransformFn};
use std::sync::Arc;

/// A property the engine interpolates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AnimProp {
    Opacity,
    Color,
    BackgroundColor,
    BorderColor(usize),
    OutlineColor,
    TextDecorationColor,
    Transform,
    BoxShadow,
}

const ALL: [AnimProp; 11] = [
    AnimProp::Opacity,
    AnimProp::Color,
    AnimProp::BackgroundColor,
    AnimProp::BorderColor(0),
    AnimProp::BorderColor(1),
    AnimProp::BorderColor(2),
    AnimProp::BorderColor(3),
    AnimProp::OutlineColor,
    AnimProp::TextDecorationColor,
    AnimProp::Transform,
    AnimProp::BoxShadow,
];

impl AnimProp {
    /// The properties a `transition-property` name covers.
    fn named(name: &str) -> &'static [AnimProp] {
        match name {
            "all" => &ALL,
            "opacity" => &ALL[0..1],
            "color" => &ALL[1..2],
            "background-color" | "background" => &ALL[2..3],
            "border-color" | "border" => &ALL[3..7],
            "border-top-color" | "border-top" => &ALL[3..4],
            "border-right-color" | "border-right" => &ALL[4..5],
            "border-bottom-color" | "border-bottom" => &ALL[5..6],
            "border-left-color" | "border-left" => &ALL[6..7],
            "outline-color" | "outline" => &ALL[7..8],
            "text-decoration-color" => &ALL[8..9],
            "transform" => &ALL[9..10],
            "box-shadow" => &ALL[10..11],
            _ => &[],
        }
    }

    /// The property a longhand declaration sets, if the engine interpolates it.
    pub(crate) fn of(p: Prop) -> Option<AnimProp> {
        Some(match p {
            Prop::Opacity => AnimProp::Opacity,
            Prop::Color => AnimProp::Color,
            Prop::BackgroundColor => AnimProp::BackgroundColor,
            Prop::BorderTopColor => AnimProp::BorderColor(0),
            Prop::BorderRightColor => AnimProp::BorderColor(1),
            Prop::BorderBottomColor => AnimProp::BorderColor(2),
            Prop::BorderLeftColor => AnimProp::BorderColor(3),
            Prop::OutlineColor => AnimProp::OutlineColor,
            Prop::TextDecorationColor => AnimProp::TextDecorationColor,
            Prop::Transform => AnimProp::Transform,
            Prop::BoxShadow => AnimProp::BoxShadow,
            _ => return None,
        })
    }

    fn get(self, s: &ComputedStyle) -> Val {
        match self {
            AnimProp::Opacity => Val::Num(s.opacity),
            AnimProp::Color => Val::Color(s.color),
            AnimProp::BackgroundColor => Val::Color(s.background_color),
            AnimProp::BorderColor(i) => Val::Color(s.border_color[i]),
            AnimProp::OutlineColor => Val::Color(s.outline_color),
            AnimProp::TextDecorationColor => Val::Color(s.text_decoration_color.unwrap_or(s.color)),
            AnimProp::Transform => Val::Transform(s.transform.clone()),
            AnimProp::BoxShadow => Val::Shadows(s.box_shadow.clone()),
        }
    }

    fn set(self, s: &mut ComputedStyle, v: &Val) {
        match (self, v) {
            (AnimProp::Opacity, Val::Num(n)) => s.opacity = n.clamp(0.0, 1.0),
            (AnimProp::Color, Val::Color(c)) => s.color = *c,
            (AnimProp::BackgroundColor, Val::Color(c)) => s.background_color = *c,
            (AnimProp::BorderColor(i), Val::Color(c)) => s.border_color[i] = *c,
            (AnimProp::OutlineColor, Val::Color(c)) => s.outline_color = *c,
            (AnimProp::TextDecorationColor, Val::Color(c)) => s.text_decoration_color = Some(*c),
            (AnimProp::Transform, Val::Transform(t)) => s.transform = t.clone(),
            (AnimProp::BoxShadow, Val::Shadows(v)) => s.box_shadow = v.clone(),
            _ => {}
        }
    }
}

/// An animated value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    Num(f32),
    Color(Color),
    Transform(Option<Arc<Vec<TransformFn>>>),
    Shadows(Option<Arc<Vec<Shadow>>>),
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: lerp(a.r, b.r, t),
        g: lerp(a.g, b.g, t),
        b: lerp(a.b, b.b, t),
        a: lerp(a.a, b.a, t),
    }
}

/// Shadow lists interpolate pairwise, the shorter padded with transparent zero shadows; a pair
/// where one is inset and the other isn't can't (CSS Backgrounds 3 §7.1).
fn lerp_shadows(a: &[Shadow], b: &[Shadow], t: f32) -> Option<Vec<Shadow>> {
    let none = |like: &Shadow| Shadow {
        inset: like.inset,
        x: 0.0,
        y: 0.0,
        blur: 0.0,
        spread: 0.0,
        color: Color::TRANSPARENT,
    };
    (0..a.len().max(b.len()))
        .map(|i| {
            let x = a.get(i).copied().unwrap_or_else(|| none(&b[i]));
            let y = b.get(i).copied().unwrap_or_else(|| none(&a[i]));
            (x.inset == y.inset).then(|| Shadow {
                inset: x.inset,
                x: lerp(x.x, y.x, t),
                y: lerp(x.y, y.y, t),
                blur: lerp(x.blur, y.blur, t).max(0.0),
                spread: lerp(x.spread, y.spread, t),
                color: lerp_color(x.color, y.color, t),
            })
        })
        .collect()
}

fn lerp_len(a: &LengthPct, b: &LengthPct, t: f32) -> Option<LengthPct> {
    match (a, b) {
        (LengthPct::Px(x), LengthPct::Px(y)) => Some(LengthPct::Px(lerp(*x, *y, t))),
        (LengthPct::Pct(x), LengthPct::Pct(y)) => Some(LengthPct::Pct(lerp(*x, *y, t))),
        _ => None,
    }
}

/// The identity a missing transform function interpolates from (CSS Transforms §9).
fn identity(f: &TransformFn) -> TransformFn {
    match f {
        TransformFn::Translate(..) => {
            TransformFn::Translate(LengthPct::Px(0.0), LengthPct::Px(0.0))
        }
        TransformFn::Scale(..) => TransformFn::Scale(1.0, 1.0),
        TransformFn::Rotate(_) => TransformFn::Rotate(0.0),
        TransformFn::Skew(..) => TransformFn::Skew(0.0, 0.0),
        TransformFn::Matrix(_) => TransformFn::Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
    }
}

fn lerp_fn(a: &TransformFn, b: &TransformFn, t: f32) -> Option<TransformFn> {
    use TransformFn as F;
    Some(match (a, b) {
        (F::Translate(ax, ay), F::Translate(bx, by)) => {
            F::Translate(lerp_len(ax, bx, t)?, lerp_len(ay, by, t)?)
        }
        (F::Scale(ax, ay), F::Scale(bx, by)) => F::Scale(lerp(*ax, *bx, t), lerp(*ay, *by, t)),
        (F::Rotate(a), F::Rotate(b)) => F::Rotate(lerp(*a, *b, t)),
        (F::Skew(ax, ay), F::Skew(bx, by)) => F::Skew(lerp(*ax, *bx, t), lerp(*ay, *by, t)),
        (F::Matrix(a), F::Matrix(b)) => {
            let mut m = [0.0; 6];
            for i in 0..6 {
                m[i] = lerp(a[i], b[i], t);
            }
            F::Matrix(m)
        }
        _ => return None,
    })
}

impl Val {
    /// The value `t` of the way from `self` to `to`; a pair that can't interpolate switches at
    /// the halfway point.
    fn lerp(&self, to: &Val, t: f32) -> Val {
        let discrete = || if t < 0.5 { self.clone() } else { to.clone() };
        match (self, to) {
            (Val::Num(a), Val::Num(b)) => Val::Num(lerp(*a, *b, t)),
            (Val::Color(a), Val::Color(b)) => Val::Color(lerp_color(*a, *b, t)),
            (Val::Shadows(a), Val::Shadows(b)) => {
                let (sa, sb) = (
                    a.as_deref().map_or(&[][..], Vec::as_slice),
                    b.as_deref().map_or(&[][..], Vec::as_slice),
                );
                match lerp_shadows(sa, sb, t) {
                    Some(v) => Val::Shadows((!v.is_empty()).then(|| Arc::new(v))),
                    None => discrete(),
                }
            }
            (Val::Transform(a), Val::Transform(b)) => {
                let empty = Vec::new();
                let (fa, fb) = (
                    a.as_deref().unwrap_or(&empty),
                    b.as_deref().unwrap_or(&empty),
                );
                let n = fa.len().max(fb.len());
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    let x = fa.get(i).cloned().unwrap_or_else(|| identity(&fb[i]));
                    let y = fb.get(i).cloned().unwrap_or_else(|| identity(&fa[i]));
                    match lerp_fn(&x, &y, t) {
                        Some(f) => out.push(f),
                        None => return discrete(),
                    }
                }
                Val::Transform((!out.is_empty()).then(|| Arc::new(out)))
            }
            _ => discrete(),
        }
    }
}

/// The eased progress for linear progress `x` in 0..=1.
pub(crate) fn ease(e: Easing, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    match e {
        // Linear (both control points on the diagonal).
        Easing::CubicBezier(x1, y1, x2, y2) if x1 == y1 && x2 == y2 => x,
        Easing::CubicBezier(x1, y1, x2, y2) => {
            // Solve bezier_x(u) = x by bisection, then return bezier_y(u).
            let b = |p1: f32, p2: f32, u: f32| {
                let v = 1.0 - u;
                3.0 * v * v * u * p1 + 3.0 * v * u * u * p2 + u * u * u
            };
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..24 {
                let mid = (lo + hi) / 2.0;
                if b(x1, x2, mid) < x {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            b(y1, y2, (lo + hi) / 2.0)
        }
        Easing::Steps(n, start) => {
            let n = n.max(1) as f32;
            let step = if start {
                (x * n).floor() + 1.0
            } else {
                (x * n).floor()
            };
            (step / n).clamp(0.0, 1.0)
        }
        Easing::Spring(_, bounce) => {
            // A critically damped (bounce 0) or underdamped spring settling over the duration.
            let b = bounce.clamp(0.0, 0.95);
            let zeta = 1.0 - b;
            let w = 9.0;
            if zeta >= 1.0 {
                1.0 - (1.0 + w * x) * (-w * x).exp()
            } else {
                let wd = w * (1.0 - zeta * zeta).sqrt();
                1.0 - (-zeta * w * x).exp() * ((wd * x).cos() + zeta * w / wd * (wd * x).sin())
            }
        }
    }
}

/// A running transition of one property.
#[derive(Clone, Debug)]
struct Transition {
    prop: AnimProp,
    from: Val,
    to: Val,
    /// When it starts moving (after the delay), in ms.
    start: f64,
    duration: f64,
    easing: Easing,
}

/// A running keyframe animation.
#[derive(Clone, Debug)]
struct Running {
    spec: Animation,
    start: f64,
    /// `(offset, values)` in offset order; offsets 0 and 1 present.
    frames: Vec<(f32, Vec<(AnimProp, Val)>)>,
    /// While paused: when it paused.
    paused_at: Option<f64>,
}

/// One element's motion: its cascaded style and what runs over it.
#[derive(Clone, Debug)]
pub(crate) struct NodeMotion {
    pub base: Arc<ComputedStyle>,
    transitions: Vec<Transition>,
    animations: Vec<Running>,
    /// Animations that ran to the end and were dropped, by name: they don't start again while
    /// the element keeps listing them (CSS Animations 1 §3: an animation starts when its name
    /// appears in `animation-name`, not on every restyle).
    pub finished: Vec<Arc<str>>,
}

/// The keyframes of an animation, resolved for one element: `(offset, declared values)`.
pub(crate) type Frames = Vec<(f32, Vec<(AnimProp, Val)>)>;

impl NodeMotion {
    pub fn new(base: Arc<ComputedStyle>) -> NodeMotion {
        NodeMotion {
            base,
            transitions: Vec::new(),
            animations: Vec::new(),
            finished: Vec::new(),
        }
    }

    pub fn is_idle(&self) -> bool {
        self.transitions.is_empty() && self.animations.is_empty()
    }

    /// Takes a new cascaded style at `now`. `shown` is the style shown until now (which new
    /// transitions start from); `frames` resolves an animation's keyframes by name.
    pub fn restyle(
        &mut self,
        base: Arc<ComputedStyle>,
        shown: Option<&ComputedStyle>,
        now: f64,
        reduce: bool,
        mut frames: impl FnMut(&str) -> Option<Frames>,
    ) {
        if reduce {
            // Listed animations that would end count as finished, so they don't all play at
            // once when motion comes back (ones that loop or fill forwards start then).
            self.transitions.clear();
            self.animations.clear();
            self.finished = base
                .animations
                .as_deref()
                .into_iter()
                .flatten()
                .filter(|a| {
                    a.duration > 0.0
                        && a.iterations.is_finite()
                        && !matches!(&*a.fill_mode, "forwards" | "both")
                })
                .map(|a| a.name.clone())
                .collect();
            self.base = base;
            return;
        }
        // Transitions: per property, the last listed entry naming it wins.
        let mut wanted: Vec<(AnimProp, &crate::style::Transition)> = Vec::new();
        for t in base.transitions.as_deref().into_iter().flatten() {
            for &p in AnimProp::named(&t.property) {
                wanted.retain(|(q, _)| *q != p);
                wanted.push((p, t));
            }
        }
        self.transitions
            .retain(|r| wanted.iter().any(|(p, _)| *p == r.prop));
        for (p, t) in wanted {
            let to = p.get(&base);
            let running = self.transitions.iter().position(|r| r.prop == p);
            if let Some(i) = running
                && self.transitions[i].to == to
            {
                continue;
            }
            let Some(shown) = shown else { continue };
            let from = p.get(shown);
            if let Some(i) = running {
                self.transitions.remove(i);
            }
            if from == to || t.duration + t.delay <= 0.0 || t.duration <= 0.0 {
                continue;
            }
            self.transitions.push(Transition {
                prop: p,
                from,
                to,
                start: now + f64::from(t.delay) * 1000.0,
                duration: f64::from(t.duration) * 1000.0,
                easing: t.easing,
            });
        }
        // Animations: kept by name, new ones start now, removed ones stop; finished ones stay
        // finished while still listed.
        let specs: Vec<Animation> = base.animations.as_deref().cloned().unwrap_or_default();
        let mut next = Vec::with_capacity(specs.len());
        let mut finished = Vec::new();
        for spec in specs {
            if spec.duration <= 0.0 {
                continue;
            }
            if self.finished.contains(&spec.name) {
                finished.push(spec.name);
                continue;
            }
            match self
                .animations
                .iter()
                .position(|a| a.spec.name == spec.name)
            {
                Some(i) => {
                    let mut a = self.animations.remove(i);
                    match (a.paused_at, spec.paused) {
                        (None, true) => a.paused_at = Some(now),
                        (Some(at), false) => {
                            a.start += now - at;
                            a.paused_at = None;
                        }
                        _ => {}
                    }
                    a.spec = spec;
                    next.push(a);
                }
                None => {
                    let Some(frames) = frames(&spec.name) else {
                        continue;
                    };
                    next.push(Running {
                        paused_at: spec.paused.then_some(now),
                        start: now + f64::from(spec.delay) * 1000.0,
                        spec,
                        frames,
                    });
                }
            }
        }
        self.animations = next;
        self.finished = finished;
        self.base = base;
    }

    /// The style shown at `now`, and drops what has finished. `None` when it equals the base.
    pub fn at(&mut self, now: f64) -> Option<ComputedStyle> {
        self.transitions.retain(|t| now < t.start + t.duration);
        let finished = &mut self.finished;
        self.animations.retain(|a| {
            let fill_forwards = matches!(&*a.spec.fill_mode, "forwards" | "both");
            let keep = fill_forwards
                || a.paused_at.is_some()
                || !a.spec.iterations.is_finite()
                || now
                    < a.start + f64::from(a.spec.duration) * 1000.0 * f64::from(a.spec.iterations);
            if !keep {
                finished.push(a.spec.name.clone());
            }
            keep
        });
        if self.is_idle() {
            return None;
        }
        let mut s = (*self.base).clone();
        for t in &self.transitions {
            // During the delay the old value shows.
            let x = ((now - t.start) / t.duration).max(0.0) as f32;
            t.prop.set(&mut s, &t.from.lerp(&t.to, ease(t.easing, x)));
        }
        for a in &self.animations {
            let at = a.paused_at.unwrap_or(now);
            let dur = f64::from(a.spec.duration) * 1000.0;
            let elapsed = at - a.start;
            let backwards = matches!(&*a.spec.fill_mode, "backwards" | "both");
            if elapsed < 0.0 && !backwards {
                continue;
            }
            let iters = f64::from(a.spec.iterations.max(0.0));
            let total = dur * iters;
            let (iteration, local) = if elapsed < 0.0 {
                (0.0, 0.0)
            } else if iters.is_finite() && elapsed >= total {
                // Finished (filling forwards): the end of the last iteration.
                let last = (iters.ceil() - 1.0).max(0.0);
                let frac = iters - last;
                (last, if frac > 0.0 { frac } else { 1.0 })
            } else {
                let it = (elapsed / dur).floor();
                (it, elapsed / dur - it)
            };
            let reverse = match &*a.spec.direction {
                "reverse" => true,
                "alternate" => !(iteration as u64).is_multiple_of(2),
                "alternate-reverse" => (iteration as u64).is_multiple_of(2),
                _ => false,
            };
            let p = if reverse { 1.0 - local } else { local } as f32;
            apply_frames(&mut s, &self.base, &a.frames, p, a.spec.easing);
        }
        Some(s)
    }

    /// Whether anything still moves (so the host keeps asking for frames).
    pub fn moving(&self, now: f64) -> bool {
        !self.transitions.is_empty()
            || self.animations.iter().any(|a| {
                a.paused_at.is_none()
                    && (!a.spec.iterations.is_finite()
                        || now
                            < a.start
                                + f64::from(a.spec.duration)
                                    * 1000.0
                                    * f64::from(a.spec.iterations))
            })
    }
}

/// Applies keyframes at progress `p`: per property, between the keyframes around `p` that
/// declare it (the base value standing in at 0 and 1 when they don't), eased by `easing`.
fn apply_frames(
    s: &mut ComputedStyle,
    base: &ComputedStyle,
    frames: &Frames,
    p: f32,
    easing: Easing,
) {
    let mut props: Vec<AnimProp> = Vec::new();
    for (_, vals) in frames {
        for (q, _) in vals {
            if !props.contains(q) {
                props.push(*q);
            }
        }
    }
    for q in props {
        let mut stops: Vec<(f32, Val)> = frames
            .iter()
            .filter_map(|(o, vals)| {
                vals.iter()
                    .find(|(x, _)| *x == q)
                    .map(|(_, v)| (*o, v.clone()))
            })
            .collect();
        if stops.first().is_none_or(|(o, _)| *o > 0.0) {
            stops.insert(0, (0.0, q.get(base)));
        }
        if stops.last().is_none_or(|(o, _)| *o < 1.0) {
            stops.push((1.0, q.get(base)));
        }
        let i = stops
            .windows(2)
            .position(|w| p >= w[0].0 && p <= w[1].0)
            .unwrap_or(stops.len() - 2);
        let (a, b) = (&stops[i], &stops[i + 1]);
        let span = (b.0 - a.0).max(f32::EPSILON);
        let x = ease(easing, (p - a.0) / span);
        q.set(s, &a.1.lerp(&b.1, x));
    }
}

/// The values a keyframe's declarations give, from the element's style with the keyframe
/// cascaded over it.
pub(crate) fn frame_values(declared: &[Prop], styled: &ComputedStyle) -> Vec<(AnimProp, Val)> {
    let mut out: Vec<(AnimProp, Val)> = Vec::new();
    for &p in declared {
        if let Some(q) = AnimProp::of(p)
            && !out.iter().any(|(x, _)| *x == q)
        {
            out.push((q, q.get(styled)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easings() {
        let linear = Easing::CubicBezier(0.0, 0.0, 1.0, 1.0);
        assert!((ease(linear, 0.3) - 0.3).abs() < 0.01);
        let ease_out = Easing::CubicBezier(0.0, 0.0, 0.58, 1.0);
        assert!(ease(ease_out, 0.5) > 0.5);
        assert_eq!(ease(Easing::Steps(4, false), 0.3), 0.25);
        assert_eq!(ease(Easing::Steps(4, true), 0.3), 0.5);
        assert!((ease(Easing::Spring(0.5, 0.0), 1.0) - 1.0).abs() < 0.01);
    }

    #[test]
    fn transforms_interpolate_function_by_function() {
        let a = Val::Transform(Some(Arc::new(vec![TransformFn::Scale(0.5, 0.5)])));
        let b = Val::Transform(None);
        assert_eq!(
            a.lerp(&b, 0.5),
            Val::Transform(Some(Arc::new(vec![TransformFn::Scale(0.75, 0.75)])))
        );
    }
}
