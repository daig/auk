//! A headless window's scenario commands (§11.3): harness, control and terminal commands. Each
//! settles before it answers; waits on real work (a prompt, expected text, a replica catching
//! up) give up after 20 s.
//!
//! | Command | Does |
//! | --- | --- |
//! | `size WxH[@S]` / `size W H` | the window's size in points (and scale) |
//! | `appearance light\|dark` | the window's appearance |
//! | `wait MS` | advances the clock (pinned in scenario mode) |
//! | `freeze-anims`, `thaw`, `motion-at MS` | stop the clock; show motion MS into the freeze |
//! | `key KEY…`, `type TEXT…`, `paste TEXT…` | input to the focused pane |
//! | `shot NAME` | writes `NAME.png` and `NAME.json` (the layout) to the output dir |
//! | `tree [PANE] [SELECTOR]` | the focused (or given) pane's surface boxes |
//! | `css [PANE] [SELECTOR [PROP…]]` | the installed sheets in cascade order (program sheets as `tsp:<scope>:<name>`), or the computed values of the first matching element |
//! | `state`, `dump`, `perf` | the window's state; plus every pane's text; shot timings |
//! | `ready` | a first session if there is none, at its prompt |
//! | `run TEXT…` | types a command line and Enter; waits for the next prompt |
//! | `expect TEXT…` | waits until the focused pane shows TEXT |
//! | `split right\|down [CMD…]`, `new tab\|session`, `close [PANE]`, `focus PANE\|DIRECTION` | layout |
//! | `feed TEXT` | TEXT as program output in the focused pane (through `surface-play`) |
//! | `surface-play FILE [from N] [until N] [paced [TIMES]]` | plays a recording into the focused pane |
//! | `plugins fixtures\|list\|reload\|link DIR\|unlink ID` | the daemon's plugins |
//!
//! Selectors are a tag and classes (`div.sf-row`, `.sf-text`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use auk_mirror::{ClientMsg, PaneId};
use serde_json::{Value, json};

use super::draw::{self, Drawer, PaneInput, WindowSpec};
use super::{Headless, View};

/// How long a wait on real work may take.
pub const REAL_WORK: Duration = Duration::from_secs(20);

/// How long the stream must be quiet before a command counts as settled.
const QUIET: Duration = Duration::from_millis(60);

/// The window's clock: pinned (scenario mode) or real.
#[derive(Debug)]
enum Clock {
    Pinned { ms: f64 },
    Real { start: Instant },
}

/// Drawing and timing state of a headless window.
#[derive(Debug)]
pub struct WindowState {
    pub spec: WindowSpec,
    pub out: PathBuf,
    clock: Clock,
    /// `freeze-anims`: the clock reading when frozen; `motion-at` offsets from it.
    frozen: Option<f64>,
    motion: f64,
    shots: u64,
}

impl WindowState {
    pub fn new(spec: WindowSpec, out: PathBuf, pinned: bool) -> WindowState {
        WindowState {
            spec,
            out,
            clock: if pinned {
                Clock::Pinned { ms: 0.0 }
            } else {
                Clock::Real {
                    start: Instant::now(),
                }
            },
            frozen: None,
            motion: 0.0,
            shots: 0,
        }
    }

    /// The view clock, in ms.
    pub fn now(&self) -> f64 {
        if let Some(at) = self.frozen {
            return at + self.motion;
        }
        match &self.clock {
            Clock::Pinned { ms } => *ms,
            Clock::Real { start } => start.elapsed().as_secs_f64() * 1000.0,
        }
    }
}

fn words_text(args: &[String]) -> String {
    args.join(" ")
}

impl Headless {
    fn state_lock(&self) -> std::sync::MutexGuard<'_, WindowState> {
        self.script.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends a command to the daemon's control endpoint.
    fn daemon_request(&self, words: &[String]) -> Result<Value, String> {
        let path = self
            .host()
            .daemon
            .ok_or("this window's host has no control endpoint here")?;
        let mut c = crate::control::Control::connect(&path)
            .map_err(|e| format!("the daemon's control endpoint: {e}"))?;
        c.request(words).map_err(|e| e.to_string())
    }

    fn focused(&self) -> Result<PaneId, String> {
        self.view()
            .focused
            .ok_or_else(|| "no pane is focused".into())
    }

    pub(super) fn set_focus(&self, pane: PaneId) -> Result<(), String> {
        self.view().focused = Some(pane);
        self.sent(&ClientMsg::Focus { pane }).map(drop)
    }

    /// Runs `f` on the draw thread with the drawer, making it on first use. `f` must not wait on
    /// anything the calling thread holds; it may lock the view.
    fn with_drawer<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Headless, &mut Drawer) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let spec = self.state_lock().spec;
        let (tx, rx) = std::sync::mpsc::channel();
        let job: super::DrawJob = Box::new(move |h: &Headless, slot: &mut Option<Drawer>| {
            let result = (|| {
                if slot.is_none() {
                    *slot = Some(Drawer::new(spec)?);
                }
                let d = slot.as_mut().expect("made above");
                d.set_spec(spec);
                f(h, d)
            })();
            let _ = tx.send(result);
        });
        self.draw_tx
            .get()
            .ok_or("the window has no draw thread")?
            .send(job)
            .map_err(|_| "the draw thread stopped")?;
        rx.recv()
            .map_err(|_| "the draw thread stopped".to_owned())?
    }

    /// Asks the shown panes for the size this window gives them. Returns whether they have it.
    fn fit(&self) -> Result<bool, String> {
        let (window, focused) = {
            let v = self.view();
            (v.window.clone(), v.focused)
        };
        let wanted = self.with_drawer(move |_, d| d.fit(&window, focused))?;
        let mut fitted = true;
        for (pane, cols, rows) in wanted {
            let size = self
                .view()
                .replicas
                .get(&pane)
                .map(|r| (r.terminal().cols(), r.terminal().rows()));
            if size.is_some_and(|s| s != (cols, rows)) {
                fitted = false;
                self.sent(&ClientMsg::Resize { pane, cols, rows })?;
            }
        }
        Ok(fitted)
    }

    /// Waits until the window has caught up: every live pane's replica at the daemon's stream
    /// position, shown panes at their size (scenario mode), and the stream quiet.
    fn settle(&self) -> Result<(), String> {
        let deadline = Instant::now() + REAL_WORK;
        let mut last_resize = Instant::now() - Duration::from_secs(1);
        loop {
            let fitted = if self.deterministic {
                // Resizes go out at most every 200 ms while the daemon catches up.
                if last_resize.elapsed() >= Duration::from_millis(200) {
                    last_resize = Instant::now();
                    self.fit()?
                } else {
                    let (window, focused) = {
                        let v = self.view();
                        (v.window.clone(), v.focused)
                    };
                    let wanted = self.with_drawer(move |_, d| d.fit(&window, focused))?;
                    let v = self.view();
                    wanted.iter().all(|(p, c, r)| {
                        v.replicas
                            .get(p)
                            .is_none_or(|x| (x.terminal().cols(), x.terminal().rows()) == (*c, *r))
                    })
                }
            } else {
                true
            };
            let caught_up = self.caught_up()?;
            let quiet = self
                .view()
                .last_message
                .is_none_or(|t| t.elapsed() >= QUIET);
            if fitted && caught_up && quiet {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the window didn't settle in {} s (fitted {fitted}, caught up {caught_up})",
                    REAL_WORK.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn caught_up(&self) -> Result<bool, String> {
        if self.host().daemon.is_none() {
            return Ok(true);
        }
        let panes: Vec<(PaneId, u64)> = {
            let v = self.view();
            v.replicas
                .iter()
                .filter(|(id, _)| !v.exited.contains_key(id))
                .map(|(id, r)| (*id, r.seq()))
                .collect()
        };
        for (pane, seq) in panes {
            match self.daemon_request(&["state-hash".into(), pane.to_string()]) {
                Ok(v) if v["seq"].as_u64() == Some(seq) => {}
                Ok(_) => return Ok(false),
                // A pane the daemon dropped meanwhile.
                Err(e) if e.starts_with("no pane") => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// Waits until the window has a replica of `pane`.
    fn wait_replica(&self, pane: PaneId) -> Result<(), String> {
        let deadline = Instant::now() + REAL_WORK;
        while !self.view().replicas.contains_key(&pane) {
            if Instant::now() >= deadline {
                return Err(format!("pane {pane} never reached the window"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn wait_prompt(&self, pane: PaneId) -> Result<(), String> {
        self.daemon_request(&[
            "wait".into(),
            pane.to_string(),
            "--until".into(),
            "prompt".into(),
            "--timeout".into(),
            REAL_WORK.as_secs().to_string(),
        ])
        .map(drop)
        .map_err(|e| format!("pane {pane}: no prompt: {e}"))
    }

    fn pane_info(&self, pane: PaneId) -> Result<Value, String> {
        let ls = self.daemon_request(&["ls".into()])?;
        ls["panes"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["id"] == pane).cloned())
            .ok_or_else(|| format!("no pane {pane}"))
    }

    /// A new pane: placed, focused, sized, at its prompt when it is a shell.
    fn adopt(&self, pane: PaneId, shell: bool) -> Result<(), String> {
        self.wait_replica(pane)?;
        self.set_focus(pane)?;
        self.settle()?;
        if shell {
            self.wait_prompt(pane)?;
            self.settle()?;
        }
        Ok(())
    }

    /// Draws the window.
    fn draw_now(&self) -> Result<(draw::Timing, auk_render::Image, Value), String> {
        let now = self.state_lock().now();
        self.with_drawer(move |h, d| {
            let mut view = h.view();
            let View {
                replicas,
                window,
                focused,
                pending,
                reset,
                ..
            } = &mut *view;
            let resets: Vec<PaneId> = std::mem::take(reset).into_iter().collect();
            d.retain(&|p| replicas.contains_key(&p) && !resets.contains(&p));
            let inputs: Vec<PaneInput<'_>> = replicas
                .iter()
                .map(|(id, r)| PaneInput {
                    pane: *id,
                    replica: r,
                    changes: pending.remove(id).unwrap_or_default(),
                })
                .collect();
            let (image, layout) = d.draw(window, *focused, &inputs, now)?;
            let timing = d.timings.last().copied().unwrap_or_default();
            Ok((timing, image, layout))
        })
    }

    fn state_json(&self) -> Value {
        let ws = self.state_lock();
        let (spec, now) = (ws.spec, ws.now());
        drop(ws);
        let v = self.view();
        let panes: Vec<Value> = v
            .replicas
            .iter()
            .map(|(id, r)| {
                let t = r.terminal();
                json!({
                    "id": id,
                    "cols": t.cols(),
                    "rows": t.rows(),
                    "title": t.title(),
                    "seq": r.seq(),
                    "exit": v.exited.get(id),
                    "primary": v.primary.get(id).copied().unwrap_or(false),
                    "surfaces": r.tsp().surfaces().len(),
                })
            })
            .collect();
        json!({
            "window": {
                "key": self.window_key,
                "size": [spec.width, spec.height],
                "scale": spec.scale,
                "appearance": if spec.dark { "dark" } else { "light" },
                "clock": now,
            },
            "focused": v.focused,
            "layout": v.window,
            "panes": panes,
            "errors": v.errors,
        })
    }

    /// A scenario command, or `None` when `cmd` isn't one.
    pub(super) fn scenario_command(
        self: &Arc<Self>,
        cmd: &str,
        args: &[String],
    ) -> Option<Result<Value, String>> {
        Some(match cmd {
            "size" => self.cmd_size(args),
            "appearance" => match args.first().map(String::as_str) {
                Some("light") | Some("dark") => {
                    self.state_lock().spec.dark = args[0] == "dark";
                    self.settle().map(|()| Value::Null)
                }
                _ => Err("appearance: expected light or dark".into()),
            },
            "wait" => self.cmd_wait(args),
            "freeze-anims" => {
                let mut ws = self.state_lock();
                ws.frozen = Some(ws.now());
                ws.motion = 0.0;
                Ok(Value::Null)
            }
            "thaw" => {
                let mut ws = self.state_lock();
                if let (Some(at), Clock::Pinned { ms }) = (ws.frozen, &mut ws.clock) {
                    *ms = at;
                }
                ws.frozen = None;
                Ok(Value::Null)
            }
            "motion-at" => match args.first().map(|a| a.parse::<f64>()) {
                Some(Ok(ms)) if ms >= 0.0 => {
                    let mut ws = self.state_lock();
                    if ws.frozen.is_none() {
                        ws.frozen = Some(ws.now());
                    }
                    ws.motion = ms;
                    Ok(Value::Null)
                }
                _ => Err("motion-at: expected milliseconds".into()),
            },
            "key" | "type" | "paste" => self.cmd_input(cmd, args),
            "shot" => self.cmd_shot(args),
            "tree" => self.cmd_tree(args),
            "css" => self.cmd_css(args),
            "state" => self.settle().map(|()| self.state_json()),
            "dump" => self.cmd_dump(),
            "perf" => self.cmd_perf(),
            "ready" => self.cmd_ready(),
            "run" => self.cmd_run(args),
            "expect" => self.cmd_expect(args),
            "split" => self.cmd_split(args),
            "new" => self.cmd_new(args),
            "close" => self.cmd_close(args),
            "feed" => self.cmd_feed(args),
            "surface-play" => self.cmd_play(args),
            "plugins" => self.cmd_plugins(args),
            "remote" => self.cmd_remote(args),
            _ if cmd == "focus" && args.first().is_some_and(|a| a.parse::<PaneId>().is_err()) => {
                self.cmd_focus_dir(&args[0])
            }
            _ => return None,
        })
    }

    fn cmd_size(&self, args: &[String]) -> Result<Value, String> {
        let text = match args {
            [one] => one.clone(),
            [w, h] => format!("{w}x{h}"),
            _ => return Err("size: expected WxH[@S] or W H".into()),
        };
        self.state_lock().spec.parse_size(&text)?;
        self.settle()?;
        Ok(Value::Null)
    }

    fn cmd_wait(&self, args: &[String]) -> Result<Value, String> {
        let ms = args
            .first()
            .and_then(|a| a.parse::<f64>().ok())
            .filter(|ms| *ms >= 0.0)
            .ok_or("wait: expected milliseconds")?;
        let real = {
            let mut ws = self.state_lock();
            let frozen = ws.frozen.is_some();
            match (&mut ws.clock, frozen) {
                (Clock::Pinned { ms: now }, false) => {
                    *now += ms;
                    false
                }
                (Clock::Pinned { .. }, true) => false,
                (Clock::Real { .. }, _) => true,
            }
        };
        if real {
            std::thread::sleep(Duration::from_secs_f64(ms / 1000.0));
        }
        self.settle()?;
        Ok(Value::Null)
    }

    fn cmd_input(&self, cmd: &str, args: &[String]) -> Result<Value, String> {
        let pane = self.focused()?;
        let mode = match cmd {
            "key" => "keys",
            "type" => "text",
            _ => "paste",
        };
        let mut words = vec![pane.to_string(), mode.to_owned()];
        words.extend(args.iter().cloned());
        self.cmd_send(&words)?;
        self.settle()?;
        Ok(Value::Null)
    }

    fn cmd_shot(&self, args: &[String]) -> Result<Value, String> {
        let name = match args.first() {
            Some(n) if !n.is_empty() && !n.contains('/') && n != ".." => n.clone(),
            Some(n) => return Err(format!("shot: bad name `{n}`")),
            None => {
                let mut ws = self.state_lock();
                ws.shots += 1;
                format!("shot-{}", ws.shots)
            }
        };
        self.settle()?;
        let out = self.state_lock().out.clone();
        std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        let (timing, image, layout) = self.draw_now()?;
        let png = out.join(format!("{name}.png"));
        let json_path = out.join(format!("{name}.json"));
        image
            .save_png(&png)
            .map_err(|e| format!("{}: {e}", png.display()))?;
        let text = serde_json::to_string_pretty(&layout).expect("JSON serializes") + "\n";
        std::fs::write(&json_path, text).map_err(|e| format!("{}: {e}", json_path.display()))?;
        Ok(json!({
            "png": png,
            "json": json_path,
            "size": [image.width, image.height],
            "ms": timing.cpu_ms,
        }))
    }

    /// `[PANE]` then the rest.
    fn pane_arg<'a>(&self, args: &'a [String]) -> Result<(PaneId, &'a [String]), String> {
        match args.first().map(|a| a.parse::<PaneId>()) {
            Some(Ok(p)) => Ok((p, &args[1..])),
            _ => Ok((self.focused()?, args)),
        }
    }

    fn cmd_tree(&self, args: &[String]) -> Result<Value, String> {
        let (pane, rest) = self.pane_arg(args)?;
        self.settle()?;
        self.draw_now()?;
        let sel = rest.first().cloned();
        self.with_drawer(move |_, d| d.tree(pane, sel.as_deref()))
    }

    fn cmd_css(&self, args: &[String]) -> Result<Value, String> {
        let (pane, rest) = self.pane_arg(args)?;
        let Some((sel, props)) = rest.split_first() else {
            // No selector: the installed sheets, in cascade order.
            self.settle()?;
            self.draw_now()?;
            return self.with_drawer(move |_, d| d.sheets(pane));
        };
        let props: Vec<String> = if props.is_empty() {
            [
                "display",
                "position",
                "color",
                "background-color",
                "font-size",
                "font-weight",
                "width",
                "height",
            ]
            .map(str::to_owned)
            .to_vec()
        } else {
            props.to_vec()
        };
        self.settle()?;
        self.draw_now()?;
        let sel = sel.clone();
        self.with_drawer(move |_, d| d.css(pane, &sel, &props))
    }

    fn cmd_dump(&self) -> Result<Value, String> {
        self.settle()?;
        let mut state = self.state_json();
        let v = self.view();
        let texts: serde_json::Map<String, Value> = v
            .replicas
            .iter()
            .map(|(id, r)| {
                let text = auk_daemon::capture::capture(
                    r.terminal(),
                    r.tsp(),
                    auk_daemon::capture::Options {
                        format: auk_daemon::capture::Format::Text,
                        scrollback: false,
                        surfaces: true,
                    },
                );
                (id.to_string(), json!(text))
            })
            .collect();
        state["text"] = Value::Object(texts);
        Ok(state)
    }

    fn cmd_perf(&self) -> Result<Value, String> {
        let timings = self.with_drawer(|_, d| Ok(d.timings.clone()))?;
        let mut cpu: Vec<f64> = timings.iter().map(|t| t.cpu_ms).collect();
        cpu.sort_by(f64::total_cmp);
        let pct = |p: f64| -> Option<f64> {
            (!cpu.is_empty()).then(|| cpu[((cpu.len() - 1) as f64 * p).round() as usize])
        };
        Ok(json!({
            "shots": timings.len(),
            "cpu_ms": {"p50": pct(0.5), "p95": pct(0.95), "max": cpu.last()},
            "readback_ms": timings.iter().map(|t| t.readback_ms).collect::<Vec<_>>(),
        }))
    }

    fn cmd_ready(&self) -> Result<Value, String> {
        let existing = self.view().focused;
        let pane = match existing {
            Some(p) => p,
            None => {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                let v = self.daemon_request(&[
                    "new".into(),
                    "session".into(),
                    "--window".into(),
                    self.window_key.clone(),
                    "--cwd".into(),
                    home,
                ])?;
                v["pane"].as_u64().ok_or("new: no pane id")?
            }
        };
        let shell = self.pane_info(pane)?["kind"] == "shell";
        self.adopt(pane, shell)?;
        Ok(json!({"pane": pane}))
    }

    fn cmd_run(&self, args: &[String]) -> Result<Value, String> {
        let pane = self.focused()?;
        if args.is_empty() {
            return Err("run: expected a command line".into());
        }
        let at_prompt = self.pane_info(pane)?["at_prompt"] == true;
        let mut words = vec!["run".to_owned(), pane.to_string()];
        words.push(words_text(args));
        self.daemon_request(&words)?;
        if at_prompt {
            self.wait_prompt(pane)?;
        }
        self.settle()?;
        Ok(Value::Null)
    }

    fn cmd_expect(&self, args: &[String]) -> Result<Value, String> {
        let pane = self.focused()?;
        let want = words_text(args);
        if want.is_empty() {
            return Err("expect: expected text".into());
        }
        let deadline = Instant::now() + REAL_WORK;
        loop {
            let text = {
                let v = self.view();
                let r = v
                    .replicas
                    .get(&pane)
                    .ok_or_else(|| format!("no replica of pane {pane}"))?;
                auk_daemon::capture::capture(
                    r.terminal(),
                    r.tsp(),
                    auk_daemon::capture::Options {
                        format: auk_daemon::capture::Format::Text,
                        scrollback: false,
                        surfaces: true,
                    },
                )
            };
            if text.contains(&want) {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "expect: `{want}` didn't show in {} s; the pane shows:\n{text}",
                    REAL_WORK.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.settle()?;
        Ok(Value::Null)
    }

    fn cmd_split(&self, args: &[String]) -> Result<Value, String> {
        let pane = self.focused()?;
        let (dir, cmd) = args.split_first().ok_or("split: expected right or down")?;
        let mut words = vec!["split".to_owned(), pane.to_string(), dir.clone()];
        if !cmd.is_empty() {
            words.push("--".into());
            words.push(words_text(cmd));
        }
        let v = self.daemon_request(&words)?;
        let new = v["pane"].as_u64().ok_or("split: no pane id")?;
        self.adopt(new, cmd.is_empty())?;
        Ok(json!({"pane": new}))
    }

    fn cmd_new(&self, args: &[String]) -> Result<Value, String> {
        let what = args.first().map(String::as_str).unwrap_or("tab");
        let mut words = vec![
            "new".to_owned(),
            what.to_owned(),
            "--window".into(),
            self.window_key.clone(),
        ];
        if what == "tab" {
            // In the focused pane's session.
            let focused = self.focused()?;
            let ls = self.pane_info(focused)?;
            if let Some(s) = ls["session"].as_u64() {
                words.extend(["--session".into(), s.to_string()]);
            }
        }
        if args.len() > 1 {
            words.push("--".into());
            words.push(words_text(&args[1..]));
        }
        let v = self.daemon_request(&words)?;
        let new = v["pane"].as_u64().ok_or("new: no pane id")?;
        self.adopt(new, args.len() <= 1)?;
        Ok(json!({"pane": new}))
    }

    fn cmd_close(&self, args: &[String]) -> Result<Value, String> {
        let pane = match args.first() {
            Some(a) => a.parse().map_err(|_| format!("close: bad pane `{a}`"))?,
            None => self.focused()?,
        };
        self.daemon_request(&["close".into(), pane.to_string()])?;
        let deadline = Instant::now() + REAL_WORK;
        while self.view().window.panes().contains(&pane) {
            if Instant::now() >= deadline {
                return Err(format!("pane {pane} stayed in the layout"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.settle()?;
        Ok(Value::Null)
    }

    fn cmd_focus_dir(&self, dir: &str) -> Result<Value, String> {
        let from = self.focused()?;
        let (window, spec) = (self.view().window.clone(), self.state_lock().spec);
        let (_, tab) = draw::shown_tab(&window, Some(from)).ok_or("no tab is shown")?;
        let area = draw::Rect {
            x: 0.0,
            y: 0.0,
            w: f64::from(spec.width),
            h: f64::from(spec.height),
        };
        let rects = draw::layout_tab(tab, area);
        let a = rects
            .iter()
            .find(|(p, _)| *p == from)
            .map(|(_, r)| *r)
            .ok_or("the focused pane isn't shown")?;
        let overlap =
            |lo1: f64, hi1: f64, lo2: f64, hi2: f64| (hi1.min(hi2) - lo1.max(lo2)).max(0.0);
        let best = rects
            .iter()
            .filter(|(p, _)| *p != from)
            .filter_map(|(p, b)| {
                let (beside, dist, shared) = match dir {
                    "left" => (
                        b.x + b.w <= a.x + 0.5,
                        a.x - (b.x + b.w),
                        overlap(a.y, a.y + a.h, b.y, b.y + b.h),
                    ),
                    "right" => (
                        b.x >= a.x + a.w - 0.5,
                        b.x - (a.x + a.w),
                        overlap(a.y, a.y + a.h, b.y, b.y + b.h),
                    ),
                    "up" => (
                        b.y + b.h <= a.y + 0.5,
                        a.y - (b.y + b.h),
                        overlap(a.x, a.x + a.w, b.x, b.x + b.w),
                    ),
                    "down" => (
                        b.y >= a.y + a.h - 0.5,
                        b.y - (a.y + a.h),
                        overlap(a.x, a.x + a.w, b.x, b.x + b.w),
                    ),
                    _ => return None,
                };
                (beside && shared > 0.0).then_some((*p, shared, dist))
            })
            .max_by(|x, y| x.1.total_cmp(&y.1).then(y.2.total_cmp(&x.2)));
        if !["left", "right", "up", "down"].contains(&dir) {
            return Err(format!(
                "focus: expected a pane or left, right, up or down; got `{dir}`"
            ));
        }
        let (pane, _, _) = best.ok_or_else(|| format!("no pane {dir} of pane {from}"))?;
        self.set_focus(pane)?;
        self.settle()?;
        Ok(json!({"pane": pane}))
    }

    fn cmd_feed(&self, args: &[String]) -> Result<Value, String> {
        let pane = self.focused()?;
        let text = words_text(args);
        // A tty turns the program's newlines into CR LF (onlcr).
        let mut out = String::with_capacity(text.len());
        let mut prev = '\0';
        for c in text.chars() {
            if c == '\n' && prev != '\r' {
                out.push('\r');
            }
            out.push(c);
            prev = c;
        }
        let dir = std::env::temp_dir().join(format!("auk-feed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(format!("{pane}.jsonl"));
        let line = json!({"t": 0, "dir": "out", "verb": "pty", "params": {}, "body": out});
        std::fs::write(&file, format!("{line}\n")).map_err(|e| e.to_string())?;
        let result = self.daemon_request(&[
            "surface-play".into(),
            "--pane".into(),
            pane.to_string(),
            file.to_string_lossy().into_owned(),
        ]);
        let _ = std::fs::remove_dir_all(&dir);
        result?;
        self.settle()?;
        Ok(Value::Null)
    }

    fn cmd_play(&self, args: &[String]) -> Result<Value, String> {
        let pane = self.focused()?;
        let (file, opts) = args
            .split_first()
            .ok_or("surface-play: expected a recording")?;
        let file = std::path::absolute(file).map_err(|e| e.to_string())?;
        let mut words = vec![
            "surface-play".to_owned(),
            "--pane".into(),
            pane.to_string(),
            file.to_string_lossy().into_owned(),
        ];
        words.extend(opts.iter().cloned());
        let v = self.daemon_request(&words)?;
        self.settle()?;
        Ok(v)
    }

    /// `remote loopback`: a second private daemon becomes this window's host, attached the way
    /// a remote host is (§10.2): `auk attach --stdio` over a pipe, through `auk-remote`'s link,
    /// with a stand-in for `ssh` that runs the remote command here. No network is involved.
    fn cmd_remote(self: &Arc<Self>, args: &[String]) -> Result<Value, String> {
        match args {
            [l] if l == "loopback" => {}
            [l, ..] if l == "loopback" => return Err("remote loopback takes no address".into()),
            _ => return Err("remote: expected loopback".into()),
        }
        let private = crate::scenario::Private::start(&self.exe).map_err(|e| e.to_string())?;
        let ssh = private.root().join("ssh");
        // Skips ssh's options up to `--`, then the destination; runs the remote command.
        std::fs::write(
            &ssh,
            "#!/bin/sh\n# ssh stand-in for `remote loopback`\nwhile [ \"$1\" != \"--\" ]; do shift; done\nshift 2\nexec /bin/sh -c \"$1\"\n",
        )
        .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        let env: Vec<String> = private
            .env()
            .iter()
            .map(|(k, v)| format!("{k}={}", crate::control::quote(&[v.as_str()])))
            .collect();
        let remote_command = format!(
            "exec env {} {} attach --stdio",
            env.join(" "),
            crate::control::quote(&[self.exe.to_string_lossy().as_ref()])
        );
        let target = auk_remote::Target::Ssh(auk_remote::SshTarget {
            destination: "loopback".into(),
            program: ssh,
            config: None,
            options: Vec::new(),
            remote_command,
        });
        let host = super::Host {
            daemon: Some(auk_daemon::paths::control_socket(private.socket())),
            socket: private.socket().to_owned(),
            plugin_env: private.env(),
        };
        self.switch_host(target, host).map_err(|e| e.to_string())?;
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(private);
        // Connected once the host's welcome and layout have arrived.
        let deadline = Instant::now() + REAL_WORK;
        while self.link().status().label != "connected" {
            if Instant::now() >= deadline {
                return Err(format!("remote loopback: {}", self.link().status().label));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.settle()?;
        Ok(json!(self.link().status()))
    }

    fn cmd_plugins(&self, args: &[String]) -> Result<Value, String> {
        let host = self.host();
        let run = |words: &[String]| -> Result<String, String> {
            let mut cmd = std::process::Command::new(&self.exe);
            cmd.arg("--socket")
                .arg(&host.socket)
                .arg("plugin")
                .args(words);
            for (k, v) in &host.plugin_env {
                cmd.env(k, v);
            }
            let out = cmd.output().map_err(|e| format!("auk plugin: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "auk plugin {}: {}{}",
                    words.join(" "),
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let result = match args.first().map(String::as_str) {
            Some("fixtures") => {
                // The SDK's example plugins, from the fixture root (§21: `plugins fixtures`).
                let root = fixture_root()?;
                let dir = root.join("third_party/tern-sdk/plugins/examples");
                let entries =
                    std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let mut linked = Vec::new();
                let mut dirs: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.join("plugin.toml").is_file())
                    .collect();
                dirs.sort();
                for d in dirs {
                    run(&["link".into(), d.to_string_lossy().into_owned()])?;
                    linked.push(d);
                }
                json!({"linked": linked, "list": run(&["list".into(), "--json".into()])
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())})
            }
            Some(_) => {
                let text = run(args)?;
                serde_json::from_str(&text).unwrap_or(json!(text))
            }
            None => {
                return Err(
                    "plugins: expected fixtures, list, reload, link DIR or unlink ID".into(),
                );
            }
        };
        self.settle()?;
        Ok(result)
    }
}

/// The fixture root: `$AUK_FIXTURE_ROOT` (a checkout of this repository).
pub fn fixture_root() -> Result<PathBuf, String> {
    std::env::var_os("AUK_FIXTURE_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "set AUK_FIXTURE_ROOT to a checkout of the repository for fixtures".into())
}
