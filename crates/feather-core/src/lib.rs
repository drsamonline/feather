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
//! * [`tile_plan`] — how a page becomes a grid of tiles, so a 20,000-pixel
//!   scan costs the same as a letter-sized page.
//! * [`session`] — which documents are open and which one you are reading.
//! * [`text_layer`] — where the words are, so a click can become a position.

pub mod document;
pub mod error;
pub mod format;
pub mod layout;
pub mod natural;
pub mod session;
pub mod tile;
pub mod text_layer;
pub mod tile_plan;

pub use document::{Document, PageSize, RenderedPage};
pub use error::DocumentError;
pub use format::{detect, Format};
pub use layout::{level_capped, level_for_scale, scale_for_level, Layout};
pub use natural::natural_cmp;
pub use session::{Session, Tab};
pub use tile::{Tile, TileCache, TileKey};
pub use text_layer::{TextLayer, Word};
pub use tile_plan::TileGrid;
