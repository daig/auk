//! Replaced content: a decoded picture an element shows in its content box (an `img`). The
//! element lays out as a replaced box with the picture's natural size and ratio (CSS 2.1 §10.3.2,
//! §10.4, §10.6.2), and paints it placed by `object-fit` and scaled per `image-rendering`.

use crate::style::{ImageRendering, ObjectFit};
use vello::peniko::{ImageBrush, ImageData, ImageQuality};

/// A picture for an element ([`crate::Document::set_image`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageContent {
    /// The pixels. Reusing one `ImageData` (its blob) across frames lets the renderer keep it.
    pub data: ImageData,
    /// The box's natural size in CSS px, before the element's zoom: what an `auto` width and
    /// height lay out to. Usually the picture's own size; a declared size (Tern's `w`/`h`) can
    /// differ from it, and `object-fit` then places the picture in the box.
    pub natural: (f32, f32),
    /// Image pixels per CSS px of the picture (an SVG drawn at twice its size for a Retina
    /// display has density 2): the picture's own size is its pixel size over this.
    pub density: f32,
}

impl ImageContent {
    /// A picture whose natural size is its pixel size.
    pub fn new(data: ImageData) -> ImageContent {
        let natural = (data.width as f32, data.height as f32);
        ImageContent {
            data,
            natural,
            density: 1.0,
        }
    }

    /// The picture's own size in CSS px, before the element's zoom.
    pub fn picture_size(&self) -> (f32, f32) {
        let d = if self.density > 0.0 {
            self.density
        } else {
            1.0
        };
        (self.data.width as f32 / d, self.data.height as f32 / d)
    }

    /// The natural width over height, if both are positive.
    pub fn ratio(&self) -> Option<f32> {
        let (w, h) = self.natural;
        (w > 0.0 && h > 0.0).then(|| w / h)
    }

    pub(crate) fn brush(&self, rendering: ImageRendering) -> ImageBrush<ImageData> {
        let quality = match rendering {
            ImageRendering::Pixelated => ImageQuality::Low,
            ImageRendering::Smooth => ImageQuality::High,
        };
        ImageBrush::new(self.data.clone()).with_quality(quality)
    }
}

/// Where the picture goes inside a content box `(w, h)` under `object-fit`, centered (Tern has no
/// `object-position`): `(x, y, w, h)` relative to the box. The caller clips to the box.
pub fn fit(fit: ObjectFit, natural: (f32, f32), content: (f32, f32)) -> (f32, f32, f32, f32) {
    let (nw, nh) = natural;
    let (cw, ch) = content;
    if nw <= 0.0 || nh <= 0.0 {
        return (0.0, 0.0, cw, ch);
    }
    let contain = (cw / nw).min(ch / nh);
    let (w, h) = match fit {
        ObjectFit::Fill => (cw, ch),
        ObjectFit::Contain => (nw * contain, nh * contain),
        ObjectFit::Cover => {
            let s = (cw / nw).max(ch / nh);
            (nw * s, nh * s)
        }
        ObjectFit::None => (nw, nh),
        ObjectFit::ScaleDown => {
            if contain < 1.0 {
                (nw * contain, nh * contain)
            } else {
                (nw, nh)
            }
        }
    };
    ((cw - w) / 2.0, (ch - h) / 2.0, w, h)
}

/// The used content size of a replaced box (CSS 2.1 §10.3.2 and §10.6.2, with the min/max table
/// of §10.4): `width` and `height` are the specified content sizes (`None` for `auto`), `min` and
/// `max` the resolved constraints, and `natural` the picture's size.
pub fn replaced_size(
    width: Option<f32>,
    height: Option<f32>,
    min: (f32, f32),
    max: (Option<f32>, Option<f32>),
    natural: (f32, f32),
) -> (f32, f32) {
    let (nw, nh) = natural;
    let ratio = (nw > 0.0 && nh > 0.0).then(|| nw / nh);
    let clamp_w = |w: f32| w.min(max.0.unwrap_or(f32::INFINITY)).max(min.0);
    let clamp_h = |h: f32| h.min(max.1.unwrap_or(f32::INFINITY)).max(min.1);
    match (width, height, ratio) {
        (Some(w), Some(h), _) => (clamp_w(w), clamp_h(h)),
        (Some(w), None, Some(r)) => {
            let w = clamp_w(w);
            (w, clamp_h(w / r))
        }
        (None, Some(h), Some(r)) => {
            let h = clamp_h(h);
            (clamp_w(h * r), h)
        }
        (Some(w), None, None) => (clamp_w(w), clamp_h(nh)),
        (None, Some(h), None) => (clamp_w(nw), clamp_h(h)),
        (None, None, None) => (clamp_w(nw), clamp_h(nh)),
        (None, None, Some(_)) => constrain(nw, nh, min, max),
    }
}

/// CSS 2.1 §10.4's table for a replaced box with `auto` width and height and a ratio.
fn constrain(w: f32, h: f32, min: (f32, f32), max: (Option<f32>, Option<f32>)) -> (f32, f32) {
    let (min_w, min_h) = min;
    // A max below its min is raised to the min.
    let max_w = max.0.unwrap_or(f32::INFINITY).max(min_w);
    let max_h = max.1.unwrap_or(f32::INFINITY).max(min_h);
    if w <= 0.0 || h <= 0.0 {
        return (w.clamp(min_w, max_w), h.clamp(min_h, max_h));
    }
    let over_w = w > max_w;
    let under_w = w < min_w;
    let over_h = h > max_h;
    let under_h = h < min_h;
    match (over_w, under_w, over_h, under_h) {
        (false, false, false, false) => (w, h),
        (true, _, false, false) => (max_w, (max_w * h / w).max(min_h)),
        (_, true, false, false) => (min_w, (min_w * h / w).min(max_h)),
        (false, false, true, _) => ((max_h * w / h).max(min_w), max_h),
        (false, false, _, true) => ((min_h * w / h).min(max_w), min_h),
        (true, _, true, _) => {
            if max_w / w <= max_h / h {
                (max_w, (max_w * h / w).max(min_h))
            } else {
                ((max_h * w / h).max(min_w), max_h)
            }
        }
        (_, true, _, true) => {
            if min_w / w <= min_h / h {
                ((min_h * w / h).min(max_w), min_h)
            } else {
                (min_w, (min_w * h / w).min(max_h))
            }
        }
        (_, true, true, _) => (min_w, max_h),
        (true, _, _, true) => (max_w, min_h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_fit_places_centered() {
        let n = (200.0, 100.0);
        assert_eq!(
            fit(ObjectFit::Fill, n, (100.0, 100.0)),
            (0.0, 0.0, 100.0, 100.0)
        );
        assert_eq!(
            fit(ObjectFit::Contain, n, (100.0, 100.0)),
            (0.0, 25.0, 100.0, 50.0)
        );
        assert_eq!(
            fit(ObjectFit::Cover, n, (100.0, 100.0)),
            (-50.0, 0.0, 200.0, 100.0)
        );
        assert_eq!(
            fit(ObjectFit::None, n, (100.0, 100.0)),
            (-50.0, 0.0, 200.0, 100.0)
        );
        assert_eq!(
            fit(ObjectFit::ScaleDown, n, (100.0, 100.0)),
            (0.0, 25.0, 100.0, 50.0)
        );
        assert_eq!(
            fit(ObjectFit::ScaleDown, n, (400.0, 400.0)),
            (100.0, 150.0, 200.0, 100.0)
        );
    }

    #[test]
    fn replaced_sizes_keep_the_ratio_under_constraints() {
        let n = (720.0, 240.0);
        let none = (None, None);
        assert_eq!(
            replaced_size(None, None, (0.0, 0.0), none, n),
            (720.0, 240.0)
        );
        // max-width: 100% of a 360 px container shrinks both axes.
        assert_eq!(
            replaced_size(None, None, (0.0, 0.0), (Some(360.0), None), n),
            (360.0, 120.0)
        );
        assert_eq!(
            replaced_size(None, None, (0.0, 0.0), (Some(360.0), Some(60.0)), n),
            (180.0, 60.0)
        );
        assert_eq!(
            replaced_size(Some(72.0), None, (0.0, 0.0), none, n),
            (72.0, 24.0)
        );
        assert_eq!(
            replaced_size(None, Some(24.0), (0.0, 0.0), none, n),
            (72.0, 24.0)
        );
        assert_eq!(
            replaced_size(Some(10.0), Some(20.0), (0.0, 0.0), none, n),
            (10.0, 20.0)
        );
    }
}
