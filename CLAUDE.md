# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status

`foxhole` is an off-grid, keyboard-only, monochrome LXMF comms terminal (TUI),
Rust edition 2024. The UI shell is built; live networking is being wired in.

## Architecture

A Cargo **workspace** splits the program into layers by dependency weight: the
logic and rendering are dependency-light member crates under `crates/`, the heavy
async/live-protocol stack is the optional `foxhole-net` crate, and the root binary
only wires them to the tokio runtime. The boundary is compiler-enforced —
`foxhole-core` *cannot* reach for tokio/ratatui/reticulum because they aren't in
its manifest.

### `crates/foxhole-core` — domain model + state machine (logic only)

Depends only on `crossterm` (key enums) and `foxhole-micron`. No async runtime,
terminal, or networking. Fast to build, fully unit-tested.

- `src/domain/` — the shared model every layer agrees on: `Conversation`,
  `Entry`, `MsgStatus`; the UI↔network events/commands (`NetEvent`,
  `NetCommand`, `Outbound`, `PeerKind`); the Network/Browser registries (`Node`,
  `PathProbe`, `NomadNode`, `Page`). The geographic types `GeoPos`/`Zone` are
  re-exported here from `foxhole-map`. Carries no UI focus/navigation semantics.
- `src/app/` — all state and key routing (`App`). Two focus tiers mirror
  Nomadnet: top-level **tools** (tabs: Conversations / Network / Map / Browser /
  Log / Interfaces / Notes / Guide, switched with Ctrl+N/Ctrl+P) and **panes**
  within a tool (PeerList / Thread / Transmit, cycled with Tab). The struct +
  program-global key routing + modals live in `mod.rs`; per-tool behaviour is
  split into sibling `impl App` blocks (`conversations.rs`, `network.rs`,
  `browser.rs`, `map.rs`, `intel.rs`) and the cold-boot/scroll machinery into
  `boot.rs`. Free of I/O and rendering. `map.rs` is only the App-level *binding*
  for the World Map — deriving markers from peer telemetry/intel and routing keys
  to `MapView`; the geometry/data live in `foxhole-map`. `intel.rs` is the
  **received-intel layer** (P2 of the intel-sharing plan): `apply_cot` folds a
  decoded `CotEvent` in with trust gating (Trusted→live, Unknown/Untrusted→staged
  for review, Compromised→dropped), newest-`(source,uid)`-wins upsert, revocation,
  and a `sweep_intel` stale sweep (default TTL from config). The incoming-intel
  review modal accepts/discards staged events; `share_zone` (P3) produces a
  `u-d-c-c` CoT event from a local `zones.conf` zone and enqueues it (with a
  summary body) for a peer, and `revoke_shared_zone` (P4) sends a `stale==time`
  revocation (same deterministic uid) so the peer's `apply_cot` revoke path drops
  it. In-app authoring (P4, `AuthorForm`) places/edits markers & zones of any
  affiliation into the live intel layer (map keys `a`/`e`), and
  `remove_selected_intel` (`x`) drops the selected object locally — so a received
  report can be cleared without a network round-trip.
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

### `crates/foxhole-cot` — CoT (Cursor-on-Target) codec (dependency-free)

Pure, `std`-only codec for foxhole's **intel-sharing** wire format: a subset of
CoT, the open ATAK/TAK situational-awareness event model, carried inside LXMF
messages (see `docs/intel-sharing.md`). `parse(&str) -> CotEvent` decodes one CoT
`<event>` (markers + circular hazard zones) **leniently** (unknown
tags/attributes ignored) and **safely** — a hand-rolled hardened XML subset
reader rejects DOCTYPE/ENTITY (no XXE) and bounds size/depth/text; `CotEvent::{to_xml,
summary}` generate the standard event + human one-liner, and `CotEvent::{marker,
zone}` are the producer side (a `Zone` becomes a `u-d-c-c`). `Affiliation`/`Kind`
read the `type` for the TUI tint/glyph + map layer. No XML/date crates (ISO-8601
↔ epoch is in-house); fully unit-tested, standalone. This is **P1** of the
intel-sharing plan; **P2** (ingest + render) is wired: `net.rs` decodes the
`cot/xml` custom field → `NetEvent::Cot` → `foxhole-core`'s `app/intel.rs`
applies it, and `foxhole-tui`'s map renders the affiliation-tinted layer + INTEL
panel. **P3** (share) is wired too: Ctrl+G in Conversations shares a local zone
as a `cot/xml` LXMF message (`net.rs` `build_message` attaches the custom
fields). **P4** is under way: received intel now persists across restarts
(`foxhole-net/src/intel_store.rs`, encrypted); the protobuf transport
(`cot/proto`) and a TAK gateway remain. `tools/cot_inject.py` is the reference injector (Appendix A) for
live ingest + decoder fixtures.

### `crates/foxhole-map` — World Map domain (pure logic + data)

The whole map feature's logic and data, extracted into a standalone crate whose
**only dependency is the dependency-free `foxhole-cot`** (for `Affiliation`,
which tints intel markers) — so it builds fast and is fully unit-tested in
isolation. `geo` (`GeoPos` + `wrap_lon`), `view` (the `MapView` pan/zoom
viewport — all geometry/limits/antimeridian projection behind intent-named
methods like `pan_east`/`zoom_in`/`frame_on` — plus `MapMarker`/`MarkerKind`,
including the intel-tinted `MarkerKind::Intel`), `zones` (`Zone` + the
`parse`/`demo` hazard-area overlay), `cities` (the embedded `CITIES`
capitals/major-cities gazetteer with zoom-staged `label_span`s), and `mgrs` (a
dependency-free Military Grid Reference System codec — `format(GeoPos, digits)` ↔
`parse(&str) -> GeoPos`, via WGS-84 UTM, so the operator can reframe the map onto
or designate a position by a grid reference). Knows nothing of `App`, the
terminal, or networking: `foxhole-core` owns the field state, routes keys to
`MapView`'s methods, and builds the marker list from peer telemetry and the intel
layer; `foxhole-tui` draws it. The Map binds MGRS in two ways: a "go to MGRS"
modal (`/`, `app/map.rs`'s `GotoMgrs`) reframes the view on a typed reference, and
the intel author form carries an MGRS field two-way synced with its lat/lon; the
canvas HUD reads the viewport centre out as MGRS.

### `crates/foxhole-tui` — rendering (ratatui), pure `&App` → frame

Depends on `foxhole-core` + `foxhole-micron` + `foxhole-map` (the map body draws
the latter's `MapView`/markers/zones/cities). **Truecolor tactical theme over
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

### `crates/foxhole-net` — live networking layer (`net` feature)

The whole live-protocol layer, extracted into a member crate so the heavy async
stack (tokio + the `rns-*` Reticulum crates + `lxmf-core`) stays off the
dependency-light logic/rendering crates. Built **only** when the binary's `net`
feature pulls it in (an optional dep), so the default build stays offline. Depends
on `foxhole-core` (domain model + `storage::atomic_write`, with its `net` feature)
and `foxhole-cot` (inbound intel decode). Three modules:

- `src/net.rs` — *(in progress)* live LXMF/Reticulum stack: identity,
  `ReticulumHandle`, `LxmRouter`, announce/delivery tasks. Also Nomad Network node
  discovery (recent-announce-cache poll for `nomadnetwork.node`) and page fetching
  via `LinkClient::query` (spawned off the select loop), reported as
  `NetEvent::{NomadNode,Page}`. Inbound CoT intel is decoded from the
  `FIELD_CUSTOM_TYPE=cot/xml` / `FIELD_CUSTOM_DATA` fields and reported as
  `NetEvent::Cot` (malformed payloads logged + dropped, never fatal).
- `src/store.rs` — encrypted, atomic, per-conversation history store: `FXC1` blob
  → `rns_crypto::token` (AES-256-CBC + HMAC) → `atomic_write`, key HKDF-derived
  from the identity (`derive_key`, also used by `net`). Corruption/foreign files
  are skipped on load.
- `src/intel_store.rs` — the same encrypted/atomic recipe for the received-intel
  layer (P4 durability): one `FXI1` blob holding the live + staged `IntelRecord`s
  (reusing the identity store key), loaded at boot and re-saved when
  `app.intel_dirty` is set. `Option` timestamps are preserved so a stale-less
  event reloads stale-less; a corrupt/foreign file loads empty.

### `foxhole` (root binary) — runtime wiring

- `src/main.rs` — terminal lifecycle (raw mode, alt screen, panic-safe restore)
  and the single async `select!` event loop multiplexing keyboard input and
  inbound network events. Holds no UI or state rules. Re-exports the member crates
  under `crate::app`/`crate::config`/`crate::burn` and, under `net`, imports
  `foxhole_net::{net, store, intel_store}` so its call sites read unchanged.

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
