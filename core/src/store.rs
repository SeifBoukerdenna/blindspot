//! Persistence for the frecency store.
//!
//! Empty at M1 by design. CLAUDE.md defers the redb-vs-rusqlite choice to M3, when
//! there is an actual access pattern to judge it against; picking now would be
//! guessing. `bs_activate` exists as a no-op so that landing this module is a
//! pure-Rust change with no FFI or Swift churn.
