//! FoxHole core: the domain model, application state machine, persistent
//! configuration, and durable-storage primitives.
//!
//! Deliberately free of any async runtime, terminal, or networking dependency —
//! just the logic — so it builds fast and stays unit-testable. The `ui`/`splash`
//! rendering lives in `foxhole-tui`; the live LXMF/Reticulum stack in the binary.

// Re-exported so `crate::micron::…` keeps resolving inside the domain model and
// the Browser state (which store and parse micron page elements).
pub use foxhole_micron as micron;

pub mod app;
pub mod burn;
pub mod config;
pub mod domain;
pub mod mnemonic;
pub mod storage;
