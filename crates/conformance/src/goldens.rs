//! PNG and layout-JSON goldens for scenario shots, in `fixtures/goldens/<scenario>/`.
//!
//! A shot matches its golden when:
//! - the PNG has the same size, and at most [`MAX_DIFF_FRACTION`] of its pixels differ by more
//!   than [`CHANNEL_TOLERANCE`] in any channel (GPU antialiasing and glyph rasterization noise
//!   stay under both);
//! - the layout JSON has the same structure and values, numbers within [`NUMBER_TOLERANCE`].
//!
//! `AUK_UPDATE_GOLDENS=1` rewrites the goldens from the shots instead of comparing. Use it only
//! deliberately, after looking at the new PNGs, and commit the change.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// A pixel channel may differ by this much (of 255) without counting as different.
pub const CHANNEL_TOLERANCE: u8 = 48;
/// At most this fraction of pixels may differ.
pub const MAX_DIFF_FRACTION: f64 = 0.001;
/// Layout numbers (points) may differ by this much.
pub const NUMBER_TOLERANCE: f64 = 1.0;

/// The env var that switches to update mode.
pub const UPDATE_ENV: &str = "AUK_UPDATE_GOLDENS";

/// Where the goldens live.
pub fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/goldens")
}

/// Whether this run rewrites goldens.
pub fn updating() -> bool {
    std::env::var_os(UPDATE_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// An RGBA8 image.
#[derive(Clone, Debug)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Decodes a PNG into RGBA8.
pub fn read_png(path: &Path) -> Result<Rgba, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    buf.truncate(info.buffer_size());
    let data = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => {
            return Err(format!(
                "{}: unexpected color type {other:?}",
                path.display()
            ));
        }
    };
    Ok(Rgba {
        width: info.width,
        height: info.height,
        data,
    })
}

/// How two images differ.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageDiff {
    pub differing: usize,
    pub total: usize,
    pub max_delta: u8,
}

/// Compares two images pixel by pixel.
pub fn diff_images(a: &Rgba, b: &Rgba) -> Result<ImageDiff, String> {
    if (a.width, a.height) != (b.width, b.height) {
        return Err(format!(
            "sizes differ: {}x{} vs {}x{}",
            a.width, a.height, b.width, b.height
        ));
    }
    let mut differing = 0;
    let mut max_delta = 0;
    for (p, q) in a.data.chunks(4).zip(b.data.chunks(4)) {
        let d = p
            .iter()
            .zip(q)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        max_delta = max_delta.max(d);
        if d > CHANNEL_TOLERANCE {
            differing += 1;
        }
    }
    Ok(ImageDiff {
        differing,
        total: (a.width * a.height) as usize,
        max_delta,
    })
}

/// Compares two layout JSON values; returns the first difference's path.
pub fn diff_json(path: &str, a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64()?, y.as_f64()?);
            ((x - y).abs() > NUMBER_TOLERANCE).then(|| format!("{path}: {x} vs {y}"))
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: {} items vs {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (x, y))| diff_json(&format!("{path}[{i}]"), x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(x), Some(y)) => diff_json(&format!("{path}.{k}"), x, y),
                _ => Some(format!("{path}.{k}: present on one side only")),
            })
        }
        _ => (a != b).then(|| format!("{path}: {a} vs {b}")),
    }
}

/// Checks a shot (`<dir>/<name>.png` and `.json`) against `goldens/<scenario>/<name>.*`, or
/// rewrites the golden in update mode.
pub fn check_shot(scenario: &str, shot_dir: &Path, name: &str) -> Result<(), String> {
    let golden = goldens_dir().join(scenario);
    let (png, json) = (
        shot_dir.join(format!("{name}.png")),
        shot_dir.join(format!("{name}.json")),
    );
    let (gpng, gjson) = (
        golden.join(format!("{name}.png")),
        golden.join(format!("{name}.json")),
    );
    if updating() {
        std::fs::create_dir_all(&golden).map_err(|e| e.to_string())?;
        std::fs::copy(&png, &gpng).map_err(|e| format!("{}: {e}", gpng.display()))?;
        std::fs::copy(&json, &gjson).map_err(|e| format!("{}: {e}", gjson.display()))?;
        eprintln!("updated golden {}", gpng.display());
        return Ok(());
    }
    if !gpng.is_file() || !gjson.is_file() {
        return Err(format!(
            "no golden {} (and .json): look at {} and create it with {UPDATE_ENV}=1",
            gpng.display(),
            png.display()
        ));
    }
    let ours = read_png(&png)?;
    let theirs = read_png(&gpng)?;
    let d = diff_images(&ours, &theirs).map_err(|e| format!("{name}.png: {e}"))?;
    if d.differing as f64 > d.total as f64 * MAX_DIFF_FRACTION {
        return Err(format!(
            "{name}.png differs from {}: {} of {} pixels by more than {CHANNEL_TOLERANCE} (max {}); \
             the shot is {}",
            gpng.display(),
            d.differing,
            d.total,
            d.max_delta,
            png.display()
        ));
    }
    let read = |p: &Path| -> Result<Value, String> {
        serde_json::from_str(
            &std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        )
        .map_err(|e| format!("{}: {e}", p.display()))
    };
    if let Some(diff) = diff_json("$", &read(&json)?, &read(&gjson)?) {
        return Err(format!(
            "{name}.json differs from {}: {diff}",
            gjson.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_numbers_have_a_tolerance() {
        assert_eq!(
            diff_json("$", &json!({"a": [1.0, 2.5]}), &json!({"a": [1.4, 2.0]})),
            None
        );
        assert!(diff_json("$", &json!({"a": 1.0}), &json!({"a": 2.5})).is_some());
        assert!(diff_json("$", &json!({"a": "x"}), &json!({"a": "y"})).is_some());
        assert!(diff_json("$", &json!({"a": 1}), &json!({"b": 1})).is_some());
    }

    #[test]
    fn images_tolerate_small_deltas() {
        let a = Rgba {
            width: 2,
            height: 1,
            data: vec![0, 0, 0, 255, 100, 100, 100, 255],
        };
        let mut b = a.clone();
        b.data[4] = 130;
        assert_eq!(diff_images(&a, &b).unwrap().differing, 0);
        b.data[4] = 200;
        assert_eq!(diff_images(&a, &b).unwrap().differing, 1);
    }
}
