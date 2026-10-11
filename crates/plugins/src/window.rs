//! The window half's runtime: one VM per plugin per window, on the window's UI thread,
//! behind the [`WindowHost`] trait the app implements.
//!
//! [`WindowPlugins`] loads the window entries (manifest styles first), and gives the app what it
//! builds from them: palette rows and actions ([`WindowPlugins::commands`],
//! [`WindowPlugins::run_action`]), the plugin keymap layer ([`WindowPlugins::binds`]),
//! overrides, routes, chrome formatters (4 ms each, cached by input), sheets and window events.
//!
//! **Recursion guard.** Every call takes `&mut self` and lends the handler a `&mut dyn
//! WindowHost`, so while any plugin handler runs the runtime is out of the window: a host method
//! the handler calls (`cx:command`, `cx:open`) can't reach another plugin's override or route.
//! [`WindowHost::command`] must run the built-in command and [`WindowHost::open`] the built-in
//! open, without overrides or routes.
//!
//! **Window cx.** Each handler gets a fresh cx, valid only during the call; using a kept one
//! raises. It has no way to write a block's frames: blocks live in host workers.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use mlua::{AnyUserData, Function, UserData, UserDataFields, UserDataMethods, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::api::effects::ToastLevel;
use crate::api::tasks::Completer;
use crate::api::window::BindTarget;
use crate::budget::Budget;
use crate::catalog::Catalog;
use crate::discovery::{self, PluginSettings};
use crate::manifest::Manifest;
use crate::paths;
use crate::vm::{CallError, Context, LogSink, Vm, VmOptions, tracing_log_sink};

/// Where a file or block opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum How {
    /// The block showing it, else a new one.
    Default,
    /// A split beside the focused pane.
    Beside,
    /// A split below it.
    Below,
    /// A split the gesture picked (`beside` in a decision).
    Split,
    /// A new tab.
    Tab,
    /// In place of the focused block.
    Replace,
    /// The tab's preview block.
    Preview,
}

impl How {
    /// Parses a `how`.
    pub fn parse(s: &str) -> Option<How> {
        Some(match s {
            "default" => How::Default,
            "beside" => How::Beside,
            "below" => How::Below,
            "split" => How::Split,
            "tab" => How::Tab,
            "replace" => How::Replace,
            "preview" => How::Preview,
            _ => return None,
        })
    }
}

/// A side of a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// Right.
    Right,
    /// Down.
    Down,
    /// Left.
    Left,
    /// Up.
    Up,
}

impl Dir {
    /// Parses a direction.
    pub fn parse(s: &str) -> Option<Dir> {
        Some(match s {
            "right" => Dir::Right,
            "down" => Dir::Down,
            "left" => Dir::Left,
            "up" => Dir::Up,
            _ => return None,
        })
    }
}

/// `Reveal = {focus?}`: whether a result takes the focus (default true).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reveal {
    /// Focus the result.
    pub focus: bool,
}

/// What a new pane runs (`Launch`); empty is an interactive shell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    /// Working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// A command line for the login shell.
    #[serde(default)]
    pub command: Option<String>,
    /// A plugin block kind.
    #[serde(default)]
    pub block: Option<String>,
    /// Block launch arguments.
    #[serde(default)]
    pub args: Option<Vec<String>>,
}

/// A workspace tab for `cx.layout:tab`: a leaf (`launch`) or a split of `first` and `second`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TabSpec {
    /// Working directory (inherited downward).
    pub cwd: Option<String>,
    /// Tab name (root only).
    pub name: Option<String>,
    /// Direction of the split.
    pub split: Option<Dir>,
    /// The first side's share.
    pub ratio: Option<f64>,
    /// What a leaf runs.
    pub launch: Option<Launch>,
    /// `[1]`.
    pub first: Option<Box<TabSpec>>,
    /// `[2]`.
    pub second: Option<Box<TabSpec>>,
}

/// A pane as the window sees it (`PaneInfo` in the window context).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowPane {
    /// Pane id (window ids, with remote tags).
    pub pane: u64,
    /// Working directory (`""` when unknown).
    pub cwd: String,
    /// The program.
    pub program: String,
    /// Title.
    pub title: String,
    /// A command is running.
    pub busy: bool,
    /// `terminal`, `agent`, `plugin`, …
    pub kind: String,
    /// Its tab (none when parked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<u64>,
    /// Whether it has the focus.
    pub focused: bool,
    /// A plugin block's kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
}

/// A tab (`TabInfo`), also the `tab_title` formatter's input.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    /// Tab id.
    pub id: u64,
    /// Its session.
    pub session: u64,
    /// Color label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// One-based position.
    pub position: u64,
    /// Focused pane.
    pub pane: u64,
    /// Name the user gave it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Focused pane's cwd (`""` when unknown).
    pub cwd: String,
    /// Focused pane's program.
    pub program: String,
    /// Focused pane's title.
    pub title: String,
    /// Number of panes.
    pub panes: u64,
    /// A command is running.
    pub busy: bool,
}

/// The `status` formatter's input: the focused pane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusPane {
    /// Pane id.
    pub pane: u64,
    /// Working directory (`""` when unknown).
    pub cwd: String,
    /// Program.
    pub program: String,
    /// Title.
    pub title: String,
    /// A command is running.
    pub busy: bool,
}

/// A status line segment, tagged with its plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSegment {
    /// The plugin (`data-plugin`).
    pub plugin: String,
    /// Text.
    pub text: String,
    /// Icon name.
    pub icon: Option<String>,
    /// Tone.
    pub tone: Option<String>,
    /// Action run on click.
    pub command: Option<String>,
}

/// A file open `route.open` may claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenRequest {
    /// Absolute path.
    pub path: String,
    /// 1-based line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// 1-based column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u64>,
    /// The file's host.
    pub host: String,
    /// Where it opens.
    pub how: How,
    /// `palette`, `files`, `drop`, `link`, `system` or `cli`.
    pub origin: String,
}

/// A link click `route.link` may claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkRequest {
    /// The URL.
    pub url: String,
    /// The pane it is in.
    pub pane: u64,
    /// Modifiers held.
    pub mods: LinkMods,
}

/// Modifiers of a link click.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LinkMods {
    /// Cmd.
    pub cmd: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Ctrl.
    pub ctrl: bool,
}

/// A route's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Open a plugin block of this kind with these args.
    Block {
        /// `<plugin>.<block>`.
        kind: String,
        /// Launch arguments.
        args: Vec<String>,
    },
    /// Open this URL instead.
    Url(String),
    /// Rewrite the path (and placement), then the built-in open.
    Path {
        /// The path.
        path: String,
        /// Placement.
        how: Option<How>,
    },
    /// The plugin handled it.
    Handled,
}

/// A window event, queued by the session and delivered after each render.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowEvent {
    /// The focused pane of the active tab changed.
    Focus {
        /// Pane.
        pane: u64,
        /// Its tab.
        tab: u64,
    },
    /// This window added a pane.
    PaneCreated {
        /// Pane.
        pane: u64,
    },
    /// This window closed a pane.
    PaneClosed {
        /// Pane.
        pane: u64,
    },
    /// A tab appeared.
    TabCreated {
        /// Tab.
        tab: u64,
    },
    /// A tab closed.
    TabClosed {
        /// Tab.
        tab: u64,
    },
    /// A command started (no cwd in the window event).
    CommandStarted {
        /// Pane.
        pane: u64,
        /// The line.
        line: String,
    },
    /// A command finished.
    CommandFinished {
        /// Pane.
        pane: u64,
        /// The line.
        line: String,
        /// Exit status.
        status: i32,
        /// Duration, measured by the window.
        took_ms: f64,
    },
    /// A pane's directory changed.
    Cwd {
        /// Pane.
        pane: u64,
        /// The path (on the pane's host).
        path: String,
    },
    /// A pane's title changed.
    Title {
        /// Pane.
        pane: u64,
        /// Title.
        title: String,
    },
}

impl WindowEvent {
    /// The `tern.on` name.
    pub fn name(&self) -> &'static str {
        match self {
            WindowEvent::Focus { .. } => "focus",
            WindowEvent::PaneCreated { .. } => "pane_created",
            WindowEvent::PaneClosed { .. } => "pane_closed",
            WindowEvent::TabCreated { .. } => "tab_created",
            WindowEvent::TabClosed { .. } => "tab_closed",
            WindowEvent::CommandStarted { .. } => "command_started",
            WindowEvent::CommandFinished { .. } => "command_finished",
            WindowEvent::Cwd { .. } => "cwd",
            WindowEvent::Title { .. } => "title",
        }
    }

    /// The payload.
    pub fn payload(&self) -> Json {
        match self {
            WindowEvent::Focus { pane, tab } => json!({"pane": pane, "tab": tab}),
            WindowEvent::PaneCreated { pane } | WindowEvent::PaneClosed { pane } => {
                json!({"pane": pane})
            }
            WindowEvent::TabCreated { tab } | WindowEvent::TabClosed { tab } => json!({"tab": tab}),
            WindowEvent::CommandStarted { pane, line } => json!({"pane": pane, "line": line}),
            WindowEvent::CommandFinished {
                pane,
                line,
                status,
                took_ms,
            } => json!({"pane": pane, "line": line, "status": status, "took_ms": took_ms}),
            WindowEvent::Cwd { pane, path } => json!({"pane": pane, "path": path}),
            WindowEvent::Title { pane, title } => json!({"pane": pane, "title": title}),
        }
    }
}

/// What the app provides to window plugin code (the window cx).
///
/// Pane and tab ids are the window's. Errors are the message the cx call raises.
pub trait WindowHost {
    /// `cx:run(pane, text)`: types into the pane's program. Error: `no program runs in that pane`.
    fn run(&mut self, pane: u64, text: &str) -> Result<(), String>;
    /// `cx:open(target, how?, reveal?)`: the built-in open; routes don't run.
    fn open(&mut self, target: &str, how: Option<How>, reveal: Reveal) -> Result<(), String>;
    /// `cx:new_block(kind, args, how, reveal)`: on the window's current host; `None` when no
    /// Ready plugin there defines the kind (no toast), and in a remote window, whose link opens
    /// the block but carries no pane id back.
    fn new_block(&mut self, kind: &str, args: &[String], how: How, reveal: Reveal) -> Option<u64>;
    /// `cx:command(id)`: a built-in command; overrides don't run. Error:
    /// ``unknown command `<id>` ``.
    fn command(&mut self, id: &str) -> Result<(), String>;
    /// `cx:action(name)`: any action, plugin commands included.
    fn action(&mut self, name: &str) -> Result<(), String>;
    /// `cx:toast`.
    fn toast(&mut self, level: ToastLevel, text: &str, sub: Option<&str>);
    /// `cx:copy`.
    fn copy(&mut self, text: &str);
    /// `cx.session:panes()`: every pane in the window's tabs, parked ones included.
    fn panes(&self) -> Vec<WindowPane>;
    /// `cx.session:tabs()`: every tab in every session of the window.
    fn tabs(&self) -> Vec<TabInfo>;
    /// `cx.session:focused()`.
    fn focused(&self) -> Option<u64>;
    /// `cx.session:resolve(spec)`: `@focused`, an id, a title, a program.
    fn resolve(&self, spec: &str) -> Option<u64>;
    /// `cx.session:sessions()`: `{id, name, current, locked, tabs}` each.
    fn sessions(&self) -> Json;
    /// `cx.session:layout(tab)`.
    fn layout(&self, tab: u64) -> Option<Json>;
    /// `cx.session:read(pane, opts)`.
    fn read(&self, pane: u64, opts: &Json) -> Option<Json>;
    /// `cx.session:surface(pane, opts)`.
    fn surface(&self, pane: u64, opts: &Json) -> Option<Json>;
    /// `cx.session:event(pane, ev)`: sends a TSP event to the pane's program.
    fn event(&mut self, pane: u64, ev: &Json) -> Result<(), String>;
    /// `cx.session:settle(pane, ms)`: resolve `done` with the command's output once it
    /// finishes, or at the deadline; a closed pane fails.
    fn settle(&mut self, pane: u64, ms: f64, done: Completer);
    /// `cx.layout:new_tab(launch?, reveal?)`.
    fn new_tab(&mut self, launch: &Launch, reveal: Reveal) -> Option<u64>;
    /// `cx.layout:split(target, dir, launch?, reveal?)`.
    fn split(&mut self, target: u64, dir: Dir, launch: &Launch, reveal: Reveal) -> Option<u64>;
    /// `cx.layout:focus(pane)`.
    fn focus(&mut self, pane: u64);
    /// `cx.layout:close(pane)`.
    fn close(&mut self, pane: u64);
    /// `cx.layout:move(pane, target, dir|"swap", reveal?)`; `dir` `None` is a swap.
    fn move_pane(&mut self, pane: u64, target: u64, dir: Option<Dir>, reveal: Reveal) -> bool;
    /// `cx.layout:resize(pane, dir, cells)`.
    fn resize(&mut self, pane: u64, dir: Dir, cells: f64) -> bool;
    /// `cx.layout:tab(spec, reveal?)`: the tab, and any launch failure.
    fn tab(&mut self, spec: &TabSpec, reveal: Reveal) -> (Option<u64>, Option<String>);
}

/// How to build a window's plugin runtime.
#[derive(Clone)]
pub struct WindowConfig {
    /// `<config>/plugins/`.
    pub plugins_dir: PathBuf,
    /// `<state>` (holds `plugin-data/`).
    pub state_dir: PathBuf,
    /// `plugins` and `plugins_disabled`.
    pub settings: PluginSettings,
    /// Built-in palette command ids `tern.override` accepts.
    pub builtin_commands: Arc<Vec<String>>,
    /// Where `tern.log` and `print` go.
    pub log: LogSink,
    /// Wakes the UI thread when a process, request or settle completes.
    pub waker: Option<crate::api::tasks::Wake>,
}

impl std::fmt::Debug for WindowConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowConfig")
            .field("plugins_dir", &self.plugins_dir)
            .field("state_dir", &self.state_dir)
            .finish_non_exhaustive()
    }
}

impl WindowConfig {
    /// The standard locations, default settings, logs to tracing.
    pub fn standard(builtin_commands: Arc<Vec<String>>) -> Self {
        WindowConfig {
            plugins_dir: paths::plugins_dir(&paths::config_dir()),
            state_dir: paths::state_dir(),
            settings: PluginSettings::default(),
            builtin_commands,
            log: tracing_log_sink(),
            waker: None,
        }
    }
}

/// A palette row a plugin added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteCommand {
    /// `plugin.<plugin>.<id>`.
    pub action: String,
    /// Plugin id.
    pub plugin: String,
    /// Command id.
    pub id: String,
    /// Label.
    pub title: String,
    /// Icon.
    pub icon: String,
    /// Group (default: the plugin's name).
    pub group: String,
}

/// A plugin keymap entry: a chord and the action it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginBind {
    /// Plugin id.
    pub plugin: String,
    /// The chord as written.
    pub chord: String,
    /// The action (`plugin.<p>.bind.<n>` for a function bind).
    pub action: String,
}

struct WindowPlugin {
    id: String,
    name: String,
    vm: Vm,
    css: String,
    /// The plugin's directory, which its sheets' `url()`s resolve against.
    dir: PathBuf,
}

/// Formatter slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Slot {
    TabTitle,
    WindowTitle,
    Status,
}

impl Slot {
    fn hook(self) -> &'static str {
        match self {
            Slot::TabTitle => "tab_title",
            Slot::WindowTitle => "window_title",
            Slot::Status => "status",
        }
    }
}

/// Inputs each formatter remembers before its cache is cleared.
const CACHE: usize = 256;

/// The window halves of every plugin, for one window.
pub struct WindowPlugins {
    config: WindowConfig,
    plugins: Vec<WindowPlugin>,
    /// Failed window entries, `(id, error)`.
    failed: Vec<(String, String)>,
    /// The failures the last load toasted (identical ones aren't toasted again).
    toasted: Vec<(String, String)>,
    /// Plugins whose `window_start` ran in this window.
    started: BTreeSet<String>,
    caches: HashMap<(String, Slot), HashMap<String, Json>>,
    /// Formatter failures to toast after the frame: `(text, sub)`.
    pending_toasts: Vec<(String, String)>,
    chrome_dirty: bool,
    catalog: Catalog,
}

impl std::fmt::Debug for WindowPlugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowPlugins")
            .field(
                "plugins",
                &self.plugins.iter().map(|p| &p.id).collect::<Vec<_>>(),
            )
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------------------------
// The cx userdata

type HostCell<'a> = RefCell<&'a mut dyn WindowHost>;

struct Cx<'a> {
    host: &'a HostCell<'a>,
    session: AnyUserData,
    layout: AnyUserData,
}

struct SessionCx<'a> {
    host: &'a HostCell<'a>,
    vm: &'a Vm,
    catalog: &'a Catalog,
}

struct LayoutCx<'a> {
    host: &'a HostCell<'a>,
}

fn borrow<'b, 'a>(
    cell: &'b HostCell<'a>,
) -> mlua::Result<std::cell::RefMut<'b, &'a mut dyn WindowHost>> {
    cell.try_borrow_mut()
        .map_err(|_| mlua::Error::runtime("cx is already in use"))
}

fn err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

fn how_arg(v: &Value) -> mlua::Result<Option<How>> {
    match v {
        Value::Nil => Ok(None),
        Value::String(s) => {
            let s = s.to_str()?.to_owned();
            How::parse(&s)
                .map(Some)
                .ok_or_else(|| err(format!("unknown how `{s}`")))
        }
        other => Err(err(format!("unknown how `{}`", other.type_name()))),
    }
}

fn dir_arg(s: &str) -> mlua::Result<Dir> {
    Dir::parse(s).ok_or_else(|| err(format!("unknown direction `{s}` (right, down, left or up)")))
}

/// A `TabTree`'s split sides are `[1]` and `[2]`: JSON carries them as the keys `"1"` and `"2"`,
/// which become numbers here, through the whole tree.
fn number_sides(t: &mlua::Table) -> mlua::Result<()> {
    for i in 1..=2_i64 {
        let key = i.to_string();
        if let Value::Table(side) = t.raw_get::<Value>(key.as_str())? {
            number_sides(&side)?;
            t.raw_set(key.as_str(), Value::Nil)?;
            t.raw_set(i, side)?;
        }
    }
    Ok(())
}

fn reveal_arg(v: &Value) -> mlua::Result<Reveal> {
    match v {
        Value::Table(t) => Ok(Reveal {
            focus: t.get::<Option<bool>>("focus")?.unwrap_or(true),
        }),
        _ => Ok(Reveal { focus: true }),
    }
}

fn launch_arg(vm: &dyn ToJson, v: &Value) -> mlua::Result<Launch> {
    match v {
        Value::Nil => Ok(Launch::default()),
        other => {
            let json = vm.to_json(other).map_err(err)?;
            serde_json::from_value(json).map_err(|e| err(format!("bad launch: {e}")))
        }
    }
}

fn tab_spec(vm: &dyn ToJson, t: &mlua::Table) -> mlua::Result<TabSpec> {
    let side = |i: i64| -> mlua::Result<Option<Box<TabSpec>>> {
        match t.raw_get::<Value>(i)? {
            Value::Table(sub) => Ok(Some(Box::new(tab_spec(vm, &sub)?))),
            _ => Ok(None),
        }
    };
    let split = match t.get::<Option<String>>("split")? {
        Some(s) => Some(dir_arg(&s)?),
        None => None,
    };
    let launch = match t.get::<Value>("launch")? {
        Value::Nil => None,
        v => Some(launch_arg(vm, &v)?),
    };
    Ok(TabSpec {
        cwd: t.get("cwd")?,
        name: t.get("name")?,
        split,
        ratio: t.get("ratio")?,
        launch,
        first: side(1)?,
        second: side(2)?,
    })
}

fn pane_arg(host: &dyn WindowHost, v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) if *n >= 0.0 => Some(*n as u64),
        Value::Integer(n) if *n >= 0 => Some(*n as u64),
        Value::String(s) => s.to_str().ok().and_then(|s| host.resolve(&s)),
        _ => None,
    }
}

impl UserData for Cx<'_> {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("session", |_, this| Ok(this.session.clone()));
        fields.add_field_method_get("layout", |_, this| Ok(this.layout.clone()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("run", |_, this, (pane, text): (u64, String)| {
            borrow(this.host)?.run(pane, &text).map_err(err)
        });
        methods.add_method(
            "open",
            |_, this, (target, how, reveal): (String, Value, Value)| {
                let how = how_arg(&how)?;
                let reveal = reveal_arg(&reveal)?;
                borrow(this.host)?.open(&target, how, reveal).map_err(err)
            },
        );
        methods.add_method(
            "new_block",
            |_, this, (kind, args, how, reveal): (String, Option<Vec<String>>, Value, Value)| {
                let how = how_arg(&how)?.unwrap_or(How::Default);
                let reveal = reveal_arg(&reveal)?;
                let args = args.unwrap_or_default();
                Ok(borrow(this.host)?
                    .new_block(&kind, &args, how, reveal)
                    .map(|p| p as f64))
            },
        );
        methods.add_method("command", |_, this, id: String| {
            borrow(this.host)?.command(&id).map_err(err)
        });
        methods.add_method("action", |_, this, name: String| {
            borrow(this.host)?.action(&name).map_err(err)
        });
        methods.add_method(
            "toast",
            |_, this, (level, text, sub): (String, String, Option<String>)| {
                let level = match level.as_str() {
                    "success" => ToastLevel::Success,
                    "info" => ToastLevel::Info,
                    "error" => ToastLevel::Error,
                    _ => {
                        return Err(err(format!(
                            "unknown toast level `{level}` (success, info or error)"
                        )));
                    }
                };
                borrow(this.host)?.toast(level, &text, sub.as_deref());
                Ok(())
            },
        );
        methods.add_method("copy", |_, this, text: String| {
            borrow(this.host)?.copy(&text);
            Ok(())
        });
    }
}

impl UserData for SessionCx<'_> {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("panes", |_, this, ()| {
            let panes = borrow(this.host)?.panes();
            let json = serde_json::to_value(panes).map_err(mlua::Error::external)?;
            this.vm.to_lua(&json)
        });
        methods.add_method("tabs", |_, this, ()| {
            let tabs = borrow(this.host)?.tabs();
            let json = serde_json::to_value(tabs).map_err(mlua::Error::external)?;
            this.vm.to_lua(&json)
        });
        methods.add_method("focused", |_, this, ()| {
            Ok(borrow(this.host)?.focused().map(|p| p as f64))
        });
        methods.add_method("resolve", |_, this, spec: String| {
            Ok(borrow(this.host)?.resolve(&spec).map(|p| p as f64))
        });
        methods.add_method("pane", |_, this, spec: Value| {
            let host = borrow(this.host)?;
            let Some(id) = pane_arg(&**host, &spec) else {
                return Ok(Value::Nil);
            };
            match host.panes().into_iter().find(|p| p.pane == id) {
                Some(p) => this
                    .vm
                    .to_lua(&serde_json::to_value(p).map_err(mlua::Error::external)?),
                None => Ok(Value::Nil),
            }
        });
        methods.add_method("tab_of", |_, this, pane: u64| {
            let host = borrow(this.host)?;
            Ok(host
                .panes()
                .into_iter()
                .find(|p| p.pane == pane)
                .and_then(|p| p.tab)
                .map(|t| t as f64))
        });
        methods.add_method("sessions", |_, this, ()| {
            let json = borrow(this.host)?.sessions();
            this.vm.to_lua(&json)
        });
        methods.add_method("layout", |_, this, tab: u64| {
            match borrow(this.host)?.layout(tab) {
                Some(json) => {
                    let v = this.vm.to_lua(&json)?;
                    if let Value::Table(t) = &v
                        && let Ok(Value::Table(root)) = t.raw_get::<Value>("root")
                    {
                        number_sides(&root)?;
                    }
                    Ok(v)
                }
                None => Ok(Value::Nil),
            }
        });
        methods.add_method("read", |_, this, (pane, opts): (Value, Value)| {
            let host = borrow(this.host)?;
            let Some(id) = pane_arg(&**host, &pane) else {
                return Ok(Value::Nil);
            };
            let opts = this.vm.to_json(&opts).unwrap_or(Json::Null);
            match host.read(id, &opts) {
                Some(json) => this.vm.to_lua(&json),
                None => Ok(Value::Nil),
            }
        });
        methods.add_method("surface", |_, this, (pane, opts): (Value, Value)| {
            let host = borrow(this.host)?;
            let Some(id) = pane_arg(&**host, &pane) else {
                return Ok(Value::Nil);
            };
            let opts = this.vm.to_json(&opts).unwrap_or(Json::Null);
            match host.surface(id, &opts) {
                Some(json) => this.vm.to_lua(&json),
                None => Ok(Value::Nil),
            }
        });
        methods.add_method("event", |_, this, (pane, ev): (Value, Value)| {
            let mut host = borrow(this.host)?;
            let Some(id) = pane_arg(&**host, &pane) else {
                return Err(err("no such pane"));
            };
            let ev = this.vm.to_json(&ev).map_err(err)?;
            host.event(id, &ev).map_err(err)
        });
        methods.add_method("settle", |lua, this, (pane, ms): (u64, Option<f64>)| {
            let ms = ms.unwrap_or(30_000.0).clamp(0.0, 120_000.0);
            let (id, completer) = this.vm.api().tasks.borrow_mut().start(None);
            borrow(this.host)?.settle(pane, ms, completer);
            crate::api::awaitable(lua, this.vm.api(), "settle", id)
        });
        methods.add_method("block_types", |_, this, ()| {
            let types: Vec<Json> = this
                .catalog
                .block_types()
                .map(|(p, b)| {
                    json!({"kind": format!("{}.{}", p.id, b.id), "title": b.title, "icon": b.icon, "palette": b.palette})
                })
                .collect();
            this.vm.to_lua(&Json::Array(types))
        });
    }
}

impl UserData for LayoutCx<'_> {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("new_tab", |lua, this, (launch, reveal): (Value, Value)| {
            let vm = lua_vm(lua)?;
            let launch = launch_arg(&vm, &launch)?;
            let reveal = reveal_arg(&reveal)?;
            Ok(borrow(this.host)?
                .new_tab(&launch, reveal)
                .map(|p| p as f64))
        });
        methods.add_method(
            "split",
            |lua, this, (target, dir, launch, reveal): (u64, String, Value, Value)| {
                let vm = lua_vm(lua)?;
                let dir = dir_arg(&dir)?;
                let launch = launch_arg(&vm, &launch)?;
                let reveal = reveal_arg(&reveal)?;
                Ok(borrow(this.host)?
                    .split(target, dir, &launch, reveal)
                    .map(|p| p as f64))
            },
        );
        methods.add_method("focus", |_, this, pane: u64| {
            borrow(this.host)?.focus(pane);
            Ok(())
        });
        methods.add_method("close", |_, this, pane: u64| {
            borrow(this.host)?.close(pane);
            Ok(())
        });
        methods.add_method(
            "move",
            |_, this, (pane, target, dir, reveal): (u64, u64, String, Value)| {
                let dir = if dir == "swap" {
                    None
                } else {
                    Some(dir_arg(&dir)?)
                };
                let reveal = reveal_arg(&reveal)?;
                Ok(borrow(this.host)?.move_pane(pane, target, dir, reveal))
            },
        );
        methods.add_method(
            "resize",
            |_, this, (pane, dir, cells): (u64, String, f64)| {
                let dir = dir_arg(&dir)?;
                Ok(borrow(this.host)?.resize(pane, dir, cells))
            },
        );
        methods.add_method("tab", |lua, this, (spec, reveal): (mlua::Table, Value)| {
            let vm = lua_vm(lua)?;
            let spec = tab_spec(&vm, &spec)?;
            let reveal = reveal_arg(&reveal)?;
            let (tab, error) = borrow(this.host)?.tab(&spec, reveal);
            Ok((tab.map(|t| t as f64), error))
        });
    }
}

/// A converter bound to `lua`, for userdata that don't hold their VM.
struct LuaConv<'l>(&'l mlua::Lua);

impl LuaConv<'_> {
    fn to_json(&self, v: &Value) -> Result<Json, String> {
        crate::api::convert::to_json(self.0, v)
    }
}

fn lua_vm(lua: &mlua::Lua) -> mlua::Result<LuaConv<'_>> {
    Ok(LuaConv(lua))
}

// `launch_arg` and `tab_spec` take a converter; the VM's and a bare Lua's convert alike.
trait ToJson {
    fn to_json(&self, v: &Value) -> Result<Json, String>;
}

impl ToJson for Vm {
    fn to_json(&self, v: &Value) -> Result<Json, String> {
        Vm::to_json(self, v)
    }
}

impl ToJson for LuaConv<'_> {
    fn to_json(&self, v: &Value) -> Result<Json, String> {
        LuaConv::to_json(self, v)
    }
}

/// Calls `f(cx)` with a fresh window cx in `vm`, valid only during the call.
fn with_cx<R>(
    vm: &Vm,
    catalog: &Catalog,
    host: &mut dyn WindowHost,
    f: impl FnOnce(Value) -> R,
) -> mlua::Result<R> {
    let cell: HostCell<'_> = RefCell::new(host);
    vm.lua().scope(|scope| {
        let session = scope.create_userdata(SessionCx {
            host: &cell,
            vm,
            catalog,
        })?;
        let layout = scope.create_userdata(LayoutCx { host: &cell })?;
        let cx = scope.create_userdata(Cx {
            host: &cell,
            session,
            layout,
        })?;
        Ok(f(Value::UserData(cx)))
    })
}

// ---------------------------------------------------------------------------------------------

fn first_line(e: &CallError) -> String {
    e.first_line()
}

/// Reads a route's return value.
fn decision(v: Value, vm: &Vm) -> Result<Option<RouteDecision>, String> {
    let t = match v {
        Value::Nil => return Ok(None),
        Value::Table(t) => t,
        _ => return Err("a route returns a decision table or nil".into()),
    };
    let get = |k: &str| t.get::<Value>(k).unwrap_or(Value::Nil);
    if let Value::String(kind) = get("block") {
        let args = match get("args") {
            Value::Nil => Vec::new(),
            v => match vm.to_json(&v) {
                Ok(Json::Array(a)) => a
                    .into_iter()
                    .map(|x| match x {
                        Json::String(s) => s,
                        other => other.to_string(),
                    })
                    .collect(),
                Ok(Json::Object(m)) if m.is_empty() => Vec::new(),
                _ => return Err("a route decision's args are a list of strings".into()),
            },
        };
        return Ok(Some(RouteDecision::Block {
            kind: kind.to_string_lossy(),
            args,
        }));
    }
    if let Value::String(url) = get("url") {
        return Ok(Some(RouteDecision::Url(url.to_string_lossy())));
    }
    if let Value::String(path) = get("path") {
        let how = match get("how") {
            Value::Nil => None,
            Value::String(h) => {
                let h = h.to_string_lossy();
                Some(
                    How::parse(&h)
                        .ok_or_else(|| format!("a route decision has an unknown how `{h}`"))?,
                )
            }
            other => {
                return Err(format!(
                    "a route decision has an unknown how `{}`",
                    other.type_name()
                ));
            }
        };
        return Ok(Some(RouteDecision::Path {
            path: path.to_string_lossy(),
            how,
        }));
    }
    if let Value::Boolean(true) = get("handled") {
        return Ok(Some(RouteDecision::Handled));
    }
    Err("a route decision sets block, url, path or handled = true".into())
}

impl WindowPlugins {
    /// Loads the window halves of every allowed package in id order (manifest styles first).
    /// A failed entry toasts "Plugin <name> failed to load" through `host`.
    pub fn load(config: WindowConfig, host: &mut dyn WindowHost) -> Self {
        let mut this = WindowPlugins {
            config,
            plugins: Vec::new(),
            failed: Vec::new(),
            toasted: Vec::new(),
            started: BTreeSet::new(),
            caches: HashMap::new(),
            pending_toasts: Vec::new(),
            chrome_dirty: false,
            catalog: Catalog::default(),
        };
        this.load_all(host);
        this
    }

    /// The host catalog the window follows (for `cx.session:block_types`).
    pub fn set_catalog(&mut self, catalog: Catalog) {
        self.catalog = catalog;
    }

    fn load_all(&mut self, host: &mut dyn WindowHost) {
        let scan = discovery::scan(&self.config.plugins_dir);
        self.plugins.clear();
        self.failed.clear();
        self.caches.clear();
        for package in &scan.packages {
            let m: &Manifest = &package.manifest;
            if !self.config.settings.allows(&m.id) {
                continue;
            }
            let data = paths::data_dir(&self.config.state_dir, &m.id);
            let _ = std::fs::create_dir_all(&data);
            let Some(entry) = m.window.as_deref() else {
                if !package.css.is_empty() {
                    // Manifest styles install without a window entry.
                    let options = self.options(m, package.dir.clone(), data);
                    if let Ok(vm) = Vm::new(options) {
                        self.plugins.push(WindowPlugin {
                            id: m.id.clone(),
                            name: m.name.clone(),
                            vm,
                            css: package.css.clone(),
                            dir: package.dir.clone(),
                        });
                    }
                }
                continue;
            };
            let options = self.options(m, package.dir.clone(), data);
            let vm = match Vm::new(options) {
                Ok(vm) => vm,
                Err(e) => {
                    self.failed
                        .push((m.id.clone(), CallError::Lua(e).message()));
                    continue;
                }
            };
            vm.set_waker(self.config.waker.clone());
            match vm.run_entry(entry) {
                Ok(()) => {
                    vm.take_registrations_changed();
                    self.plugins.push(WindowPlugin {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        vm,
                        css: package.css.clone(),
                        dir: package.dir.clone(),
                    });
                }
                Err(error) => {
                    tracing::warn!(plugin = %m.id, %error, "plugin window entry failed to load");
                    self.failed.push((m.id.clone(), error));
                    // Manifest styles stay when the entry fails.
                    if !package.css.is_empty() {
                        let mut o = self.options(m, package.dir.clone(), PathBuf::new());
                        o.log = Arc::new(|_| {});
                        if let Ok(vm) = Vm::new(o) {
                            self.plugins.push(WindowPlugin {
                                id: m.id.clone(),
                                name: m.name.clone(),
                                vm,
                                css: package.css.clone(),
                                dir: package.dir.clone(),
                            });
                        }
                    }
                }
            }
        }
        if self.failed != self.toasted {
            for (id, error) in &self.failed {
                let name = scan
                    .packages
                    .iter()
                    .find(|p| p.manifest.id == *id)
                    .map_or(id.as_str(), |p| p.manifest.name.as_str());
                host.toast(
                    ToastLevel::Error,
                    &format!("Plugin {name} failed to load"),
                    Some(error.lines().next().unwrap_or_default()),
                );
            }
            self.toasted = self.failed.clone();
        }
    }

    fn options(&self, m: &Manifest, dir: PathBuf, data: PathBuf) -> VmOptions {
        let mut o = VmOptions::for_manifest(Context::Window, m, &dir, &data);
        // Blocks and lenses are the host half's; the window entry defines none.
        o.declared_blocks.clear();
        o.declared_lenses.clear();
        o.log = Arc::clone(&self.config.log);
        o.builtin_commands = Some(Arc::clone(&self.config.builtin_commands));
        o
    }

    /// Rebuilds every VM. `window_start` then runs only for plugins that
    /// haven't run it in this window.
    pub fn reload(&mut self, host: &mut dyn WindowHost) {
        self.load_all(host);
        self.window_start(host);
    }

    /// Loaded plugin ids, in id order.
    pub fn plugins(&self) -> Vec<&str> {
        self.plugins.iter().map(|p| p.id.as_str()).collect()
    }

    /// Window entries that failed, `(id, error)`; their window status is Failed.
    pub fn failures(&self) -> &[(String, String)] {
        &self.failed
    }

    fn plugin(&self, id: &str) -> Option<&WindowPlugin> {
        self.plugins.iter().find(|p| p.id == id)
    }

    /// Calls `f` as `hook` with a fresh cx appended; a failure toasts "Plugin <name>: <hook>
    /// failed" through `host` and returns `None`.
    fn call_cx(
        &self,
        index: usize,
        host: &mut dyn WindowHost,
        hook: &str,
        budget: Budget,
        f: &Function,
        args: Vec<Value>,
    ) -> Option<Value> {
        let plugin = &self.plugins[index];
        let result = with_cx(&plugin.vm, &self.catalog, host, |cx| {
            let mut args = args;
            args.push(cx);
            plugin
                .vm
                .call::<Value>(hook, budget, f, mlua::MultiValue::from_iter(args))
        });
        let outcome = match result {
            Ok(r) => r,
            Err(e) => Err(CallError::Lua(e)),
        };
        match outcome {
            Ok(v) => Some(v),
            Err(CallError::Disabled { .. }) => None,
            Err(e) => {
                tracing::warn!(plugin = %plugin.id, hook, error = %e.message(), "plugin handler failed");
                host.toast(
                    ToastLevel::Error,
                    &format!("Plugin {}: {hook} failed", plugin.name),
                    Some(&first_line(&e)),
                );
                None
            }
        }
    }

    /// Runs `window_start` once per plugin per window, after the window's session settled.
    pub fn window_start(&mut self, host: &mut dyn WindowHost) {
        for i in 0..self.plugins.len() {
            if !self.started.insert(self.plugins[i].id.clone()) {
                continue;
            }
            for f in self.plugins[i].vm.handlers("window_start") {
                self.call_cx(i, host, "window_start", Budget::Window, &f, Vec::new());
            }
        }
        self.after_calls();
    }

    /// Delivers queued window events (after each render): each event in order, to every plugin
    /// in id order, to each handler in registration order, each with a fresh cx.
    pub fn deliver(&mut self, host: &mut dyn WindowHost, events: &[WindowEvent]) {
        for event in events {
            let name = event.name();
            let payload = event.payload();
            for i in 0..self.plugins.len() {
                let handlers = self.plugins[i].vm.handlers(name);
                if handlers.is_empty() {
                    continue;
                }
                let Ok(table) = self.plugins[i].vm.to_lua(&payload) else {
                    continue;
                };
                for f in handlers {
                    if self.plugins[i].vm.is_disabled(name) {
                        break;
                    }
                    self.call_cx(i, host, name, Budget::Window, &f, vec![table.clone()]);
                }
            }
        }
        self.after_calls();
    }

    /// Runs callbacks that are due (timers, processes, requests, awaitables), each with a fresh
    /// cx. Call it after rendering, and when the waker fires or [`next_wake`](Self::next_wake)
    /// passes.
    pub fn pump(&mut self, host: &mut dyn WindowHost) {
        let now = Instant::now();
        for i in 0..self.plugins.len() {
            while let Some(cb) = self.plugins[i].vm.next_callback(now) {
                self.call_cx(i, host, cb.hook, Budget::Window, &cb.f, cb.args);
            }
        }
        self.after_calls();
    }

    /// When the next callback is due: now when a completion (a process, a request, a `settle`)
    /// is waiting for its callback, else the next timer.
    pub fn next_wake(&self) -> Option<Instant> {
        if self
            .plugins
            .iter()
            .any(|p| !p.vm.completion_receiver().is_empty())
        {
            return Some(Instant::now());
        }
        self.plugins.iter().filter_map(|p| p.vm.next_timer()).min()
    }

    /// Clears formatter caches when registrations changed or `tern.chrome.refresh` ran.
    fn after_calls(&mut self) {
        for p in &self.plugins {
            let changed = p.vm.take_registrations_changed();
            let refresh = p.vm.take_chrome_refresh();
            if changed || refresh {
                self.caches.clear();
                self.chrome_dirty = true;
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Palette, actions and the keymap

    /// The palette rows plugins added, in plugin id order then registration order.
    pub fn commands(&self) -> Vec<PaletteCommand> {
        let mut out = Vec::new();
        for p in &self.plugins {
            for c in &p.vm.window_registry().commands {
                out.push(PaletteCommand {
                    action: format!("plugin.{}.{}", p.id, c.id),
                    plugin: p.id.clone(),
                    id: c.id.clone(),
                    title: c.title.clone(),
                    icon: c.icon.clone(),
                    group: c.group.clone().unwrap_or_else(|| p.name.clone()),
                });
            }
        }
        out
    }

    /// Whether the palette shows command `action` now: its `available(cx)` (4 ms) returns
    /// anything but nil or false. An error or overrun hides the row and toasts.
    pub fn available(&mut self, host: &mut dyn WindowHost, action: &str) -> bool {
        let Some((plugin, id)) = crate::chord::plugin_action(action) else {
            return false;
        };
        let Some(i) = self.plugins.iter().position(|p| p.id == plugin) else {
            return false;
        };
        let available = self.plugins[i]
            .vm
            .window_registry()
            .commands
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.available.clone());
        let r = match available {
            None => false,
            Some(None) => true,
            Some(Some(f)) => {
                let hook = format!("available {action}");
                let v = self.call_cx(i, host, &hook, Budget::Formatter, &f, Vec::new());
                matches!(v, Some(v) if !matches!(v, Value::Nil | Value::Boolean(false)))
            }
        };
        self.after_calls();
        r
    }

    /// The plugin keymap layer: every plugin's binds and command `keys`, in plugin id order then
    /// registration order. The app leaves out chords that don't parse or that the defaults or
    /// preset bind ([`crate::chord::Chord::overlaps`]).
    pub fn binds(&self) -> Vec<PluginBind> {
        let mut out = Vec::new();
        for p in &self.plugins {
            for b in &p.vm.window_registry().binds {
                let action = match &b.target {
                    BindTarget::Action(a) => a.clone(),
                    BindTarget::Function(_, n) => format!("plugin.{}.bind.{n}", p.id),
                };
                out.push(PluginBind {
                    plugin: p.id.clone(),
                    chord: b.chord.clone(),
                    action,
                });
            }
        }
        out
    }

    /// Runs a plugin action (`plugin.<p>.<command>` or `plugin.<p>.bind.<n>`). `Ok(false)` when no
    /// plugin has it ("No plugin has this action").
    pub fn run_action(&mut self, host: &mut dyn WindowHost, action: &str) -> Result<bool, String> {
        let Some((plugin, rest)) = crate::chord::plugin_action(action) else {
            return Ok(false);
        };
        let Some(i) = self.plugins.iter().position(|p| p.id == plugin) else {
            return Ok(false);
        };
        let target = {
            let reg = self.plugins[i].vm.window_registry();
            if let Some(n) = rest
                .strip_prefix("bind.")
                .and_then(|n| n.parse::<usize>().ok())
            {
                reg.binds.iter().find_map(|b| match &b.target {
                    BindTarget::Function(f, k) if *k == n => Some((format!("bind {n}"), f.clone())),
                    _ => None,
                })
            } else {
                reg.commands
                    .iter()
                    .find(|c| c.id == rest)
                    .map(|c| (format!("command {}", c.id), c.run.clone()))
            }
        };
        let Some((hook, f)) = target else {
            return Ok(false);
        };
        self.call_cx(i, host, &hook, Budget::Window, &f, Vec::new());
        self.after_calls();
        Ok(true)
    }

    /// Runs the overrides of built-in command `id`, however it was started: every plugin's, in
    /// plugin id order then registration order, until one returns truthy. True skips the
    /// built-in. A raising or over-budget override counts as false (and toasts).
    pub fn run_override(&mut self, host: &mut dyn WindowHost, id: &str) -> bool {
        let hook = format!("override {id}");
        for i in 0..self.plugins.len() {
            let overrides: Vec<Function> = self.plugins[i]
                .vm
                .window_registry()
                .overrides
                .iter()
                .filter(|(c, _)| c == id)
                .map(|(_, f)| f.clone())
                .collect();
            for f in overrides {
                let v = self.call_cx(i, host, &hook, Budget::Window, &f, Vec::new());
                if matches!(v, Some(v) if !matches!(v, Value::Nil | Value::Boolean(false))) {
                    self.after_calls();
                    return true;
                }
            }
        }
        self.after_calls();
        false
    }

    // -----------------------------------------------------------------------------------------
    // Routes

    fn route(&mut self, host: &mut dyn WindowHost, link: bool, req: Json) -> Option<RouteDecision> {
        let hook = if link { "route.link" } else { "route.open" };
        for i in 0..self.plugins.len() {
            let handlers: Vec<Function> = {
                let reg = self.plugins[i].vm.window_registry();
                if link {
                    reg.route_link.clone()
                } else {
                    reg.route_open.clone()
                }
            };
            for f in handlers {
                let Ok(table) = self.plugins[i].vm.to_lua(&req) else {
                    continue;
                };
                let Some(v) = self.call_cx(i, host, hook, Budget::Window, &f, vec![table]) else {
                    continue;
                };
                match decision(v, &self.plugins[i].vm) {
                    Ok(Some(d)) => {
                        self.after_calls();
                        return Some(d);
                    }
                    Ok(None) => {}
                    Err(e) => host.toast(
                        ToastLevel::Error,
                        &format!("Plugin {}: {hook} failed", self.plugins[i].name),
                        Some(&e),
                    ),
                }
            }
        }
        self.after_calls();
        None
    }

    /// `route.open`: may claim a file open (`auk open PATH`, a clicked file path). `None` runs
    /// the built-in open.
    pub fn route_open(
        &mut self,
        host: &mut dyn WindowHost,
        req: &OpenRequest,
    ) -> Option<RouteDecision> {
        let json = serde_json::to_value(req).unwrap_or(Json::Null);
        self.route(host, false, json)
    }

    /// `route.link`: may claim a link click. `None` runs the built-in handling.
    pub fn route_link(
        &mut self,
        host: &mut dyn WindowHost,
        req: &LinkRequest,
    ) -> Option<RouteDecision> {
        let json = serde_json::to_value(req).unwrap_or(Json::Null);
        self.route(host, true, json)
    }

    // -----------------------------------------------------------------------------------------
    // Chrome formatters

    fn format(&mut self, slot: Slot, input: Json) -> Vec<(String, Json)> {
        let key = input.to_string();
        let mut out = Vec::new();
        for i in 0..self.plugins.len() {
            let f = {
                let reg = self.plugins[i].vm.window_registry();
                match slot {
                    Slot::TabTitle => reg.tab_title.clone(),
                    Slot::WindowTitle => reg.window_title.clone(),
                    Slot::Status => reg.status.clone(),
                }
            };
            let Some(f) = f else { continue };
            let id = self.plugins[i].id.clone();
            let cache = self.caches.entry((id.clone(), slot)).or_default();
            if let Some(hit) = cache.get(&key) {
                out.push((id, hit.clone()));
                continue;
            }
            let plugin = &self.plugins[i];
            let result = plugin
                .vm
                .to_lua(&input)
                .map_err(CallError::Lua)
                .and_then(|table| {
                    plugin.vm.set_in_formatter(true);
                    let r = plugin
                        .vm
                        .call::<Value>(slot.hook(), Budget::Formatter, &f, table);
                    plugin.vm.set_in_formatter(false);
                    r
                });
            let value = match result {
                Ok(v) => plugin.vm.to_json(&v).map_err(|e| e.to_string()),
                Err(CallError::Disabled { .. }) => Ok(Json::Null),
                Err(e) => Err(e.first_line()),
            };
            let value = match value {
                Ok(v) => v,
                Err(e) => {
                    self.pending_toasts
                        .push((format!("Plugin {}: {} failed", plugin.name, slot.hook()), e));
                    Json::Null
                }
            };
            let cache = self.caches.entry((id.clone(), slot)).or_default();
            if cache.len() >= CACHE {
                cache.clear();
            }
            cache.insert(key.clone(), value.clone());
            out.push((id, value));
        }
        out
    }

    fn title(&mut self, slot: Slot, input: Json) -> Option<String> {
        for (id, v) in self.format(slot, input) {
            match v {
                Json::String(s) if !s.is_empty() => return Some(s),
                Json::Null | Json::String(_) => {}
                _ => {
                    let name = self.plugin(&id).map_or(id.clone(), |p| p.name.clone());
                    self.pending_toasts.push((
                        format!("Plugin {name}: {} failed", slot.hook()),
                        "a title formatter returns a string or nil".into(),
                    ));
                }
            }
        }
        None
    }

    /// A tab's title from `tern.chrome.tab_title`: the first plugin, in id order, with a
    /// non-empty string wins; `None` keeps the default.
    pub fn tab_title(&mut self, tab: &TabInfo) -> Option<String> {
        self.title(
            Slot::TabTitle,
            serde_json::to_value(tab).unwrap_or(Json::Null),
        )
    }

    /// The window's title from `tern.chrome.window_title`.
    pub fn window_title(&mut self, win: &Json) -> Option<String> {
        self.title(Slot::WindowTitle, win.clone())
    }

    /// Status segments for the focused pane: every plugin's, in id order.
    pub fn status(&mut self, pane: &StatusPane) -> Vec<StatusSegment> {
        let mut out = Vec::new();
        for (id, v) in self.format(
            Slot::Status,
            serde_json::to_value(pane).unwrap_or(Json::Null),
        ) {
            let items = match v {
                Json::Null => continue,
                Json::Array(items) => items,
                Json::Object(m) if m.is_empty() => continue,
                _ => {
                    self.status_failed(&id);
                    continue;
                }
            };
            let mut segs = Vec::new();
            let mut bad = false;
            for item in items {
                let Json::Object(m) = item else {
                    bad = true;
                    break;
                };
                let Some(text) = m.get("text").and_then(Json::as_str) else {
                    bad = true;
                    break;
                };
                let s = |k: &str| m.get(k).and_then(Json::as_str).map(str::to_owned);
                segs.push(StatusSegment {
                    plugin: id.clone(),
                    text: text.to_owned(),
                    icon: s("icon"),
                    tone: s("tone"),
                    command: s("command"),
                });
            }
            if bad {
                self.status_failed(&id);
            } else {
                out.extend(segs);
            }
        }
        out
    }

    fn status_failed(&mut self, id: &str) {
        let name = self.plugin(id).map_or(id.to_owned(), |p| p.name.clone());
        self.pending_toasts.push((
            format!("Plugin {name}: status failed"),
            "a status formatter returns a list of segments or nil".into(),
        ));
    }

    /// Formatter failures since the last call, `(text, sub)`, to toast after the frame.
    pub fn take_toasts(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending_toasts)
    }

    /// Whether the chrome should redraw (a refresh or a registration since the last call).
    pub fn take_chrome_dirty(&mut self) -> bool {
        std::mem::take(&mut self.chrome_dirty)
    }

    // -----------------------------------------------------------------------------------------
    // CSS

    /// Sheets to install, in order: each plugin's manifest styles (`plugin:local:<id>:styles`),
    /// then its `tern.css` sheets (`plugin:local:<id>:<name>`).
    pub fn sheets(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        // A plugin file's `url()` becomes a `data:` URL here, at install.
        let inline = |css: &str, p: &WindowPlugin| {
            crate::css_url::inline_urls_logged(css, Some(&p.dir), &p.id)
        };
        for p in &self.plugins {
            if !p.css.is_empty() {
                out.push((format!("plugin:local:{}:styles", p.id), inline(&p.css, p)));
            }
            for (name, css) in &p.vm.window_registry().css {
                out.push((format!("plugin:local:{}:{name}", p.id), inline(css, p)));
            }
        }
        out
    }

    /// The VM of plugin `id` (for diagnostics and tests).
    pub fn vm(&self, id: &str) -> Option<&Vm> {
        self.plugin(id).map(|p| &p.vm)
    }
}
