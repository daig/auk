//! `effort`: omp's thinking-effort glyph (§8.2 T2, amendment A23; book:7074-7146). A 14px ring
//! that fills clockwise one sixth per level in that level's palette color; at `max` the ring
//! closes and catches fire (a blazing ring, a crown of flames and an ember, flickering). Any other
//! level draws a dashed empty ring and sets `.unknown`.
//!
//! The UI engine has no SVG DOM, so the glyph is boxes the engine paints: the arc is a full ring
//! clipped to a wedge per half (a half-width clip holding a rotor, which holds a half-width cut of
//! the ring), so a level's arc is two rotations. The parts persist across renders, so a level
//! change transitions the rotors (the sweep) and the fire's opacity (the cross-fade); a new glyph
//! sweeps from empty once (CSS keyframes, `surface-agent.css`). No events of its own.

use crate::common::str_prop;
use crate::view::{Cx, Look, Mount};
use auk_tsp::NodeRef;
use auk_ui::NodeId;

/// The rungs, 0/6 through 6/6.
pub const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The rung of a level, or `None` when it is unresolved (for example `auto`).
pub fn rung(level: &str) -> Option<usize> {
    LEVELS.iter().position(|l| *l == level)
}

/// The rotations of the right and left rotors (degrees) that draw `rung` sixths of the ring.
pub fn rotations(rung: Option<usize>) -> (f64, f64) {
    let sweep = rung.map_or(0.0, |r| r as f64 * 60.0);
    (sweep.min(180.0), (sweep - 180.0).max(0.0))
}

/// One half of the arc: a clip over that half of the glyph, a rotor, and the other half of the
/// ring inside it.
fn half(cx: &mut Cx, arc: NodeId, side: &str) -> NodeId {
    let clip = cx.dom.child(arc, "span", &format!("half {side}"));
    let rot = cx.dom.child(clip, "span", "rot");
    let cut = cx.dom.child(rot, "span", "cut");
    cx.dom.child(cut, "span", "ring");
    rot
}

/// Build the glyph once; returns the right and left rotors.
fn glyph(cx: &mut Cx, m: &mut Mount) -> (NodeId, NodeId) {
    if let Some(&eff) = m.generated.first()
        && cx.dom.doc.try_node(eff).is_some()
    {
        // .sf-eff > .arc > .half > .rot
        let kids = cx.dom.doc.children(eff).to_vec();
        if let Some(&arc) = kids.get(1) {
            let halves = cx.dom.doc.children(arc).to_vec();
            if let [r, l] = halves[..] {
                let rot = |cx: &Cx, h: NodeId| cx.dom.doc.children(h)[0];
                return (rot(cx, r), rot(cx, l));
            }
        }
    }
    super::begin(cx, m);
    let eff = super::part(cx, m, "span", "sf-eff");
    cx.dom.attr(eff, "aria-hidden", "true");
    cx.dom.child(eff, "span", "track");
    let arc = cx.dom.child(eff, "span", "arc");
    let r = half(cx, arc, "r");
    let l = half(cx, arc, "l");
    cx.dom.child(eff, "span", "blaze");
    let crown = cx.dom.child(eff, "span", "crown");
    for f in ["f1", "f2", "f3"] {
        cx.dom.child(crown, "span", &format!("flame {f}"));
    }
    cx.dom.child(eff, "span", "ember");
    (r, l)
}

pub(crate) fn effort(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let p = node.props();
    let level = str_prop(p, "level").unwrap_or("");
    let n = rung(level);
    look.class_if(n.is_none(), "unknown");
    look.attr("data-level", level);
    look.attr("role", "meter");
    look.attr("aria-valuemin", "0");
    look.attr("aria-valuemax", "6");
    look.attr("aria-valuenow", n.unwrap_or(0).to_string());
    look.attr("aria-valuetext", level);
    if str_prop(p, "aria").is_none() {
        look.attr("aria-label", "Thinking effort");
    }
    let (r, l) = glyph(cx, m);
    let (a, b) = rotations(n);
    let was = |el: NodeId| cx.dom.w.style(el).and_then(rotation);
    // Under Reduce Motion nothing sweeps (and no delay holds the new fill back).
    let (ra, rb) = if cx.set.reduce_motion {
        (String::new(), String::new())
    } else {
        sweep((was(r), was(l)), (a, b))
    };
    let fmt = crate::common::fmt_num;
    let style = |deg: f64, t: &str| {
        if t.is_empty() {
            format!("transform: rotate({}deg)", fmt(deg))
        } else {
            format!("transform: rotate({}deg);{t}", fmt(deg))
        }
    };
    cx.dom.set_style(r, &style(a, &ra));
    cx.dom.set_style(l, &style(b, &rb));
}

/// A level change's sweep (book: "the arc sweeps to the new fill"): one clockwise arc, so the
/// right rotor turns first and the left one after it when filling (the left first, then the
/// right, when emptying), each for its share of 0.4 s. The transitions for the right and left
/// rotors, as inline CSS; none without a change (the first show sweeps by keyframes).
fn sweep(was: (Option<f64>, Option<f64>), now: (f64, f64)) -> (String, String) {
    let (Some(a0), Some(b0)) = was else {
        return (String::new(), String::new());
    };
    let (dr, dl) = ((now.0 - a0).abs(), (now.1 - b0).abs());
    let total = dr + dl;
    if total < 1e-6 {
        return (String::new(), String::new());
    }
    const T: f64 = 0.4;
    let (tr, tl) = (T * dr / total, T * dl / total);
    let filling = now.0 + now.1 > a0 + b0;
    let t = |dur: f64, delay: f64| {
        format!(
            " transition: transform {}s linear {}s;",
            crate::common::fmt_num((dur * 1000.0).round() / 1000.0),
            crate::common::fmt_num((delay * 1000.0).round() / 1000.0)
        )
    };
    if filling {
        (t(tr, 0.0), t(tl, tr))
    } else {
        (t(tr, tl), t(tl, 0.0))
    }
}

/// The rotation a rotor's inline style sets, in degrees.
fn rotation(style: &str) -> Option<f64> {
    let at = style.find("rotate(")? + "rotate(".len();
    style[at..].split("deg").next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_change_sweeps_as_one_clockwise_arc() {
        // minimal (60, 0) to high (180, 60): the right rotor's 120° first, then the left's 60°.
        let (r, l) = sweep((Some(60.0), Some(0.0)), (180.0, 60.0));
        assert_eq!(r, " transition: transform 0.267s linear 0s;");
        assert_eq!(l, " transition: transform 0.133s linear 0.267s;");
        // Back down: the left empties first, then the right.
        let (r, l) = sweep((Some(180.0), Some(60.0)), (60.0, 0.0));
        assert_eq!(l, " transition: transform 0.133s linear 0s;");
        assert_eq!(r, " transition: transform 0.267s linear 0.133s;");
        // No change, or a first show: no transition of its own.
        assert_eq!(
            sweep((Some(60.0), Some(0.0)), (60.0, 0.0)),
            (String::new(), String::new())
        );
        assert_eq!(
            sweep((None, None), (60.0, 0.0)),
            (String::new(), String::new())
        );
        assert_eq!(
            rotation("transform: rotate(120deg); transition: x"),
            Some(120.0)
        );
    }

    #[test]
    fn each_rung_is_a_sixth_split_over_the_two_halves() {
        assert_eq!(rotations(rung("off")), (0.0, 0.0));
        assert_eq!(rotations(rung("minimal")), (60.0, 0.0));
        assert_eq!(rotations(rung("medium")), (180.0, 0.0));
        assert_eq!(rotations(rung("high")), (180.0, 60.0));
        assert_eq!(rotations(rung("max")), (180.0, 180.0));
        assert_eq!(rotations(rung("auto")), (0.0, 0.0));
        assert_eq!(rung("auto"), None);
    }
}
