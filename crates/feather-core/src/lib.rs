//! Feather core — the parts that know nothing about pixels on screen.
//!
//! Three things live here and nothing else:
//!
//! * [`document`] — the `Document` trait every format implements, so the UI
//!   never learns what a PDF is.
//! * [`format`] — the router: extension and magic bytes to a [`Format`], so a
//!   new format costs one enum variant and one arm.
//! * [`layout`] — the scroll model: where pages sit and which ones are on
//!   screen, as pure arithmetic with no rendering in it.
//! * [`tile`] — a byte-budgeted LRU cache, because the whole speed story is
//!   "render only what is on screen, and remember what you just drew".

pub mod document;
pub mod error;
pub mod format;
pub mod layout;
pub mod tile;

pub use document::{Document, PageSize, RenderedPage};
pub use error::DocumentError;
pub use format::{detect, Format};
pub use layout::Layout;
pub use tile::{Tile, TileCache, TileKey};
