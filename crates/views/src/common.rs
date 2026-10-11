//! Props every node takes and small prop readers.

use serde_json::{Map, Value};

/// The nine tones; unknown values are dropped.
pub const TONES: [&str; 9] = [
    "neutral", "accent", "info", "success", "warning", "error", "pending", "muted", "user",
];

pub fn tone(p: &Map<String, Value>) -> Option<&str> {
    p.get("tone")
        .and_then(Value::as_str)
        .filter(|t| TONES.contains(t))
}

pub fn str_prop<'a>(p: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

pub fn bool_prop(p: &Map<String, Value>, k: &str) -> bool {
    match p.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => false,
    }
}

pub fn num_prop(p: &Map<String, Value>, k: &str) -> Option<f64> {
    p.get(k).and_then(Value::as_f64).filter(|f| f.is_finite())
}

/// A prop extent: `"Nch"` → `calc(N*var(--sf-cw))`, `"Nlines"` → `calc(N*var(--sf-lh))`, a number
/// (fraction) → a percentage. Pixels, negatives and anything else are ignored ("never pixels").
pub fn extent(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => {
            let f = n.as_f64()?;
            (f.is_finite() && f >= 0.0).then(|| format!("{}%", fmt_num(f * 100.0)))
        }
        Value::String(s) => {
            let s = s.trim();
            let (num, unit) = if let Some(n) = s.strip_suffix("lines") {
                (n, "--sf-lh")
            } else if let Some(n) = s.strip_suffix("line") {
                (n, "--sf-lh")
            } else {
                let n = s.strip_suffix("ch")?;
                (n, "--sf-cw")
            };
            let f: f64 = num.trim().parse().ok()?;
            (f.is_finite() && f >= 0.0).then(|| format!("calc({}*var({unit}))", fmt_num(f)))
        }
        _ => None,
    }
}

/// A number without a trailing `.0`, rounded to 4 decimals.
pub fn fmt_num(f: f64) -> String {
    let r = (f * 10_000.0).round() / 10_000.0;
    if r.fract() == 0.0 {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

/// The inline style the common flex and size props write.
pub fn common_style(p: &Map<String, Value>, out: &mut String) -> bool {
    let mut clip = false;
    if let Some(g) = num_prop(p, "grow").filter(|g| *g >= 0.0) {
        out.push_str(&format!("flex-grow: {}; ", fmt_num(g)));
    }
    if let Some(s) = num_prop(p, "shrink").filter(|s| *s >= 0.0) {
        out.push_str(&format!("flex-shrink: {}; ", fmt_num(s)));
    }
    match p.get("basis") {
        Some(Value::String(s)) if s == "content" => {
            out.push_str("flex-basis: auto; flex-shrink: 0; ");
        }
        Some(Value::Number(n)) => {
            if let Some(f) = n.as_f64().filter(|f| f.is_finite() && *f >= 0.0) {
                out.push_str(&format!("flex-basis: {}%; ", fmt_num(f * 100.0)));
            }
        }
        _ => {}
    }
    for (prop, css) in [("min", "min"), ("max", "max")] {
        let Some(Value::Object(o)) = p.get(prop) else {
            continue;
        };
        if let Some(w) = o.get("w").and_then(extent) {
            out.push_str(&format!("{css}-width: {w}; "));
        }
        if let Some(h) = o.get("h").and_then(extent) {
            out.push_str(&format!("{css}-height: {h}; "));
            if prop == "max" {
                clip = true;
            }
        }
    }
    clip
}

/// A `menu` action name as a label: `open-file` reads "Open file".
pub fn action_label(name: &str) -> String {
    let base = name.split('=').next().unwrap_or(name);
    let words = base.replace(['-', '_'], " ");
    let mut c = words.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The `actions` prop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Actions {
    pub click: Option<String>,
    pub dblclick: Option<String>,
    pub menu: Vec<String>,
}

impl Actions {
    pub fn read(p: &Map<String, Value>, kind: &str) -> Actions {
        // `picker` reuses the name for its action bar.
        if kind == "picker" {
            return Actions::default();
        }
        let Some(Value::Object(o)) = p.get("actions") else {
            return Actions::default();
        };
        let s = |k: &str| {
            o.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        Actions {
            click: s("click"),
            dblclick: s("dblclick"),
            menu: o
                .get("menu")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn clickable(&self) -> bool {
        self.click.is_some() || self.dblclick.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extents_compile_to_cells() {
        assert_eq!(
            extent(&json!("40ch")).as_deref(),
            Some("calc(40*var(--sf-cw))")
        );
        assert_eq!(
            extent(&json!("3lines")).as_deref(),
            Some("calc(3*var(--sf-lh))")
        );
        assert_eq!(extent(&json!(0.5)).as_deref(), Some("50%"));
        assert_eq!(extent(&json!("40px")), None);
        assert_eq!(extent(&json!(-1)), None);
        let p = json!({"grow": 1, "basis": "content", "max": {"w": "40ch", "h": "2lines"}, "min": {"w": "12px"}});
        let mut s = String::new();
        assert!(common_style(p.as_object().unwrap(), &mut s));
        assert_eq!(
            s,
            "flex-grow: 1; flex-basis: auto; flex-shrink: 0; max-width: calc(40*var(--sf-cw)); max-height: calc(2*var(--sf-lh)); "
        );
    }

    #[test]
    fn labels() {
        assert_eq!(action_label("open-file"), "Open file");
        assert_eq!(action_label("sort=name"), "Sort");
    }
}
