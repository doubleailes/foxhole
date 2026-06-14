//! FoxHole terminal UI: pure `&App` → frame rendering, plus the cold-boot
//! splash. Depends only on `foxhole-core` (the state it draws) and
//! `foxhole-micron` (page rendering) — no async runtime or networking.

// Re-exported so the render modules' `crate::app::…` / `crate::notes::…` /
// `crate::micron::…` paths keep resolving against the core/micron crates.
pub use foxhole_core::app;
pub use foxhole_core::notes;
pub use foxhole_micron as micron;

#[cfg(feature = "splash")]
pub mod splash;
pub mod ui;
