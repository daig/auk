//! Paint: build a Vello scene from a laid-out document and render it offscreen with wgpu, then
//! check pixels. Also hit testing.

mod common;

use auk_ui::{hit_test, paint};

const PAGE: &str = r#"<!DOCTYPE html><html><head><style>
html { font: 20px/1 Ahem; }
body { margin: 0; background: white; }
.a { position: absolute; left: 10px; top: 10px; width: 40px; height: 30px; background: rgb(255, 0, 0); }
.b { position: absolute; left: 60px; top: 10px; width: 40px; height: 30px; border: 5px solid rgb(0, 0, 255); }
.t { position: absolute; left: 10px; top: 60px; color: rgb(0, 128, 0); }
.clip { position: absolute; left: 120px; top: 10px; width: 20px; height: 20px; overflow: hidden; }
.clip div { width: 100px; height: 100px; background: rgb(255, 0, 255); }
.half { position: absolute; left: 10px; top: 100px; width: 20px; height: 20px; background: black; opacity: 0.5; }
</style></head><body><div class="a"></div><div class="b"></div><div class="t">X</div>
<div class="clip"><div></div></div><div class="half"></div></body></html>"#;

fn render(doc: &auk_ui::Document, w: u32, h: u32) -> Option<Vec<u8>> {
    let mut scene = vello::Scene::new();
    paint(doc, &mut scene);
    render_scene(&scene, w, h)
}

fn render_scene(scene: &vello::Scene, w: u32, h: u32) -> Option<Vec<u8>> {
    let instance =
        vello::wgpu::Instance::new(vello::wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(
        instance.request_adapter(&vello::wgpu::RequestAdapterOptions::default()),
    )
    .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&vello::wgpu::DeviceDescriptor::default()))
            .ok()?;
    let mut renderer = vello::Renderer::new(&device, vello::RendererOptions::default()).ok()?;
    let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
        label: Some("target"),
        size: vello::wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: vello::wgpu::TextureDimension::D2,
        format: vello::wgpu::TextureFormat::Rgba8Unorm,
        usage: vello::wgpu::TextureUsages::STORAGE_BINDING | vello::wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    renderer
        .render_to_texture(
            &device,
            &queue,
            scene,
            &view,
            &vello::RenderParams {
                base_color: vello::peniko::Color::new([1.0, 1.0, 1.0, 1.0]),
                width: w,
                height: h,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .ok()?;
    let padded = (w * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&vello::wgpu::BufferDescriptor {
        label: None,
        size: (padded * h) as u64,
        usage: vello::wgpu::BufferUsages::MAP_READ | vello::wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        texture.as_image_copy(),
        vello::wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: vello::wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        vello::wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(vello::wgpu::MapMode::Read, |_| {});
    device
        .poll(vello::wgpu::PollType::wait_indefinitely())
        .ok()?;
    let data = slice.get_mapped_range().ok()?;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let start = (row * padded) as usize;
        out.extend_from_slice(&data[start..start + (w * 4) as usize]);
    }
    Some(out)
}

#[test]
fn paints_backgrounds_borders_text_clips_and_opacity() {
    let (doc, _) = common::our_layout(PAGE, 200.0, 150.0);
    let Some(px) = render(&doc, 200, 150) else {
        panic!("no wgpu adapter for offscreen rendering");
    };
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * 200 + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    assert_eq!(at(30, 25), [255, 0, 0, 255], "background");
    assert_eq!(at(62, 20), [0, 0, 255, 255], "border");
    assert_eq!(at(80, 25), [255, 255, 255, 255], "inside the border");
    // Ahem's X is a full em square: the glyph covers the text box.
    assert_eq!(at(20, 70), [0, 128, 0, 255], "text");
    assert_eq!(at(130, 20), [255, 0, 255, 255], "clipped child inside");
    assert_eq!(
        at(150, 20),
        [255, 255, 255, 255],
        "clipped child outside the clip"
    );
    let half = at(20, 110);
    assert!((120..=135).contains(&half[0]), "opacity layer {half:?}");
}

#[test]
fn hit_testing_finds_the_deepest_box() {
    let (doc, body) = common::our_layout(PAGE, 200.0, 150.0);
    let kids = doc.element_children(body);
    assert_eq!(hit_test(&doc, 30.0, 25.0), Some(kids[0]));
    assert_eq!(hit_test(&doc, 80.0, 25.0), Some(kids[1]));
    assert_eq!(hit_test(&doc, 15.0, 65.0), Some(kids[2]));
    let clip_child = doc.element_children(kids[3])[0];
    assert_eq!(hit_test(&doc, 130.0, 20.0), Some(clip_child));
    // Outside the clip the child isn't hit; the body has no height here, so the root is.
    assert_eq!(hit_test(&doc, 160.0, 60.0), Some(doc.root()));
}

#[test]
fn text_overflow_ellipsis_replaces_the_overflowing_glyphs() {
    // Ahem has no "…"; use a system font for this one.
    let page = r#"<!DOCTYPE html><html><head><style>
    body { margin: 0; background: white; }
    .e { width: 60px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font: 20px Menlo; color: black; }
    </style></head><body><div class="e">MMMMMMMMMMMM</div></body></html>"#;
    let (doc, _) = common::our_layout(page, 100.0, 40.0);
    let Some(px) = render(&doc, 100, 40) else {
        panic!("no wgpu adapter")
    };
    let dark = |x0: u32, x1: u32| {
        (x0..x1).any(|x| (0..40).any(|y| px[((y * 100 + x) * 4) as usize] < 128))
    };
    assert!(dark(0, 20), "text drawn");
    assert!(!dark(61, 100), "nothing past the box");
}

#[test]
fn plugin_sheet_data_url_backgrounds_paint_tiled_and_program_sheets_drop_url() {
    use auk_ui::{ImageContent, SheetKind};
    use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
    let mut doc = auk_ui::Document::new();
    doc.set_device_scale(1.0);
    let root = doc.root();
    let body = doc.create_element("body");
    doc.append_child(root, body);
    let a = doc.create_element("div");
    doc.set_attribute(a, "class", "pic");
    doc.append_child(body, a);
    let scoped = doc.create_element("div");
    doc.set_attribute(scoped, "data-scope", "sf1");
    doc.append_child(body, scoped);
    let b = doc.create_element("div");
    doc.set_attribute(b, "class", "pic2");
    doc.append_child(scoped, b);
    // A 2×1 picture, red then blue, for any data: URL the loader sees.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    doc.set_url_loader(Box::new(move |url| {
        log.lock().unwrap().push(url.to_owned());
        let px = vec![255, 0, 0, 255, 0, 0, 255, 255];
        Some(ImageContent::new(ImageData {
            data: Blob::from(px),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 1,
        }))
    }));
    doc.set_stylesheet(
        "base",
        SheetKind::BuiltIn,
        None,
        "body { margin: 0; background: white } div { height: 10px; width: 40px }",
    );
    let url = "data:image/png;base64,AAAA";
    let errors = doc.set_stylesheet(
        "plugin:local:p:styles",
        SheetKind::Plugin,
        None,
        &format!(".pic {{ background-image: url(\"{url}\"); image-rendering: pixelated }} .bad {{ background-image: url(x.png) }}"),
    );
    assert_eq!(
        errors.len(),
        1,
        "a file url() never reaches the engine: {errors:?}"
    );
    assert!(errors[0].contains("url()"), "{errors:?}");
    let errors = doc.set_stylesheet(
        "tsp:sf1:main",
        SheetKind::Program,
        Some("sf1"),
        &format!(".pic2 {{ background-image: url(\"{url}\") }}"),
    );
    assert_eq!(errors.len(), 1, "program sheets drop url(): {errors:?}");
    doc.set_viewport(64.0, 32.0);
    doc.update();
    assert_eq!(
        doc.computed_style(a).unwrap().background_image.as_deref(),
        Some(url)
    );
    assert_eq!(doc.computed_style(b).unwrap().background_image, None);
    assert_eq!(*seen.lock().unwrap(), [url], "each URL loads once");
    let Some(px) = render(&doc, 64, 32) else {
        panic!("no wgpu adapter for offscreen rendering");
    };
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * 64 + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    // Repeated at its own size over the box, clipped to it.
    assert_eq!(at(0, 5), [255, 0, 0, 255]);
    assert_eq!(at(1, 5), [0, 0, 255, 255]);
    assert_eq!(at(38, 5), [255, 0, 0, 255]);
    assert_eq!(at(39, 5), [0, 0, 255, 255]);
    assert_eq!(at(45, 5), [255, 255, 255, 255], "nothing past the box");
    assert_eq!(
        at(5, 15),
        [255, 255, 255, 255],
        "the program sheet's url() was dropped"
    );
}

const SHADOWS: &str = r#"<!DOCTYPE html><html><head><style>
body { margin: 0; background: white; }
div { position: absolute; background: rgb(0, 0, 255); }
.outer { left: 20px; top: 20px; width: 40px; height: 40px; box-shadow: 10px 10px rgb(255, 0, 0); }
.spread { left: 100px; top: 20px; width: 20px; height: 20px; box-shadow: 0 0 0 5px rgb(0, 128, 0); }
.inset { left: 150px; top: 20px; width: 40px; height: 40px; border: 2px solid black;
         box-shadow: inset 4px 0 0 rgb(255, 0, 0), inset 0 0 0 2px rgb(0, 128, 0); }
.round { left: 220px; top: 20px; width: 40px; height: 40px; border-radius: 20px;
         box-shadow: 0 0 0 4px rgb(255, 0, 0); }
.blur { left: 20px; top: 100px; width: 40px; height: 40px; box-shadow: 0 0 10px rgb(255, 0, 0); }
.iblur { left: 100px; top: 100px; width: 60px; height: 60px; box-shadow: inset 0 0 10px rgb(255, 0, 0); }
.faded { left: 200px; top: 100px; width: 20px; height: 20px; opacity: 0.5; box-shadow: 10px 0 black; }
.multi { left: 20px; top: 180px; width: 20px; height: 20px;
         box-shadow: 4px 0 rgb(0, 128, 0), 8px 0 rgb(255, 0, 0); }
.turned { left: 260px; top: 100px; width: 20px; height: 20px; transform: rotate(90deg);
          box-shadow: 10px 0 rgb(255, 0, 0); }
</style></head><body><div class="outer"></div><div class="spread"></div><div class="inset"></div>
<div class="round"></div><div class="blur"></div><div class="iblur"></div><div class="faded"></div>
<div class="multi"></div><div class="turned"></div></body></html>"#;

/// A7: outer shadows (offset, spread, radii, several, under an opacity layer) draw outside the
/// border box and never inside it; inset shadows draw inside the padding box over the
/// background, under the border. The first shadow in the list is on top.
#[test]
fn box_shadows_paint_outer_inset_spread_and_stacked() {
    let (doc, _) = common::our_layout(SHADOWS, 300.0, 220.0);
    let Some(px) = render(&doc, 300, 220) else {
        panic!("no wgpu adapter for offscreen rendering");
    };
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * 300 + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 128, 0, 255];
    // Offset: the shadow shows past the box's bottom-right, under the box elsewhere.
    assert_eq!(at(65, 65), RED, "outer shadow past the box");
    assert_eq!(at(65, 25), WHITE, "the shadow moved down");
    assert_eq!(at(25, 65), WHITE, "the shadow moved right");
    assert_eq!(at(55, 55), BLUE, "the box covers its shadow");
    // Spread grows the shadow on every side.
    assert_eq!(at(97, 30), GREEN, "spread left");
    assert_eq!(at(122, 30), GREEN, "spread right");
    assert_eq!(at(110, 17), GREEN, "spread top");
    assert_eq!(at(93, 30), WHITE, "past the spread");
    // Inset: over the background inside the padding box, the border on top; the first shadow
    // is on top of the second.
    assert_eq!(at(151, 40), [0, 0, 0, 255], "the border");
    assert_eq!(at(153, 40), RED, "the stripe (first) is over the ring");
    assert_eq!(at(155, 40), RED, "the stripe is 4px");
    assert_eq!(at(157, 40), BLUE, "the background past the stripe");
    assert_eq!(at(191, 40), GREEN, "the ring on the right");
    assert_eq!(at(170, 23), GREEN, "the ring along the top");
    assert_eq!(at(170, 40), BLUE, "the background inside");
    assert_eq!(at(147, 40), WHITE, "inset shadows stay inside");
    // Radii: a ring around a circle follows it, and leaves the corner square empty.
    assert_eq!(at(240, 18), RED, "ring above the circle");
    assert_eq!(at(218, 18), WHITE, "the corner stays round");
    // Under opacity: the shadow is in the layer, half black over white.
    let faded = at(225, 110);
    assert!((120..=135).contains(&faded[0]), "faded shadow {faded:?}");
    // Several shadows: the first on top.
    assert_eq!(at(42, 190), GREEN, "first shadow on top");
    assert_eq!(at(46, 190), RED, "second shadow past it");
    // Transformed with its box: rotated a quarter turn, the shadow's x offset points down.
    assert_eq!(at(270, 125), RED, "the turned shadow below the box");
    assert_eq!(at(285, 110), WHITE, "not to the right");
    assert_eq!(at(270, 110), BLUE, "the box over it");
}

/// A7: blur softens the shadow's edge (Gaussian, standard deviation half the blur radius), and an
/// inset blur fades inward without taking out the background.
#[test]
fn blurred_box_shadows_fade_with_distance() {
    let (doc, _) = common::our_layout(SHADOWS, 300.0, 220.0);
    let Some(px) = render(&doc, 300, 220) else {
        panic!("no wgpu adapter for offscreen rendering");
    };
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * 300 + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    // Outer: the box spans x 20..60; going left from its edge, red fades to white.
    let g = |x: u32| at(x, 120)[1];
    assert_eq!(at(18, 120)[0], 255, "red over white");
    assert!(
        g(18) < g(14) && g(14) < g(8),
        "fades: {} {} {}",
        g(18),
        g(14),
        g(8)
    );
    assert!(
        (80..=200).contains(&g(18)),
        "partial near the edge: {}",
        g(18)
    );
    assert!(g(4) >= 250, "gone well past the blur: {}", g(4));
    assert_eq!(at(40, 120), [0, 0, 255, 255], "not inside the box");
    // Inset: the box spans x 100..160; red near the edge, fading inward to the background.
    let r = |x: u32| at(x, 130)[0];
    assert!(
        r(101) > r(105) && r(105) > r(110),
        "fades: {} {} {}",
        r(101),
        r(105),
        r(110)
    );
    assert!(r(100) >= 80, "strong at the edge: {}", r(100));
    assert_eq!(
        at(130, 130),
        [0, 0, 255, 255],
        "the background in the middle"
    );
    assert_eq!(at(98, 130), [255, 255, 255, 255], "nothing outside");
}

/// A7: culling counts a box's outer shadows, so a shadow that reaches into view is painted
/// though its box is far outside it.
#[test]
fn culling_keeps_boxes_whose_shadows_reach_into_view() {
    let page = r#"<!DOCTYPE html><html><head><style>
    body { margin: 0; background: white; }
    .far { position: absolute; left: 10px; top: 10px; width: 20px; height: 20px;
           background: rgb(0, 0, 255); box-shadow: 0 300px rgb(255, 0, 0); }
    </style></head><body><div class="far"></div></body></html>"#;
    let (doc, _) = common::our_layout(page, 100.0, 400.0);
    let mut scene = vello::Scene::new();
    let view = auk_ui::Rect::new(0.0, 300.0, 100.0, 50.0);
    auk_ui::paint::paint_except(
        &doc,
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &[],
        Some(view),
    );
    let Some(px) = render_scene(&scene, 100, 400) else {
        panic!("no wgpu adapter for offscreen rendering");
    };
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * 100 + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    assert_eq!(
        at(20, 320),
        [255, 0, 0, 255],
        "the shadow in view is painted"
    );
}
