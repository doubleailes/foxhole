# Security Policy

FoxHole is an off-grid, encrypted comms terminal intended for adversarial
environments. This document states what it does and does not protect against,
and how to report a vulnerability. Status is **experimental** (0.1.x) — read
the threat model before relying on it.

## Supported versions

| Version | Supported            |
|---------|----------------------|
| 0.1.x   | ✅ (current pre-release) |
| < 0.1   | ❌                   |

## Reporting a vulnerability

**Do not open a public issue for a security vulnerability.** Report it
privately via GitHub's **"Report a vulnerability"** (Security Advisories) on
the `doubleailes/foxhole` repository. Please include affected version/commit,
build configuration (`offline` vs `--features net`), reproduction steps, and
impact. We aim to acknowledge within a few days and will coordinate a fix and
disclosure timeline with you.

## Threat model

### What FoxHole aims to protect

- **Confidentiality of data at rest.** Conversation history (`FXC1`) and the
  received-intel layer (`FXI1`) are sealed with AES-256-CBC + HMAC, written
  atomically (write-temp → fsync → rename), keyed by an HKDF derivation of the
  operator's own Reticulum identity. A foreign, corrupt, or truncated container
  is skipped on load and never compromises the rest of the store.
- **No unsolicited emissions.** Propagation sync and intel shares are
  **operator-initiated only**; the terminal does not beacon or phone home, and
  carries no telemetry or analytics.
- **Trust-gated ingest.** Inbound intel from a peer is gated by an
  operator-assigned trust level — `TRUSTED` posts live, `UNKNOWN`/`UNTRUSTED`
  are staged for review before touching the map, `COMPROMISED` is dropped.
- **Emergency destruction.** `Ctrl+K` → typing `BURN` zero-overwrites, fsyncs,
  and unlinks every file under the config directory, then removes the tree.

### What FoxHole does NOT protect against

- **A compromised host.** If the machine is owned (malware, keylogger, memory
  capture, an attacker with the identity file *and* read access while the
  process runs), at-rest encryption does not help — the identity key derives
  the store key. In **this configuration there is no passphrase** on the
  identity, so anyone who can read the identity file and the stores can decrypt
  history. Protect the host and the identity file accordingly.
- **Forensic media recovery after BURN.** BURN is **best-effort against
  filesystem forensics**: on copy-on-write filesystems, journaling filesystems,
  or SSDs with wear-leveling, overwritten bytes may persist in unreachable
  blocks. The real guarantee is that destroying the identity key makes the
  remaining sealed stores **undecryptable** — not that the ciphertext is
  physically gone. Treat BURN as "render the data unrecoverable by destroying
  the key," not "securely wipe the platter."
- **Traffic analysis / metadata.** FoxHole moves text when a path exists; it
  does not add cover traffic or hide that you are transmitting. Network-layer
  privacy is whatever Reticulum and your interfaces provide.
- **Endpoint authenticity beyond Reticulum's identities + your trust
  assignments.** Verify a correspondent's address out-of-band (the spoken-word
  mnemonic exists for exactly this) before trusting it.

### Known security-relevant limitations (0.1.x)

- **DIRECT delivery is not acknowledged** with the pinned `rsReticulum`
  revision: a delivery proof is signed with the identity key, which the sender
  validates against the link key, so DIRECT messages never reach `Delivered`.
  This is a *deliverability/availability* gap, not a confidentiality one. See
  [`docs/rsreticulum-delivery-proof-issue.md`](docs/rsreticulum-delivery-proof-issue.md).
- **Ratchets and delivery-proof surfacing** are not yet wired (see
  `docs/lxmf-integration.md`).
- The `net` build links **AGPL-3.0-or-later** upstreams and is a combined AGPL
  work, including the network-use clause — see [`LICENSE`](LICENSE).

## Cryptographic dependencies

The live stack (`--features net`) pins its `rns-*` (Reticulum) and `lxmf-core`
dependencies **by git commit** in `Cargo.toml` for reproducibility. Review and
bump those revisions deliberately; a dependency advisory there affects the
combined work.
