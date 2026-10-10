//! The block cx (§9.5 "Block cx"): one per running block, kept by the block's code as it likes.
//! Requests (`render`, `save`, `exit`, `frame`, `blob`) queue until the handler returns; `toast`,
//! `open` and `copy` are effects aimed at the windows showing the pane, and aren't guarded.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{UserData, UserDataFields, UserDataMethods, Value};
use serde_json::Value as Json;

use super::ApiState;
use super::convert::to_json;
use super::effects::{Effect, EffectTarget, ToastLevel, display};
use crate::events::PaneId;

/// What a block's code asked for during a handler.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockRequest {
    /// `cx:render()`.
    Render,
    /// `cx:save()`.
    Save,
    /// `cx:exit(code)`.
    Exit(i32),
    /// `cx:frame(ops)`: raw ops sent as one frame.
    Frame(Vec<Json>),
    /// `cx:blob(bytes, mime)`.
    Blob {
        /// The bytes.
        bytes: Vec<u8>,
        /// The MIME type.
        mime: String,
    },
}

/// The pane facts a block cx reports.
#[derive(Debug, Clone, Default)]
pub(crate) struct Geometry {
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

/// The block cx userdata.
pub(crate) struct BlockCx {
    pub pane: PaneId,
    pub geometry: Rc<RefCell<Geometry>>,
    pub state: Rc<ApiState>,
}

impl BlockCx {
    fn request(&self, r: BlockRequest) {
        self.state.block_requests.borrow_mut().push((self.pane, r));
    }
}

fn text(v: &Value) -> String {
    if v.is_nil() {
        String::new()
    } else {
        display(v)
    }
}

impl UserData for BlockCx {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("pane", |_, this| Ok(this.pane as f64));
        fields.add_field_method_get("cwd", |_, this| Ok(this.geometry.borrow().cwd.clone()));
        fields.add_field_method_get("cols", |_, this| Ok(f64::from(this.geometry.borrow().cols)));
        fields.add_field_method_get("rows", |_, this| Ok(f64::from(this.geometry.borrow().rows)));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("render", |_, this, ()| {
            this.request(BlockRequest::Render);
            Ok(())
        });
        methods.add_method("save", |_, this, ()| {
            this.request(BlockRequest::Save);
            Ok(())
        });
        methods.add_method("exit", |_, this, code: Option<f64>| {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "exit codes are small integers"
            )]
            this.request(BlockRequest::Exit(code.unwrap_or(0.0) as i32));
            Ok(())
        });
        methods.add_method("frame", |lua, this, ops: Value| {
            let Value::Table(_) = &ops else {
                return Err(mlua::Error::runtime("cx:frame: ops must be a list"));
            };
            let json = to_json(lua, &ops).map_err(mlua::Error::runtime)?;
            let list = match json {
                Json::Array(list) => list,
                Json::Object(m) if m.is_empty() => Vec::new(),
                _ => return Err(mlua::Error::runtime("cx:frame: ops must be a list")),
            };
            this.request(BlockRequest::Frame(list));
            Ok(())
        });
        methods.add_method(
            "blob",
            |_, this, (bytes, mime): (mlua::LuaString, String)| {
                let bytes = bytes.as_bytes().to_vec();
                let id = tern_sdk::wire::blob_id(&bytes);
                this.request(BlockRequest::Blob { bytes, mime });
                Ok(id)
            },
        );
        methods.add_method(
            "toast",
            |_, this, (level, message, sub): (Value, Value, Value)| {
                this.state.push_effect(
                    EffectTarget::Pane(this.pane),
                    Effect::Toast {
                        level: ToastLevel::from_host(&display(&level)),
                        text: text(&message),
                        sub: (!sub.is_nil()).then(|| display(&sub)),
                    },
                );
                Ok(())
            },
        );
        methods.add_method("open", |_, this, target: Value| {
            this.state
                .push_effect(EffectTarget::Pane(this.pane), Effect::Open(text(&target)));
            Ok(())
        });
        methods.add_method("copy", |_, this, value: Value| {
            this.state
                .push_effect(EffectTarget::Pane(this.pane), Effect::Copy(text(&value)));
            Ok(())
        });
    }
}
