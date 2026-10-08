//! The main menu, built from a [`Menu`] description: items that send a command id down the
//! responder chain (to the key window's view, else to the app's command handler), and AppKit's
//! standard actions (About, Hide, Quit, full screen…).

use std::cell::RefCell;

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::sel;
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

use crate::{KeyMods, Menu, MenuItem, StandardAction};

fn modifier_flags(mods: KeyMods) -> NSEventModifierFlags {
    let mut flags = NSEventModifierFlags::empty();
    if mods.cmd {
        flags |= NSEventModifierFlags::Command;
    }
    if mods.shift {
        flags |= NSEventModifierFlags::Shift;
    }
    if mods.opt {
        flags |= NSEventModifierFlags::Option;
    }
    if mods.ctrl {
        flags |= NSEventModifierFlags::Control;
    }
    flags
}

fn standard_selector(action: StandardAction) -> Sel {
    match action {
        StandardAction::About => sel!(orderFrontStandardAboutPanel:),
        StandardAction::Hide => sel!(hide:),
        StandardAction::HideOthers => sel!(hideOtherApplications:),
        StandardAction::ShowAll => sel!(unhideAllApplications:),
        StandardAction::Quit => sel!(terminate:),
        StandardAction::ToggleFullScreen => sel!(toggleFullScreen:),
        StandardAction::Minimize => sel!(performMiniaturize:),
        StandardAction::Zoom => sel!(performZoom:),
        StandardAction::BringAllToFront => sel!(arrangeInFront:),
        StandardAction::Copy => sel!(copy:),
        StandardAction::Paste => sel!(paste:),
        StandardAction::SelectAll => sel!(selectAll:),
    }
}

/// Builds the menu bar and installs it.
pub(crate) fn install(mtm: MainThreadMarker, menu: &Menu) {
    let app = NSApplication::sharedApplication(mtm);
    let bar = NSMenu::new(mtm);
    for item in &menu.items {
        match item {
            MenuItem::Submenu(sub) => {
                let ns = build(mtm, sub);
                let holder = NSMenuItem::new(mtm);
                holder.setTitle(&NSString::from_str(&sub.title));
                holder.setSubmenu(Some(&ns));
                bar.addItem(&holder);
            }
            // Items directly on the bar aren't meaningful in AppKit; put them in the app menu.
            other => {
                let _ = other;
            }
        }
    }
    app.setMainMenu(Some(&bar));
}

fn build(mtm: MainThreadMarker, menu: &Menu) -> Retained<NSMenu> {
    let ns = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(&menu.title));
    for item in &menu.items {
        match item {
            MenuItem::Separator => ns.addItem(&NSMenuItem::separatorItem(mtm)),
            MenuItem::Command {
                title,
                command,
                key,
                mods,
            } => {
                let item = add(mtm, &ns, title, Some(sel!(aukCommand:)), key, *mods);
                item.setTag(*command as isize);
            }
            MenuItem::Standard {
                title,
                action,
                key,
                mods,
            } => {
                add(mtm, &ns, title, Some(standard_selector(*action)), key, *mods);
            }
            MenuItem::Services { title } => {
                let services = NSMenu::new(mtm);
                let item = add(mtm, &ns, title, None, "", KeyMods::NONE);
                item.setSubmenu(Some(&services));
                NSApplication::sharedApplication(mtm).setServicesMenu(Some(&services));
            }
            MenuItem::Submenu(sub) => {
                let child = build(mtm, sub);
                let item = add(mtm, &ns, &sub.title, None, "", KeyMods::NONE);
                item.setSubmenu(Some(&child));
            }
        }
    }
    if menu.windows_menu {
        NSApplication::sharedApplication(mtm).setWindowsMenu(Some(&ns));
    }
    ns
}

fn add(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    title: &str,
    action: Option<Sel>,
    key: &str,
    mods: KeyMods,
) -> Retained<NSMenuItem> {
    // SAFETY: every action is a responder-chain selector taking a sender.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    };
    item.setKeyEquivalentModifierMask(modifier_flags(mods));
    menu.addItem(&item);
    item
}

/// The installed menu bar as titles: `[(menu, [item…])]`, separators left out.
pub(crate) fn dump(mtm: MainThreadMarker) -> Vec<(String, Vec<String>)> {
    let Some(bar) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for top in bar.itemArray().iter() {
        let Some(sub) = top.submenu() else { continue };
        let items = sub
            .itemArray()
            .iter()
            .filter(|i| !i.isSeparatorItem())
            .map(|i| i.title().to_string())
            .collect();
        out.push((sub.title().to_string(), items));
    }
    out
}

/// Invokes the item at `path` (a top-level menu's title, then item titles) the way a click
/// does: `-[NSMenu performActionForItemAtIndex:]`, which validates the item and sends its action
/// down the responder chain. Returns false when no enabled item has that path.
pub(crate) fn perform(mtm: MainThreadMarker, path: &[&str]) -> bool {
    let Some(mut menu) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return false;
    };
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    for title in parents {
        let found = menu.itemArray().iter().find_map(|item| {
            let sub = item.submenu()?;
            (sub.title().to_string() == *title || item.title().to_string() == *title)
                .then_some(sub)
        });
        match found {
            Some(sub) => menu = sub,
            None => return false,
        }
    }
    menu.update();
    let index = menu.indexOfItemWithTitle(&NSString::from_str(last));
    if index < 0 {
        return false;
    }
    match menu.itemAtIndex(index) {
        Some(item) if item.isEnabled() => {
            menu.performActionForItemAtIndex(index);
            true
        }
        _ => false,
    }
}

type CommandHandler = Box<dyn FnMut(&crate::AppContext, u32)>;

thread_local! {
    /// Receives menu commands no window took (no key window): `New Window` with every window
    /// closed, for example.
    static APP_COMMANDS: RefCell<Option<CommandHandler>> = const { RefCell::new(None) };
}

pub(crate) fn set_app_handler(handler: CommandHandler) {
    APP_COMMANDS.with(|h| *h.borrow_mut() = Some(handler));
}

pub(crate) fn app_command(ctx: &crate::AppContext, command: u32) {
    let handler = APP_COMMANDS.with(|h| h.borrow_mut().take());
    if let Some(mut handler) = handler {
        handler(ctx, command);
        APP_COMMANDS.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(handler);
            }
        });
    }
}

/// The default menu for tools and examples that don't describe their own.
pub(crate) fn default_menu() -> Menu {
    use MenuItem::*;
    let cmd = KeyMods::CMD;
    Menu {
        title: String::new(),
        windows_menu: false,
        items: vec![
            Submenu(Menu {
                title: "Auk".into(),
                windows_menu: false,
                items: vec![
                    Standard {
                        title: "About Auk".into(),
                        action: StandardAction::About,
                        key: String::new(),
                        mods: KeyMods::NONE,
                    },
                    Separator,
                    Standard {
                        title: "Quit Auk".into(),
                        action: StandardAction::Quit,
                        key: "q".into(),
                        mods: cmd,
                    },
                ],
            }),
            Submenu(Menu {
                title: "Edit".into(),
                windows_menu: false,
                items: vec![
                    Standard {
                        title: "Copy".into(),
                        action: StandardAction::Copy,
                        key: "c".into(),
                        mods: cmd,
                    },
                    Standard {
                        title: "Paste".into(),
                        action: StandardAction::Paste,
                        key: "v".into(),
                        mods: cmd,
                    },
                    Standard {
                        title: "Select All".into(),
                        action: StandardAction::SelectAll,
                        key: "a".into(),
                        mods: cmd,
                    },
                ],
            }),
            Submenu(Menu {
                title: "Window".into(),
                windows_menu: true,
                items: vec![Standard {
                    title: "Minimize".into(),
                    action: StandardAction::Minimize,
                    key: "m".into(),
                    mods: cmd,
                }],
            }),
        ],
    }
}
