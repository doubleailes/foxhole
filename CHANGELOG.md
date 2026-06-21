# Changelog

All notable changes to FoxHole are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-21

First public release — the keyboard-only, monochrome-legible LXMF comms
terminal. **Experimental.**

### Added

- **Operator console (TUI).** Two-tier layout — tools (Conversations, Network,
  Map, Browser, Log, Interfaces, Notes, Guide) and panes within each — over a
  truecolor tactical theme that stays legible stripped to one bit (focus reads
  via box-drawing weight and bold nameplates). Cold-boot bring-up splash that
  doubles as a live readiness monitor.
- **Encrypted LXMF/Reticulum messaging** (`--features net`): identity bring-up,
  peer/propagation-node discovery, and the `DIRECT → PROPAGATED → OPPORTUNISTIC`
  delivery cascade with per-message disposition tracking. Propagation sync is
  operator-initiated only. Off by default; the offline build ships seeded demo
  contacts and zones.
- **At-rest encryption.** Per-conversation history (`FXC1`) and the received
  intel layer (`FXI1`) sealed with AES-256-CBC + HMAC, written atomically,
  keyed off the operator identity.
- **Emergency destruction (BURN).** `Ctrl+K` → `BURN` zero-overwrites and
  unlinks all session data, then exits.
- **World Map.** Pan/zoom tactical display of the operator's fix, position-
  broadcasting peers, an embedded capitals/major-cities gazetteer, and shared
  intel; MGRS readout, go-to-grid, and designate-by-grid.
- **Intel sharing over the mesh (CoT).** A Cursor-on-Target subset carried in
  LXMF messages — affiliation-tinted markers and circular hazard zones — with
  trust-gated ingest, staged review of unvetted reports, share/revoke of a
  local zone, in-app authoring, and encrypted persistence.
- **Operator-assigned peer trust** (`TRUSTED`/`UNKNOWN`/`UNTRUSTED`/
  `COMPROMISED`), persisted, gating inbound intel.
- **Spoken-word addressing.** 32-hex destination hashes render as a 12-word
  mnemonic phrase (with checksum) for read-aloud verification.
- **Nomad Network browser.** Reads discovered `nomadnetwork.node` stations and
  renders `micron` page markup, including link/field navigation and form submit.
- **Scratch buffer.** A ten-slot persistent note pad, burn-wiped with the rest.

### Packaging

- Licensed **AGPL-3.0-or-later** across the workspace (`LICENSE`).
- Cargo metadata + MSRV (Rust 1.88, edition 2024) declared via
  `[workspace.package]`.
- CI: rustfmt, clippy (`-D warnings`), and tests across the offline and `net`
  configurations, plus an MSRV build gate.
- Security policy and threat model (`SECURITY.md`).

### Known limitations

- **DIRECT delivery is not acknowledged** with the pinned `rsReticulum`
  revision (a deliverability gap, not a confidentiality one) — see
  `docs/rsreticulum-delivery-proof-issue.md`.
- Ratchets, delivery-proof surfacing, the `cot/proto` transport, and a TAK
  gateway are not yet built — see `docs/lxmf-integration.md` and
  `docs/intel-sharing.md` §13.

[Unreleased]: https://github.com/doubleailes/foxhole/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/doubleailes/foxhole/releases/tag/v0.1.0
