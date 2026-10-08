//! The app's automation endpoint (`auk --control <socket>`, or `$AUK_APP_CONTROL`): one command
//! per line in, one JSON line out, in the control endpoint's shape. Tests drive the real app
//! through it: window, pane and layout state; a replica's state hash and text; the accessibility
//! tree read through `NSAccessibility`; keystrokes through the real `NSEvent` → `keyDown:` →
//! `interpretKeyEvents:` → `NSTextInputClient` path; marked text; the clipboard; menu items;
//! mouse and scroll events; frame statistics.
//!
//! Commands (window-scoped ones take `--win N`, an index into `windows`; default 0):
//!
//! | Command | Does |
//! | --- | --- |
//! | `ping`, `version` | |
//! | `windows` | the open windows |
//! | `state` | the window's shown session, tab, focus and pane rectangles; the IME cursor area |
//! | `layout` | the window key's sessions, as the daemon replicated them |
//! | `panes` | the window's replicas: seq, size, exit, primary; stream errors |
//! | `state-hash PANE`, `capture PANE [--scrollback]` | a replica's hash and text |
//! | `a11y [--depth N]` | the accessibility tree, through `NSAccessibility` |
//! | `type STROKE…` | keystrokes (`ctrl+c`, `opt+e`, `return`) through `NSEvent` |
//! | `tap MOD…` | modifier taps (`shift`, `ctrl`, `opt`, `cmd`) as `flagsChanged:` events |
//! | `post-key STROKE…` | keystrokes posted as `CGEvent`s to this process (needs Accessibility) |
//! | `can-post` | whether posting is permitted |
//! | `text TEXT`, `mark TEXT [START LEN]`, `unmark`, `preedit` | `NSTextInputClient` calls |
//! | `clipboard`, `clipboard-set TEXT` | the general pasteboard |
//! | `menus`, `menu MENU ITEM` | the menu bar; choosing an item as a click does |
//! | `mouse down|up|drag|move X Y [--clicks N] [--right] [--shift] [--alt] [--cmd]` | mouse events |
//! | `scroll LINES X Y` | a scroll-wheel event |
//! | `focus PANE` | focuses a pane (its session and tab come forward) |
//! | `selection` | the focused pane's selected text |
//! | `pane-layout PANE` | the pane's flow as last drawn (§6.2): first line, blocks, dock, screen |
//! | `frame-stats [--reset]` | frame times: start of work to GPU and CPU done, less the drawable wait; CPU work |
//! | `input-source [ID]` | the keyboard input source, or selects one |
//! | `option-as-alt none|left|right|both` | the Option-as-Alt setting |
//! | `plugins` | the window halves: loaded, failed, commands, binds and the keymap layer, sheets |
//! | `palette [open]` | the command palette: open, query, rows (opens it first with `open`) |
//! | `toasts` | every toast the window showed |
//! | `opens` | built-in opens (the system opener) and route decisions |
//! | `chrome` | status segments, tab and window titles, clickable chrome boxes, context menu, viewer |
//! | `chrome-style PLUGIN` | the computed style of a plugin's status segment (plugin sheets) |
//! | `lens PANE` | the pane's command blocks as drawn: view shown, toolbar and action rects |
//! | `presence` | the clients attached to the host, as presence reported them |
//! | `settings` | the settings the window uses (program colors, links, motion, keymap) |
//! | `surface-sheets PANE` | the plugin sheets installed in the pane's documents |
//! | `lens-style PANE ROLE` | the computed padding of a command-block element with that role |
//! | `menu-key MENU ITEM` | a menu item's key equivalent (the user's keybinds take theirs) |
//! | `catalog` | the host catalog's block types and styles, the sheets installed, the block card |
//! | `find` | the find bar: query, matches in flow order, the current one |
//! | `node-box PANE ID` | where a surface node of the pane was drawn, in window points |
//! | `field-box PANE` | the pane's focused program field's text box, and the cell size |
//! | `activate` | brings the app forward |
//! | `quit` | quits the app |

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use auk_daemon::control::{self, error, ok};
use auk_platform_macos::synth::{self, MouseAction, Stroke};
use auk_platform_macos::{AxNode, OptionAsAlt, keys::flags, run_on_main};
use serde_json::{Value, json};

use crate::app;

/// How long a command may wait for the main thread.
const MAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Serves the endpoint on its own threads.
pub fn serve(path: &Path) -> std::io::Result<()> {
    let listener = control::bind(path)?;
    std::thread::Builder::new()
        .name("auk app control".into())
        .spawn(move || control::serve_listener_with(listener, Arc::new(handle)))?;
    Ok(())
}

/// Runs `f` on the main thread and returns its result.
fn on_main<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Result<R, String> {
    let (tx, rx) = mpsc::channel();
    run_on_main(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(MAIN_TIMEOUT)
        .map_err(|_| "the main thread didn't answer".to_owned())
}

/// The value of `--name VALUE` in `args`, removing both.
fn take_flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
}

/// Whether `--name` is in `args`, removing it.
fn take_switch(args: &mut Vec<String>, name: &str) -> bool {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

fn handle(line: &str) -> Value {
    let Some(words) = shlex::split(line) else {
        return error("unbalanced quotes");
    };
    let Some((cmd, rest)) = words.split_first() else {
        return error("empty command");
    };
    let mut args = rest.to_vec();
    let win = take_flag(&mut args, "--win")
        .and_then(|w| w.parse::<usize>().ok())
        .unwrap_or(0);
    let cmd = cmd.clone();
    let result = match cmd.as_str() {
        "ping" | "version" => return control::handle(&cmd),
        "can-post" => Ok(json!(synth::can_post_events())),
        // Text Input Sources are main-thread API.
        "input-source" => on_main(move || match args.first() {
            None => Ok(json!(synth::input_source::current())),
            Some(id) => {
                let previous = synth::input_source::current();
                if synth::input_source::select(id) {
                    Ok(json!({"previous": previous}))
                } else {
                    Err(format!("input source {id} is not installed"))
                }
            }
        })
        .and_then(|r| r),
        "post-key" => post_keys(&args),
        _ => on_main(move || main_command(&cmd, args, win)).and_then(|r| r),
    };
    match result {
        Ok(Value::Null) => json!({"ok": true}),
        Ok(value) => ok(value),
        Err(message) => error(message),
    }
}

/// Posts keystrokes to this process as `CGEvent`s: the window server delivers them as it does a
/// real keyboard's (S1). Needs the Accessibility permission.
fn post_keys(args: &[String]) -> Result<Value, String> {
    if !synth::can_post_events() {
        return Err(
            "posting key events needs the Accessibility permission for the process that started \
             Auk (System Settings > Privacy & Security > Accessibility)"
                .into(),
        );
    }
    let pid = std::process::id() as i32;
    for name in args {
        let stroke = Stroke::parse(name).ok_or_else(|| format!("bad stroke `{name}`"))?;
        synth::post_to_pid(pid, stroke);
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(Value::Null)
}

fn ax_json(node: &AxNode) -> Value {
    json!({
        "role": node.role,
        "subrole": node.subrole,
        "label": node.label,
        "title": node.title,
        "value": node.value,
        "children": node.children.iter().map(ax_json).collect::<Vec<_>>(),
    })
}

fn pane_arg(args: &[String]) -> Result<u64, String> {
    let w = args.first().ok_or("missing pane")?;
    w.parse().map_err(|_| format!("bad pane `{w}`"))
}

fn main_command(cmd: &str, mut args: Vec<String>, win: usize) -> Result<Value, String> {
    let windows = app::windows();
    if cmd == "windows" {
        let list: Vec<Value> = windows
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let s = w.borrow();
                let platform = s.window.clone();
                json!({
                    "index": i,
                    "key": s.conn.window_key,
                    "client": s.conn.client_id,
                    "focused": platform.as_ref().is_some_and(|w| w.is_focused()),
                    "full_screen": platform.as_ref().is_some_and(|w| w.is_full_screen()),
                    "size": s.size.map(|z| [z.width, z.height]),
                    "scale": s.size.map(|z| z.scale),
                })
            })
            .collect();
        return Ok(json!(list));
    }
    match cmd {
        "menus" => {
            let menus = app::with(|a| a.ctx.menu_titles()).unwrap_or_default();
            return Ok(json!(
                menus
                    .into_iter()
                    .map(|(m, items)| json!({"menu": m, "items": items}))
                    .collect::<Vec<_>>()
            ));
        }
        "menu" => {
            let path: Vec<&str> = args.iter().map(String::as_str).collect();
            let Some(ctx) = app::with(|a| a.ctx) else {
                return Err("no app".into());
            };
            return if ctx.perform_menu(&path) {
                Ok(Value::Null)
            } else {
                Err(format!("no enabled menu item {path:?}"))
            };
        }
        "clipboard" => return Ok(json!(app::clipboard_get())),
        "clipboard-set" => {
            return Ok(json!(app::clipboard_set(&args.join(" "))));
        }
        "activate" => {
            if let Some(ctx) = app::with(|a| a.ctx) {
                ctx.activate();
            }
            return Ok(Value::Null);
        }
        "quit" => {
            if let Some(ctx) = app::with(|a| a.ctx) {
                ctx.quit();
            }
            return Ok(Value::Null);
        }
        _ => {}
    }
    let state = windows
        .get(win)
        .ok_or_else(|| format!("no window {win} ({} open)", windows.len()))?
        .clone();
    let window = state
        .borrow()
        .window
        .clone()
        .ok_or("the window isn't up yet")?;
    match cmd {
        "state" => {
            let s = state.borrow();
            let panes: Vec<Value> = s
                .tab_layout
                .panes
                .iter()
                .map(|(p, r)| json!({"pane": p, "rect": [r.x, r.y, r.w, r.h]}))
                .collect();
            Ok(json!({
                "key": s.conn.window_key,
                "session": s.active_session,
                "tab": s.active_tab_id(),
                "focused": s.focused_pane(),
                "visible": panes,
                "key_window": s.key_window,
                "preedit": s.preedit.as_ref().map(|p| &p.text),
                "ime_area": s.ime_area.map(|(x, y, w, h)| [x, y, w, h]),
                "option_as_alt": format!("{:?}", s.option_as_alt),
            }))
        }
        "layout" => Ok(json!(state.borrow().conn.shared().sessions)),
        "panes" => {
            let s = state.borrow();
            let shared = s.conn.shared();
            let panes: Vec<Value> = shared
                .panes
                .iter()
                .map(|(id, p)| {
                    let t = p.replica.terminal();
                    json!({
                        "id": id,
                        "seq": p.replica.seq(),
                        "cols": t.cols(),
                        "rows": t.rows(),
                        "exit": p.exit,
                        "primary": p.primary,
                        "title": t.title(),
                        "modes": format!("{:?}", t.modes()),
                    })
                })
                .collect();
            Ok(json!({"panes": panes, "errors": shared.errors, "closed": shared.closed}))
        }
        "state-hash" | "capture" => {
            let pane = pane_arg(&args)?;
            let scrollback = args.iter().any(|a| a == "--scrollback");
            let s = state.borrow();
            let shared = s.conn.shared();
            let p = shared
                .panes
                .get(&pane)
                .ok_or_else(|| format!("no replica of pane {pane}"))?;
            if cmd == "state-hash" {
                let hash: String = p
                    .replica
                    .state_hash()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                Ok(json!({"hash": hash, "seq": p.replica.seq()}))
            } else {
                let text = if scrollback {
                    p.replica.text()
                } else {
                    p.replica.screen_text()
                };
                Ok(json!({"text": text, "seq": p.replica.seq()}))
            }
        }
        "a11y" => {
            let depth = take_flag(&mut args, "--depth")
                .and_then(|d| d.parse().ok())
                .unwrap_or(8);
            Ok(ax_json(&window.accessibility_tree(depth)))
        }
        "type" => {
            for name in &args {
                let stroke = Stroke::parse(name).ok_or_else(|| format!("bad stroke `{name}`"))?;
                if !window.type_stroke(stroke) {
                    return Err(format!("couldn't make an event for `{name}`"));
                }
            }
            Ok(Value::Null)
        }
        "tap" => {
            for name in &args {
                if !window.tap_modifier(name) {
                    return Err(format!("bad modifier `{name}`"));
                }
            }
            Ok(Value::Null)
        }
        "text" => {
            window.insert_text(&args.join(" "));
            Ok(Value::Null)
        }
        "mark" => {
            let text = args.first().cloned().unwrap_or_default();
            let len = text.encode_utf16().count();
            let start = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(len);
            let sel_len = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(0usize);
            window.set_marked_text(&text, start..start + sel_len);
            Ok(json!({"has_marked_text": window.has_marked_text()}))
        }
        "unmark" => {
            window.unmark_text();
            Ok(Value::Null)
        }
        "preedit" => {
            let s = state.borrow();
            Ok(json!({
                "text": s.preedit.as_ref().map(|p| &p.text),
                "selection": s.preedit.as_ref().map(|p| [p.selection.start, p.selection.end]),
                "has_marked_text": window.has_marked_text(),
            }))
        }
        "mouse" => {
            let clicks = take_flag(&mut args, "--clicks")
                .and_then(|c| c.parse().ok())
                .unwrap_or(1);
            let right = take_switch(&mut args, "--right");
            let mut bits = 0;
            if take_switch(&mut args, "--shift") {
                bits |= flags::SHIFT;
            }
            if take_switch(&mut args, "--alt") {
                bits |= flags::OPTION | flags::LEFT_OPTION;
            }
            if take_switch(&mut args, "--cmd") {
                bits |= flags::COMMAND;
            }
            let action = match args.first().map(String::as_str) {
                Some("down") => MouseAction::Down,
                Some("up") => MouseAction::Up,
                Some("drag") => MouseAction::Dragged,
                Some("move") => MouseAction::Moved,
                _ => return Err("mouse: expected down, up, drag or move".into()),
            };
            let (x, y) = coords(&args[1..])?;
            if window.send_mouse(action, right, x, y, clicks, bits) {
                Ok(Value::Null)
            } else {
                Err("couldn't make a mouse event".into())
            }
        }
        "scroll" => {
            let lines: i32 = args
                .first()
                .and_then(|l| l.parse().ok())
                .ok_or("scroll: expected LINES X Y")?;
            let (x, y) = coords(&args[1..])?;
            if window.send_scroll(lines, x, y) {
                Ok(Value::Null)
            } else {
                Err("couldn't make a scroll event".into())
            }
        }
        "focus" => {
            let pane = pane_arg(&args)?;
            if state.borrow_mut().focus_pane(pane) {
                Ok(Value::Null)
            } else {
                Err(format!("pane {pane} isn't in this window"))
            }
        }
        "selection" => Ok(json!(state.borrow().selection_text())),
        "pane-layout" => {
            let pane = pane_arg(&args)?;
            let s = state.borrow();
            let scale = s.size.map_or(2.0, |z| z.scale) as f32;
            let view = s
                .views
                .get(&pane)
                .ok_or_else(|| format!("pane {pane} hasn't been drawn"))?;
            let a = &view.arranged;
            let r = |v: f32| (f64::from(v / scale) * 100.0).round() / 100.0;
            let blocks: Vec<Value> = a
                .composed
                .blocks
                .iter()
                .map(|&(i, y)| {
                    let b = &a.blocks[i];
                    json!({"sf": b.sf, "line": b.line, "top": r(y), "height": r(b.height)})
                })
                .collect();
            // Each drawn row: its line, top and height in points (blocks included), the cell
            // size, the rows where prompts started, and the first drawn line's text.
            let rows: Vec<Value> = a
                .composed
                .slots
                .iter()
                .enumerate()
                .map(|(i, sl)| json!([a.composed.first + i, r(sl.top), r(sl.height)]))
                .collect();
            let (prompts, first_text, surfaces) = {
                let shared = s.conn.shared();
                let term = shared.panes.get(&pane).map(|p| p.replica.terminal());
                let surfaces: Vec<Value> = shared
                    .panes
                    .get(&pane)
                    .map(|p| {
                        p.replica
                            .tsp()
                            .surfaces()
                            .iter()
                            .map(|sf| json!({"id": sf.id(), "mode": format!("{:?}", sf.mode()), "live": sf.is_live()}))
                            .collect()
                    })
                    .unwrap_or_default();
                (
                    term.map(|t| t.prompt_lines()).unwrap_or_default(),
                    term.and_then(|t| t.line(a.composed.first)).map(|row| {
                        row.cells
                            .iter()
                            .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
                            .collect::<String>()
                            .trim_end()
                            .to_owned()
                    }),
                    surfaces,
                )
            };
            let cell = s.surfaces.get(&pane).map(|sf| sf.view().settings().cell);
            Ok(json!({
                "origin": [view.grid_origin.0, view.grid_origin.1],
                "content_height": r(view.content_px),
                "cell": cell.map(|c| [c.0, c.1]),
                "rows": rows,
                "prompts": prompts,
                "surfaces": surfaces,
                "first_text": first_text,
                "selection": view.selection.map(|sel| json!({"anchor": [sel.anchor.0, sel.anchor.1], "head": [sel.head.0, sel.head.1]})),
                "first_line": a.composed.first,
                "lines_drawn": a.composed.slots.len(),
                "following": a.composed.scroll.following(),
                "from_bottom": r(a.composed.scroll.from_bottom),
                "max_scroll": r(a.composed.max_scroll),
                "blocks": blocks,
                "dock": a.dock.map(|(_, b)| json!({"height": b.h})),
                "screen": a.screen.is_some(),
                "layers": a.layers,
            }))
        }
        "frame-stats" => {
            let s = state.borrow();
            let reset = args.iter().any(|a| a == "--reset");
            let gpu = s.clock.times();
            let stats = json!({
                "frames": s.stats.frames,
                "frame": summary(&gpu),
                "cpu": summary(&s.stats.cpu),
                "drawable_wait": summary(&s.stats.drawable_wait),
                "size": s.size.map(|z| [z.width, z.height]),
                "scale": s.size.map(|z| z.scale),
            });
            drop(s);
            if reset {
                let mut s = state.borrow_mut();
                s.clock.reset();
                s.stats = Default::default();
            }
            Ok(stats)
        }
        "option-as-alt" => {
            let setting = match args.first().map(String::as_str) {
                Some("none") => OptionAsAlt::None,
                Some("left") => OptionAsAlt::Left,
                Some("right") => OptionAsAlt::Right,
                Some("both") => OptionAsAlt::Both,
                _ => return Err("option-as-alt: expected none, left, right or both".into()),
            };
            state.borrow_mut().set_option_as_alt(setting);
            Ok(Value::Null)
        }
        "window-size" => {
            let (w, h) = coords(&args)?;
            window.set_content_size(w, h);
            Ok(Value::Null)
        }
        "focus-window" => {
            window.focus();
            Ok(Value::Null)
        }
        "plugins" => {
            let s = state.borrow();
            let rt = s.plugins.rt.as_ref();
            Ok(json!({
                "loaded": rt.map(|r| r.plugins()).unwrap_or_default(),
                "failures": rt.map(|r| r.failures().to_vec()).unwrap_or_default(),
                "commands": rt.map(|r| r.commands().into_iter().map(|c| json!({
                    "action": c.action, "title": c.title, "group": c.group, "icon": c.icon,
                })).collect::<Vec<_>>()).unwrap_or_default(),
                "keymap": s.plugins.keymap.iter().map(|(c, a)| json!({"steps": c.steps.len(), "action": a})).collect::<Vec<_>>(),
                "binds": rt.map(|r| r.binds().into_iter().map(|b| json!({"chord": b.chord, "action": b.action})).collect::<Vec<_>>()).unwrap_or_default(),
                "dropped_binds": s.plugins.dropped_binds,
                "sheets": s.chrome.plugin_sheets(),
            }))
        }
        "palette" => {
            if args.first().map(String::as_str) == Some("open") {
                state.borrow_mut().command(&window, crate::commands::Cmd::CommandPalette);
                window.request_frame();
            }
            let s = state.borrow();
            Ok(match &s.overlays.palette {
                Some(p) => json!({
                    "open": true,
                    "drawn_app": s.chrome.palette_app().map(|(app, text)| json!({"app": app, "text": text})),
                    "query": p.query,
                    "selected": p.selected,
                    "rows": p.rows().iter().map(|r| json!({"label": r.label, "group": r.group, "action": r.action_name()})).collect::<Vec<_>>(),
                }),
                None => json!({"open": false}),
            })
        }
        "toasts" => Ok(json!(state.borrow().plugins.toast_log)),
        "opens" => {
            let s = state.borrow();
            Ok(json!({"opened": s.plugins.opened, "routed": s.plugins.routed, "refused": s.plugins.refused}))
        }
        "chrome" => {
            let s = state.borrow();
            let model_status: Vec<Value> = s
                .plugins
                .status
                .iter()
                .map(|x| json!({"plugin": x.plugin, "text": x.text, "icon": x.icon, "tone": x.tone, "command": x.command}))
                .collect();
            let actions: Vec<Value> = s
                .chrome
                .actions()
                .into_iter()
                .map(|(a, r)| json!({"action": a, "rect": [r.x, r.y, r.w, r.h]}))
                .collect();
            Ok(json!({
                "pane_app": s.chrome_pane_app(),
                "tab_apps": s.chrome_tab_apps(),
                "agent_badges": s.chrome.agent_badges().into_iter().map(|(t, r)| json!({"tab": t, "rect": [r.x, r.y, r.w, r.h]})).collect::<Vec<_>>(),
                "hover_link": s.hover_link,
                "tooltip": s.hover_title.as_ref().map(|(t, x, y)| json!({"text": t, "at": [x, y]})),
                "tooltip_drawn": s.chrome.tooltip().map(|(t, r)| json!({"text": t, "rect": [r.x, r.y, r.w, r.h]})),
                "find_bar": s.find.as_ref().is_some_and(|f| f.open),
                "status": model_status,
                "tab_titles": s.plugins.tab_titles,
                "window_title": s.plugins.window_title,
                "actions": actions,
                "menu": s.overlays.menu.as_ref().map(|m| m.entries.iter().map(|e| json!({"label": e.label, "action": e.action})).collect::<Vec<_>>()),
                "viewer": s.overlays.viewer.as_ref().map(|v| json!({
                    "pane": v.pane,
                    "index": v.zoom.index,
                    "images": v.zoom.images.iter().map(|i| json!({"id": i.id, "blob": i.blob, "title": i.title})).collect::<Vec<_>>(),
                })),
            }))
        }
        "chrome-style" => {
            // The computed style of a plugin's status segment, to check plugin sheets restyle
            // the chrome.
            let plugin = args.first().ok_or("chrome-style: expected a plugin id")?.clone();
            let s = state.borrow();
            let doc = &s.chrome.doc;
            let mut stack = vec![doc.root()];
            while let Some(n) = stack.pop() {
                if doc.attribute(n, "data-plugin") == Some(plugin.as_str())
                    && let Some(style) = doc.computed_style(n)
                {
                    return Ok(json!({"font_weight": format!("{:?}", style.font_weight)}));
                }
                stack.extend(doc.children(n).iter().copied());
            }
            Err(format!("no status segment of plugin {plugin}"))
        }
        "find" => Ok(state
            .borrow()
            .find
            .as_ref()
            .map_or_else(|| json!({"open": false, "count": 0}), |f| f.json())),
        "node-box" => {
            let pane = pane_arg(&args)?;
            let id = args.get(1).ok_or("node-box: expected PANE ID")?;
            let s = state.borrow();
            let b = s
                .node_box(pane, id)
                .ok_or_else(|| format!("node {id} isn't drawn in pane {pane}"))?;
            Ok(json!([b.x, b.y, b.w, b.h]))
        }
        "toast" => {
            // The toast a pane's toast layer shows (null: none), with the font families its
            // icon and its text are drawn in.
            let pane = pane_arg(&args)?;
            let s = state.borrow();
            let sf = s
                .surfaces
                .get(&pane)
                .ok_or_else(|| format!("pane {pane} isn't followed"))?;
            let doc = sf.doc();
            Ok(sf.toast().map_or(Value::Null, |t| {
                let icon: String = doc
                    .children(t.icon)
                    .iter()
                    .filter_map(|&c| doc.node(c).text())
                    .collect();
                json!({
                    "text": t.text,
                    "icon": icon,
                    "icon_fonts": doc.text_font_families(t.icon),
                    "text_fonts": doc.text_font_families(t.el),
                })
            }))
        }
        "node-fonts" => {
            // The font families a drawn surface node's text is set in, after fallback.
            let pane = pane_arg(&args)?;
            let id = args.get(1).ok_or("node-fonts: expected PANE ID")?;
            let s = state.borrow();
            let sf = s
                .surfaces
                .get(&pane)
                .ok_or_else(|| format!("pane {pane} isn't followed"))?;
            let el = {
                let shared = s.conn.shared();
                let tsp = shared
                    .panes
                    .get(&pane)
                    .ok_or_else(|| format!("pane {pane} isn't followed"))?
                    .replica
                    .tsp();
                tsp.surfaces()
                    .iter()
                    .rev()
                    .find_map(|x| sf.view().node_element(x.key(), id))
                    .ok_or_else(|| format!("node {id} isn't drawn in pane {pane}"))?
            };
            Ok(json!(sf.doc().text_font_families(el)))
        }
        "field-box" => {
            let pane = pane_arg(&args)?;
            let s = state.borrow();
            let (b, cell) = s
                .field_box(pane)
                .ok_or_else(|| format!("pane {pane} has no focused field drawn"))?;
            Ok(json!({"box": [b.x, b.y, b.w, b.h], "cell": [cell.0, cell.1]}))
        }
        // Lenses, presence, settings and the other window-half commands (window/extras.rs).
        other => state
            .borrow_mut()
            .extra_command(&window, other, &args)
            .unwrap_or_else(|| Err(format!("unknown command `{other}`"))),
    }
}

fn coords(args: &[String]) -> Result<(f64, f64), String> {
    match args {
        [x, y, ..] => match (x.parse(), y.parse()) {
            (Ok(x), Ok(y)) => Ok((x, y)),
            _ => Err("expected two numbers".into()),
        },
        _ => Err("expected two numbers".into()),
    }
}

/// Count, p50, p95 and max of durations, in milliseconds.
fn summary(times: &[Duration]) -> Value {
    if times.is_empty() {
        return json!({"count": 0});
    }
    let mut ms: Vec<f64> = times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
    json!({
        "count": ms.len(),
        "p50": at(0.5),
        "p95": at(0.95),
        "max": ms[ms.len() - 1],
    })
}
