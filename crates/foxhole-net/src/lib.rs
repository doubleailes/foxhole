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

pub mod intel_store;
pub mod net;
pub mod store;
