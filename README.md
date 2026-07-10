<!--
  ████████████████████████████████████████████████████████████████████
  THIS DOCUMENT HAS BEEN DECLASSIFIED IN PART UNDER DIRECTIVE ██-████-██
  REMAINING REDACTIONS WITHHELD PURSUANT TO EXEMPTION (b)(█) AND (b)(██)
  ████████████████████████████████████████████████████████████████████
-->

```
        ████████  ██████  ██   ██ ██   ██  ██████  ██      ███████
        ██       ██    ██  ██ ██  ██   ██ ██    ██ ██      ██
        █████    ██    ██   ███   ███████ ██    ██ ██      █████
        ██       ██    ██  ██ ██  ██   ██ ██    ██ ██      ██
        ██        ██████  ██   ██ ██   ██  ██████  ███████ ███████
```

# PROJECT **FOXHOLE**

> CLASSIFICATION: ~~TOP SECRET // ████████ // NOFORN~~ **DECLASSIFIED (PARTIAL)**
> PROGRAM DESIGNATION: `FOX-█████`  ·  COMPARTMENT: ████████  ·  COPY ██ OF ███

A field-expedient, keyboard-only, ████████-tolerant **LXMF comms terminal**
for environments where the ████████ has gone dark and the ████████████ is no
longer ███████████████. No mouse. No cloud. No ██████████.

Built in Rust (edition 2024). Runs offline by default; the live mesh stack is
armed only when ██████████ requires it.

---

## 1. MISSION STATEMENT  ▒▒ REDACTED ▒▒

> *"In the event of ████████████████████████, designated **operators** shall
> maintain encrypted text contact across ████████████ using whatever ██████
> remains. The terminal SHALL NOT ████████████, SHALL NOT phone ████████, and
> SHALL fit on hardware recovered from ██████████████████████████."*
>
> — Memo `███-77`, declassified ██/██/████, signature ████████████ (redacted)

The remainder of Section 1 is withheld under exemption (b)(█).
████████████████████████████████████████████████████████████████████████████
████████████████████████████████████████████████████████████████████████████

---

## 2. CAPABILITIES  ▓ APPROVED FOR RELEASE ▓

[conversation interface](./images/conversation.png)

- **Encrypted text traffic over Reticulum/LXMF** — peer-to-peer, no central
  ████████, no operator account, no ███████████.
- **Tiered delivery doctrine:** `DIRECT → PROPAGATED → OPPORTUNISTIC`. The
  terminal attempts a confirmed link first, falls back to a store-and-forward
  ████████ node, and finally to a single fire-and-forget packet.
- **At-rest encryption.** Every conversation is sealed (AES-256-CBC + HMAC),
  written ████████ically, keyed off the operator's own cryptographic
  identity. See the CRYPTOGRAPHIC ANNEX, ████ of which remains classified.
- **Per-message disposition tracking** — `[sending]`, `[sent]`, `[delivered]`,
  `[propagated]`, `[failed]` — so the operator knows the fate of a transmission
  without ████████████████.
- **Zero unsolicited emissions.** Propagation sync is **operator-initiated
  only**; the terminal does not beacon on its own ████████████.
- **Truecolor tactical HUD** — a dark field-night console with phosphor-green
  panels, lit nameplates on the live pane, brass callsign keys, instrument-cluster
  status gauges and colour-graded signal meters. Tuned for a modern terminal
  (Raspberry Pi OS Bookworm and friends); colour only reinforces — it stays
  legible stripped to one bit on ██████ recovered from ████████████.
- **World map situational display.** A pan/zoom tactical map plots the
  operator's own fix, peers broadcasting position (Sideband-style LXMF location
  telemetry), an embedded capitals/major-cities gazetteer, and shared intel.
  Reads the viewport centre out as an **MGRS** grid reference; reframe onto a
  typed reference, or designate a position by grid. Hazard zones overlay as
  circular keep-out rings from a local `████████.conf`.
- **Intel sharing over the mesh (CoT).** A `████████`-on-Target subset rides
  inside LXMF messages — markers and circular hazard zones, tinted by
  affiliation (friendly / hostile / neutral / unknown). Inbound reports from
  vetted peers post live; unvetted ones are **staged for operator review**
  before they touch the map. Share a local zone to a peer with one key, and
  **revoke** it later so the peer's map drops the object. Authored in-app or
  ingested off the wire; the received layer is sealed at rest like history.
- **Operator-assigned trust.** Every peer carries a trust level
  (`TRUSTED / UNKNOWN / UNTRUSTED / COMPROMISED`), shown as a colour-coded glyph
  on its roster row and persisted across sessions. Trust gates whether inbound
  intel posts live, is staged, or is dropped.
- **Spoken-word addressing.** A 32-hex destination hash renders as a 12-word
  mnemonic phrase (with checksum word) you can read aloud over a radio to verify
  or share; the New Conversation prompt accepts either form.
- **Scratch buffer.** A ten-slot note pad for stashing a hash, a grid reference,
  or a frequency without copy/paste; persists across restarts and is destroyed
  by a BURN with everything else.

Capabilities ██ through ██ are withheld. ████████████████████████████████████

[map](./images/map.png)


---

## 3. FIELD DEPLOYMENT

> WARNING: Handling instructions for ███████████ hardware appear in Annex ██,
> not reproduced here.

**Prerequisites.** A Rust toolchain ≥ **1.88** (edition 2024). The offline
build needs nothing else; the `net` build fetches its mesh stack from git on
first compile, so that one build needs network reachability to GitHub.

**Standalone (offline / bench):**
```sh
git clone https://github.com/doubleailes/foxhole && cd foxhole
cargo build              # arm the terminal, dependency-light, fully offline
cargo run                # boot the operator console (seeded demo contacts)
```
The offline console boots with a small set of **seeded demo contacts and
zones** so the UI is explorable on the bench — no live traffic moves until the
`net` compartment is armed.

**Live mesh (the `net` compartment):**
```sh
cargo build --features net
cargo run   --features net
```
The `net` compartment links the **Reticulum** + **LXMF** stack (the `rns-*` and
`lxmf-core` crates) directly from their upstream repositories, **pinned by
commit** in `Cargo.toml` for a reproducible build — cargo fetches them on the
first `--features net` build; no sibling checkout is required. Bump the stack by
editing the `rev`s in `Cargo.toml`. (Both upstreams are AGPL-3.0-or-later, which
is why a distributed `net` binary is a combined AGPL work — see Section 7 and
`LICENSE`.)

| Directive            | Command                                          |
|----------------------|--------------------------------------------------|
| Range-check (tests)  | `cargo test` · `cargo test --features net`       |
| Inspection (lint)    | `cargo clippy --all-targets -- -D warnings`      |
| Dress uniform (fmt)  | `cargo fmt`                                       |

Egress to the wider mesh is configured via `FOXHOLE_HUB=host:port`, or by
hand-editing the interface manifest under `~/.config/foxhole/████████`. The
former entry hub at ████████████████████████████`:4965` has been
**decommissioned**; current ingress points are ████████████████.

**Cold-boot sequence.** On launch the terminal runs a brief ASCII bring-up
monitor — encrypted store, identity keys, network interface, mesh stack — that
reports each subsystem's status as it actually comes up, then drops into the
console the moment the operator address is live. Pure text, no emblem image.
`FOXHOLE_NO_SPLASH=1` boots straight to console; any key skips. The leanest
build (`--no-default-features`) omits the sequence entirely.

**Operating surface.** The console is tuned for a modern UTF-8 + 24-bit
("truecolor") terminal — the default on Raspberry Pi OS Bookworm and comparable
field hardware. Colour only reinforces hierarchy; on a monochrome or colour-blind
surface the heavy/double box-drawing and bold nameplates still carry focus.

---

## 4. OPERATOR CONSOLE

Two-tier layout. **Tools** along the top; **panes** within each.

```
 Conversations | Network | Map | Browser | Log | Interfaces | Notes | Guide
```

| Key            | Action                                              |
|----------------|-----------------------------------------------------|
| `Ctrl+N/Ctrl+P`| Cycle tools (tabs)                                  |
| `Ctrl+O`       | Open a conversation by LXMF address or mnemonic     |
| `Tab`          | Cycle panes (Peers / Thread / Transmit)             |
| `Up/Down`      | Select contact / node / link / map marker / slot    |
| `PgUp/PgDn`    | Scroll the focused text pane (page / log / thread)  |
| `Home/End`     | Jump to top / bottom of the focused pane            |
| `Ctrl+T`       | Toggle message title / body (Conversations)         |
| `Ctrl+S`       | Transmit                                            |
| `Ctrl+R`       | Sync from propagation node (operator-initiated)     |
| `Ctrl+G`       | Share / revoke a hazard zone to the peer (CoT intel)|
| `t`            | Cycle selected peer's trust level (Conv / Network)  |
| `m`            | Show selected address as a mnemonic phrase (Network)|
| `p`            | Path probe selected peer/node (Network, rnpath)     |
| `Enter`        | Open node index / follow selected link (Browser)    |
| `Backspace`    | Back to previous page (Browser, page pane)          |
| `Ctrl+X`       | Purge compose buffer / clear note slot              |
| `Ctrl+K`       | **BURN** — destroy all session data (confirm `BURN`)|
| `Ctrl+Q`       | ████████ (terminate session)                        |

**World Map** keys: `Arrows` pan, `+`/`-` zoom, `Tab`/`[`/`]` cycle markers,
`Enter`/`c` centre, `g` toggle the cities layer, `/` go to an MGRS grid
reference, `r` reset the view, `i` review staged intel, `a`/`e`/`x` author /
edit / remove the selected intel object.

The **Browser** tool reads Nomad Network ████ pages: it lists discovered
`nomadnetwork.node` stations and fetches `index.mu` over a Reticulum link,
rendering the ████ micron markup. Two panes (`Tab` to switch): a node list, and
the page viewport where `Up/Down` move between **links and input fields**, typing
edits a focused field (masked fields show `*`), and `Enter` follows a link or
**submits its form** — same-node (`:/page/…`) or to another discovered node —
with `Backspace` to go back. Checkbox/radio inputs are ████████████.

Additional bindings exist for ████████ and ████████████ but are omitted from
this release.

---

## 5. CRYPTOGRAPHIC ANNEX  ██ CLASSIFIED — EXTRACT ONLY ██

- Store key: HKDF-derived from the operator identity. No passphrase prompt in
  this configuration; ████████ tier withheld.
- Container format: `FXC1` (per-conversation history) and `FXI1` (the received
  intel layer) → authenticated token → atomic write (write-temp → fsync →
  rename), both keyed off the same operator identity. Torn writes ████ possible.
- Foreign, corrupt, or ████████ containers are skipped on load; one bad file
  does not compromise the ████████████.

Key-escrow provisions, ████████ rotation, and ████████████████ are detailed in
the full annex, classification ████████████, not included.

---

## 6. OPERATIONAL CAVEATS  ⚠ READ BEFORE ████████

> **THE PROGRAM OFFICE MAKES NO REPRESENTATION AS TO OPERATOR SURVIVABILITY,
> MESSAGE DELIVERY, OR CONTINUITY OF SERVICE IN A CONTESTED ████████████.**
> This terminal moves text when a path exists. It does not move ████████, stop
> ████████, or alter the ████████████████. Outcomes depend on factors outside
> the scope of this document and ████████████████████████████████████████████.

- Status is **experimental**. Sections ██–██ describe known ████████.
- Tied to your identity: lose the identity file, lose access to sealed history.
- No telemetry, no analytics, no ████████. By design.
- **DIRECT delivery is not acknowledged** on the currently pinned mesh stack:
  with `rsReticulum` rev `3b91b36`, a link-delivered message's proof is signed
  with the wrong key and rejected by the sender, so it never reaches
  `[delivered]` (it may retry or fall back to propagation). A *deliverability*
  gap, not a confidentiality one — tracked in
  `docs/rsreticulum-delivery-proof-issue.md`, resolved by pinning a patched
  revision. See `SECURITY.md` for the full threat model.

---

## 7. PROVENANCE & LICENSE

FoxHole's own source — every workspace crate and the binary — is licensed
**AGPL-3.0-or-later**; the full text is in [`LICENSE`](LICENSE). Linking the
`net` compartment additionally incorporates AGPL-3.0-or-later upstream
components (the `rns-*` and `lxmf-core` crates), so a distributed `net` binary
is a combined AGPL work — including the **network-use clause** (§13): operators
who let others interact with a running instance over a network must offer those
users the corresponding source. Govern distribution accordingly.

Security posture, threat model, and responsible-disclosure contact are in
[`SECURITY.md`](SECURITY.md); release history in [`CHANGELOG.md`](CHANGELOG.md).
Architecture, build matrix, and the mesh binding are documented for cleared
maintainers in `CLAUDE.md` and `docs/lxmf-integration.md`.

```
END OF DECLASSIFIED PORTION
████████████████████████████████████████████████████████████████████████████
████████████████  REMAINDER WITHHELD  ·  DESTROY AFTER READING  ██████████████
████████████████████████████████████████████████████████████████████████████
```
