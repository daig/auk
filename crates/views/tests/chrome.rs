//! The portable chrome widgets (§22 `views`, D3) the app and the browser client share: the tab
//! bar, dividers and focus ring, the status line and the palette, laid out by the UI engine.

use auk_ui::{Document, Rect, SheetKind};
use auk_views::chrome::{
    self, BarModel, ChromeAction, ChromeDoc, PaletteView, SessionItem, StatusItem, TabItem,
};

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
            tabs: 2,
            ..SessionItem::default()
        },
        SessionItem {
            id: 2,
            name: "ops".into(),
            active: false,
            tabs: 1,
            ..SessionItem::default()
        },
    ];
    let tabs = [
        TabItem {
            id: 7,
            title: "zsh".into(),
            active: true,
            zoomed: false,
            app: None,
            agent: false,
        },
        TabItem {
            id: 8,
            title: "omp".into(),
            active: false,
            zoomed: true,
            app: Some("omp".into()),
            agent: false,
        },
    ];
    let bar = chrome::tab_bar(
        &mut doc,
        body,
        chrome::session_chip(&sessions),
        &tabs,
        true,
        false,
    );
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
        app: Some("omp".into()),
        query: "spl".into(),
        rows: (0..20)
            .map(|i| (format!("Split {i}"), "Layout".to_owned()))
            .collect(),
        selected: 1,
        placeholder: None,
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
    // Two sessions: the chip for the shown one opens the session menu; the bar has no
    // per-session tabs and no New Session button.
    for want in [
        "session-menu",
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
    assert!(
        !acts
            .iter()
            .any(|a| a.starts_with("session:") || a == "new-session"),
        "{acts:?}"
    );
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

/// §5.1: an agent pane's tab shows the agent icon, a badge before its title, beside the
/// program's name when one draws, and no other tab does.
#[test]
fn agent_tabs_carry_the_agent_icon() {
    let mut doc = Document::new();
    assert!(
        doc.set_stylesheet("chrome-widgets", SheetKind::BuiltIn, None, chrome::CSS)
            .is_empty()
    );
    doc.set_viewport(800.0, 500.0);
    let root = doc.root();
    let body = doc.create_element("body");
    doc.append_child(root, body);
    let tab = |id, agent, app: Option<&str>| TabItem {
        id,
        title: format!("t{id}"),
        active: id == 1,
        zoomed: false,
        app: app.map(str::to_owned),
        agent,
    };
    chrome::tab_bar(
        &mut doc,
        body,
        None,
        &[
            tab(1, false, None),
            tab(2, true, None),
            tab(3, true, Some("omp")),
        ],
        true,
        false,
    );
    doc.update();
    let badges = chrome::agent_badges(&doc);
    assert_eq!(
        badges.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![2, 3]
    );
    let tabs: Vec<(String, Rect)> = chrome::actions(&doc)
        .into_iter()
        .filter(|(a, _)| a == "tab:2" || a == "tab:3")
        .collect();
    for ((_, badge), (_, tab)) in badges.iter().zip(&tabs) {
        assert!(badge.w > 4.0 && badge.h > 4.0, "{badge:?}");
        assert!(
            badge.x >= tab.x && badge.x + badge.w <= tab.x + tab.w,
            "{badge:?} in {tab:?}"
        );
    }
    // The badge reads as "Agent", and the title follows it.
    let mut found = 0;
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if doc.attribute(n, "data-icon") == Some("agent") {
            assert_eq!(doc.attribute(n, "aria-label"), Some("Agent"));
            assert_eq!(doc.attribute(n, "role"), Some("img"));
            let tab = doc.parent(n).unwrap();
            assert_eq!(doc.attribute(tab, "data-kind"), Some("agent"));
            let kids = doc.children(tab);
            assert_eq!(kids[0], n, "the icon leads the tab");
            assert_eq!(
                kids.len(),
                2 + usize::from(doc.attribute(tab, "data-app").is_some())
            );
            found += 1;
        }
        stack.extend(doc.children(n).iter().copied());
    }
    assert_eq!(found, 2);
}

fn session(id: u64, name: &str, active: bool) -> SessionItem {
    SessionItem {
        id,
        name: name.into(),
        active,
        tabs: 1,
        ..SessionItem::default()
    }
}

fn tab(id: u64, title: &str, app: Option<&str>) -> TabItem {
    TabItem {
        id,
        title: title.into(),
        active: id == 1,
        app: app.map(str::to_owned),
        ..TabItem::default()
    }
}

fn bar(sessions: Vec<SessionItem>, tabs: Vec<TabItem>) -> BarModel {
    BarModel {
        width: 800.0,
        height: 500.0,
        dark: true,
        sessions,
        tabs,
        can_create: true,
        ..BarModel::default()
    }
}

fn class_of(doc: &Document, n: auk_ui::NodeId) -> String {
    let e = doc.node(n).element().unwrap();
    e.classes
        .iter()
        .map(|a| a.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn action_names(c: &ChromeDoc) -> Vec<String> {
    c.actions().into_iter().map(|(a, _)| a).collect()
}

/// A11: the session chip shows only when it says something (more than one session, a locked
/// session, or one the user named), and opens the session menu.
#[test]
fn the_session_chip_shows_only_when_it_says_something_and_opens_the_session_menu() {
    let mut c = ChromeDoc::new(2.0, "");
    // One session with its generated name: tabs only.
    assert!(c.update(&bar(
        vec![session(1, "brisk-otter", true)],
        vec![tab(1, "zsh", None)]
    )));
    let acts = action_names(&c);
    assert_eq!(acts, ["tab:1", "new-tab"], "{acts:?}");
    // Locked, or named by the user: the chip, with the lock drawn.
    let mut locked = session(1, "brisk-otter", true);
    locked.locked = true;
    c.update(&bar(vec![locked], vec![tab(1, "zsh", None)]));
    assert_eq!(action_names(&c)[0], "session-menu");
    assert!(
        c.accessible()
            .iter()
            .any(|n| n.role == "button" && n.label == "Session brisk-otter, locked")
    );
    let mut named = session(1, "work", true);
    named.named = true;
    c.update(&bar(vec![named], vec![tab(1, "zsh", None)]));
    assert_eq!(action_names(&c)[0], "session-menu");
    // Two sessions: the chip names the shown one; a click on it asks for the menu.
    let two = bar(
        vec![
            session(1, "brisk-otter", true),
            session(2, "calm-heron", false),
        ],
        vec![tab(1, "zsh", None)],
    );
    c.update(&two);
    let chip = c.actions()[0].1;
    assert!(chip.w > 40.0 && chip.y < 8.0, "{chip:?}");
    assert_eq!(
        c.action_at(chip.x + chip.w / 2.0, chip.y + chip.h / 2.0),
        Some(ChromeAction::SessionMenu)
    );
    // Open: a row per session (the shown one checked), then New, Rename and Lock, under the chip.
    let mut open = two.clone();
    open.session_menu = true;
    c.update(&open);
    let rows: Vec<(String, Rect)> = c
        .actions()
        .into_iter()
        .filter(|(a, _)| a.starts_with("session:") || a.ends_with("-session"))
        .collect();
    let names: Vec<&str> = rows.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(
        names,
        [
            "session:1",
            "session:2",
            "new-session",
            "rename-session",
            "lock-session"
        ]
    );
    assert!(rows[0].1.y >= chip.y + chip.h, "{rows:?} under {chip:?}");
    let current: Vec<_> = c
        .accessible()
        .into_iter()
        .filter(|n| n.role == "menuitemradio")
        .map(|n| (n.label, n.selected))
        .collect();
    assert_eq!(current.len(), 2);
    assert!(
        current[0].0.starts_with("brisk-otter") && current[0].1,
        "{current:?}"
    );
    assert!(!current[1].1);
    let r = rows[1].1;
    assert_eq!(
        c.action_at(r.x + 4.0, r.y + r.h / 2.0),
        Some(ChromeAction::Session(2))
    );
    // A host that can't create (the browser) gets neither `+` nor the menu's New/Rename/Lock.
    open.can_create = false;
    c.update(&open);
    let acts = action_names(&c);
    assert!(acts.iter().any(|a| a == "session:2"), "{acts:?}");
    assert!(
        !acts
            .iter()
            .any(|a| a == "new-tab" || a.ends_with("-session")),
        "{acts:?}"
    );
}

/// A11: a tab whose program's badge says the same as its title shows the badge alone; its
/// accessible name is still the program and the title.
#[test]
fn a_tab_whose_title_is_its_app_shows_the_badge_only() {
    let mut c = ChromeDoc::new(2.0, "");
    c.update(&bar(
        vec![session(1, "a", true)],
        vec![
            tab(1, "omp", Some("omp")),
            tab(2, "OMP ", Some("omp")),
            tab(3, "omp \u{2014} fixing tests", Some("omp")),
            tab(4, "vim", None),
        ],
    ));
    let mut seen = Vec::new();
    let mut stack = vec![c.doc.root()];
    while let Some(n) = stack.pop() {
        if let Some(a) = c.doc.attribute(n, "data-action")
            && a.starts_with("tab:")
        {
            let classes: Vec<String> = c
                .doc
                .element_children(n)
                .iter()
                .map(|&k| class_of(&c.doc, k))
                .collect();
            seen.push((
                a.to_owned(),
                classes,
                c.doc.attribute(n, "aria-label").unwrap().to_owned(),
            ));
        }
        for &k in c.doc.children(n).iter().rev() {
            stack.push(k);
        }
    }
    assert_eq!(
        seen,
        [
            (
                "tab:1".into(),
                vec!["tab-app".to_owned()],
                "omp \u{2014} omp".into()
            ),
            (
                "tab:2".into(),
                vec!["tab-app".to_owned()],
                "omp \u{2014} OMP ".into()
            ),
            (
                "tab:3".into(),
                vec!["tab-app".to_owned(), "tab-title".to_owned()],
                "omp \u{2014} omp \u{2014} fixing tests".into()
            ),
            ("tab:4".into(), vec![], "vim".into()),
        ]
    );
    assert_eq!(chrome::tab_label("", Some("omp")), None);
    assert_eq!(chrome::tab_label("zsh", None), Some("zsh"));
}

/// A12: the chrome document hosts share rebuilds only when its model changes, and plugin
/// sheets restyle its widgets.
#[test]
fn the_chrome_doc_rebuilds_on_change_and_takes_plugin_sheets() {
    let mut c = ChromeDoc::new(2.0, "");
    let one = bar(vec![session(1, "a", true)], vec![tab(1, "zsh", None)]);
    assert!(c.update(&one));
    assert!(!c.update(&one), "the same model keeps the document");
    let mut two = one.clone();
    two.tabs.push(tab(2, "vim", None));
    assert!(c.update(&two));
    assert_eq!(c.shown(), Some(&two));
    let tab_box = |c: &ChromeDoc| {
        c.actions()
            .into_iter()
            .find(|(a, _)| a == "tab:2")
            .unwrap()
            .1
    };
    let before = tab_box(&c);
    let (changed, problems) = c.set_plugin_sheets(&[(
        "plugin:local:wide:chrome".to_owned(),
        ".tab { padding: 3px 40px; }".to_owned(),
    )]);
    assert!(changed && problems.is_empty(), "{problems:?}");
    let after = tab_box(&c);
    assert!(after.w > before.w + 40.0, "{before:?} -> {after:?}");
    assert_eq!(c.plugin_sheets(), ["plugin:local:wide:chrome"]);
    // The dividers and the focus ring come with the bar.
    let mut split = two.clone();
    split.dividers = vec![Rect::new(400.0, 30.0, 1.0, 470.0)];
    split.focus = Some(Rect::new(0.0, 30.0, 400.0, 470.0));
    c.update(&split);
    let kids = c.doc.element_children(c.body());
    assert_eq!(c.doc.border_box(kids[1]), Some(split.dividers[0]));
    assert_eq!(class_of(&c.doc, kids[2]), "focus");
}
