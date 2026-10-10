//! The command registry: every app command with its menu placement and key equivalent. The
//! main menu is generated from it (§19), so the menu is also the keymap.

use auk_platform_macos::{KeyMods, Menu, MenuItem, StandardAction};

use crate::geometry::Direction;

/// An app command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    NewWindow,
    NewSession,
    SwitchSession,
    RenameSession,
    NextSession,
    PreviousSession,
    NewTab,
    NextTab,
    PreviousTab,
    SelectTab(u8),
    SplitRight,
    SplitDown,
    ClosePane,
    ToggleZoom,
    Focus(Direction),
    Copy,
    Paste,
    SelectAll,
    ClearScrollback,
    ScrollToTop,
    ScrollToBottom,
    ToggleFullScreen,
    CommandPalette,
    ReloadPlugins,
    ReloadSettings,
    LockSession,
    Find,
    FindNext,
    FindPrevious,
    PreviousPrompt,
    NextPrompt,
}

/// One registry entry: where the command sits in the menu bar and its key equivalent.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub cmd: Cmd,
    pub menu: &'static str,
    pub title: &'static str,
    pub key: &'static str,
    pub mods: KeyMods,
    /// Starts a group in its menu (a separator before it).
    pub group: bool,
}

const fn mods(cmd: bool, shift: bool, opt: bool, ctrl: bool) -> KeyMods {
    KeyMods {
        cmd,
        shift,
        opt,
        ctrl,
    }
}

const CMD: KeyMods = mods(true, false, false, false);
const CMD_SHIFT: KeyMods = mods(true, true, false, false);
const CMD_OPT: KeyMods = mods(true, false, true, false);
const CTRL_CMD: KeyMods = mods(true, false, false, true);
const NONE: KeyMods = mods(false, false, false, false);

const fn entry(
    cmd: Cmd,
    menu: &'static str,
    title: &'static str,
    key: &'static str,
    mods: KeyMods,
) -> Entry {
    Entry {
        cmd,
        menu,
        title,
        key,
        mods,
        group: false,
    }
}

const fn grouped(e: Entry) -> Entry {
    Entry { group: true, ..e }
}

/// The registry, in menu order.
pub const REGISTRY: &[Entry] = &[
    entry(Cmd::NewWindow, "Shell", "New Window", "n", CMD),
    entry(Cmd::NewSession, "Shell", "New Session", "n", CMD_SHIFT),
    entry(Cmd::NewTab, "Shell", "New Tab", "t", CMD),
    grouped(entry(Cmd::SplitRight, "Shell", "Split Right", "d", CMD)),
    entry(Cmd::SplitDown, "Shell", "Split Down", "d", CMD_SHIFT),
    grouped(entry(Cmd::ClosePane, "Shell", "Close Pane", "w", CMD)),
    entry(Cmd::RenameSession, "Shell", "Rename Session…", "", NONE),
    entry(Cmd::LockSession, "Shell", "Lock Session", "", NONE),
    entry(Cmd::Copy, "Edit", "Copy", "c", CMD),
    entry(Cmd::Paste, "Edit", "Paste", "v", CMD),
    entry(Cmd::SelectAll, "Edit", "Select All", "a", CMD),
    grouped(entry(Cmd::Find, "Edit", "Find…", "f", CMD)),
    entry(Cmd::FindNext, "Edit", "Find Next", "g", CMD),
    entry(Cmd::FindPrevious, "Edit", "Find Previous", "g", CMD_SHIFT),
    grouped(entry(
        Cmd::ClearScrollback,
        "Edit",
        "Clear Scrollback",
        "k",
        CMD,
    )),
    entry(Cmd::ToggleZoom, "View", "Zoom Pane", "\r", CMD_SHIFT),
    grouped(entry(
        Cmd::ScrollToTop,
        "View",
        "Scroll to Top",
        "\u{F729}",
        CMD,
    )),
    entry(
        Cmd::ScrollToBottom,
        "View",
        "Scroll to Bottom",
        "\u{F72B}",
        CMD,
    ),
    entry(
        Cmd::PreviousPrompt,
        "View",
        "Jump to Previous Prompt",
        "\u{F700}",
        CMD,
    ),
    entry(
        Cmd::NextPrompt,
        "View",
        "Jump to Next Prompt",
        "\u{F701}",
        CMD,
    ),
    grouped(entry(
        Cmd::CommandPalette,
        "View",
        "Command Palette…",
        "p",
        CMD_SHIFT,
    )),
    entry(Cmd::ReloadPlugins, "View", "Reload Plugins", "", NONE),
    entry(Cmd::ReloadSettings, "View", "Reload Settings", "", NONE),
    grouped(entry(
        Cmd::ToggleFullScreen,
        "View",
        "Toggle Full Screen",
        "f",
        mods(true, false, false, true),
    )),
    entry(Cmd::NextTab, "Window", "Show Next Tab", "]", CMD_SHIFT),
    entry(
        Cmd::PreviousTab,
        "Window",
        "Show Previous Tab",
        "[",
        CMD_SHIFT,
    ),
    grouped(entry(
        Cmd::SwitchSession,
        "Window",
        "Switch Session…",
        "s",
        CTRL_CMD,
    )),
    entry(
        Cmd::NextSession,
        "Window",
        "Show Next Session",
        "]",
        CMD_OPT,
    ),
    entry(
        Cmd::PreviousSession,
        "Window",
        "Show Previous Session",
        "[",
        CMD_OPT,
    ),
    grouped(entry(
        Cmd::Focus(Direction::Left),
        "Window",
        "Focus Pane Left",
        "\u{F702}",
        CMD_OPT,
    )),
    entry(
        Cmd::Focus(Direction::Right),
        "Window",
        "Focus Pane Right",
        "\u{F703}",
        CMD_OPT,
    ),
    entry(
        Cmd::Focus(Direction::Up),
        "Window",
        "Focus Pane Above",
        "\u{F700}",
        CMD_OPT,
    ),
    entry(
        Cmd::Focus(Direction::Down),
        "Window",
        "Focus Pane Below",
        "\u{F701}",
        CMD_OPT,
    ),
    grouped(entry(Cmd::SelectTab(1), "Window", "Tab 1", "1", CMD)),
    entry(Cmd::SelectTab(2), "Window", "Tab 2", "2", CMD),
    entry(Cmd::SelectTab(3), "Window", "Tab 3", "3", CMD),
    entry(Cmd::SelectTab(4), "Window", "Tab 4", "4", CMD),
    entry(Cmd::SelectTab(5), "Window", "Tab 5", "5", CMD),
    entry(Cmd::SelectTab(6), "Window", "Tab 6", "6", CMD),
    entry(Cmd::SelectTab(7), "Window", "Tab 7", "7", CMD),
    entry(Cmd::SelectTab(8), "Window", "Tab 8", "8", CMD),
    entry(Cmd::SelectTab(9), "Window", "Last Tab", "9", CMD),
];

/// A command's id in the menu (its registry index).
pub fn id_of(cmd: Cmd) -> u32 {
    REGISTRY
        .iter()
        .position(|e| e.cmd == cmd)
        .expect("every command is registered") as u32
}

/// The command a menu id names.
pub fn by_id(id: u32) -> Option<Cmd> {
    REGISTRY.get(id as usize).map(|e| e.cmd)
}

/// A command's built-in id: the action name plugins and the palette use (`cx:command`,
/// `tern.override`, status segment `command`s).
pub fn builtin_id(cmd: Cmd) -> String {
    match cmd {
        Cmd::NewWindow => "new_window".into(),
        Cmd::NewSession => "new_session".into(),
        Cmd::SwitchSession => "switch_session".into(),
        Cmd::RenameSession => "rename_session".into(),
        Cmd::NextSession => "next_session".into(),
        Cmd::PreviousSession => "previous_session".into(),
        Cmd::NewTab => "new_tab".into(),
        Cmd::NextTab => "next_tab".into(),
        Cmd::PreviousTab => "previous_tab".into(),
        Cmd::SelectTab(9) => "last_tab".into(),
        Cmd::SelectTab(n) => format!("tab_{n}"),
        Cmd::SplitRight => "split_right".into(),
        Cmd::SplitDown => "split_down".into(),
        Cmd::ClosePane => "close_pane".into(),
        Cmd::ToggleZoom => "toggle_zoom".into(),
        Cmd::Focus(Direction::Left) => "focus_left".into(),
        Cmd::Focus(Direction::Right) => "focus_right".into(),
        Cmd::Focus(Direction::Up) => "focus_up".into(),
        Cmd::Focus(Direction::Down) => "focus_down".into(),
        Cmd::Copy => "copy".into(),
        Cmd::Paste => "paste".into(),
        Cmd::SelectAll => "select_all".into(),
        Cmd::ClearScrollback => "clear_scrollback".into(),
        Cmd::ScrollToTop => "scroll_to_top".into(),
        Cmd::ScrollToBottom => "scroll_to_bottom".into(),
        Cmd::ToggleFullScreen => "toggle_full_screen".into(),
        Cmd::CommandPalette => "command_palette".into(),
        Cmd::ReloadPlugins => "reload_plugins".into(),
        Cmd::ReloadSettings => "reload_settings".into(),
        Cmd::LockSession => "lock_session".into(),
        Cmd::Find => "find".into(),
        Cmd::FindNext => "find_next".into(),
        Cmd::FindPrevious => "find_previous".into(),
        Cmd::PreviousPrompt => "previous_prompt".into(),
        Cmd::NextPrompt => "next_prompt".into(),
    }
}

/// The command a built-in id names.
pub fn by_builtin_id(id: &str) -> Option<Cmd> {
    REGISTRY
        .iter()
        .map(|e| e.cmd)
        .find(|c| builtin_id(*c) == id)
}

/// Every built-in id, in registry order.
pub fn builtin_ids() -> Vec<String> {
    REGISTRY.iter().map(|e| builtin_id(e.cmd)).collect()
}

/// The key equivalents the menu binds, as chords (`cmd+shift+p`): the default keymap layer a
/// plugin bind may not take (§9.8 "Binds").
pub fn default_chords() -> Vec<String> {
    REGISTRY.iter().filter_map(entry_chord).collect()
}

/// A registry entry's key equivalent as a chord.
fn entry_chord(e: &Entry) -> Option<String> {
    Some(e).filter(|e| !e.key.is_empty()).and_then(|e| {
        let key = match e.key {
            "\r" => "enter".to_owned(),
            "\u{F729}" => "home".to_owned(),
            "\u{F72B}" => "end".to_owned(),
            "\u{F702}" => "left".to_owned(),
            "\u{F703}" => "right".to_owned(),
            "\u{F700}" => "up".to_owned(),
            "\u{F701}" => "down".to_owned(),
            k if k.chars().count() == 1 => k.to_owned(),
            _ => return None,
        };
        let m = e.mods;
        let mut parts = Vec::new();
        if m.ctrl {
            parts.push("ctrl");
        }
        if m.opt {
            parts.push("alt");
        }
        if m.shift {
            parts.push("shift");
        }
        if m.cmd {
            parts.push("cmd");
        }
        Some(format!("{}+{key}", parts.join("+")))
    })
}

/// The command a menu title names (for the automation endpoint).
pub fn by_title(menu: &str, title: &str) -> Option<Cmd> {
    REGISTRY
        .iter()
        .find(|e| e.menu == menu && e.title == title)
        .map(|e| e.cmd)
}

fn standard(title: &str, action: StandardAction, key: &str, mods: KeyMods) -> MenuItem {
    MenuItem::Standard {
        title: title.into(),
        action,
        key: key.into(),
        mods,
    }
}

/// The menu bar: the app menu, then the registry's menus in order, with AppKit's standard items
/// where they belong.
pub fn menu_bar() -> Menu {
    menu_bar_without(&[])
}

/// The menu bar with the key equivalents of `taken` left out: chords the user's `keybinds`
/// rebind or unbind (§9.8: the user layer always wins), so those keys reach the keymap.
pub fn menu_bar_without(taken: &[auk_plugins::chord::Chord]) -> Menu {
    let free = |e: &Entry| {
        entry_chord(e)
            .and_then(|c| auk_plugins::chord::Chord::parse(&c).ok())
            .is_none_or(|c| !taken.contains(&c))
    };
    let app_menu = Menu {
        title: "Auk".into(),
        windows_menu: false,
        items: vec![
            standard("About Auk", StandardAction::About, "", NONE),
            MenuItem::Separator,
            MenuItem::Services {
                title: "Services".into(),
            },
            MenuItem::Separator,
            standard("Hide Auk", StandardAction::Hide, "h", CMD),
            standard("Hide Others", StandardAction::HideOthers, "h", CMD_OPT),
            standard("Show All", StandardAction::ShowAll, "", NONE),
            MenuItem::Separator,
            standard("Quit Auk", StandardAction::Quit, "q", CMD),
        ],
    };
    let mut menus = vec![app_menu];
    for name in ["Shell", "Edit", "View", "Window"] {
        let mut items = Vec::new();
        for e in REGISTRY.iter().filter(|e| e.menu == name) {
            if e.group && !items.is_empty() {
                items.push(MenuItem::Separator);
            }
            let (key, mods) = if free(e) { (e.key, e.mods) } else { ("", NONE) };
            items.push(MenuItem::Command {
                title: e.title.into(),
                command: id_of(e.cmd),
                key: key.into(),
                mods,
            });
        }
        if name == "Window" {
            items.insert(0, standard("Minimize", StandardAction::Minimize, "m", CMD));
            items.insert(1, standard("Zoom", StandardAction::Zoom, "", NONE));
            items.insert(2, MenuItem::Separator);
            items.push(MenuItem::Separator);
            items.push(standard(
                "Bring All to Front",
                StandardAction::BringAllToFront,
                "",
                NONE,
            ));
        }
        menus.push(Menu {
            title: name.into(),
            items,
            windows_menu: name == "Window",
        });
    }
    Menu {
        title: String::new(),
        items: menus.into_iter().map(MenuItem::Submenu).collect(),
        windows_menu: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_key_equivalents_are_unique() {
        for e in REGISTRY {
            assert_eq!(by_id(id_of(e.cmd)), Some(e.cmd));
            assert_eq!(by_title(e.menu, e.title), Some(e.cmd));
        }
        let mut seen = std::collections::HashSet::new();
        for e in REGISTRY.iter().filter(|e| !e.key.is_empty()) {
            assert!(
                seen.insert((e.key, e.mods)),
                "{} reuses a key equivalent",
                e.title
            );
        }
    }

    #[test]
    fn the_menu_bar_holds_every_command() {
        let bar = menu_bar();
        let mut commands = 0;
        fn count(menu: &Menu, n: &mut usize) {
            for item in &menu.items {
                match item {
                    MenuItem::Command { .. } => *n += 1,
                    MenuItem::Submenu(m) => count(m, n),
                    _ => {}
                }
            }
        }
        count(&bar, &mut commands);
        assert_eq!(commands, REGISTRY.len());
    }
}
