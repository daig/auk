//! The UI engine: an arena DOM with incremental mutation, CSS parsed with
//! `cssparser` and matched with `selectors`, our own cascade enforcing Tern's accept/reject rules,
//! layout through Taffy (block, flex, grid) and Parley (inline formatting), our own tables,
//! absolutely positioned boxes, virtualized lists and scroll anchoring, replaced boxes (images),
//! and painting into a Vello scene.

mod anim;
pub mod css;
mod document;
pub mod dom;
pub mod image;
pub mod layout;
pub mod paint;
pub mod style;
pub mod text;
mod ua;

pub use css::cascade::{SheetId, SheetKind};
pub use document::{Document, UpdateStats};
pub use dom::{ElementState, NodeId, Rect};
pub use image::ImageContent;
pub use paint::{hit_test, paint};
pub use text::Fonts;
