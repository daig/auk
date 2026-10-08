//! The portable chrome widgets (§22 `views`, D3) the app and the browser client share: the tab
//! bar, dividers and focus ring, the status line and the palette, laid out by the UI engine.

use auk_ui::{Document, Rect, SheetKind};
use auk_views::chrome::{self, PaletteView, SessionItem, StatusItem, TabItem};

#[test]
fn tab_bar_dividers_status_line_and_palette_lay_out_with_actions_and_roles() {
    let mut doc = Document::new();
    assert!(
        doc.set_stylesheet("chrome-widgets", SheetKind::BuiltIn, None, chrome::CSS)
            .is_empty()
    );
    doc.set_viewport(800.0, 500.0);
    let root = doc.root();
    let body = doc.create_element("body");
    doc.append_child(root, body);
    doc.set_inline_style(body, "margin: 0");
    let sessions = [
        SessionItem {
            id: 1,
            name: "main".into(),
            active: true,
        },
        SessionItem {
            id: 2,
            name: "ops".into(),
            active: false,
        },
    ];
    let tabs = [
        TabItem {
            id: 7,
            title: "zsh".into(),
            active: true,
            zoomed: false,
            app: None,
        },
        TabItem {
            id: 8,
            title: "omp".into(),
            active: false,
            zoomed: true,
            app: Some("omp".into()),
        },
    ];
    let bar = chrome::tab_bar(&mut doc, body, &sessions, &tabs);
    let split = [Rect::new(400.0, 30.0, 1.0, 448.0)];
    chrome::dividers(
        &mut doc,
        body,
        &split,
        Some(Rect::new(401.0, 30.0, 399.0, 448.0)),
    );
    let segs = [StatusItem {
        plugin: "longrun".into(),
        text: "cargo · 3s".into(),
        tone: Some("muted".into()),
        command: Some("plugin.longrun.show".into()),
        ..Default::default()
    }];
    let line = chrome::status_line(
        &mut doc,
        body,
        500.0 - chrome::STATUS_HEIGHT,
        Some("omp"),
        "~/src",
        &segs,
    );
    let pal = PaletteView {
        query: "spl".into(),
        rows: (0..20)
            .map(|i| (format!("Split {i}"), "Layout".to_owned()))
            .collect(),
        selected: 1,
    };
    let palette = chrome::palette(&mut doc, body, 800.0, 500.0, &pal);
    doc.update();

    let b = doc.border_box(bar).unwrap();
    assert_eq!(
        (b.x, b.y, b.w, b.h),
        (0.0, 0.0, 800.0, chrome::BAR_HEIGHT as f32)
    );
    assert_eq!(doc.attribute(bar, "role"), Some("toolbar"));
    let l = doc.border_box(line).unwrap();
    assert_eq!((l.y, l.h, l.w), (478.0, 22.0, 800.0));
    let p = doc.border_box(palette).unwrap();
    assert_eq!((p.x, p.w), (140.0, 520.0), "centered");
    let acts: Vec<String> = chrome::actions(&doc).into_iter().map(|(a, _)| a).collect();
    for want in [
        "session:1",
        "session:2",
        "new-session",
        "tab:7",
        "tab:8",
        "new-tab",
        "run:plugin.longrun.show",
        "dismiss",
        "palette:0",
        "palette:13",
    ] {
        assert!(acts.iter().any(|a| a == want), "{want} in {acts:?}");
    }
    assert!(!acts.iter().any(|a| a == "palette:14"), "at most 14 rows");
    // Topmost wins: over the palette a click picks its row, elsewhere the backdrop dismisses.
    let row1 = chrome::actions(&doc)
        .into_iter()
        .find(|(a, _)| a == "palette:1")
        .unwrap()
        .1;
    assert_eq!(
        chrome::action_at(&doc, row1.x + 5.0, row1.y + 5.0).as_deref(),
        Some("palette:1")
    );
    assert_eq!(
        chrome::action_at(&doc, 20.0, 300.0).as_deref(),
        Some("dismiss")
    );
    // The divider and the focus ring sit where they were placed, after the bar.
    let kids = doc.element_children(body);
    assert_eq!(doc.border_box(kids[1]), Some(split[0]));
    assert_eq!(
        doc.border_box(kids[2]),
        Some(Rect::new(401.0, 30.0, 399.0, 448.0))
    );
    // No ring around a lone pane.
    let lone = doc.create_element("div");
    doc.append_child(body, lone);
    chrome::dividers(
        &mut doc,
        lone,
        &[],
        Some(Rect::new(0.0, 30.0, 800.0, 448.0)),
    );
    assert!(doc.element_children(lone).is_empty());
    // A tab whose pane reports its program shows it; light look through `html.light`.
    let dark_bg = doc.computed_style(bar).unwrap().background_color.to_rgba8();
    doc.set_attribute(root, "class", "light");
    doc.update();
    assert_ne!(
        doc.computed_style(bar).unwrap().background_color.to_rgba8(),
        dark_bg
    );
}
