//! The general pasteboard, or a named one when `$AUK_PASTEBOARD` is set (tests use their own, so
//! they never touch the user's clipboard).

use objc2::MainThreadMarker;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

/// The system clipboard (plain text).
#[derive(Debug)]
pub struct Clipboard {
    _mtm: MainThreadMarker,
}

impl Clipboard {
    pub(crate) fn new(mtm: MainThreadMarker) -> Clipboard {
        Clipboard { _mtm: mtm }
    }

    fn board(&self) -> objc2::rc::Retained<NSPasteboard> {
        match std::env::var("AUK_PASTEBOARD") {
            Ok(name) if !name.is_empty() => {
                NSPasteboard::pasteboardWithName(&NSString::from_str(&name))
            }
            _ => NSPasteboard::generalPasteboard(),
        }
    }

    /// The clipboard's text, if it holds any.
    pub fn text(&self) -> Option<String> {
        let board = self.board();
        // SAFETY: a constant pasteboard type.
        board
            .stringForType(unsafe { NSPasteboardTypeString })
            .map(|s| s.to_string())
    }

    /// Replaces the clipboard's contents with `text`.
    pub fn set_text(&self, text: &str) -> bool {
        let board = self.board();
        board.clearContents();
        // SAFETY: a constant pasteboard type.
        board.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
    }
}
