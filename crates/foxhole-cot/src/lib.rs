//! `foxhole-cot` — the CoT (Cursor-on-Target) subset codec for foxhole's intel
//! sharing layer.
//!
//! Foxhole shares operator-authored intel (markers and hazard zones) as **CoT
//! events carried inside LXMF messages** over Reticulum, rather than a bespoke
//! schema — adopting the open ATAK/TAK situational-awareness format so the wire
//! is standard and a TAK bridge stays possible later. The full rationale and
//! wire framing live in `docs/intel-sharing.md`; this crate is **P1** of that
//! plan: the pure, dependency-free, exhaustively-tested codec for the subset
//! foxhole actually renders.
//!
//! # What it does
//!
//! - [`parse`] decodes one CoT `<event>` from XML into a [`CotEvent`], leniently
//!   (unknown tags/attributes ignored) and **safely** — the reader rejects
//!   DOCTYPE/ENTITY declarations (no XXE) and bounds size, depth and text length
//!   ([design note §9]).
//! - [`CotEvent::to_xml`] generates a standards-shaped event for sharing, and
//!   [`CotEvent::summary`] the human-readable one-liner for the LXMF body.
//! - [`CotEvent::marker`] / [`CotEvent::zone`] are the producer constructors —
//!   the latter is how a foxhole `Zone` becomes a `u-d-c-c` event.
//! - [`Affiliation`] / [`Kind`] interpret the `type` for the TUI (tint/glyph and
//!   map layer).
//!
//! Everything here is `std`-only and side-effect-free; the LXMF transport
//! binding (`FIELD_CUSTOM_TYPE`/`FIELD_CUSTOM_DATA`) and ingest/render live in
//! the binary's `net` layer and `foxhole-core` (P2+).
//!
//! [design note §9]: ../docs/intel-sharing.md

mod affiliation;
mod event;
mod iso8601;
mod xml;

pub use affiliation::{Affiliation, Kind};
pub use event::{CotError, CotEvent, Point, parse};
pub use xml::XmlError;

/// The `FIELD_CUSTOM_TYPE` (`0xFB`) content tag identifying a CoT-XML payload —
/// what a receiver matches before handing the `FIELD_CUSTOM_DATA` bytes to
/// [`parse`] (design note §5).
pub const CONTENT_TAG_XML: &str = "cot/xml";
