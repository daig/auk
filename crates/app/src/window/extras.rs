//! Automation-endpoint commands for command blocks, presence, settings, the catalog and plugin
//! sheets (see `control.rs` for the command table), and the keymap's own actions.

use auk_mirror::PaneId;
use auk_platform_macos::Window;
use serde_json::{Value, json};

use super::WinState;

fn pane_arg(args: &[String]) -> Result<PaneId, String> {
    args.first()
        .and_then(|a| a.parse().ok())
        .ok_or_else(|| "expected a pane id".to_owned())
}

impl WinState {
    /// A command this module answers, or `None` when it isn't one.
    pub fn extra_command(
        &mut self,
        _window: &Window,
        cmd: &str,
        args: &[String],
    ) -> Option<Result<Value, String>> {
        Some(match cmd {
            "lens" => self.lens_report(args),
            "presence" => {
                let clients = self.conn.shared().presence.clone();
                Ok(json!({
                    "clients": clients,
                    "you": self.conn.client_id,
                    "shown": self.presence_text(),
                }))
            }
            "surface-sheets" => pane_arg(args).and_then(|pane| {
                self.surfaces
                    .get(&pane)
                    .map(|sf| json!(sf.plugin_sheets()))
                    .ok_or_else(|| format!("pane {pane} has no surfaces yet"))
            }),
            "settings" => Ok(self.settings_report()),
            "menu-key" => {
                let path: Vec<&str> = args.iter().map(String::as_str).collect();
                crate::app::with(|a| a.ctx)
                    .and_then(|ctx| ctx.menu_key(&path))
                    .map(|(key, mask)| json!({"key": key, "mask": mask}))
                    .ok_or_else(|| format!("no menu item {path:?}"))
            }
            // The host catalog this window follows: block types and the card, if one is up.
            "catalog" => {
                self.plugins_catalog();
                let types: Vec<Value> = self
                    .plugins
                    .catalog
                    .block_types()
                    .map(|(p, b)| json!({"kind": format!("{}.{}", p.id, b.id), "title": b.title, "palette": b.palette}))
                    .collect();
                Ok(json!({
                    "host": self.conn.host_name(),
                    "block_types": types,
                    "styles": self.plugins.catalog.styles().map(|(id, _)| id).collect::<Vec<_>>(),
                    "sheets": self.plugins.sheets.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                    "card": self.plugins.block_card.as_ref().map(|c| json!({"kind": c.kind, "error": c.error})),
                }))
            }
            // The computed padding of the element with this `data-role` in the pane's command
            // blocks (plugin sheets style them, §8.4 step 3).
            "lens-style" => {
                let (Some(pane), Some(role)) = (
                    args.first().and_then(|a| a.parse::<PaneId>().ok()),
                    args.get(1),
                ) else {
                    return Some(Err("lens-style: expected PANE ROLE".into()));
                };
                let Some(l) = self.surfaces.get(&pane).and_then(|sf| sf.lenses()) else {
                    return Some(Err(format!("pane {pane} has no command blocks drawn")));
                };
                let doc = l.doc();
                let mut stack = vec![doc.root()];
                let mut found = None;
                while let Some(n) = stack.pop() {
                    if doc.attribute(n, "data-role") == Some(role.as_str())
                        && let Some(style) = doc.computed_style(n)
                    {
                        found = Some(json!({"padding": format!("{:?}", style.padding)}));
                        break;
                    }
                    stack.extend(doc.children(n).iter().copied());
                }
                found.ok_or_else(|| format!("no element with role {role}"))
            }
            _ => return None,
        })
    }

    /// `lens PANE`: the replica's command blocks and how this window drew them.
    fn lens_report(&self, args: &[String]) -> Result<Value, String> {
        let pane = pane_arg(args)?;
        let scale = self.size.map_or(2.0, |z| z.scale) as f32;
        let r = |v: f32| (f64::from(v) * 100.0).round() / 100.0;
        let shared = self.conn.shared();
        let data = shared
            .panes
            .get(&pane)
            .ok_or_else(|| format!("no pane {pane}"))?;
        let layer = self.surfaces.get(&pane).and_then(|sf| sf.lenses());
        let view = self.views.get(&pane);
        let blocks: Vec<Value> = data
            .replica
            .lens_blocks()
            .iter()
            .map(|b| {
                // Where the block sits in the window this frame, if it shows.
                let top = view.and_then(|v| {
                    let a = &v.arranged;
                    a.composed.blocks.iter().find_map(|&(i, y)| {
                        (a.blocks[i].sf == format!("{}{}", auk_views::lenses::LENS_PREFIX, b.id))
                            .then(|| (v.grid_origin, y / scale, a.blocks[i].height / scale))
                    })
                });
                let parts: Vec<Value> = match (layer, top) {
                    (Some(l), Some((origin, y, _))) => l
                        .parts(b.id)
                        .into_iter()
                        .map(|(class, name, text, [px, py, w, h])| {
                            json!({
                                "class": class, "name": name, "text": text,
                                "rect": [
                                    r(origin.0 as f32 + px),
                                    r(origin.1 as f32 + y + py),
                                    r(w),
                                    r(h),
                                ],
                            })
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                json!({
                    "id": b.id,
                    "plugin": b.plugin,
                    "lens": b.lens,
                    "done": b.done,
                    "status": b.status,
                    "has_view": b.view.is_some(),
                    "view": b.view,
                    "shown": layer.and_then(|l| l.shown(b.id)),
                    "text": layer.map(|l| l.text(b.id)),
                    "rect": top.map(|(o, y, h)| json!([o.0, r(o.1 as f32 + y), r(h)])),
                    "parts": parts,
                })
            })
            .collect();
        Ok(json!({"blocks": blocks}))
    }
}

impl WinState {
    /// Whether motion is reduced: Auk's setting, or the system's (§7.13).
    pub fn reduce_motion(&self) -> bool {
        self.settings
            .reduces_motion(auk_platform_macos::system_reduce_motion())
    }

    /// Reads the settings file again: views, facts, the keymap and the menu's key equivalents.
    pub(super) fn reload_settings(&mut self) {
        self.settings = crate::settings::Settings::load();
        for p in &self.settings.problems {
            eprintln!("auk: settings: {p}");
        }
        self.send_facts();
        self.plugins_rebuilt();
        let taken = self.settings.user_chords();
        if let Some(ctx) = crate::app::with(|a| a.ctx) {
            ctx.set_main_menu(&crate::commands::menu_bar_without(&taken));
        }
        self.ui_dirty = true;
        if let Some(w) = &self.window {
            w.request_frame();
        }
    }

    fn settings_report(&self) -> Value {
        let s = &self.settings;
        json!({
            "program_colors": s.program_colors,
            "native_editing": s.native_editing,
            "links_plain_click": s.links_plain_click,
            "reduce_motion": self.reduce_motion(),
            "system_reduce_motion": auk_platform_macos::system_reduce_motion(),
            "keymap": s.keymap,
            "keymap_prefix": s.keymap_prefix,
            "keybinds": s.keybinds.iter().map(|b| json!({"chord": b.text, "actions": b.actions})).collect::<Vec<_>>(),
            "preset": self.plugins.preset.iter().map(|(_, a)| a).collect::<Vec<_>>(),
            "user": self.plugins.user.iter().map(|(_, a)| a).collect::<Vec<_>>(),
            "problems": s.problems,
            // What the focused pane's views were last configured with.
            "view": self.focused_pane().and_then(|p| self.surfaces.get(&p)).map(|sf| {
                let v = sf.view().settings();
                json!({
                    "program_colors": v.program_colors,
                    "native_editing": v.native_editing,
                    "links_plain_click": v.links_plain_click,
                    "reduce_motion": v.reduce_motion,
                })
            }),
        })
    }

    /// The other clients attached to the host, for the status line (§10.1): `None` when this
    /// window is alone.
    pub fn presence_text(&self) -> Option<String> {
        let shared = self.conn.shared();
        let me = self.conn.client_id;
        let others: Vec<String> = shared
            .presence
            .iter()
            .filter(|c| c.id != me)
            .map(|c| {
                let kind = serde_json::to_value(c.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "client".to_owned());
                match c.identity.last() {
                    Some(h) if h.method != "local" => {
                        format!(
                            "{} via {} from {} as {}",
                            h.principal, h.method, h.origin, h.acting
                        )
                    }
                    Some(h) => format!("{kind} ({})", h.acting),
                    None => kind,
                }
            })
            .collect();
        (!others.is_empty()).then(|| format!("Also here: {}", others.join(", ")))
    }
}

impl WinState {
    /// Runs a keymap action (§9.8, research/plugins.md §12.2): a plugin's or a built-in command,
    /// or one of the keymap-only actions (`palette`, `goto_tab:N`, `last_tab`,
    /// `focus_pane:DIR`, `text:BYTES`).
    pub(super) fn run_keymap_action(&mut self, name: &str) -> Result<(), String> {
        use crate::commands::Cmd;
        use crate::geometry::Direction;
        // The block card's buttons (§9.5).
        if name == "card:retry" {
            let card = self.plugins.block_card.take().ok_or("no block card")?;
            self.ui_dirty = true;
            return self
                .open_block(&card.kind, &card.args, card.how, card.reveal, card.cwd)
                .map(drop)
                .ok_or_else(|| "the block still could not start".to_owned());
        }
        if name == "card:dismiss" {
            self.plugins.block_card = None;
            self.ui_dirty = true;
            return Ok(());
        }
        let window = self.window.clone().ok_or("no window")?;
        let cmd = match name {
            "palette" => Some(Cmd::CommandPalette),
            "last_tab" => Some(Cmd::SelectTab(9)),
            _ => None,
        };
        if let Some(cmd) = cmd {
            self.command(&window, cmd);
            return Ok(());
        }
        if let Some(n) = name.strip_prefix("goto_tab:") {
            let n: u8 = n
                .parse()
                .ok()
                .filter(|n| (1..=9).contains(n))
                .ok_or_else(|| format!("`{name}` names no tab"))?;
            self.command(&window, Cmd::SelectTab(n));
            return Ok(());
        }
        if let Some(dir) = name.strip_prefix("focus_pane:") {
            let d = match dir {
                "left" => Direction::Left,
                "right" => Direction::Right,
                "up" => Direction::Up,
                "down" => Direction::Down,
                _ => return Err(format!("`{name}`: unknown direction")),
            };
            self.command(&window, Cmd::Focus(d));
            return Ok(());
        }
        if let Some(text) = name.strip_prefix("text:") {
            let bytes = unescape(text).ok_or_else(|| format!("`{name}`: bad escape"))?;
            let pane = self.focused_pane().ok_or("no focused pane")?;
            self.conn.input(pane, bytes);
            return Ok(());
        }
        self.run_action_named(name)
    }
}

/// `text:` escapes: `\e \r \n \t \\ \xHH`.
fn unescape(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let c = *b.get(i + 1)?;
        i += 2;
        match c {
            b'e' => out.push(0x1b),
            b'r' => out.push(b'\r'),
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'\\' => out.push(b'\\'),
            b'x' => {
                let hex = std::str::from_utf8(b.get(i..i + 2)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            _ => return None,
        }
    }
    Some(out)
}

impl WinState {
    /// Whether a plain click at a grid cell opens its link: the `links_plain_click` setting, a
    /// single click without ⇧, an OSC 8 link there, and no program taking the mouse.
    pub(super) fn plain_click_link(
        &self,
        pane: PaneId,
        line: auk_term::Line,
        col: u16,
        m: &auk_platform_macos::MouseEvent,
    ) -> bool {
        if !self.settings.links_plain_click || m.mods.shift || m.clicks > 1 {
            return false;
        }
        let shared = self.conn.shared();
        let Some(data) = shared.panes.get(&pane) else {
            return false;
        };
        let term = data.replica.terminal();
        term.modes().mouse == auk_term::MouseMode::Off
            && term
                .line(line)
                .and_then(|row| row.cells.get(usize::from(col)).map(|c| c.link != 0))
                .unwrap_or(false)
    }
}
