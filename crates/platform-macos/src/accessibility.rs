//! Accessibility (§19): AccessKit's macOS adapter, hosted by the content view. The app supplies
//! the tree ([`AccessibilityHandler`]); the view answers `NSAccessibility` requests from it.
//! [`crate::Window::accessibility_tree`] reads the tree back through the same `NSAccessibility`
//! calls an assistive app makes.

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSString};

/// Supplies a window's accessibility tree and performs actions on it. Called on the main thread,
/// when an assistive app first asks for the tree and when it acts.
pub trait AccessibilityHandler {
    /// The whole tree, the first time an assistive app asks.
    fn initial_tree(&mut self) -> TreeUpdate;

    /// An assistive app asked for an action (focus, click, scroll…).
    fn action(&mut self, request: ActionRequest);
}

/// AccessKit's adapter and the app's handler, owned by the view.
pub(crate) struct Accessibility {
    pub adapter: accesskit_macos::Adapter,
    pub activation: Activation,
}

/// Bridges the app's handler to AccessKit's two handler traits.
pub(crate) struct Activation(pub std::rc::Rc<std::cell::RefCell<Box<dyn AccessibilityHandler>>>);

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        // An assistive app may ask while the handler is busy further up the stack (it is only
        // ever asked from AppKit's accessibility calls, which don't nest inside it).
        let mut handler = self.0.try_borrow_mut().ok()?;
        Some(handler.initial_tree())
    }
}

pub(crate) struct Actions(pub std::rc::Rc<std::cell::RefCell<Box<dyn AccessibilityHandler>>>);

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        if let Ok(mut handler) = self.0.try_borrow_mut() {
            handler.action(request);
        }
    }
}

/// One element of the accessibility tree, as `NSAccessibility` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AxNode {
    /// `accessibilityRole`, e.g. `AXGroup`, `AXTextArea`, `AXTabGroup`, `AXProgressIndicator`.
    pub role: String,
    pub subrole: Option<String>,
    pub label: Option<String>,
    pub title: Option<String>,
    /// `accessibilityValue`, as text.
    pub value: Option<String>,
    pub children: Vec<AxNode>,
}

fn string_of(object: Option<Retained<AnyObject>>) -> Option<String> {
    let object = object?;
    if let Some(s) = object.downcast_ref::<NSString>() {
        return Some(s.to_string());
    }
    // NSNumber and other values: their description.
    // SAFETY: every NSObject answers `description` with a string.
    let description: Option<Retained<NSString>> = unsafe { msg_send![&*object, description] };
    description.map(|d| d.to_string())
}

/// Reads an element and its descendants through `NSAccessibility`.
pub(crate) fn read(element: &AnyObject, depth: usize) -> AxNode {
    // SAFETY: these are informal NSAccessibility protocol methods every NSObject-based element
    // answers (they return nil when unset).
    unsafe {
        let role: Option<Retained<AnyObject>> = msg_send![element, accessibilityRole];
        let subrole: Option<Retained<AnyObject>> = msg_send![element, accessibilitySubrole];
        let label: Option<Retained<AnyObject>> = msg_send![element, accessibilityLabel];
        let title: Option<Retained<AnyObject>> = msg_send![element, accessibilityTitle];
        let value: Option<Retained<AnyObject>> = msg_send![element, accessibilityValue];
        let children: Option<Retained<NSArray<AnyObject>>> =
            msg_send![element, accessibilityChildren];
        AxNode {
            role: string_of(role).unwrap_or_default(),
            subrole: string_of(subrole),
            label: string_of(label).filter(|s| !s.is_empty()),
            title: string_of(title).filter(|s| !s.is_empty()),
            value: string_of(value),
            children: if depth == 0 {
                Vec::new()
            } else {
                children
                    .map(|c| c.iter().map(|e| read(&e, depth - 1)).collect())
                    .unwrap_or_default()
            },
        }
    }
}
