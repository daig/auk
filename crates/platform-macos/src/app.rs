//! The application: `NSApplication` setup, its delegate and the main menu.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSEventModifierFlags,
    NSMenu, NSMenuItem,
};
use objc2_foundation::{NSNotification, NSObject, NSString};

use crate::{Clipboard, Window, WindowAttributes, WindowHandler};

/// What the app's startup closure and later main-thread code create windows with.
#[derive(Clone, Copy, Debug)]
pub struct AppContext {
    mtm: MainThreadMarker,
}

impl AppContext {
    /// The context, on the main thread (`None` elsewhere).
    pub fn current() -> Option<AppContext> {
        MainThreadMarker::new().map(|mtm| AppContext { mtm })
    }

    pub fn create_window(
        &self,
        attrs: WindowAttributes,
        handler: Box<dyn WindowHandler>,
    ) -> Window {
        Window::create(self.mtm, &attrs, handler)
    }

    pub fn clipboard(&self) -> Clipboard {
        Clipboard::new(self.mtm)
    }

    /// Quits the app.
    pub fn quit(&self) {
        NSApplication::sharedApplication(self.mtm).terminate(None);
    }
}

/// Runs `f` on the main thread, from the event loop, soon. Callable from any thread.
pub fn run_on_main(f: impl FnOnce() + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(f);
}

type Init = Box<dyn FnOnce(&AppContext)>;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AukAppDelegate"]
    #[ivars = RefCell<Option<Init>>]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            // `activate` is cooperative and is refused for an app launched from a terminal; the
            // older call still brings the app forward there.
            #[allow(deprecated)]
            NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            if let Some(init) = self.ivars().borrow_mut().take() {
                init(&AppContext { mtm });
            }
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }
    }
);

/// Runs the app: sets up `NSApplication` and the main menu, calls `init` once the app has
/// launched, and runs the event loop until the app quits. Must be called on the main thread.
pub fn run(init: impl FnOnce(&AppContext) + 'static) {
    let mtm =
        MainThreadMarker::new().expect("auk_platform_macos::run must be called on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate: Retained<AppDelegate> = {
        let this = AppDelegate::alloc(mtm).set_ivars(RefCell::new(Some(Box::new(init) as Init)));
        // SAFETY: NSObject's initializer.
        unsafe { msg_send![super(this), init] }
    };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setMainMenu(Some(&main_menu(mtm)));
    app.run();
}

/// The menu bar: the app menu, Edit, View and Window.
fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let none = NSEventModifierFlags::empty();
    let cmd = NSEventModifierFlags::Command;
    let cmd_opt = NSEventModifierFlags::Command | NSEventModifierFlags::Option;
    let cmd_ctrl = NSEventModifierFlags::Command | NSEventModifierFlags::Control;

    let app_menu = submenu(mtm, &bar, "Auk");
    add(
        mtm,
        &app_menu,
        "About Auk",
        Some(sel!(orderFrontStandardAboutPanel:)),
        "",
        none,
    );
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    let services = NSMenu::new(mtm);
    let services_item = add(mtm, &app_menu, "Services", None, "", none);
    services_item.setSubmenu(Some(&services));
    NSApplication::sharedApplication(mtm).setServicesMenu(Some(&services));
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add(mtm, &app_menu, "Hide Auk", Some(sel!(hide:)), "h", cmd);
    add(
        mtm,
        &app_menu,
        "Hide Others",
        Some(sel!(hideOtherApplications:)),
        "h",
        cmd_opt,
    );
    add(
        mtm,
        &app_menu,
        "Show All",
        Some(sel!(unhideAllApplications:)),
        "",
        none,
    );
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add(mtm, &app_menu, "Quit Auk", Some(sel!(terminate:)), "q", cmd);

    let edit = submenu(mtm, &bar, "Edit");
    add(mtm, &edit, "Copy", Some(sel!(copy:)), "c", cmd);
    add(mtm, &edit, "Paste", Some(sel!(paste:)), "v", cmd);
    add(mtm, &edit, "Select All", Some(sel!(selectAll:)), "a", cmd);

    let view = submenu(mtm, &bar, "View");
    add(
        mtm,
        &view,
        "Enter Full Screen",
        Some(sel!(toggleFullScreen:)),
        "f",
        cmd_ctrl,
    );

    let window = submenu(mtm, &bar, "Window");
    add(mtm, &window, "Close", Some(sel!(performClose:)), "w", cmd);
    add(
        mtm,
        &window,
        "Minimize",
        Some(sel!(performMiniaturize:)),
        "m",
        cmd,
    );
    add(mtm, &window, "Zoom", Some(sel!(performZoom:)), "", none);
    NSApplication::sharedApplication(mtm).setWindowsMenu(Some(&window));
    bar
}

fn submenu(mtm: MainThreadMarker, bar: &NSMenu, title: &str) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    let item = NSMenuItem::new(mtm);
    item.setSubmenu(Some(&menu));
    bar.addItem(&item);
    menu
}

fn add(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    title: &str,
    action: Option<Sel>,
    key: &str,
    mods: NSEventModifierFlags,
) -> Retained<NSMenuItem> {
    // SAFETY: the actions are standard responder-chain selectors taking a sender.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    };
    item.setKeyEquivalentModifierMask(mods);
    menu.addItem(&item);
    item
}
