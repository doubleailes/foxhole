# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status

`foxhole` is an off-grid, keyboard-only, monochrome LXMF comms terminal (TUI),
Rust edition 2024. The UI shell is built; live networking is being wired in.

## Architecture

A Cargo **workspace** splits the program into layers by dependency weight: the
logic and rendering are dependency-light member crates under `crates/`, while the
async runtime and the live protocol stack stay in the root binary. The boundary
is compiler-enforced — `foxhole-core` *cannot* reach for tokio/ratatui/reticulum
because they aren't in its manifest.

### `crates/foxhole-core` — domain model + state machine (logic only)

Depends only on `crossterm` (key enums) and `foxhole-micron`. No async runtime,
terminal, or networking. Fast to build, fully unit-tested.

- `src/domain/` — the shared model every layer agrees on: `Conversation`,
  `Entry`, `MsgStatus`; the UI↔network events/commands (`NetEvent`,
  `NetCommand`, `Outbound`, `PeerKind`); the Network/Browser registries (`Node`,
  `PathProbe`, `NomadNode`, `Page`). Carries no UI focus/navigation semantics.
- `src/app/` — all state and key routing (`App`). Two focus tiers mirror
  Nomadnet: top-level **tools** (tabs: Conversations / Network / Browser / Log /
  Interfaces / Guide, switched with Ctrl+N/Ctrl+P) and **panes** within a tool
  (PeerList / Thread / Transmit, cycled with Tab). The struct + program-global
  key routing + modals live in `mod.rs`; per-tool behaviour is split into
  sibling `impl App` blocks (`conversations.rs`, `network.rs`, `browser.rs`) and
  the cold-boot/scroll machinery into `boot.rs`. Free of I/O and rendering.
- `src/config.rs` — persistent `key = value` settings (no serde/TOML);
  `config_dir()` (overridable via `FOXHOLE_CONFIG_DIR`).
- `src/storage.rs` — `atomic_write` (write-temp → fsync → rename) for durable state.
- `src/burn.rs` — emergency data destruction (Ctrl+K → type `BURN`). `execute(dir)`
  zero-overwrites + `fsync`es + unlinks every file under `config_dir()`, then
  removes the tree; `main` runs it after the loop and `process::exit`s. Pure
  `std::fs`, always compiled, unit-tested. (Best-effort vs FS forensics; the real
  guarantee is the destroyed identity key making the stores undecryptable.)

### `crates/foxhole-micron` — micron renderer (ratatui only)

Pure renderer for Nomad Network **micron** page markup, mirroring NomadNet's
`MicronParser.py` (NomadNet-dark-theme heading bars, section indent, dividers,
bold/italic/underline, `` `F``/`` `B`` colours, alignment, escapes, literal
blocks). `render(&str, width, focus, &values)` draws the page (highlighting the
focused element, filling text fields from `values`); `elements(&str)` lists the
focusable `Element`s (links + text fields) the Browser navigates/submits. Unknown
tags stripped, never fatal; unit-tested. Standalone (reusable by other NomadNet
tooling).

### `crates/foxhole-tui` — rendering (ratatui), pure `&App` → frame

Depends on `foxhole-core` + `foxhole-micron`. **Truecolor tactical theme over
Unicode box-drawing** — a dark field-night surface (`style::BG`, painted under the
whole frame and shared by the boot splash) with phosphor-green panels: resting
panes use the heavy `FRAME_BORDER` (`┏━┓┃┗┛`) with a dim border, the focused pane
the double-ruled `FOCUS_BORDER` (`╔═╗║╚╝`) with a lit border + ink-on-green
nameplate, a `▶` selection chevron, brass callsign/active-tab keys, and
instrument-cluster status chips. This targets a modern UTF-8 + 24-bit terminal
(Raspberry Pi OS Bookworm's default), trading the former strict 7-bit ASCII
chrome; colour only reinforces hierarchy, so focus still reads stripped to mono
via border weight + bold/`REVERSED` nameplates. Scrollback *content* is tinted by
`style::tag_style` (RX/TX/DLV/LNK/RT/CFG/WRN/ERR/…, muted timestamps). The frame
helper (`tactical_block`) carries an optional right-corner HUD readout — scroll
position (`12–34/80`), roster counts, a `◆LIVE◆` focus stamp; overflowing scroll
panes get a colour-graded `▲█┊▼` scrollbar on the right border, the Network roster
a colour-graded `▰▰▱▱` hop-count signal meter (green→amber→red). `src/ui/` is split
into a shared toolkit (`style.rs`, `widgets.rs`), chrome (`chrome.rs`), overlays
(`popups.rs`), and one body module per tool.
`src/splash.rs` *(default-on `splash` feature)* renders the cold-boot bring-up
monitor; state lives in core's `App` (`AppState::{Splash,Running}`,
`BootStep`/`Boot`), advanced by `main`'s 120 ms tick and folded from real
readiness events via `mark_boot`. `cfg(test)` and `FOXHOLE_NO_SPLASH` start in
`Running`.

### `foxhole` (root binary) — runtime + protocol wiring

- `src/main.rs` — terminal lifecycle (raw mode, alt screen, panic-safe restore)
  and the single async `select!` event loop multiplexing keyboard input and
  inbound network events. Holds no UI or state rules. Re-exports the member
  crates under `crate::app`/`crate::config`/`crate::storage`/`crate::burn` so the
  networking modules below read unchanged.
- `src/store.rs` — *(`net` feature)* encrypted, atomic, per-conversation history
  store: `FXC1` blob → `rns_crypto::token` (AES-256-CBC + HMAC) → `atomic_write`,
  key HKDF-derived from the identity. Corruption/foreign files are skipped on load.
- `src/net.rs` — *(in progress, behind the `net` feature)* live LXMF/Reticulum
  stack: identity, `ReticulumHandle`, `LxmRouter`, announce/delivery tasks. Also
  Nomad Network node discovery (recent-announce-cache poll for
  `nomadnetwork.node`) and page fetching via `LinkClient::query` (spawned off the
  select loop), reported as `NetEvent::{NomadNode,Page}`.

## Networking (the `net` feature)

Off by default (the build stays dependency-light and offline, with seeded demo
peers). `--features net` pulls the `rns-*` (Reticulum) and `lxmf-core` crates as
**git deps pinned by commit** from `github.com/doubleailes/rsReticulum` and
`…/rsLXMF` (both AGPL-3.0-or-later). `rsLXMF`'s own `rns-*` deps use sibling-path
references, so a root `[patch."…/rsLXMF"]` redirects them to the same pinned
`rsReticulum` revision (cargo unifies the stack on one source). Bump by editing
the `rev`s (and the matching `[patch]` revs) in `Cargo.toml`. The integration
mirrors the Ratspeak reference client — see `docs/lxmf-integration.md` for the
full binding.

## Commands

The `splash`/`net` features are declared on the root binary and forwarded to the
member crates, so drive everything from the workspace root.

- Build: `cargo build` (release: `cargo build --release`)
- Build with networking: `cargo build --features net` (fetches the pinned `rsReticulum`/`rsLXMF` git revs)
- Run: `cargo run` (or `cargo run --features net`)
- Test: `cargo test --workspace` (single test: `cargo test <name>`)
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all`
