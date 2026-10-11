//! Effects: requests host Lua makes of the windows showing a pane.

use std::rc::Rc;

use mlua::{UserData, UserDataMethods, Value};

use super::ApiState;
use crate::events::PaneId;

/// Which windows receive an effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectTarget {
    /// Every window with the pane's window key; dropped when none is attached.
    Pane(PaneId),
    /// Every window of the host (timer, process and fetch callbacks, and `spawn`).
    Host,
}

/// A toast's level. `"success"` is success; `"error"`, `"warn"` and `"warning"` are error; any
/// other string is info.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    /// Success.
    Success,
    /// Info.
    Info,
    /// Error.
    Error,
}

impl ToastLevel {
    /// Maps a host `cx:toast` level string.
    pub fn from_host(level: &str) -> Self {
        match level {
            "success" => ToastLevel::Success,
            "error" | "warn" | "warning" => ToastLevel::Error,
            _ => ToastLevel::Info,
        }
    }
}

/// An effect, carried from host to windows as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// A toast: two lines.
    Toast {
        /// Level.
        level: ToastLevel,
        /// First line.
        text: String,
        /// Optional second line.
        sub: Option<String>,
    },
    /// Handled like a link click in each receiving window.
    Open(String),
    /// Put on each receiving window's clipboard.
    Copy(String),
}

/// The effect cx host event handlers (and lens `open`/`event`) receive. It holds only its
/// target, so it may be kept; requests queue until the handler returns.
#[derive(Clone)]
pub(crate) struct EffectCx {
    target: EffectTarget,
    state: Rc<ApiState>,
}

impl EffectCx {
    pub(crate) fn new(target: EffectTarget, state: Rc<ApiState>) -> Self {
        EffectCx { target, state }
    }
}

/// Converts a Lua value to display text the way `tostring` does, never raising.
pub(crate) fn display(value: &Value) -> String {
    value
        .to_string()
        .unwrap_or_else(|_| format!("<{}>", value.type_name()))
}

/// Like [`display`], but nil is empty.
fn text(value: &Value) -> String {
    if value.is_nil() {
        String::new()
    } else {
        display(value)
    }
}

impl UserData for EffectCx {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method(
            "toast",
            |_, this, (level, message, sub): (Value, Value, Value)| {
                let effect = Effect::Toast {
                    level: ToastLevel::from_host(&display(&level)),
                    text: text(&message),
                    sub: (!sub.is_nil()).then(|| display(&sub)),
                };
                this.state.push_effect(this.target, effect);
                Ok(())
            },
        );
        methods.add_method("open", |_, this, target: Value| {
            this.state
                .push_effect(this.target, Effect::Open(text(&target)));
            Ok(())
        });
        methods.add_method("copy", |_, this, value: Value| {
            this.state
                .push_effect(this.target, Effect::Copy(text(&value)));
            Ok(())
        });
    }
}
