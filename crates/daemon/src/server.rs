//! The daemon (§14.2): the pane table, sessions and layout, attached clients, and the loops that
//! serve them.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use auk_mirror::layout::{Layout, SessionId, SplitDir};
use auk_mirror::{ClientFacts, ClientKind, DaemonMsg, LayoutEdit, Message, PaneId, Versions};
use auk_replica::{Replica, Role, Settings};
use bytes::Bytes;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{self, ClientId, ClientTx};
use crate::login::LoginEnv;
use crate::pane::{Pane, PaneIo, PaneKind, default_facts};
use crate::{control, pane_socket, paths, pty, zsh};

/// The window key panes go to when none is named.
pub const DEFAULT_WINDOW_KEY: &str = "main";

/// The size of a pane no client has sized yet.
pub const DEFAULT_SIZE: (u16, u16) = (80, 24);

/// How many exited panes stay in the table for `capture` and `wait`.
const KEEP_EXITED: usize = 32;

/// How long a pane's output may keep draining after its process exits.
const EXIT_DRAIN: Duration = Duration::from_secs(1);

/// Where the daemon listens and keeps its files.
#[derive(Clone, Debug)]
pub struct Config {
    pub socket: PathBuf,
    pub state_dir: PathBuf,
}

impl Config {
    /// The defaults of [`paths`], from the environment.
    pub fn from_env() -> Self {
        Config {
            socket: paths::daemon_socket(),
            state_dir: paths::state_dir(),
        }
    }
}

/// Where a new pane goes in the layout.
#[derive(Clone, Debug)]
pub enum Place {
    NewSession { name: Option<String> },
    NewTab { session: SessionId },
    Split { pane: PaneId, dir: SplitDir },
}

/// A request for a new pane.
#[derive(Clone, Debug)]
pub struct NewPane {
    pub window_key: Option<String>,
    pub place: Place,
    pub cwd: Option<PathBuf>,
    /// A command line for a command pane; a login shell otherwise.
    pub command: Option<String>,
    pub size: Option<(u16, u16)>,
}

#[derive(Debug)]
pub(crate) struct ClientEntry {
    pub kind: ClientKind,
    pub window_key: Option<String>,
    pub tx: ClientTx,
    pub facts: ClientFacts,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pub panes: BTreeMap<PaneId, Arc<Mutex<Pane>>>,
    pub layout: Layout,
    pub clients: BTreeMap<ClientId, ClientEntry>,
    next_pane: PaneId,
    next_client: ClientId,
    /// The pane most recently focused in any client, for the `@focused` target.
    pub focused: Option<PaneId>,
    exited: VecDeque<PaneId>,
}

/// The running daemon. Lock order: the state, then a pane.
#[derive(Debug)]
pub struct Daemon {
    pub(crate) socket: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) login: LoginEnv,
    zdotdir: Option<PathBuf>,
    settings: Settings,
    pub(crate) versions: Versions,
    state: Mutex<State>,
    pub(crate) shutdown: CancellationToken,
}

/// Locks a mutex, recovering from a panic in another holder: daemon state stays usable.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Runs a daemon until it is told to stop (`shutdown`, SIGTERM or SIGINT).
pub fn run(config: Config) -> io::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(config))
}

/// The daemon's main loop.
pub async fn serve(config: Config) -> io::Result<()> {
    private_dir(&config.state_dir)?;
    let socket_dir = config.socket.parent().unwrap_or(Path::new("."));
    private_dir(socket_dir)?;
    let _lock = acquire_lock(&config.socket)?;

    let listener = listen(&config.socket)?;
    let control_path = paths::control_socket(&config.socket);
    let control_listener = listen(&control_path)?;
    private_dir(&paths::pane_socket_dir(&config.socket))?;

    let login = LoginEnv::probe().await;
    let zdotdir = if zsh::is_zsh(&login.shell) {
        Some(zsh::install(&config.state_dir.join("shell/zsh"))?)
    } else {
        None
    };
    let daemon = Arc::new(Daemon {
        socket: config.socket.clone(),
        state_dir: config.state_dir.clone(),
        login,
        zdotdir,
        settings: Settings::default(),
        versions: Versions::current(),
        state: Mutex::new(State::default()),
        shutdown: CancellationToken::new(),
    });
    eprintln!(
        "auk daemon {} (pid {}) on {}",
        daemon.versions.auk,
        std::process::id(),
        config.socket.display()
    );

    let accept = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(client::serve(daemon.clone(), stream));
            }
        })
    };
    let control = {
        let daemon = daemon.clone();
        tokio::spawn(control::serve_async(control_listener, move |line| {
            let daemon = daemon.clone();
            async move { daemon.command(&line).await }
        }))
    };

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = daemon.shutdown.cancelled() => {}
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    daemon.shutdown.cancel();
    accept.abort();
    control.abort();
    let _ = fs::remove_file(&config.socket);
    let _ = fs::remove_file(&control_path);
    daemon.hang_up_all();
    eprintln!("auk daemon: stopped");
    Ok(())
}

/// Holds the single-daemon lock for a socket while alive.
fn acquire_lock(socket: &Path) -> io::Result<fs::File> {
    let path = paths::lock_file(socket);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
        |_| {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("another daemon serves {}", socket.display()),
            )
        },
    )?;
    Ok(file)
}

fn listen(path: &Path) -> io::Result<UnixListener> {
    let listener = control::bind(path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    UnixListener::from_std(listener)
}

impl Daemon {
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub(crate) fn pane(&self, id: PaneId) -> Option<Arc<Mutex<Pane>>> {
        self.state().panes.get(&id).cloned()
    }

    /// Asks the daemon to stop.
    pub fn stop(&self) {
        self.shutdown.cancel();
    }

    // Panes.

    /// Spawns a pane and places it in the layout.
    pub fn new_pane(self: &Arc<Self>, req: NewPane) -> Result<PaneId, String> {
        let mut st = self.state();
        let window_key = match &req.place {
            Place::Split { pane, .. } => st
                .layout
                .window_of(*pane)
                .ok_or_else(|| format!("pane {pane} is not in a window"))?
                .to_owned(),
            Place::NewTab { session } => st
                .layout
                .windows
                .iter()
                .find(|(_, w)| w.sessions.iter().any(|s| s.id == *session))
                .map(|(k, _)| k.clone())
                .ok_or_else(|| format!("no session {session}"))?,
            Place::NewSession { .. } => req
                .window_key
                .clone()
                .unwrap_or_else(|| DEFAULT_WINDOW_KEY.to_owned()),
        };
        let cwd = match &req.cwd {
            Some(dir) if dir.is_dir() => dir.clone(),
            Some(dir) => return Err(format!("{} is not a directory", dir.display())),
            None => self.login.home.clone(),
        };
        let (cols, rows) = req.size.unwrap_or(DEFAULT_SIZE);
        let id = st.next_pane + 1;
        let kind = match &req.command {
            Some(line) => PaneKind::Command { line: line.clone() },
            None => PaneKind::Shell,
        };
        let pane_socket = paths::pane_socket_dir(&self.socket).join(format!("{id}.sock"));
        let spec = self.spawn_spec(id, &kind, &window_key, &cwd, &pane_socket, cols, rows);
        let (master, child) = pty::spawn(&spec).map_err(|e| format!("spawn: {e}"))?;
        let master = Arc::new(master);
        st.next_pane = id;

        let mut replica = Replica::new(cols, rows, self.settings.clone(), Role::Authoritative);
        replica.set_facts(default_facts());
        let mut pane = Pane::new(id, kind, cwd, window_key.clone(), replica);
        pane.pid = child.id();
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        pane.io = Some(PaneIo {
            master: master.clone(),
            input: input_tx,
            pane_socket: pane_socket::spawn(&pane_socket)
                .map_err(|e| eprintln!("pane {id}: pane socket: {e}"))
                .ok(),
        });
        let pane = Arc::new(Mutex::new(pane));

        match req.place {
            Place::NewSession { name } => {
                st.layout.new_session(&window_key, name.as_deref(), id);
            }
            Place::NewTab { session } => {
                st.layout.new_tab(session, id).map_err(|e| e.to_string())?;
            }
            Place::Split { pane: at, dir } => {
                st.layout.split(at, dir, id).map_err(|e| e.to_string())?;
            }
        }
        st.panes.insert(id, pane.clone());

        // PTY I/O runs on threads: kqueue doesn't report a macOS PTY master as readable.
        spawn_thread(format!("pane {id} writer"), {
            let master = master.clone();
            move || write_pty(&master, input_rx)
        })?;
        let (drained_tx, drained) = tokio::sync::oneshot::channel::<()>();
        spawn_thread(format!("pane {id} reader"), {
            let pane = pane.clone();
            move || {
                read_pty(&pane, &master);
                let _ = drained_tx.send(());
            }
        })?;
        let daemon = self.clone();
        tokio::spawn(async move {
            let mut child = child;
            let status = match child.wait().await {
                Ok(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
                Err(_) => -1,
            };
            // Let the output drain. If something else still holds the tty open, the pane is
            // done anyway; its output keeps reaching the replica until the tty closes.
            let _ = tokio::time::timeout(EXIT_DRAIN, drained).await;
            daemon.pane_exited(id, status);
        });

        // Clients showing this window key see the new pane.
        let viewers: Vec<_> = st
            .clients
            .iter()
            .filter(|(_, c)| c.window_key.as_deref() == Some(window_key.as_str()))
            .map(|(id, c)| (*id, c.tx.clone(), c.facts.clone()))
            .collect();
        broadcast_layout(&st, &window_key);
        {
            let mut p = lock(&pane);
            for (client, tx, facts) in viewers {
                p.add_viewer(client, tx, facts);
            }
        }
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_spec(
        &self,
        id: PaneId,
        kind: &PaneKind,
        window_key: &str,
        cwd: &Path,
        pane_socket: &Path,
        cols: u16,
        rows: u16,
    ) -> pty::SpawnSpec {
        let mut env = self.login.vars.clone();
        let mut set = |k: &str, v: &str| {
            env.insert(k.to_owned(), v.to_owned());
        };
        set("TERM", "xterm-256color");
        set("COLORTERM", "truecolor");
        set("TERM_PROGRAM", "auk");
        set("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        set("PWD", &cwd.to_string_lossy());
        // The pane-identity set (§5.2, Appendix B).
        set("AUK_IDENTITY", &self.login.identity);
        set("AUK_PANE", &id.to_string());
        set("AUK_PANE_SOCKET", &pane_socket.to_string_lossy());
        set("AUK_WINDOW_KEY", window_key);
        set("AUK_WINDOW_SOCKET", "");
        // Lens claims (M8) and native completion (out of v1) aren't offered yet.
        set("AUK_LENSES", "0");
        set("AUK_COMPLETE", "0");
        set(
            "AUK_BLOB_DIR",
            &self.state_dir.join("blobs").to_string_lossy(),
        );
        // `auk` run inside the pane talks to this daemon.
        set(paths::DAEMON_SOCKET_ENV, &self.socket.to_string_lossy());
        if let Some(dir) = &self.zdotdir {
            set("ZDOTDIR", &dir.to_string_lossy());
            match &self.login.user_zdotdir {
                Some(user) => set(zsh::USER_ZDOTDIR_ENV, &user.to_string_lossy()),
                None => {
                    env.remove(zsh::USER_ZDOTDIR_ENV);
                }
            }
        }
        let mut args = vec!["-l".into()];
        if let PaneKind::Command { line } = kind {
            args.push("-c".into());
            args.push(line.into());
        }
        pty::SpawnSpec {
            program: self.login.shell.clone().into(),
            args,
            cwd: cwd.to_owned(),
            env,
            cols,
            rows,
        }
    }

    fn pane_exited(&self, id: PaneId, status: i32) {
        let mut st = self.state();
        let Some(pane) = st.panes.get(&id).cloned() else {
            return;
        };
        {
            let mut p = lock(&pane);
            p.on_exit(status);
            p.clear_viewers();
        }
        let window = st.layout.window_of(id).map(str::to_owned);
        if st.layout.remove_pane(id)
            && let Some(key) = window
        {
            broadcast_layout(&st, &key);
        }
        st.exited.push_back(id);
        while st.exited.len() > KEEP_EXITED {
            if let Some(old) = st.exited.pop_front() {
                st.panes.remove(&old);
            }
        }
    }

    /// Hangs up a pane's process group and closes the pane.
    pub fn kill(&self, id: PaneId) -> Result<(), String> {
        let mut st = self.state();
        let pane = st
            .panes
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("no pane {id}"))?;
        let p = lock(&pane);
        match (p.pid, p.exit) {
            (Some(pid), None) => hang_up(pid),
            _ => {
                drop(p);
                st.panes.remove(&id);
                st.exited.retain(|e| *e != id);
            }
        }
        Ok(())
    }

    fn hang_up_all(&self) {
        let st = self.state();
        for pane in st.panes.values() {
            let p = lock(pane);
            if let (Some(pid), None) = (p.pid, p.exit) {
                hang_up(pid);
            }
        }
    }

    // Clients.

    pub(crate) fn add_client(
        &self,
        kind: ClientKind,
        facts: ClientFacts,
        tx: ClientTx,
    ) -> ClientId {
        let mut st = self.state();
        st.next_client += 1;
        let id = st.next_client;
        tx.send(Bytes::from(
            DaemonMsg::Welcome {
                versions: self.versions.clone(),
                catalog: serde_json::Value::Null,
                sessions: st.layout.clone(),
                client: id,
            }
            .to_bytes(),
        ));
        st.clients.insert(
            id,
            ClientEntry {
                kind,
                window_key: None,
                tx,
                facts,
            },
        );
        id
    }

    pub(crate) fn remove_client(&self, id: ClientId) {
        let mut st = self.state();
        st.clients.remove(&id);
        for pane in st.panes.values() {
            lock(pane).remove_viewer(id);
        }
    }

    /// Shows a window key's sessions to a client: its layout, then a snapshot per pane, then
    /// live output (§15.3).
    pub(crate) fn attach(&self, id: ClientId, window_key: String) {
        let mut st = self.state();
        for pane in st.panes.values() {
            lock(pane).remove_viewer(id);
        }
        let Some(client) = st.clients.get_mut(&id) else {
            return;
        };
        client.window_key = Some(window_key.clone());
        let (tx, facts) = (client.tx.clone(), client.facts.clone());
        let window = st.layout.window(&window_key).cloned().unwrap_or_default();
        tx.send(Bytes::from(
            DaemonMsg::Layout {
                window_key,
                window: window.clone(),
            }
            .to_bytes(),
        ));
        for pane in window.panes() {
            if let Some(p) = st.panes.get(&pane) {
                lock(p).add_viewer(id, tx.clone(), facts.clone());
            }
        }
    }

    pub(crate) fn client_input(&self, id: ClientId, pane: PaneId, bytes: Vec<u8>) {
        if let Some(p) = self.pane(pane) {
            let mut p = lock(&p);
            p.touch(id);
            p.write_input(bytes);
        }
    }

    pub(crate) fn client_focus(&self, id: ClientId, pane: PaneId) {
        let Some(p) = self.pane(pane) else { return };
        if lock(&p).has_viewer(id) {
            self.state().focused = Some(pane);
            lock(&p).touch(id);
        }
    }

    pub(crate) fn client_resize(&self, id: ClientId, pane: PaneId, cols: u16, rows: u16) {
        if let Some(p) = self.pane(pane) {
            lock(&p).set_client_size(id, cols, rows);
        }
    }

    pub(crate) fn client_facts(&self, id: ClientId, facts: ClientFacts) {
        let mut st = self.state();
        if let Some(c) = st.clients.get_mut(&id) {
            c.facts = facts.clone();
        }
        for pane in st.panes.values() {
            lock(pane).set_client_facts(id, facts.clone());
        }
    }

    pub(crate) fn layout_edit(
        self: &Arc<Self>,
        id: ClientId,
        edit: LayoutEdit,
    ) -> Result<(), String> {
        let window_key = self
            .state()
            .clients
            .get(&id)
            .and_then(|c| c.window_key.clone());
        let new = |place| NewPane {
            window_key: window_key.clone(),
            place,
            cwd: None,
            command: None,
            size: None,
        };
        match edit {
            LayoutEdit::NewSession { name } => {
                self.new_pane(new(Place::NewSession { name }))?;
            }
            LayoutEdit::NewTab { session } => {
                self.new_pane(new(Place::NewTab { session }))?;
            }
            LayoutEdit::Split { pane, dir } => {
                let cwd = self.pane(pane).map(|p| lock(&p).cwd());
                self.new_pane(NewPane {
                    cwd,
                    ..new(Place::Split { pane, dir })
                })?;
            }
            LayoutEdit::Close { pane } => self.kill(pane)?,
            LayoutEdit::Zoom { tab, pane } => self.edit_layout(|l| l.zoom(tab, pane))?,
            LayoutEdit::RenameSession { session, name } => {
                self.edit_layout(|l| l.rename_session(session, &name))?
            }
            LayoutEdit::RenameTab { tab, title } => {
                self.edit_layout(|l| l.rename_tab(tab, title.as_deref()))?
            }
        }
        Ok(())
    }

    fn edit_layout<E: ToString>(
        &self,
        edit: impl FnOnce(&mut Layout) -> Result<(), E>,
    ) -> Result<(), String> {
        let mut st = self.state();
        edit(&mut st.layout).map_err(|e| e.to_string())?;
        let keys: Vec<String> = st.layout.windows.keys().cloned().collect();
        for key in keys {
            broadcast_layout(&st, &key);
        }
        Ok(())
    }
}

/// Sends a window key's layout to every client showing it.
fn broadcast_layout(st: &State, key: &str) {
    let window = st.layout.window(key).cloned().unwrap_or_default();
    let frame = Bytes::from(
        DaemonMsg::Layout {
            window_key: key.to_owned(),
            window,
        }
        .to_bytes(),
    );
    for client in st
        .clients
        .values()
        .filter(|c| c.window_key.as_deref() == Some(key))
    {
        client.tx.send(frame.clone());
    }
}

fn hang_up(pid: u32) {
    if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::HUP);
    }
}

fn spawn_thread(name: String, f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name)
        .spawn(f)
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Feeds PTY output to the pane until every slave descriptor is closed.
fn read_pty(pane: &Mutex<Pane>, master: &pty::Master) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match rustix::io::read(master, &mut buf) {
            Ok(0) => return,
            Ok(n) => lock(pane).on_output(&buf[..n]),
            Err(rustix::io::Errno::INTR) => {}
            // EIO: every slave descriptor closed.
            Err(_) => return,
        }
    }
}

/// Writes queued input to the PTY until the queue closes (the pane exited).
fn write_pty(master: &pty::Master, mut input: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(bytes) = input.blocking_recv() {
        let mut rest = &bytes[..];
        while !rest.is_empty() {
            match rustix::io::write(master, rest) {
                Ok(n) => rest = &rest[n..],
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return,
            }
        }
    }
}
