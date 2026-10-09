//! The window chrome (D3): the tab bar with its session chip and menu, pane dividers and the
//! focus ring, drawn by our UI engine as one document per window. The panes' areas stay
//! transparent so the grids show through.

use auk_ui::{Document, NodeId};

// The portable widgets and their document (tab bar, session menu, dividers and focus ring,
// status line, palette) live in auk-views so the browser client draws the same ones (§22, D3);
// this module adds the app's own overlays.
use auk_views::chrome::{self as widgets, ChromeDoc};
pub use auk_views::chrome::{
    BAR_HEIGHT, BarModel, ChromeAction, DIVIDER, PaletteView, STATUS_HEIGHT, SessionItem,
    StatusItem, TabItem,
};

const CSS: &str = r#"
.toasts { position: absolute; right: 12px; top: 40px; width: 300px; display: flex; flex-direction: column; gap: 6px; }
.toast {
  padding: 8px 10px; border-radius: 7px; background-color: #2a2d31; border: 1px solid #3a3e44;
  color: #eceef0; box-sizing: border-box;
}
.toast.toast-error { border-color: #a1403a; }
.toast.toast-success { border-color: #3f7e44; }
.toast .t-sub { color: #9da3a9; font-size: 11px; margin-top: 2px; }
html.light .toast { background-color: #fbfbfa; border-color: #d5d5d2; color: #1b1d20; }
.card {
  position: absolute; left: 50%; top: 80px; width: 360px; margin-left: -180px; padding: 12px 14px;
  border-radius: 9px; background-color: #2a2d31; border: 1px solid #a1403a; color: #eceef0;
  box-sizing: border-box; display: flex; flex-direction: column; gap: 6px;
}
.card .c-title { font-weight: 600; }
.card .c-sub { color: #9da3a9; font-size: 11px; }
.card .c-buttons { display: flex; gap: 8px; justify-content: flex-end; margin-top: 4px; }
.card .c-button { padding: 3px 10px; border-radius: 5px; border: 1px solid #3a3e44; }
.card .c-button.primary { background-color: #3a3e44; }
html.light .card { background-color: #fbfbfa; color: #1b1d20; }
.ctxmenu {
  position: absolute; min-width: 160px; padding: 4px; box-sizing: border-box;
  background-color: #23262a; border: 1px solid #3a3e44; border-radius: 7px;
}
.ctxmenu .ctx-item { padding: 4px 10px; border-radius: 4px; color: #d4d7db; white-space: nowrap; }
.ctxmenu .ctx-item.selected { background-color: #34507e; color: #ffffff; }
html.light .ctxmenu { background-color: #fbfbfa; border-color: #d5d5d2; }
html.light .ctxmenu .ctx-item { color: #1b1d20; }
/* The image viewer (§7.10): the picture and its title on an opaque card over the scrim
   (book: --card #161619 / #ffffff, --l1 hairline). Around the card, presses reach the scrim. */
.viewer {
  position: absolute; left: 0; top: 0; display: flex; align-items: center; justify-content: center;
  padding: 24px 40px; box-sizing: border-box; pointer-events: none;
}
.viewer .v-card {
  display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 6px 12px 12px;
  max-width: 100%; max-height: 100%; box-sizing: border-box; border-radius: 12px;
  background-color: #161619; border: 1px solid rgba(255, 255, 255, 0.06); pointer-events: auto;
}
.viewer .v-bar { display: flex; gap: 12px; align-items: center; color: #eceef0; padding: 4px 0 0; max-width: 100%; }
.viewer .v-btn { padding: 2px 9px; border-radius: 5px; background-color: #2a2b30; color: #eceef0; }
.viewer .v-title { color: #eceef0; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
html.light .viewer .v-card { background-color: #ffffff; border-color: rgba(22, 23, 28, 0.06); }
html.light .viewer .v-bar, html.light .viewer .v-title { color: #1b1c20; }
html.light .viewer .v-btn { background-color: #ececea; color: #1b1c20; }
.linkhint {
  position: absolute; left: 8px; max-width: 70%; padding: 2px 8px; box-sizing: border-box;
  border-radius: 5px; font-size: 11px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  background-color: #23262a; border: 1px solid #3a3e44; color: #c3c7cc;
}
.findbar {
  position: absolute; right: 14px; top: 38px; width: 300px; display: flex; gap: 8px; align-items: center;
  padding: 5px 8px; box-sizing: border-box; border-radius: 7px;
  background-color: #23262a; border: 1px solid #3a3e44;
}
.findbar .f-query { flex: 1 1 auto; min-width: 0; color: #eceef0; white-space: nowrap; overflow: hidden; }
.findbar .f-query.empty { color: #6d7177; }
.findbar .f-count { color: #8d939a; font-size: 11px; white-space: nowrap; }
.tooltip {
  position: absolute; max-width: 360px; padding: 3px 8px; box-sizing: border-box;
  border-radius: 5px; font-size: 11px; white-space: pre-wrap;
  background-color: #23262a; border: 1px solid #3a3e44; color: #d4d7db;
}
html.light .tooltip { background-color: #fbfbfa; border-color: #d5d5d2; color: #2a2d31; }
html.light .linkhint, html.light .findbar { background-color: #fbfbfa; border-color: #d5d5d2; color: #2a2d31; }
html.light .findbar .f-query { color: #1b1d20; }
"#;

/// What the chrome shows. The document is rebuilt when this changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChromeModel {
    /// The tab bar, dividers and focus ring, as the portable widgets draw them.
    pub bar: BarModel,
    /// The status line's left side: the focused pane's title.
    pub status_title: String,
    /// Plugin status segments, in plugin id order.
    pub status: Vec<StatusItem>,
    pub toasts: Vec<ToastItem>,
    pub palette: Option<PaletteView>,
    pub menu: Option<MenuView>,
    pub viewer: Option<ViewerView>,
    /// The focused pane's reporting program, in the status line.
    pub pane_app: Option<String>,
    /// The target of the link under the pointer (§7.14).
    pub hover_link: Option<String>,
    /// The `title` tooltip of the surface node under the pointer, shown below the pointer at
    /// that window point (§8.1).
    pub tooltip: Option<(String, f64, f64)>,
    pub find: Option<FindView>,
    /// A problem card with a retry ("Could not start the block", §9.5).
    pub card: Option<CardView>,
}

/// A card over the panes: a title, what went wrong, and Retry (runs `retry`) or Dismiss.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardView {
    pub title: String,
    pub sub: String,
    /// The action Retry runs.
    pub retry: String,
}

/// The find bar: the query and "current of count".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FindView {
    pub query: String,
    pub current: Option<usize>,
    pub count: usize,
}

/// A toast on screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToastItem {
    /// `info`, `success` or `error`.
    pub level: String,
    pub text: String,
    pub sub: Option<String>,
}

/// A context menu (§7.8 `menu` actions) at a point, in points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuView {
    pub x: f64,
    pub y: f64,
    pub entries: Vec<String>,
    pub selected: Option<usize>,
}

/// The image viewer (the `zoom` action): the shown image's title and place in its set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewerView {
    pub title: String,
    pub index: usize,
    pub count: usize,
    /// The shown blob, whose picture [`Chrome::set_viewer_image`] supplies.
    pub blob: String,
    /// The picture's size in CSS px.
    pub size: (f64, f64),
    /// Drawn from SVG: it scales up to fit (it is drawn again at that scale).
    pub svg: bool,
    pub open: Option<String>,
}

/// The scale the viewer shows a picture of `size` (CSS px) at in a window `window` points big:
/// it fits the card's room; a raster never grows past its own size, an SVG does (§7.10).
pub fn viewer_fit(window: (f64, f64), size: (f64, f64), svg: bool) -> f64 {
    // The viewer's padding, the card's padding and border, and the title bar above the picture.
    let (aw, ah) = ((window.0 - 106.0).max(1.0), (window.1 - 130.0).max(1.0));
    let (w, h) = (size.0.max(1.0), size.1.max(1.0));
    let k = (aw / w).min(ah / h);
    if svg { k } else { k.min(1.0) }
}

/// The chrome document.
pub struct Chrome {
    bar: ChromeDoc,
    model: Option<ChromeModel>,
    viewer_image: Option<auk_ui::ImageContent>,
    /// The `title` tooltip element, while one is drawn.
    tooltip_el: Option<NodeId>,
}

impl std::fmt::Debug for Chrome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chrome")
            .field("model", &self.model)
            .finish()
    }
}

impl Chrome {
    pub fn new(scale: f32) -> Chrome {
        Chrome {
            bar: ChromeDoc::new(scale, CSS),
            model: None,
            viewer_image: None,
            tooltip_el: None,
        }
    }

    /// The chrome's document.
    pub fn doc(&self) -> &Document {
        &self.bar.doc
    }

    /// Installs plugin sheets (`plugin:local:<id>:…`) over the chrome's own, replacing the last
    /// set. Returns whether anything changed.
    pub fn set_plugin_sheets(&mut self, sheets: &[(String, String)]) -> bool {
        let (changed, problems) = self.bar.set_plugin_sheets(sheets);
        for p in problems {
            eprintln!("auk: {p}");
        }
        changed
    }

    /// The tabs drawn with the agent icon, by tab id, with the icon's box.
    pub fn agent_badges(&self) -> Vec<(u64, auk_ui::Rect)> {
        self.bar.agent_badges()
    }

    /// The installed plugin sheet names, in order.
    pub fn plugin_sheets(&self) -> Vec<String> {
        self.bar.plugin_sheets()
    }

    /// The picture the viewer shows; the next [`update`](Self::update) puts it in place.
    pub fn set_viewer_image(&mut self, image: Option<auk_ui::ImageContent>) {
        self.viewer_image = image;
        self.model = None;
    }

    /// Rebuilds the document if `model` differs from what it shows. Returns whether it did.
    pub fn update(&mut self, model: &ChromeModel, scale: f32) -> bool {
        self.bar.doc.set_device_scale(scale);
        if self.model.as_ref() == Some(model) {
            return false;
        }
        self.bar.begin(&model.bar);
        self.overlays(model);
        self.bar.finish(&model.bar);
        self.model = Some(model.clone());
        true
    }

    /// The status line, toasts, palette, context menu and image viewer.
    fn overlays(&mut self, model: &ChromeModel) {
        // Boxes that span the window are placed in px: the document's own height is its content.
        let (w, h) = (model.bar.width, model.bar.height);
        let body = self.bar.body();
        widgets::status_line(
            &mut self.bar.doc,
            body,
            h - STATUS_HEIGHT,
            model.pane_app.as_deref(),
            &model.status_title,
            &model.status,
        );
        if let Some(link) = &model.hover_link {
            let hint = self.child(body, "div", "linkhint", Some(link));
            self.bar.doc.set_attribute(hint, "role", "tooltip");
            self.bar
                .doc
                .set_inline_style(hint, &format!("top: {}px", h - STATUS_HEIGHT - 26.0));
        }
        self.tooltip_el = None;
        if let Some((title, x, y)) = &model.tooltip {
            let tip = self.child(body, "div", "tooltip", Some(title));
            self.tooltip_el = Some(tip);
            self.bar.doc.set_attribute(tip, "role", "tooltip");
            // Below and right of the pointer, kept inside the window.
            let left = (x + 4.0).min(w - 120.0).max(4.0);
            let top = (y + 18.0).min(h - STATUS_HEIGHT - 24.0).max(4.0);
            self.bar
                .doc
                .set_inline_style(tip, &format!("left: {left}px; top: {top}px"));
        }
        if let Some(f) = &model.find {
            let bar = self.child(body, "div", "findbar", None);
            self.bar.doc.set_attribute(bar, "role", "search");
            self.bar.doc.set_attribute(bar, "aria-label", "Find");
            let (class, text) = if f.query.is_empty() {
                ("f-query empty", "Find")
            } else {
                ("f-query", f.query.as_str())
            };
            let q = self.child(bar, "div", class, Some(text));
            self.bar.doc.set_attribute(q, "role", "textbox");
            let count = match (f.count, f.current) {
                (0, _) if f.query.is_empty() => String::new(),
                (0, _) => "No matches".to_owned(),
                (n, Some(i)) => format!("{} of {n}", i + 1),
                (n, None) => format!("{n}"),
            };
            self.child(bar, "div", "f-count", Some(&count));
        }
        if !model.toasts.is_empty() {
            let stack = self.child(body, "div", "toasts", None);
            for (i, t) in model.toasts.iter().enumerate() {
                let el = self.child(stack, "div", &format!("toast toast-{}", t.level), None);
                self.bar.doc.set_attribute(el, "role", "alert");
                self.bar
                    .doc
                    .set_attribute(el, "data-action", &format!("toast:{i}"));
                self.child(el, "div", "t-text", Some(&t.text));
                if let Some(sub) = &t.sub {
                    self.child(el, "div", "t-sub", Some(sub));
                }
            }
        }
        if let Some(c) = &model.card {
            let card = self.child(body, "div", "card", None);
            self.bar.doc.set_attribute(card, "role", "alertdialog");
            self.child(card, "div", "c-title", Some(&c.title));
            self.child(card, "div", "c-sub", Some(&c.sub));
            let row = self.child(card, "div", "c-buttons", None);
            let retry = self.child(row, "div", "c-button primary", Some("Retry"));
            self.bar
                .doc
                .set_attribute(retry, "data-action", &format!("run:{}", c.retry));
            let dismiss = self.child(row, "div", "c-button", Some("Dismiss"));
            self.bar
                .doc
                .set_attribute(dismiss, "data-action", "run:card:dismiss");
        }
        if let Some(p) = &model.palette {
            widgets::palette(&mut self.bar.doc, body, w, h, p);
        }
        if let Some(m) = &model.menu {
            let back = self.child(body, "div", "backdrop", None);
            self.bar.doc.set_attribute(back, "data-action", "dismiss");
            self.bar
                .doc
                .set_inline_style(back, &format!("width: {w}px; height: {h}px"));
            let menu = self.child(body, "div", "ctxmenu", None);
            self.bar.doc.set_attribute(menu, "role", "menu");
            self.bar
                .doc
                .set_inline_style(menu, &format!("left: {}px; top: {}px", m.x, m.y));
            for (i, label) in m.entries.iter().enumerate() {
                let class = if m.selected == Some(i) {
                    "ctx-item selected"
                } else {
                    "ctx-item"
                };
                let item = self.child(menu, "div", class, Some(label));
                self.bar.doc.set_attribute(item, "role", "menuitem");
                self.bar
                    .doc
                    .set_attribute(item, "data-action", &format!("menu:{i}"));
            }
        }
        if let Some(v) = &model.viewer {
            let back = self.child(body, "div", "backdrop dim", None);
            self.bar.doc.set_attribute(back, "data-action", "dismiss");
            self.bar
                .doc
                .set_inline_style(back, &format!("width: {w}px; height: {h}px"));
            let viewer = self.child(body, "div", "viewer", None);
            self.bar
                .doc
                .set_inline_style(viewer, &format!("width: {w}px; height: {h}px"));
            let card = self.child(viewer, "div", "v-card", None);
            self.bar.doc.set_attribute(card, "role", "dialog");
            self.bar
                .doc
                .set_attribute(card, "aria-label", "Image Viewer");
            let bar = self.child(card, "div", "v-bar", None);
            if v.count > 1 {
                let prev = self.child(bar, "div", "v-btn", Some("‹"));
                self.bar
                    .doc
                    .set_attribute(prev, "data-action", "viewer:prev");
                self.bar.doc.set_attribute(prev, "role", "button");
            }
            let title = if v.count > 1 {
                format!("{} ({}/{})", v.title, v.index + 1, v.count)
            } else {
                v.title.clone()
            };
            self.child(bar, "div", "v-title", Some(&title));
            if v.count > 1 {
                let next = self.child(bar, "div", "v-btn", Some("›"));
                self.bar
                    .doc
                    .set_attribute(next, "data-action", "viewer:next");
                self.bar.doc.set_attribute(next, "role", "button");
            }
            if v.open.is_some() {
                let open = self.child(bar, "div", "v-btn", Some("Open"));
                self.bar
                    .doc
                    .set_attribute(open, "data-action", "viewer:open");
                self.bar.doc.set_attribute(open, "role", "button");
            }
            let img = self.child(card, "div", "v-img", None);
            self.bar.doc.set_attribute(img, "role", "img");
            self.bar.doc.set_attribute(img, "aria-label", &v.title);
            let k = viewer_fit((w, h), v.size, v.svg);
            let (w, h) = (v.size.0.max(1.0), v.size.1.max(1.0));
            self.bar.doc.set_inline_style(
                img,
                &format!(
                    "width: {}px; height: {}px",
                    (w * k).floor(),
                    (h * k).floor()
                ),
            );
            self.bar.doc.set_image(img, self.viewer_image.clone());
        }
    }

    /// The status line's segments as drawn: `(data-plugin, text, data-tone)`, in order.
    pub fn status_segments(&self) -> Vec<(String, String, Option<String>)> {
        let mut out = Vec::new();
        let mut stack = vec![self.bar.doc.root()];
        while let Some(n) = stack.pop() {
            if let Some(plugin) = self.bar.doc.attribute(n, "data-plugin") {
                let text: String = self
                    .bar
                    .doc
                    .children(n)
                    .iter()
                    .filter_map(|&c| self.bar.doc.node(c).text())
                    .collect();
                let tone = self.bar.doc.attribute(n, "data-tone").map(str::to_owned);
                out.push((plugin.to_owned(), text, tone));
            }
            for &c in self.bar.doc.children(n).iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// The palette's header as drawn: the program it names (`data-app`) and its text.
    pub fn palette_app(&self) -> Option<(String, String)> {
        let mut stack = vec![self.bar.doc.root()];
        while let Some(n) = stack.pop() {
            if self.bar.doc.attribute(n, "id") == Some("palette-app") {
                let app = self.bar.doc.attribute(n, "data-app")?.to_owned();
                let text: String = self
                    .bar
                    .doc
                    .children(n)
                    .iter()
                    .filter_map(|&c| self.bar.doc.node(c).text())
                    .collect();
                return Some((app, text));
            }
            stack.extend(self.bar.doc.children(n).iter().copied());
        }
        None
    }

    /// Every element with an action and its box, in points (for the automation endpoint).
    pub fn actions(&self) -> Vec<(String, auk_ui::Rect)> {
        self.bar.actions()
    }

    /// The tabs as drawn.
    pub fn tabs(&self) -> Vec<widgets::DrawnTab> {
        self.bar.tabs()
    }

    /// The elements assistive technology sees in the chrome.
    pub fn accessible(&self) -> Vec<widgets::AccessibleNode> {
        self.bar.accessible()
    }

    fn child(&mut self, parent: NodeId, tag: &str, class: &str, text: Option<&str>) -> NodeId {
        let e = self.bar.doc.create_element(tag);
        self.bar.doc.set_attribute(e, "class", class);
        if let Some(text) = text {
            let t = self.bar.doc.create_text(text);
            self.bar.doc.append_child(e, t);
        }
        self.bar.doc.append_child(parent, e);
        e
    }

    /// The tooltip drawn (role `tooltip`, not the link readout): its text and box, in points.
    pub fn tooltip(&self) -> Option<(String, auk_ui::Rect)> {
        let el = self
            .tooltip_el
            .filter(|&e| self.bar.doc.try_node(e).is_some())?;
        let text: String = self
            .bar
            .doc
            .children(el)
            .iter()
            .filter_map(|&c| self.bar.doc.node(c).text())
            .collect();
        Some((text, self.bar.doc.border_box(el)?))
    }

    /// The action under a point (in points), if any.
    pub fn action_at(&self, x: f64, y: f64) -> Option<ChromeAction> {
        self.bar.action_at(x as f32, y as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ChromeModel {
        let session = |id, name: &str, active| SessionItem {
            id,
            name: name.into(),
            active,
            tabs: 1,
            ..SessionItem::default()
        };
        let tab = |id, title: &str, active, zoomed| TabItem {
            id,
            title: title.into(),
            active,
            zoomed,
            ..TabItem::default()
        };
        ChromeModel {
            bar: BarModel {
                width: 800.0,
                height: 500.0,
                dark: true,
                sessions: vec![session(1, "brisk-otter", true), session(5, "work", false)],
                tabs: vec![tab(2, "~/src", true, false), tab(3, "vim", false, true)],
                dividers: vec![auk_ui::Rect::new(400.0, 30.0, 1.0, 470.0)],
                focus: Some(auk_ui::Rect::new(0.0, 30.0, 400.0, 470.0)),
                can_create: true,
                ..BarModel::default()
            },
            ..ChromeModel::default()
        }
    }

    fn bar_actions(c: &Chrome) -> Vec<ChromeAction> {
        let mut found = Vec::new();
        for x in (0..800).step_by(4) {
            if let Some(a) = c.action_at(f64::from(x), 15.0)
                && found.last() != Some(&a)
            {
                found.push(a);
            }
        }
        found
    }

    #[test]
    fn clicks_find_their_actions() {
        let mut c = Chrome::new(2.0);
        assert!(c.update(&model(), 2.0));
        assert!(!c.update(&model(), 2.0));
        // Find each tab's box and click its middle: two sessions, so the chip leads.
        assert_eq!(
            bar_actions(&c),
            vec![
                ChromeAction::SessionMenu,
                ChromeAction::Tab(2),
                ChromeAction::Tab(3),
                ChromeAction::NewTab,
            ]
        );
        assert_eq!(c.action_at(200.0, 200.0), None);
        // The open menu lists both sessions, then New, Rename and Lock, over a backdrop.
        let mut open = model();
        open.bar.session_menu = true;
        assert!(c.update(&open, 2.0));
        let acts: Vec<String> = c.actions().into_iter().map(|(a, _)| a).collect();
        for want in [
            "session:1",
            "session:5",
            "new-session",
            "rename-session",
            "lock-session",
            "dismiss",
        ] {
            assert!(acts.iter().any(|a| a == want), "{want} in {acts:?}");
        }
        assert_eq!(c.action_at(200.0, 300.0), Some(ChromeAction::Dismiss));
    }

    /// The chrome over bright pane content (pure yellow), rendered offscreen, 1 pixel a point.
    fn render_over_yellow(c: &Chrome, w: u32, h: u32) -> auk_render::Image {
        use auk_render::{Gpu, OFFSCREEN_FORMAT, OffscreenTarget, UiLayer, wgpu};
        let gpu = Gpu::new_blocking().expect("a Metal adapter");
        let mut scene = auk_render::vello::Scene::new();
        auk_ui::paint::paint_with_transform(
            c.doc(),
            &mut scene,
            auk_render::vello::kurbo::Affine::IDENTITY,
        );
        let mut ui = UiLayer::new(&gpu, OFFSCREEN_FORMAT).expect("a UI layer");
        ui.render(&gpu, &scene, w, h).expect("the UI layer renders");
        let target = OffscreenTarget::new(&gpu, w, h);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("chrome over yellow"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 1.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            ui.draw(&mut pass);
        }
        gpu.queue.submit([encoder.finish()]);
        target.read(&gpu).expect("readback")
    }

    /// The box of the first element whose `attr` is `value`.
    fn box_of(c: &Chrome, attr: &str, value: &str) -> auk_ui::Rect {
        let doc = c.doc();
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.attribute(n, attr) == Some(value) {
                return doc.border_box(n).expect("laid out");
            }
            stack.extend(doc.children(n).iter().copied());
        }
        panic!("no [{attr}={value}]")
    }

    /// Amendment A20: a transparent SVG zoomed over bright content shows on the opaque card,
    /// scaled up to fit, with its title on the card too, over a scrim of the appearance's color.
    #[test]
    fn the_viewer_shows_a_transparent_svg_on_an_opaque_card() {
        // A 200×100 chart: a blue square in its top-left corner, transparent elsewhere.
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"200\" height=\"100\">\
                   <rect width=\"40\" height=\"40\" fill=\"#0000ff\"/></svg>";
        let (w, h) = (800u32, 500u32);
        let near = |p: auk_render::Rgba, want: (u8, u8, u8)| {
            let d = |a: u8, b: u8| a.abs_diff(b) <= 3;
            d(p.0, want.0) && d(p.1, want.1) && d(p.2, want.2)
        };
        // `top` (alpha `a`) over yellow.
        let over_yellow = |top: (f64, f64, f64), a: f64| {
            let mix = |t: f64, u: f64| (t * a + u * (1.0 - a)).round() as u8;
            (mix(top.0, 255.0), mix(top.1, 255.0), mix(top.2, 0.0))
        };
        for (dark, card, scrim) in [
            (true, (0x16, 0x16, 0x19), over_yellow((0.0, 0.0, 0.0), 0.55)),
            (
                false,
                (0xff, 0xff, 0xff),
                over_yellow((244.0, 244.0, 241.0), 0.5),
            ),
        ] {
            let mut model = model();
            model.bar.dark = dark;
            model.bar.dividers.clear();
            model.bar.focus = None;
            model.viewer = Some(ViewerView {
                title: "Chart".into(),
                index: 0,
                count: 1,
                blob: "chart".into(),
                size: (200.0, 100.0),
                svg: true,
                open: None,
            });
            let k = viewer_fit((f64::from(w), f64::from(h)), (200.0, 100.0), true);
            assert!(k > 1.5, "an SVG scales up to fit: {k}");
            let picture = auk_views::image::decode(svg.as_bytes(), None, k as f32, "t-")
                .expect("the SVG decodes");
            let mut c = Chrome::new(1.0);
            c.set_viewer_image(picture.frames.first().map(|f| f.content.clone()));
            assert!(c.update(&model, 1.0));
            let img = box_of(&c, "role", "img");
            assert!(
                (f64::from(img.w) - 200.0 * k).abs() < 1.0,
                "drawn at the fit: {img:?}, k {k}"
            );
            let px = render_over_yellow(&c, w, h);
            let at = |x: f32, y: f32| px.pixel(x as u32, y as u32);
            // The picture's transparent part shows the card, not the pane.
            let clear = at(img.x + img.w * 0.75, img.y + img.h * 0.75);
            assert!(
                near(clear, card),
                "dark {dark}: transparent part {clear:?}, card {card:?}"
            );
            let blue = at(img.x + img.w * 0.05, img.y + img.h * 0.1);
            assert!(near(blue, (0, 0, 255)), "dark {dark}: the square {blue:?}");
            // The caption sits on the card: beside the title, the card's color.
            let card_box = box_of(&c, "role", "dialog");
            // The title bar: the card's top, above the picture.
            let bar = auk_ui::Rect::new(card_box.x, card_box.y, card_box.w, img.y - card_box.y);
            let beside = at(card_box.x + 4.0, bar.y + bar.h / 2.0);
            assert!(
                near(beside, card),
                "dark {dark}: beside the caption {beside:?}"
            );
            // Around the card, the scrim over the pane.
            let out = at(card_box.x / 2.0, card_box.y + card_box.h / 2.0);
            assert!(
                near(out, scrim),
                "dark {dark}: the scrim {out:?}, want {scrim:?}"
            );
        }
    }
}
