//! The CoreText rasterizer: the app's native look.
//!
//! Each cluster is set as a `CTLine` in the face fontique chose, so CoreText shapes combining
//! marks and emoji sequences itself, and draws into a CoreGraphics bitmap: alpha-only for text,
//! premultiplied sRGB RGBA for color faces.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;

use objc2_core_foundation::{
    CFArray, CFAttributedString, CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString,
    CFType, CFURL, CGAffineTransform, CGFloat, kCFAllocatorNull,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGTextDrawingMode,
    kCGColorSpaceSRGB,
};
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontManagerCreateFontDescriptorsFromData,
    CTFontManagerCreateFontDescriptorsFromURL, CTLine, kCTFontAttributeName,
    kCTForegroundColorFromContextAttributeName,
};

use crate::canvas::{Canvas, GlyphFormat};
use crate::resolve::{Face, FaceId};

#[derive(Default)]
pub(crate) struct CoreTextRasterizer {
    /// Per face, the descriptor CoreText made from its file (`None` if it couldn't).
    descriptors: HashMap<FaceId, Option<CFRetained<CTFontDescriptor>>>,
    /// Per face and size (as `f32` bits).
    fonts: HashMap<(FaceId, u32), CFRetained<CTFont>>,
}

impl CoreTextRasterizer {
    fn descriptor(&mut self, face: &Face) -> Option<CFRetained<CTFontDescriptor>> {
        self.descriptors
            .entry(face.id)
            .or_insert_with(|| create_descriptor(face))
            .clone()
    }

    fn font(&mut self, face: &Face, size_px: f32) -> Option<CFRetained<CTFont>> {
        let key = (face.id, size_px.to_bits());
        if let Some(font) = self.fonts.get(&key) {
            return Some(font.clone());
        }
        let descriptor = self.descriptor(face)?;
        let matrix = face.skew.map(|deg| CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: CGFloat::from(deg.to_radians().tan()),
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        });
        let matrix_ptr = matrix
            .as_ref()
            .map_or(ptr::null(), |m| m as *const CGAffineTransform);
        // SAFETY: the descriptor is valid and the matrix pointer is null or points at a local.
        let font = unsafe {
            CTFont::with_font_descriptor(&descriptor, CGFloat::from(size_px), matrix_ptr)
        };
        self.fonts.insert(key, font.clone());
        Some(font)
    }

    fn line(&mut self, face: &Face, size_px: f32, text: &str) -> Option<CFRetained<CTLine>> {
        let font = self.font(face, size_px)?;
        let keys: [&CFString; 2] = unsafe {
            [
                kCTFontAttributeName,
                kCTForegroundColorFromContextAttributeName,
            ]
        };
        let values: [&CFType; 2] = [&font, CFBoolean::new(true)];
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        let string = CFString::from_str(text);
        // SAFETY: the string and the dictionary of CFString keys to CFType values are valid.
        let attributed =
            unsafe { CFAttributedString::new(None, Some(&string), Some(attributes.as_opaque())) }?;
        // SAFETY: a valid attributed string.
        Some(unsafe { CTLine::with_attributed_string(&attributed) })
    }
}

fn create_descriptor(face: &Face) -> Option<CFRetained<CTFontDescriptor>> {
    // SAFETY: the URL and data are valid CF objects; the no-copy data borrows the face's blob,
    // which outlives every descriptor and font made from it (the rasterizer's caches are
    // dropped before the resolver's faces, and the blob is reference counted).
    let array: CFRetained<CFArray> = unsafe {
        match &face.path {
            Some(path) => {
                let url = CFURL::from_file_path(path)?;
                CTFontManagerCreateFontDescriptorsFromURL(&url)?
            }
            None => {
                let bytes = face.blob.data();
                let data = CFData::with_bytes_no_copy(
                    None,
                    bytes.as_ptr(),
                    bytes.len() as isize,
                    kCFAllocatorNull,
                )?;
                CTFontManagerCreateFontDescriptorsFromData(&data)
            }
        }
    };
    // SAFETY: the array holds font descriptors.
    let array: CFRetained<CFArray<CTFontDescriptor>> = unsafe { CFRetained::cast_unchecked(array) };
    let mut descriptor = array.get(face.index as usize).or_else(|| array.get(0))?;
    for &(tag, value) in &face.variations {
        let id = CFNumber::new_i64(i64::from(tag));
        // SAFETY: valid descriptor and number.
        descriptor = unsafe { descriptor.copy_with_variation(&id, CGFloat::from(value)) };
    }
    Some(descriptor)
}

impl crate::Rasterizer for CoreTextRasterizer {
    fn advance(&mut self, face: &Face, size_px: f32, text: &str) -> f32 {
        let Some(line) = self.line(face, size_px, text) else {
            return 0.0;
        };
        // SAFETY: null out-pointers are allowed.
        unsafe { line.typographic_bounds(ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) as f32 }
    }

    fn draw(
        &mut self,
        face: &Face,
        size_px: f32,
        text: &str,
        pen_x: f32,
        baseline_y: f32,
        canvas: &mut Canvas,
    ) {
        let Some(line) = self.line(face, size_px, text) else {
            return;
        };
        let stride = canvas.stride();
        let (space, info) = match canvas.format {
            GlyphFormat::Mask => (None, CGImageAlphaInfo::Only.0),
            GlyphFormat::Color => (
                CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })),
                CGImageAlphaInfo::PremultipliedLast.0,
            ),
        };
        // SAFETY: the buffer is `height * stride` bytes and outlives the context, which is
        // dropped at the end of this function.
        let Some(ctx) = (unsafe {
            CGBitmapContextCreate(
                canvas.data.as_mut_ptr().cast::<c_void>(),
                canvas.width,
                canvas.height,
                8,
                stride,
                space.as_deref(),
                info,
            )
        }) else {
            return;
        };
        let c: Option<&CGContext> = Some(&ctx);
        CGContext::set_should_antialias(c, true);
        CGContext::set_allows_font_smoothing(c, false);
        CGContext::set_should_smooth_fonts(c, false);
        CGContext::set_allows_font_subpixel_positioning(c, true);
        CGContext::set_should_subpixel_position_fonts(c, true);
        CGContext::set_allows_font_subpixel_quantization(c, false);
        CGContext::set_should_subpixel_quantize_fonts(c, false);
        CGContext::set_rgb_fill_color(c, 1.0, 1.0, 1.0, 1.0);
        CGContext::set_rgb_stroke_color(c, 1.0, 1.0, 1.0, 1.0);
        if face.embolden {
            CGContext::set_text_drawing_mode(c, CGTextDrawingMode::FillStroke);
            CGContext::set_line_width(c, CGFloat::from(size_px / 30.0));
        }
        // CoreGraphics' origin is the bottom-left corner.
        let y = canvas.height as f32 - baseline_y;
        CGContext::set_text_position(c, CGFloat::from(pen_x), CGFloat::from(y));
        // SAFETY: a valid line and context.
        unsafe { line.draw(&ctx) };
    }

    fn clear(&mut self) {
        self.fonts.clear();
    }
}
