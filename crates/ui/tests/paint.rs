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
    let mut scene = vello::Scene::new();
    paint(doc, &mut scene);
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
            &scene,
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
