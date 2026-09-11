//! blindspot core — indexing, matching and ranking for the launcher.
//!
//! The C ABI lives in [`ffi`]; everything else is a normal Rust API so it can be
//! unit-tested and benchmarked through the `rlib` target.

pub mod calc;
pub mod clips;
pub mod config;
pub mod ffi;
pub mod files;
pub mod frecency;
pub mod index;
// `match` is a keyword; the file is named `match.rs` to match the layout in CLAUDE.md.
#[path = "match.rs"]
pub mod matching;
pub mod relevance;
pub mod store;

pub use config::Config;
pub use frecency::{Frecency, Visit};
pub use index::{AppEntry, Index};
pub use matching::{Ranked, Ranker};
