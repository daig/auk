//! The command registry: every app command with its menu placement and key equivalent. The
//! main menu is generated from it (§19), so the menu is also the keymap.

use auk_platform_macos::{KeyMods, Menu, MenuItem, StandardAction};

use crate::geometry::Direction;

/// An app command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    NewWindow,
    NewSession,
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
    entry(Cmd::Copy, "Edit", "Copy", "c", CMD),
    entry(Cmd::Paste, "Edit", "Paste", "v", CMD),
    entry(Cmd::SelectAll, "Edit", "Select All", "a", CMD),
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
            items.push(MenuItem::Command {
                title: e.title.into(),
                command: id_of(e.cmd),
                key: e.key.into(),
                mods: e.mods,
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
