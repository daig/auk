//! Pointer actions, forms and native editing (§7.8, §7.9): view input becomes local flips and
//! [`ViewEvent`]s. Keys are never events, except `edit`, `undo` and `send` behind the program's
//! features; everything else returns [`Outcome::Keys`] so the host sends PTY bytes.

use crate::common::{self, Actions, bool_prop, str_prop};
use crate::kinds::{el, input};
use crate::state::{Preedit, Selection, SurfaceState, state_key};
use crate::view::PaneView;
use auk_tsp::{Mods, NodeRef, Surface, SurfaceKey, TspPane, ViewEvent};
use auk_ui::{Document, ElementState, NodeId};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Button {
    #[default]
    Primary,
    Secondary,
}

/// A pointer press, in document coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    /// 1 for a click, 2 for a double click.
    pub clicks: u8,
    pub mods: Mods,
    pub button: Button,
}

/// What the host does after view input.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Send this event through [`TspPane::view_event`] (as a record from the primary).
    Event {
        surface: SurfaceKey,
        sf: String,
        event: Box<ViewEvent>,
    },
    /// Put text on the clipboard.
    Copy(String),
    /// Open a link or path.
    Open(String),
    /// Show an image in the viewer (the `zoom` action).
    Zoom(Box<Zoom>),
    /// The input is keys: send them to the PTY.
    Keys,
    /// Only the view changed.
    Redraw,
}

/// What the image viewer shows (book:5957): the clicked image and, when its parent holds other
/// blob images (a message's attachments), all of them in order, starting at `index`.
#[derive(Clone, Debug, PartialEq)]
pub struct Zoom {
    pub surface: SurfaceKey,
    pub images: Vec<ZoomImage>,
    pub index: usize,
}

/// One image the viewer can step to.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoomImage {
    /// The node.
    pub id: String,
    /// Its blob (builtin images never zoom), or a picture the terminal made: a mermaid
    /// diagram's id (`mermaid:…`), drawn by [`crate::image::load_made_for_viewer`].
    pub blob: String,
    /// The viewer title: `alt`, else `title`.
    pub title: Option<String>,
    /// A page or file the viewer may offer to open (`href` first, then `path`).
    pub open: Option<String>,
}

/// A context menu entry: its label and the action it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: String,
    pub action: String,
}

/// The node under a point: its surface, id and the element path from the hit up.
struct Hit {
    surface: SurfaceKey,
    id: String,
    /// Elements from the hit element up to the node element (inner parts first).
    parts: Vec<NodeId>,
}

impl PaneView {
    fn surface_for_element(&self, doc: &Document, mut el: NodeId) -> Option<SurfaceKey> {
        loop {
            if let Some(scope) = doc.attribute(el, "data-scope")
                && let Some(sv) = self.surfaces.iter().find(|s| s.scope == scope)
            {
                return Some(sv.key);
            }
            el = doc.parent(el)?;
        }
    }

    /// Every node element containing the hit, innermost first.
    fn hits(&self, doc: &Document, x: f32, y: f32) -> Vec<Hit> {
        let Some(mut el) = auk_ui::hit_test(doc, x, y) else {
            return Vec::new();
        };
        let Some(surface) = self.surface_for_element(doc, el) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut parts = Vec::new();
        loop {
            if doc.node(el).is_element() {
                parts.push(el);
                if let Some(id) = doc.attribute(el, "data-id") {
                    out.push(Hit {
                        surface,
                        id: id.to_owned(),
                        parts: std::mem::take(&mut parts),
                    });
                }
                if doc.attribute(el, "data-scope").is_some() {
                    break;
                }
            }
            match doc.parent(el) {
                Some(p) => el = p,
                None => break,
            }
        }
        out
    }

    fn class_has(&self, id: NodeId, class: &str) -> bool {
        self.written
            .class
            .get(&id)
            .is_some_and(|c| c.split(' ').any(|x| x == class))
    }

    /// A pointer press (§7.8). Local flips apply at once; the returned outcomes tell the host
    /// what to send or do.
    pub fn click(&mut self, doc: &mut Document, pane: &TspPane, ev: PointerEvent) -> Vec<Outcome> {
        // A modal backdrop takes the pointer: the press reaches nothing beneath and sends no
        // event (book: overlay "Keys").
        if auk_ui::hit_test(doc, ev.x, ev.y).is_some_and(|el| self.class_has(el, "sf-ov-dim")) {
            return vec![Outcome::Redraw];
        }
        if ev.button != Button::Primary {
            return Vec::new();
        }
        let hits = self.hits(doc, ev.x, ev.y);
        for hit in hits {
            let Some(surface) = pane.surface(hit.surface) else {
                continue;
            };
            let Some(node) = surface.document().get(&hit.id) else {
                continue;
            };
            // Links: ⌘-click, or a plain click under the setting (not with ⇧).
            for &p in &hit.parts {
                if let Some(h) = doc.attribute(p, "data-href").map(str::to_owned)
                    && self.class_has(p, "sf-link")
                    && (ev.mods.meta || (self.settings.links_plain_click && !ev.mods.shift))
                {
                    return vec![Outcome::Open(h)];
                }
            }
            if let Some(out) = self.click_part(doc, pane, surface, node, &hit, ev) {
                return out;
            }
            if let Some(out) = self.click_node(doc, pane, surface, node, &hit, ev) {
                return out;
            }
        }
        Vec::new()
    }

    /// Clicks on terminal-owned parts: fold heads, "more" buttons, tree twisties, tabs, copy.
    fn click_part(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        surface: &Surface,
        node: NodeRef,
        hit: &Hit,
        ev: PointerEvent,
    ) -> Option<Vec<Outcome>> {
        let key = surface.key();
        if node.kind() == "picker"
            && let Some(press) = crate::kinds::picker::press(doc, node, &hit.parts)
        {
            return Some(self.picker_press(doc, pane, surface, node, press, ev));
        }
        if node.kind() == "prefs" {
            let events = crate::kinds::prefs::click(doc, surface, node, &hit.parts)?;
            return Some(events.into_iter().map(|e| self.event(surface, e)).collect());
        }
        for &p in &hit.parts {
            let class = self.written.class.get(&p).cloned().unwrap_or_default();
            let has = |c: &str| class.split(' ').any(|x| x == c);
            if (has("sf-card-head")
                || has("sf-section-head")
                || has("sf-tool-head")
                || has("sf-agent-row"))
                && bool_prop(node.props(), "collapsible")
                && !ev.mods.meta
            {
                return Some(self.toggle_fold(doc, pane, key, node));
            }
            if has("sf-more") || has("sf-tool-more") {
                return Some(self.toggle_fold(doc, pane, key, node));
            }
            if has("sf-ansi-more") {
                let k = state_key(node);
                let sv = self.surfaces.iter_mut().find(|s| s.key == key)?;
                let cur = sv.st.more.get(&k).copied().unwrap_or(false);
                sv.st.more.insert(k, !cur);
                self.refresh(doc, pane, key, &[node.id().to_owned()]);
                return Some(vec![Outcome::Redraw]);
            }
            if has("sf-tree-tw") {
                let item = doc.attribute(p, "data-key")?.to_owned();
                let sv = self.surfaces.iter_mut().find(|s| s.key == key)?;
                let k = (node.id().to_owned(), item.clone());
                let open = sv.st.tree.get(&k).map(|f| f.local).unwrap_or(false);
                if let Some(f) = sv.st.tree.get_mut(&k) {
                    f.local = !open;
                }
                self.refresh(doc, pane, key, &[node.id().to_owned()]);
                return Some(vec![self.event(
                    surface,
                    ViewEvent::Toggle {
                        id: node.id().to_owned(),
                        collapsed: open,
                        key: Some(Value::String(item)),
                    },
                )]);
            }
            if has("sf-tab") {
                let item = doc.attribute(p, "data-tab")?.to_owned();
                return Some(vec![self.event(
                    surface,
                    ViewEvent::Select {
                        id: node.id().to_owned(),
                        item,
                        values: self.values(surface, node.id()),
                    },
                )]);
            }
            if has("sf-ck-ph") {
                let phase = doc.attribute(p, "data-phase")?.to_owned();
                let k = format!("{}\u{1f}phase\u{1f}{phase}", node.id());
                let sv = self.surfaces.iter_mut().find(|s| s.key == key)?;
                let cur = sv.st.folds.get(&k).map(|f| f.local).unwrap_or(false);
                SurfaceState::toggle(&mut sv.st.folds, &k, cur);
                self.refresh(doc, pane, key, &[node.id().to_owned()]);
                return Some(vec![Outcome::Redraw]);
            }
            if has("sf-ck-pill") {
                let sv = self.surfaces.iter_mut().find(|s| s.key == key)?;
                let cur = sv.st.hud.get(node.id()).copied().unwrap_or(false);
                sv.st.hud.insert(node.id().to_owned(), !cur);
                self.refresh(doc, pane, key, &[node.id().to_owned()]);
                return Some(vec![self.event(
                    surface,
                    ViewEvent::Activate {
                        id: node.id().to_owned(),
                        item: "hud".to_owned(),
                        values: None,
                    },
                )]);
            }
            if has("sf-ck-rh") {
                let k = format!("{}\u{1f}reminder", node.id());
                let sv = self.surfaces.iter_mut().find(|s| s.key == key)?;
                let cur = sv.st.more.get(&k).copied().unwrap_or(false);
                sv.st.more.insert(k, !cur);
                self.refresh(doc, pane, key, &[node.id().to_owned()]);
                return Some(vec![Outcome::Redraw]);
            }
            // A lens block's Native/Raw control and Copy (§9.6): local, no events.
            if has("sf-block-opt") && node.kind() == "block" {
                let raw = doc.attribute(p, "data-view") == Some("raw");
                let k = crate::kinds::block::view_key(node);
                let sv = self.surfaces.iter_mut().find(|s| s.key == key)?;
                sv.st.more.insert(k, raw);
                self.refresh(doc, pane, key, &[node.id().to_owned()]);
                return Some(vec![Outcome::Redraw]);
            }
            if has("sf-block-copy") && node.kind() == "block" {
                if !self.command_blocks {
                    return Some(vec![Outcome::Copy(String::new())]);
                }
                let raw = node
                    .children()
                    .find(|c| crate::kinds::block::is_raw(node, *c));
                let text = raw
                    .and_then(|r| r.prop("text").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_default();
                let cols = raw
                    .and_then(|r| r.prop("cols").and_then(Value::as_u64))
                    .map_or(self.cols, |c| c.clamp(2, u64::from(u16::MAX)) as u16);
                return Some(vec![Outcome::Copy(crate::kinds::block::raw_text(
                    &text, cols,
                ))]);
            }
            // A mermaid figure's Open (book:5026): the diagram large, in the image viewer.
            if has("mfig-open") {
                let id = doc.attribute(p, "data-mfig")?.to_owned();
                let title = doc
                    .parent(p)
                    .and_then(|h| {
                        doc.element_children(h)
                            .into_iter()
                            .find(|c| self.class_has(*c, "mfig-kind"))
                    })
                    .map(|k| text_content(doc, k));
                return Some(vec![Outcome::Zoom(Box::new(Zoom {
                    surface: surface.key(),
                    images: vec![ZoomImage {
                        id: node.id().to_owned(),
                        blob: id,
                        title,
                        open: None,
                    }],
                    index: 0,
                }))]);
            }
            if has("code-copy") {
                // The fence's text: its `pre`.
                let holder = doc.parent(doc.parent(p)?)?;
                let pre = doc.element_children(holder).into_iter().find(|c| {
                    doc.node(*c)
                        .element()
                        .is_some_and(|e| e.tag.as_str() == "pre")
                })?;
                return Some(vec![Outcome::Copy(text_content(doc, pre))]);
            }
            if has("sf-tool-act") {
                let act = doc.attribute(p, "data-act")?.to_owned();
                return Some(vec![self.event(
                    surface,
                    ViewEvent::Action {
                        id: node.id().to_owned(),
                        action: act,
                        mods: ev.mods,
                        values: None,
                    },
                )]);
            }
        }
        None
    }

    /// A press on a `picker` part (book:6383-6395): rows select, and activate on the selected
    /// row, the row clicked last or a double click (once, never when disabled); buttons, scopes,
    /// tabs, strip chips and the backdrop send `action`; the narrow scope button opens its menu.
    fn picker_press(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        surface: &Surface,
        node: NodeRef,
        press: crate::kinds::picker::Press,
        ev: PointerEvent,
    ) -> Vec<Outcome> {
        use crate::kinds::picker::Press;
        let key = surface.key();
        let id = node.id().to_owned();
        let Some(sv) = self.surfaces.iter_mut().find(|s| s.key == key) else {
            return Vec::new();
        };
        let ps = sv.st.pickers.entry(id.clone()).or_default();
        match press {
            Press::Row { item, disabled } => {
                let selected = node.prop("selected").and_then(Value::as_str) == Some(item.as_str());
                let last = ps.last_click.clone();
                let activate = if ev.clicks >= 2 {
                    // Once per double click: not again when its first press activated.
                    !last.as_ref().is_some_and(|(i, a)| *i == item && *a)
                } else {
                    selected || last.as_ref().is_some_and(|(i, _)| *i == item)
                };
                let activate = activate && !disabled;
                ps.last_click = Some((item.clone(), activate || (ev.clicks >= 2)));
                if ev.clicks >= 2 && !activate {
                    return Vec::new();
                }
                let event = if activate {
                    ViewEvent::Activate {
                        id,
                        item,
                        values: None,
                    }
                } else {
                    ViewEvent::Select {
                        id,
                        item,
                        values: None,
                    }
                };
                vec![self.event(surface, event)]
            }
            Press::Act(Some(action)) => {
                let menu = ps.menu;
                ps.menu = false;
                if menu {
                    self.refresh(doc, pane, key, std::slice::from_ref(&id));
                }
                vec![self.event(
                    surface,
                    ViewEvent::Action {
                        id,
                        action,
                        mods: ev.mods,
                        values: None,
                    },
                )]
            }
            Press::Menu => {
                ps.menu = !ps.menu;
                self.refresh(doc, pane, key, &[id]);
                vec![Outcome::Redraw]
            }
            Press::Act(None) | Press::Nothing => Vec::new(),
        }
    }

    fn event(&self, surface: &Surface, event: ViewEvent) -> Outcome {
        Outcome::Event {
            surface: surface.key(),
            sf: surface.id().to_owned(),
            event: Box::new(event),
        }
    }

    /// Form `values` for an event from inside an `el` form (§7.8), with local flips.
    fn values(&self, surface: &Surface, id: &str) -> Option<Map<String, Value>> {
        let sv = self.surfaces.iter().find(|s| s.key == surface.key())?;
        let checks = &sv.st.checks;
        surface
            .document()
            .form_values(id, &|ctl| checks.get(ctl).map(|f| f.local))
    }

    /// Clicks on a node: kind behavior, then its `actions`.
    fn click_node(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        surface: &Surface,
        node: NodeRef,
        hit: &Hit,
        ev: PointerEvent,
    ) -> Option<Vec<Outcome>> {
        let key = surface.key();
        let actions = Actions::read(node.props(), node.kind());
        // el controls flip locally, and a label flips its first control.
        let control = if el::control_type(node).is_some() {
            Some(node)
        } else if node.kind() == "el" && el::requested(node) == "label" && actions.click.is_none() {
            first_control(node)
        } else {
            None
        };
        if let Some(ctl) = control {
            // Lens views are the plugin's to change: their controls don't flip (§9.6).
            if !surface.interactive() || self.inert_controls || bool_prop(ctl.props(), "disabled") {
                return Some(Vec::new());
            }
            return Some(self.flip_control(doc, pane, surface, ctl));
        }
        match node.kind() {
            "editor" | "input" if actions.click.is_none() => {
                return Some(self.click_field(doc, pane, surface, node, ev));
            }
            // A click on a drawn blob image (not on a child over it) opens the viewer.
            "image" if actions.click.is_none() && ev.clicks < 2 => {
                let on_child = hit.parts.iter().any(|&p| self.class_has(p, "sf-img-over"));
                if !on_child
                    && let Some(el) = self.node_element(surface.key(), node.id())
                    && self.class_has(el, "sf-zoom")
                    && let Some(z) = zoom(surface, node)
                {
                    return Some(vec![Outcome::Zoom(Box::new(z))]);
                }
            }
            "item" => {
                let list = node.parent().filter(|l| l.kind() == "list");
                let act = if ev.clicks >= 2 {
                    &actions.dblclick
                } else {
                    &actions.click
                };
                if act.is_none()
                    && let Some(list) = list
                {
                    if bool_prop(node.props(), "disabled") {
                        return Some(Vec::new());
                    }
                    let values = self.values(surface, list.id());
                    let event = if ev.clicks >= 2 {
                        ViewEvent::Activate {
                            id: list.id().to_owned(),
                            item: node.id().to_owned(),
                            values,
                        }
                    } else {
                        ViewEvent::Select {
                            id: list.id().to_owned(),
                            item: node.id().to_owned(),
                            values,
                        }
                    };
                    return Some(vec![self.event(surface, event)]);
                }
            }
            _ => {}
        }
        let act = if ev.clicks >= 2 {
            actions.dblclick.clone().or(actions.click.clone())
        } else {
            actions.click.clone()
        }?;
        let _ = key;
        Some(self.run_action(doc, pane, surface, node, &act, ev.mods))
    }

    /// Run an action name on a node (§7.8 table).
    fn run_action(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        surface: &Surface,
        node: NodeRef,
        act: &str,
        mods: Mods,
    ) -> Vec<Outcome> {
        match act {
            "toggle" => self.toggle_fold(doc, pane, surface.key(), node),
            "copy" => vec![Outcome::Copy(self.copy_text(node))],
            "open" => match str_prop(node.props(), "href").or(str_prop(node.props(), "path")) {
                Some(h) => vec![Outcome::Open(h.to_owned())],
                None => Vec::new(),
            },
            "zoom" => zoom(surface, node)
                .map(|z| vec![Outcome::Zoom(Box::new(z))])
                .unwrap_or_default(),
            "select" | "activate" => {
                let (id, item) = match node
                    .parent()
                    .filter(|p| p.kind() == "list" && node.kind() == "item")
                {
                    Some(list) => (list.id().to_owned(), node.id().to_owned()),
                    None => (node.id().to_owned(), node.id().to_owned()),
                };
                let values = self.values(surface, node.id());
                let event = if act == "select" {
                    ViewEvent::Select { id, item, values }
                } else {
                    ViewEvent::Activate { id, item, values }
                };
                vec![self.event(surface, event)]
            }
            other => vec![self.event(
                surface,
                ViewEvent::Action {
                    id: node.id().to_owned(),
                    action: other.to_owned(),
                    mods,
                    values: self.values(surface, node.id()),
                },
            )],
        }
    }

    /// Fold or unfold a collapsible node locally, and report it (`toggle`).
    fn toggle_fold(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        key: SurfaceKey,
        node: NodeRef,
    ) -> Vec<Outcome> {
        let Some(surface) = pane.surface(key) else {
            return Vec::new();
        };
        // A collapsible card, section, tool or agent, or a checklist (which always folds): the
        // others don't fold, so there is nothing to flip or report.
        let folds = match node.kind() {
            "card" | "section" | "tool" | "agent" => bool_prop(node.props(), "collapsible"),
            "checklist" => true,
            _ => false,
        };
        if !folds {
            return Vec::new();
        }
        let Some(sv) = self.surfaces.iter_mut().find(|s| s.key == key) else {
            return Vec::new();
        };
        let k = state_key(node);
        let cur = sv.st.fold(node, false);
        let now = SurfaceState::toggle(&mut sv.st.folds, &k, cur);
        // The node and everything under it (previews give way inside open folds).
        let mut ids = vec![node.id().to_owned()];
        let mut stack: Vec<NodeRef> = node.children().collect();
        while let Some(n) = stack.pop() {
            if matches!(n.kind(), "ansi" | "code" | "text") {
                ids.push(n.id().to_owned());
            }
            stack.extend(n.children());
        }
        self.refresh(doc, pane, key, &ids);
        vec![self.event(
            surface,
            ViewEvent::Toggle {
                id: node.id().to_owned(),
                collapsed: now,
                key: None,
            },
        )]
    }

    fn flip_control(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        surface: &Surface,
        ctl: NodeRef,
    ) -> Vec<Outcome> {
        let key = surface.key();
        let Some(sv) = self.surfaces.iter_mut().find(|s| s.key == key) else {
            return Vec::new();
        };
        let radio = el::control_type(ctl) == Some("radio");
        let cur = el::checked(&mut sv.st, ctl);
        if radio && cur {
            // Clicking a checked radio changes nothing.
            return Vec::new();
        }
        let now = SurfaceState::toggle(&mut sv.st.checks, ctl.id(), cur);
        let mut ids = vec![ctl.id().to_owned()];
        let name = str_prop(ctl.props(), "name").map(str::to_owned);
        if radio && let Some(n) = &name {
            // Uncheck the others in the group: within the nearest form, or outside any form.
            let form = nearest_form(ctl);
            let mut stack: Vec<NodeRef> = match form {
                Some(f) => vec![f],
                None => vec![surface.document().root()],
            };
            while let Some(x) = stack.pop() {
                if form.is_none() && x.kind() == "el" && el::requested(x) == "form" {
                    continue;
                }
                if x.id() != ctl.id()
                    && el::control_type(x) == Some("radio")
                    && str_prop(x.props(), "name") == Some(n.as_str())
                {
                    let c = el::checked(&mut sv.st, x);
                    if c {
                        SurfaceState::toggle(&mut sv.st.checks, x.id(), c);
                        ids.push(x.id().to_owned());
                    }
                }
                stack.extend(x.children());
            }
        }
        self.refresh(doc, pane, key, &ids);
        let value = ctl
            .prop("value")
            .cloned()
            .filter(|v| !v.is_null())
            .unwrap_or_else(|| Value::String("on".into()));
        vec![self.event(
            surface,
            ViewEvent::Change {
                id: ctl.id().to_owned(),
                value,
                checked: now,
                name,
                values: self.values(surface, ctl.id()),
            },
        )]
    }

    /// A click in a field: `focus` when unfocused, else a caret placement (`edit`).
    fn click_field(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        surface: &Surface,
        node: NodeRef,
        ev: PointerEvent,
    ) -> Vec<Outcome> {
        if bool_prop(node.props(), "readonly") || !surface.interactive() {
            return Vec::new();
        }
        if surface.document().focus() != Some(node.id()) {
            return vec![self.event(
                surface,
                ViewEvent::Focus {
                    id: node.id().to_owned(),
                },
            )];
        }
        let Some(off) = self.offset_at(doc, surface.key(), node, ev.x, ev.y) else {
            return Vec::new();
        };
        if !self.native_ok(pane, node) {
            return Vec::new();
        }
        let key = surface.key();
        if let Some(sv) = self.surfaces.iter_mut().find(|s| s.key == key) {
            sv.st.selection.remove(node.id());
        }
        self.refresh(doc, pane, key, &[node.id().to_owned()]);
        let text = input::field_text(node);
        let len = input::byte_to_utf16(&text, text.len());
        vec![self.event(
            surface,
            ViewEvent::Edit {
                id: node.id().to_owned(),
                from: off,
                to: off,
                text: String::new(),
                cursor: off,
                len,
            },
        )]
    }

    /// The UTF-16 offset in a field nearest a point, from its laid-out text (monospace cells).
    fn offset_at(
        &self,
        doc: &Document,
        key: SurfaceKey,
        node: NodeRef,
        x: f32,
        y: f32,
    ) -> Option<u64> {
        let el = self.node_element(key, node.id())?;
        let text_el = doc
            .children(doc.children(el).first().copied()?)
            .first()
            .copied()?;
        let r = doc.border_box(text_el)?;
        let zoom = self.settings.font_px / 13.0;
        let (cw, lh) = (self.settings.cell.0, self.settings.cell.1 / zoom * zoom);
        let text = input::field_text(node);
        let prompt = crate::spans::spans_text(&crate::spans::parse_spans(node.prop("prompt")));
        let row = ((y - r.y) / lh).floor().max(0.0) as usize;
        let col = ((x - r.x) / cw).round().max(0.0) as usize;
        let lines: Vec<&str> = text.split('\n').collect();
        let row = row.min(lines.len().saturating_sub(1));
        let mut byte = lines[..row].iter().map(|l| l.len() + 1).sum::<usize>();
        let skip = if row == 0 { prompt.chars().count() } else { 0 };
        let line = lines[row];
        let col = col.saturating_sub(skip);
        byte += line
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        Some(input::byte_to_utf16(&text, byte))
    }

    fn native_ok(&self, pane: &TspPane, node: NodeRef) -> bool {
        pane.hello().is_some_and(|h| h.has("edit"))
            && self.settings.native_editing
            && node.prop("mode").is_none()
            && !bool_prop(node.props(), "readonly")
    }

    /// The focused field of a surface, if any.
    fn focused_field<'p>(&self, pane: &'p TspPane) -> Option<(&'p Surface, NodeRef<'p>)> {
        for s in pane.surfaces().iter().rev() {
            if !s.is_live() {
                continue;
            }
            if let Some(id) = s.document().focus()
                && let Some(n) = s.document().get(id)
                && matches!(n.kind(), "editor" | "input")
            {
                return Some((s, n));
            }
        }
        None
    }

    /// Typed or pasted text (or an IME commit) in the focused field. With a native selection and
    /// the `edit` feature it replaces the selection as an `edit` event; otherwise it's keys.
    pub fn type_text(&mut self, doc: &mut Document, pane: &TspPane, text: &str) -> Outcome {
        self.replace_selection(doc, pane, text)
    }

    /// Delete or cut with a native selection: an `edit` that removes it; otherwise keys.
    pub fn delete_selection(&mut self, doc: &mut Document, pane: &TspPane) -> Outcome {
        self.replace_selection(doc, pane, "")
    }

    fn replace_selection(&mut self, doc: &mut Document, pane: &TspPane, with: &str) -> Outcome {
        let Some((surface, node)) = self.focused_field(pane) else {
            return Outcome::Keys;
        };
        if !self.native_ok(pane, node) {
            return Outcome::Keys;
        }
        let key = surface.key();
        let sel = self
            .surfaces
            .iter()
            .find(|s| s.key == key)
            .and_then(|s| s.st.selection.get(node.id()).copied())
            .filter(|s| !s.is_empty());
        let Some(sel) = sel else {
            return Outcome::Keys;
        };
        let (from, to) = sel.range();
        let text = input::field_text(node);
        let len = input::byte_to_utf16(&text, text.len());
        let cursor = from + with.encode_utf16().count() as u64;
        if let Some(sv) = self.surfaces.iter_mut().find(|s| s.key == key) {
            sv.st.selection.remove(node.id());
            if sv.st.preedit.as_ref().is_some_and(|p| p.id == node.id()) {
                sv.st.preedit = None;
            }
        }
        self.refresh(doc, pane, key, &[node.id().to_owned()]);
        self.event(
            surface,
            ViewEvent::Edit {
                id: node.id().to_owned(),
                from,
                to,
                text: with.to_owned(),
                cursor,
                len,
            },
        )
    }

    /// Set the native selection in the focused field (selecting keys, drags, ⌘A). Local only.
    pub fn select(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        anchor: u64,
        cursor: u64,
    ) -> Outcome {
        let Some((surface, node)) = self.focused_field(pane) else {
            return Outcome::Keys;
        };
        if !self.native_ok(pane, node) {
            return Outcome::Keys;
        }
        let text = input::field_text(node);
        let len = input::byte_to_utf16(&text, text.len());
        let key = surface.key();
        if let Some(sv) = self.surfaces.iter_mut().find(|s| s.key == key) {
            sv.st.selection.insert(
                node.id().to_owned(),
                Selection {
                    anchor: anchor.min(len),
                    cursor: cursor.min(len),
                },
            );
        }
        self.refresh(doc, pane, key, &[node.id().to_owned()]);
        Outcome::Redraw
    }

    /// ⌘A in the focused field.
    pub fn select_all(&mut self, doc: &mut Document, pane: &TspPane) -> Outcome {
        let Some((_, node)) = self.focused_field(pane) else {
            return Outcome::Keys;
        };
        let text = input::field_text(node);
        let len = input::byte_to_utf16(&text, text.len());
        self.select(doc, pane, 0, len)
    }

    /// Move the caret natively (↑/↓ between drawn lines, Home, End) as an `edit` with no text.
    pub fn move_caret(&mut self, doc: &mut Document, pane: &TspPane, to: u64) -> Outcome {
        let Some((surface, node)) = self.focused_field(pane) else {
            return Outcome::Keys;
        };
        if !self.native_ok(pane, node) {
            return Outcome::Keys;
        }
        let text = input::field_text(node);
        let len = input::byte_to_utf16(&text, text.len());
        let to = to.min(len);
        let key = surface.key();
        if let Some(sv) = self.surfaces.iter_mut().find(|s| s.key == key) {
            sv.st.selection.remove(node.id());
        }
        self.refresh(doc, pane, key, &[node.id().to_owned()]);
        self.event(
            surface,
            ViewEvent::Edit {
                id: node.id().to_owned(),
                from: to,
                to,
                text: String::new(),
                cursor: to,
                len,
            },
        )
    }

    /// ⌘Z or ⌃Z: an `undo` event with the `edit` and `undo` features; otherwise keys.
    pub fn undo(&mut self, pane: &TspPane) -> Outcome {
        let Some((surface, node)) = self.focused_field(pane) else {
            return Outcome::Keys;
        };
        if !self.native_ok(pane, node) || !pane.hello().is_some_and(|h| h.has("undo")) {
            return Outcome::Keys;
        }
        self.event(
            surface,
            ViewEvent::Undo {
                id: node.id().to_owned(),
            },
        )
    }

    /// Submit a prompt atomically into a field (feature `send`); the engine holds it until the
    /// field reports `sendable:true`.
    pub fn send(&self, pane: &TspPane, id: &str, text: &str) -> Option<Outcome> {
        if !pane.hello().is_some_and(|h| h.has("send")) {
            return None;
        }
        let s = pane
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.is_live() && s.document().get(id).is_some())?;
        let n = s.document().get(id)?;
        if !matches!(n.kind(), "editor" | "input")
            || bool_prop(n.props(), "readonly")
            || bool_prop(n.props(), "hidden")
        {
            return None;
        }
        Some(self.event(
            s,
            ViewEvent::Send {
                id: id.to_owned(),
                text: text.to_owned(),
            },
        ))
    }

    /// IME composition at the focused field's caret (drawn as `span.sf-ed-preedit`).
    pub fn ime_preedit(&mut self, doc: &mut Document, pane: &TspPane, text: &str, cursor: usize) {
        let Some((surface, node)) = self.focused_field(pane) else {
            return;
        };
        let key = surface.key();
        if let Some(sv) = self.surfaces.iter_mut().find(|s| s.key == key) {
            sv.st.preedit = (!text.is_empty()).then(|| Preedit {
                id: node.id().to_owned(),
                text: text.to_owned(),
                cursor,
            });
        }
        self.refresh(doc, pane, key, &[node.id().to_owned()]);
    }

    /// An IME commit: the preedit goes away and the text is typed.
    pub fn ime_commit(&mut self, doc: &mut Document, pane: &TspPane, text: &str) -> Outcome {
        self.ime_preedit(doc, pane, "", 0);
        self.type_text(doc, pane, text)
    }

    /// The caret rectangle of the focused field, for placing the IME window.
    pub fn caret_rect(&self, doc: &Document, pane: &TspPane) -> Option<auk_ui::Rect> {
        let (surface, node) = self.focused_field(pane)?;
        let sv = self.surfaces.iter().find(|s| s.key == surface.key())?;
        let m = sv.mounts.get(node.id())?;
        let crate::view::Extra::Field(f) = &m.extra else {
            return None;
        };
        doc.border_box(f.caret?)
    }

    /// Hover: `:hover` on the element under the point and its ancestors.
    pub fn hover(&mut self, doc: &mut Document, x: f32, y: f32) {
        let mut chain = Vec::new();
        let mut cur = auk_ui::hit_test(doc, x, y);
        while let Some(e) = cur {
            if doc.node(e).is_element() {
                chain.push(e);
            }
            cur = doc.parent(e);
        }
        for &e in &self.hover {
            if !chain.contains(&e) && doc.try_node(e).is_some() {
                doc.set_state(e, ElementState::HOVER, false);
            }
        }
        for &e in &chain {
            if !self.hover.contains(&e) {
                doc.set_state(e, ElementState::HOVER, true);
            }
        }
        self.hover = chain;
    }

    /// A wheel or trackpad scroll at a point: the innermost scroll container under it that can
    /// still move that way scrolls `up` points toward its start (negative: toward its end). A
    /// screen surface's `main` takes the height and scrolls itself this way (§7.5); inline and
    /// flow `main`s aren't scroll containers, so they leave the wheel to the pane. Returns
    /// whether something scrolled.
    pub fn wheel(&mut self, doc: &mut Document, x: f32, y: f32, up: f32) -> bool {
        let mut cur = auk_ui::hit_test(doc, x, y);
        while let Some(e) = cur {
            if doc.node(e).is_element()
                && doc
                    .computed_style(e)
                    .is_some_and(|s| s.is_scroll_container())
            {
                let (sx, sy) = doc.scroll_offset(e);
                let (_, my) = doc.max_scroll(e);
                let to = (sy - up).clamp(0.0, my.max(0.0));
                if (to - sy).abs() > 0.01 {
                    doc.set_scroll_offset(e, sx, to);
                    return true;
                }
            }
            if e == self.element() {
                break;
            }
            cur = doc.parent(e);
        }
        false
    }

    /// The context menu for the node under a point: its `menu` actions, then Copy unless named.
    pub fn menu(&self, doc: &Document, pane: &TspPane, x: f32, y: f32) -> Vec<MenuEntry> {
        // A modal backdrop has no menu, not even its overlay's: the press goes to `click`,
        // which takes it.
        if auk_ui::hit_test(doc, x, y).is_some_and(|el| self.class_has(el, "sf-ov-dim")) {
            return Vec::new();
        }
        for hit in self.hits(doc, x, y) {
            let Some(s) = pane.surface(hit.surface) else {
                continue;
            };
            let Some(n) = s.document().get(&hit.id) else {
                continue;
            };
            let a = Actions::read(n.props(), n.kind());
            if a.menu.is_empty() {
                continue;
            }
            let mut out: Vec<MenuEntry> = a
                .menu
                .iter()
                .map(|m| MenuEntry {
                    label: common::action_label(m),
                    action: m.clone(),
                })
                .collect();
            if !a.menu.iter().any(|m| m == "copy") {
                out.push(MenuEntry {
                    label: "Copy".into(),
                    action: "copy".into(),
                });
            }
            return out;
        }
        vec![MenuEntry {
            label: "Copy".into(),
            action: "copy".into(),
        }]
    }

    /// Run a menu pick on the node under a point (no modifiers, §7.7).
    pub fn menu_pick(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        x: f32,
        y: f32,
        action: &str,
    ) -> Vec<Outcome> {
        let hits = self.hits(doc, x, y);
        let Some(hit) = hits.into_iter().find(|h| {
            pane.surface(h.surface)
                .and_then(|s| s.document().get(&h.id))
                .is_some_and(|n| !Actions::read(n.props(), n.kind()).menu.is_empty())
        }) else {
            return Vec::new();
        };
        let Some(surface) = pane.surface(hit.surface) else {
            return Vec::new();
        };
        let Some(node) = surface.document().get(&hit.id) else {
            return Vec::new();
        };
        self.run_action(doc, pane, surface, node, action, Mods::NONE)
    }
}

fn nearest_form(node: NodeRef) -> Option<NodeRef> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "el" && el::requested(n) == "form" {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

fn first_control(node: NodeRef) -> Option<NodeRef> {
    let mut stack: Vec<NodeRef> = node.children().collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        if el::control_type(n).is_some() {
            return Some(n);
        }
        let mut kids: Vec<NodeRef> = n.children().collect();
        kids.reverse();
        stack.extend(kids);
    }
    None
}

impl PaneView {
    /// What `copy` copies (§7.8): a block's raw output, an `ansi` node's text as drawn (no
    /// escapes), md source, raw code, the `href`, or the node's text.
    fn copy_text(&self, node: NodeRef) -> String {
        let ansi = |n: NodeRef| {
            let text = n.prop("text").and_then(Value::as_str).unwrap_or_default();
            let cols = n
                .prop("cols")
                .and_then(Value::as_u64)
                .map_or(self.cols, |c| c.clamp(2, u64::from(u16::MAX)) as u16);
            crate::kinds::block::raw_text(text, cols)
        };
        match node.kind() {
            "block" if !self.command_blocks => String::new(),
            "block" => node
                .children()
                .find(|c| crate::kinds::block::is_raw(node, *c))
                .map(ansi)
                .unwrap_or_default(),
            "ansi" => ansi(node),
            _ => copy_text(node),
        }
    }

    /// The UTF-16 offset in the focused field nearest a document point, when the point is over
    /// that field: where a drag that selects in the field (§7.9) has got to.
    pub fn field_offset_at(&self, doc: &Document, pane: &TspPane, x: f32, y: f32) -> Option<u64> {
        let (surface, node) = self.focused_field(pane)?;
        let el = self.node_element(surface.key(), node.id())?;
        let r = doc.border_box(el)?;
        if x < r.x || x > r.x + r.w || y < r.y || y > r.y + r.h {
            return None;
        }
        self.offset_at(doc, surface.key(), node, x, y)
    }

    /// The focused field's text box in document coordinates, and the cell size: where a drag
    /// should go to select its characters.
    pub fn field_text_box(&self, doc: &Document, pane: &TspPane) -> Option<auk_ui::Rect> {
        let (surface, node) = self.focused_field(pane)?;
        let el = self.node_element(surface.key(), node.id())?;
        let text_el = doc
            .children(doc.children(el).first().copied()?)
            .first()
            .copied()?;
        doc.border_box(text_el)
    }
}

/// What `copy` copies for other kinds: md source, raw code, the `href`, or the node's text.
fn copy_text(node: NodeRef) -> String {
    if let Some(h) = str_prop(node.props(), "href") {
        return h.to_owned();
    }
    match node.prop("text") {
        Some(Value::String(s)) => s.clone(),
        _ => {
            let mut out = String::new();
            fn walk(n: NodeRef, out: &mut String) {
                if let Some(Value::String(s)) = n.prop("text") {
                    out.push_str(s);
                } else {
                    out.push_str(&crate::spans::spans_text(&crate::spans::parse_spans(
                        n.prop("spans").or(n.prop("head")),
                    )));
                }
                for c in n.children() {
                    walk(c, out);
                }
            }
            walk(node, &mut out);
            out
        }
    }
}

/// The text content of a DOM subtree.
pub(crate) fn text_content(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        if let Some(t) = doc.node(n).text() {
            out.push_str(t);
        }
        for &c in doc.children(n).iter().rev() {
            stack.push(c);
        }
    }
    out
}

/// The viewer for a blob image: it and its parent's other blob images.
fn zoom(surface: &Surface, node: NodeRef) -> Option<Zoom> {
    let entry = |n: NodeRef| -> Option<ZoomImage> {
        if n.kind() != "image" || n.prop("builtin").is_some() {
            return None;
        }
        let p = n.props();
        Some(ZoomImage {
            id: n.id().to_owned(),
            blob: str_prop(p, "blob")?.to_ascii_lowercase(),
            title: str_prop(p, "alt")
                .or(str_prop(p, "title"))
                .map(str::to_owned),
            open: str_prop(p, "href")
                .or(str_prop(p, "path"))
                .map(str::to_owned),
        })
    };
    entry(node)?;
    let images: Vec<ZoomImage> = match node.parent() {
        Some(parent) => parent.children().filter_map(entry).collect(),
        None => entry(node).into_iter().collect(),
    };
    let index = images.iter().position(|i| i.id == node.id())?;
    Some(Zoom {
        surface: surface.key(),
        images,
        index,
    })
}

/// A key in the focused program field that native editing may take (§7.9; `research/sdk-wire.md`
/// §2.5): selecting keys, caret moves between drawn lines, Home and End, deletes with a selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKey {
    Left,
    Right,
    Up,
    Down,
    /// Home, or ⌘←.
    LineStart,
    /// End, or ⌘→.
    LineEnd,
    Backspace,
    Delete,
    /// ⌘A.
    SelectAll,
}

/// A field's lines as `(start, end)` UTF-16 offsets (end before the newline).
fn field_lines(text: &str) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut start = 0u64;
    for line in text.split('\n') {
        let n = line.encode_utf16().count() as u64;
        out.push((start, start + n));
        start += n + 1;
    }
    out
}

/// The UTF-16 offset one character before or after `at`.
fn char_step(text: &str, at: u64, forward: bool) -> u64 {
    let byte = input::utf16_to_byte(text, at);
    let next = if forward {
        text[byte..]
            .chars()
            .next()
            .map_or(byte, |c| byte + c.len_utf8())
    } else {
        text[..byte]
            .chars()
            .next_back()
            .map_or(byte, |c| byte - c.len_utf8())
    };
    input::byte_to_utf16(text, next)
}

/// Characters between two UTF-16 offsets of a line.
fn chars_between(text: &str, from: u64, to: u64) -> usize {
    let (a, b) = (input::utf16_to_byte(text, from), input::utf16_to_byte(text, to));
    text[a.min(b)..a.max(b)].chars().count()
}

/// The offset `n` characters into the line starting at `start`, clamped to its end.
fn chars_into(text: &str, start: u64, end: u64, n: usize) -> u64 {
    let (a, b) = (input::utf16_to_byte(text, start), input::utf16_to_byte(text, end));
    let byte = text[a..b].char_indices().nth(n).map_or(b, |(i, _)| a + i);
    input::byte_to_utf16(text, byte)
}

impl PaneView {
    /// The elements under the pointer, innermost first (`:hover`).
    pub fn hovered(&self) -> &[NodeId] {
        &self.hover
    }

    /// The focused field's native selection, as text.
    pub fn field_selection(&self, pane: &TspPane) -> Option<String> {
        let (surface, node) = self.focused_field(pane)?;
        let sel = self
            .surfaces
            .iter()
            .find(|s| s.key == surface.key())?
            .st
            .selection
            .get(node.id())
            .copied()
            .filter(|s| !s.is_empty())?;
        let text = input::field_text(node);
        let (a, b) = sel.range();
        let (a, b) = (input::utf16_to_byte(&text, a), input::utf16_to_byte(&text, b));
        Some(text[a..b].to_owned())
    }

    /// Whether a program field has the focus (keys and IME composition belong to it).
    pub fn field_focused(&self, pane: &TspPane) -> bool {
        self.focused_field(pane).is_some()
    }

    /// A key in the focused field (§7.9). With native editing it moves the local selection
    /// (`shift`), moves the caret between drawn lines and to their ends, or deletes the
    /// selection, as `edit` events; otherwise the key is the program's ([`Outcome::Keys`]).
    pub fn field_key(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        key: FieldKey,
        shift: bool,
    ) -> Outcome {
        let Some((surface, node)) = self.focused_field(pane) else {
            return Outcome::Keys;
        };
        if !self.native_ok(pane, node) {
            return Outcome::Keys;
        }
        let text = input::field_text(node);
        let len = input::byte_to_utf16(&text, text.len());
        let local = self
            .surfaces
            .iter()
            .find(|s| s.key == surface.key())
            .and_then(|s| s.st.selection.get(node.id()).copied());
        let caret = local
            .map(|s| s.cursor)
            .unwrap_or_else(|| input::field_cursor(node))
            .min(len);
        let anchor = local.map_or(caret, |s| s.anchor.min(len));
        let selected = local.filter(|s| !s.is_empty());
        let prompt = crate::spans::spans_text(&crate::spans::parse_spans(node.prop("prompt")))
            .chars()
            .count();
        let lines = field_lines(&text);
        let line_of = |at: u64| {
            lines
                .iter()
                .position(|&(s, e)| at >= s && at <= e)
                .unwrap_or(lines.len().saturating_sub(1))
        };
        // The same drawn column on the line above or below (the prompt sits before line 0).
        let vertical = |at: u64, down: bool| -> Option<u64> {
            let i = line_of(at);
            let j = if down { i + 1 } else { i.checked_sub(1)? };
            let &(s, _) = lines.get(i)?;
            let &(ts, te) = lines.get(j)?;
            let col = chars_between(&text, s, at) + if i == 0 { prompt } else { 0 };
            let col = col.saturating_sub(if j == 0 { prompt } else { 0 });
            Some(chars_into(&text, ts, te, col))
        };
        let (start, end) = lines[line_of(caret)];
        match key {
            FieldKey::SelectAll => self.select_all(doc, pane),
            FieldKey::Backspace | FieldKey::Delete => match selected {
                Some(_) => self.delete_selection(doc, pane),
                None => Outcome::Keys,
            },
            FieldKey::Left | FieldKey::Right => {
                let forward = key == FieldKey::Right;
                if shift {
                    let to = char_step(&text, caret, forward);
                    self.select(doc, pane, anchor, to)
                } else if let Some(s) = selected {
                    let (a, b) = s.range();
                    self.move_caret(doc, pane, if forward { b } else { a })
                } else {
                    Outcome::Keys
                }
            }
            FieldKey::Up | FieldKey::Down => {
                let down = key == FieldKey::Down;
                match (vertical(caret, down), shift) {
                    (Some(to), false) => self.move_caret(doc, pane, to),
                    (Some(to), true) => self.select(doc, pane, anchor, to),
                    (None, true) => self.select(doc, pane, anchor, if down { len } else { 0 }),
                    // Past the first or last drawn line the key is the program's (history).
                    (None, false) => Outcome::Keys,
                }
            }
            FieldKey::LineStart | FieldKey::LineEnd => {
                let to = if key == FieldKey::LineStart { start } else { end };
                if shift {
                    self.select(doc, pane, anchor, to)
                } else {
                    self.move_caret(doc, pane, to)
                }
            }
        }
    }
}

#[cfg(test)]
mod field_key_tests {
    use super::*;

    #[test]
    fn lines_steps_and_columns() {
        let text = "ab\ncdé\n";
        assert_eq!(field_lines(text), vec![(0, 2), (3, 6), (7, 7)]);
        assert_eq!(char_step(text, 5, true), 6);
        assert_eq!(char_step(text, 6, false), 5);
        assert_eq!(chars_between(text, 3, 6), 3);
        assert_eq!(chars_into(text, 0, 2, 5), 2);
    }
}
