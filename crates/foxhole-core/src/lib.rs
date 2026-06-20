//! FoxHole core: the domain model, application state machine, persistent
//! configuration, and durable-storage primitives.
//!
//! Deliberately free of any async runtime, terminal, or networking dependency —
//! just the logic — so it builds fast and stays unit-testable. The `ui`/`splash`
//! rendering lives in `foxhole-tui`; the live LXMF/Reticulum stack in the binary.

// Re-exported so `crate::micron::…` keeps resolving inside the domain model and
// the Browser state (which store and parse micron page elements).
pub use foxhole_micron as micron;

// The World Map feature's logic and data live in their own dependency-free crate
// (`foxhole-map`). Re-exported here so `crate::cities`/`crate::zones` keep
// resolving internally and `foxhole_core::{cities,zones}` stays a stable public
// path for the binary; the geo/view types are re-exported via `domain`/`app`.
pub use foxhole_map::{cities, zones};

pub mod app;
pub mod burn;
pub mod config;
pub mod domain;
pub mod mnemonic;
pub mod notes;
pub mod storage;
