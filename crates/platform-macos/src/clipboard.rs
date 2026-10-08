//! The general pasteboard.

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

    /// The clipboard's text, if it holds any.
    pub fn text(&self) -> Option<String> {
        let board = NSPasteboard::generalPasteboard();
        // SAFETY: a constant pasteboard type.
        board
            .stringForType(unsafe { NSPasteboardTypeString })
            .map(|s| s.to_string())
    }

    /// Replaces the clipboard's contents with `text`.
    pub fn set_text(&self, text: &str) -> bool {
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        // SAFETY: a constant pasteboard type.
        board.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
    }
}
