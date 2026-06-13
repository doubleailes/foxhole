# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status

`foxhole` is an off-grid, keyboard-only, monochrome LXMF comms terminal (TUI),
Rust edition 2024. The UI shell is built; live networking is being wired in.

## Architecture

A clean three-way split keeps the render path trivial and the logic testable:

- `src/main.rs` — terminal lifecycle (raw mode, alt screen, panic-safe restore)
  and the single async `select!` event loop multiplexing keyboard input and
  inbound network events. Holds no UI or state rules.
- `src/app.rs` — all state and key routing (`App`). Two focus tiers mirror
  Nomadnet: top-level **tools** (tabs: Conversations / Network / Log /
  Interfaces / Guide, switched with Ctrl+N/Ctrl+P) and **panes** within a tool
  (PeerList / Thread / Transmit, cycled with Tab). Conversations are per-peer
  (`Conversation` with its own message scrollback + draft + unread count).
  Free of I/O and rendering; unit-tested.
- `src/ui.rs` — pure `&App` → frame rendering. **7-bit ASCII borders only**
  (`ASCII_BORDER`); structure (borders, active-pane `REVERSED`, titles) stays
  glyph-only so it degrades on a mono display, while scrollback *content* is
  tinted by a tactical palette (`tag_style`: RX/TX/DLV/LNK/RT/CFG/WRN/ERR/…,
  muted timestamps).
- `src/splash.rs` — *(default-on `splash` feature)* pure renderer for the
  cold-boot bring-up monitor (text only, no image). State lives in `App`
  (`AppState::{Splash,Running}`, `BootStep`/`Boot`); `main` advances it on a
  120 ms `select!` tick gated on `state == Splash`, and folds real readiness
  events (`StoreKey`, `Local`, transport/identity banners) into `mark_boot` so
  lines flip live and the console opens when the address is up. `cfg(test)` and
  `FOXHOLE_NO_SPLASH` start in `Running`.
- `src/storage.rs` — `atomic_write` (write-temp → fsync → rename) for durable
  state.
- `src/store.rs` — *(`net` feature)* encrypted, atomic, per-conversation history
  store: `FXC1` blob → `rns_crypto::token` (AES-256-CBC + HMAC) → `atomic_write`,
  key HKDF-derived from the identity. Corruption/foreign files are skipped on load.
- `src/net.rs` — *(in progress, behind the `net` feature)* live LXMF/Reticulum
  stack: identity, `ReticulumHandle`, `LxmRouter`, announce/delivery tasks.

## Networking (the `net` feature)

Off by default (the build stays dependency-light and offline, with seeded demo
peers). `--features net` pulls the `rns-*` (Reticulum) and `lxmf-core` crates as
**path deps from sibling checkouts** `../rsReticulum` and `../rsLXMF` (both must
sit next to this repo; both are AGPL-3.0-or-later). The integration mirrors the
Ratspeak reference client — see `docs/lxmf-integration.md` for the full binding.

## Commands

- Build: `cargo build` (release: `cargo build --release`)
- Build with networking: `cargo build --features net` (needs `../rsReticulum` + `../rsLXMF`)
- Run: `cargo run` (or `cargo run --features net`)
- Test: `cargo test` (single test: `cargo test <name>`)
- Lint: `cargo clippy --all-targets -- -D warnings`
- Format: `cargo fmt`
