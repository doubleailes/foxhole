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
- **7-bit ASCII chrome** — renders on ██████ serial gear and one-bit displays
  recovered from ████████████.

Capabilities 7 through ██ are withheld. ████████████████████████████████████

---

## 3. FIELD DEPLOYMENT

> WARNING: Handling instructions for ███████████ hardware appear in Annex ██,
> not reproduced here.

**Standalone (offline / bench):**
```sh
cargo build              # arm the terminal, dependency-light, fully offline
cargo run                # boot the operator console (seeded demo contacts)
```

**Live mesh (the `net` compartment):**
```sh
cargo build --features net
cargo run   --features net
```
The `net` compartment links the **Reticulum** + **LXMF** stack as path
dependencies from sibling checkouts `../rsReticulum` and `../rsLXMF` — both
must sit adjacent to this directory. (Both are AGPL-3.0; see Section ██.)

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

---

## 4. OPERATOR CONSOLE

Two-tier layout. **Tools** along the top; **panes** within each.

```
 Conversations | Network | Browser | Log | Interfaces | Guide
```

| Key            | Action                                              |
|----------------|-----------------------------------------------------|
| `Ctrl+N/Ctrl+P`| Cycle tools (tabs)                                  |
| `Ctrl+O`       | Open a conversation by raw LXMF address             |
| `Tab`          | Cycle panes (Peers / Thread / Transmit)             |
| `Up/Down`      | Select contact / node / link                        |
| `PgUp/PgDn`    | Scroll the focused text pane (page / log / thread)  |
| `Home/End`     | Jump to top / bottom of the focused pane            |
| `Ctrl+S`       | Transmit                                            |
| `Ctrl+R`       | Sync from propagation node (operator-initiated)     |
| `p`            | Path probe selected peer/node (Network, rnpath)     |
| `Enter`        | Open node index / follow selected link (Browser)    |
| `Backspace`    | Back to previous page (Browser, page pane)          |
| `Ctrl+X`       | Purge compose buffer                                |
| `Ctrl+Q`       | ████████ (terminate session)                        |

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
- Container format: `FXC1` → authenticated token → atomic write
  (write-temp → fsync → rename). Torn writes ████ possible.
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

---

## 7. PROVENANCE & LICENSE

Linking the `net` compartment incorporates **AGPL-3.0-or-later** components
(the `rns-*` and `lxmf-core` crates). A distributed binary is therefore a
combined AGPL work — including the network-use clause. Govern ████████
accordingly.

Architecture, build matrix, and the mesh binding are documented for cleared
maintainers in `CLAUDE.md` and `docs/lxmf-integration.md`.

```
END OF DECLASSIFIED PORTION
████████████████████████████████████████████████████████████████████████████
████████████████  REMAINDER WITHHELD  ·  DESTROY AFTER READING  ██████████████
████████████████████████████████████████████████████████████████████████████
```
