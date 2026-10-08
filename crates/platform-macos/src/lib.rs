//! The AppKit layer (spec §19), written with objc2: the application and its main menu, windows
//! whose content view is backed by a `CAMetalLayer` for wgpu, keyboard input through
//! `NSTextInputClient` (IME composition, dead keys, Option-as-Alt), mouse, scroll, resize, focus
//! and appearance events, frame pacing on the display's refresh, and the clipboard.
//!
//! Everything above this crate is portable: a window reports [`Event`]s to its
//! [`WindowHandler`], and the handler drives the window through [`Window`].
//!
//! ```no_run
//! use auk_platform_macos::{run, Event, Window, WindowAttributes, WindowHandler};
//!
//! struct Hello;
//! impl WindowHandler for Hello {
//!     fn event(&mut self, window: &Window, event: Event) {
//!         if let Event::CloseRequested = event {
//!             window.close();
//!         }
//!     }
//! }
//!
//! run(|app| {
//!     app.create_window(WindowAttributes::default(), Box::new(Hello));
//! });
//! ```

pub mod keys;

#[cfg(target_os = "macos")]
mod accessibility;
#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod clipboard;
#[cfg(target_os = "macos")]
mod menu;
#[cfg(target_os = "macos")]
pub mod synth;
#[cfg(target_os = "macos")]
mod view;
#[cfg(target_os = "macos")]
mod window;

#[cfg(target_os = "macos")]
pub use accessibility::{AccessibilityHandler, AxNode};
#[cfg(target_os = "macos")]
pub use app::{AppContext, run, run_on_main};
#[cfg(target_os = "macos")]
pub use clipboard::Clipboard;
#[cfg(target_os = "macos")]
pub use window::{Waker, Window, WindowId};

pub use auk_term::input::{Key, KeyEvent, Mods};
pub use keys::OptionAsAlt;

/// What a window reports to its handler.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A key press, mapped to the terminal's key model. Encode it with
    /// `auk_term::input::encode_key` for the pane's modes.
    Key(KeyInput),
    /// Text that didn't come from a single key: an input method's commit, a dead-key
    /// composition, the character viewer, dictation. Send it as typed text.
    Text(String),
    /// The input method's composition in progress (marked text), or `None` when it ends. Draw
    /// it at the cursor; don't send it.
    Preedit(Option<Preedit>),
    ModifiersChanged(Mods),
    Mouse(MouseEvent),
    Scroll(ScrollEvent),
    /// The view's size or display scale changed.
    Resized(Size),
    /// The window became (true) or stopped being the key window.
    Focus(bool),
    Appearance(Appearance),
    /// The display is ready for a frame: draw now. Sent on the display's refresh while frames
    /// are requested ([`Window::request_frame`]).
    Frame,
    /// A standard edit command (`copy:`, `paste:`, `selectAll:`) reached the window: from the
    /// Services menu, or a [`StandardAction`] menu item.
    Command(Command),
    /// A [`MenuItem::Command`] reached the window (it was key): the item's command id.
    Menu(u32),
    /// The user asked to close the window (close button, ⌘W). Call [`Window::close`] to close it.
    CloseRequested,
    /// The window closed; the handler is dropped after this.
    Closed,
    /// A [`Waker`] fired: something off the main thread has news for this window.
    Wake,
}

/// A key press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub event: KeyEvent,
    /// Auto-repeat.
    pub repeat: bool,
    /// The macOS virtual key code (`kVK_*`).
    pub keycode: u16,
    /// Option is held, whether or not it acts as Alt (keymap chords name it as `alt`).
    pub option: bool,
}

/// Marked text from an input method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    /// The input method's selection within `text`, as a byte range: where its caret or the
    /// clause being converted is.
    pub selection: std::ops::Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Further buttons, by `NSEvent.buttonNumber`.
    Other(u8),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    /// Movement with no button held.
    Moved,
    /// Movement with a button held.
    Dragged(MouseButton),
    Entered,
    Exited,
}

/// A mouse event at a position in the view, in points from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub x: f64,
    pub y: f64,
    pub mods: Mods,
    /// 1 for a single click, 2 for a double click, and so on (0 for movement).
    pub clicks: u32,
}

/// Where a gesture is (trackpads report phases; wheels don't).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollPhase {
    /// A wheel tick, or an event with no phase.
    None,
    Began,
    Changed,
    Ended,
    /// Inertial scrolling after the fingers lifted.
    Momentum,
    MomentumEnded,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollEvent {
    /// Scroll amount; positive is toward the content's start (up, left).
    pub dx: f64,
    pub dy: f64,
    /// `dx`/`dy` are in points (trackpads, Magic Mouse) rather than lines (wheels).
    pub precise: bool,
    pub phase: ScrollPhase,
    pub x: f64,
    pub y: f64,
    pub mods: Mods,
}

/// A view's size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
    /// Pixels per point.
    pub scale: f64,
}

impl Size {
    /// The size in physical pixels: the drawable's size.
    pub fn physical(&self) -> (u32, u32) {
        (
            (self.width * self.scale).round().max(1.0) as u32,
            (self.height * self.scale).round().max(1.0) as u32,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Appearance {
    Light,
    Dark,
}

/// Menu commands that reach the focused window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Copy,
    Paste,
    SelectAll,
    NewWindow,
}

/// Key-equivalent modifiers for a menu item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct KeyMods {
    pub cmd: bool,
    pub shift: bool,
    pub opt: bool,
    pub ctrl: bool,
}

impl KeyMods {
    pub const NONE: KeyMods = KeyMods {
        cmd: false,
        shift: false,
        opt: false,
        ctrl: false,
    };
    pub const CMD: KeyMods = KeyMods {
        cmd: true,
        ..KeyMods::NONE
    };
}

/// AppKit's own actions, sent down the responder chain by their standard selectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StandardAction {
    About,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    ToggleFullScreen,
    Minimize,
    Zoom,
    BringAllToFront,
    Copy,
    Paste,
    SelectAll,
}

/// A menu, for [`AppContext::set_main_menu`]. The menu bar is a `Menu` whose items are
/// submenus.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
    /// This is the Window menu: AppKit lists the open windows in it.
    pub windows_menu: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuItem {
    /// Sends `command` to the key window ([`Event::Menu`]), or, with no key window, to the
    /// app's command handler. `key` is the key equivalent (`"t"`, `"["`, `"\r"`), empty for
    /// none.
    Command {
        title: String,
        command: u32,
        key: String,
        mods: KeyMods,
    },
    Standard {
        title: String,
        action: StandardAction,
        key: String,
        mods: KeyMods,
    },
    /// The system's Services submenu.
    Services {
        title: String,
    },
    Separator,
    Submenu(Menu),
}

/// How a window starts.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowAttributes {
    pub title: String,
    /// Content size in points.
    pub width: f64,
    pub height: f64,
    pub option_as_alt: OptionAsAlt,
}

impl Default for WindowAttributes {
    fn default() -> Self {
        WindowAttributes {
            title: "Auk".into(),
            width: 1200.0,
            height: 800.0,
            option_as_alt: OptionAsAlt::None,
        }
    }
}

/// Receives a window's events, on the main thread.
#[cfg(target_os = "macos")]
pub trait WindowHandler {
    fn event(&mut self, window: &Window, event: Event);
}
