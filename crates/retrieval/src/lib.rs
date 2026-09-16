//! Retrieval decisions with no storage attached: how a file's text becomes the passages search
//! returns, and (as later phases land) how candidates are ranked.
//!
//! `blindspot_core` owns the index, SQLite and the helpers; it calls in here for the parts that
//! are pure functions of text. Keeping them apart means chunking and ranking can be tested and
//! measured in isolation, which is what the search benchmark needs.

mod chunk;
mod quantize;
mod rank;

pub use chunk::{Chunk, Chunked, Kind, Limits, Location, chunks};
pub use quantize::{quantize, dequantize};
pub use rank::{Ranked, fuse_ranks, substantially_overlaps};
