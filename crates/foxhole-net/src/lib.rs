//! FoxHole's live networking layer — the LXMF/Reticulum protocol stack plus the
//! encrypted on-disk stores it feeds.
//!
//! This crate carries the heavy async/protocol dependencies (tokio + the
//! `rns-*` Reticulum crates + `lxmf-core`) so they stay off the
//! dependency-light logic/rendering crates. The root binary pulls it in only
//! under its `net` feature; everything here is wiring for the live stack:
//!
//! - [`net`] — identity, Reticulum handle, LXMF router, announce/delivery
//!   tasks, Nomad Network discovery + page fetch, and inbound CoT intel decode.
//! - [`store`] — encrypted, atomic, per-conversation history store.
//! - [`intel_store`] — encrypted, atomic persistence for the received-intel
//!   layer (live + staged records).
//! - [`trace`] — opt-in capture of the stack's own `tracing` diagnostics, so
//!   link/delivery-proof faults are visible instead of silent.
//!
//! Both stores share their framing and their encrypt-then-atomically-write
//! envelope through the internal `wire` module, so they cannot drift apart on
//! what a durable — or a corrupt — file means.

pub mod intel_store;
pub mod net;
pub mod store;
pub mod trace;
mod wire;
