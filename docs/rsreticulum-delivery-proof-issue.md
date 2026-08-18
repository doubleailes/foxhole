# LXMF DIRECT delivery proofs were rejected (fixed upstream)

**Status: resolved.** Reported as
[ratspeak/rsReticulum#23](https://github.com/ratspeak/rsReticulum/issues/23),
fixed upstream in `f7ae0274c` ("link: enforce role-safe packet proofs") and
released in `v1.2.0`, the tag this workspace pins. Kept as a record
because the resolution corrects the model we had been working from.

## Symptom

A message delivered over an established link (the LXMF **DIRECT** path) never
reached `[delivered]`. The payload arrived and was forwarded correctly — only the
acknowledgement was unverifiable, so the sender kept the message unproven and
either retried or fell back to propagation. A deliverability bug, not a
confidentiality one.

Present on every tagged release up to and including `v1.1.0`.

## Actual root cause

Link packet proofs are **role-asymmetric**, mirroring upstream Python Reticulum:

- an **initiator** signs with its transient `LINKREQUEST` key;
- a **responder** signs with the destination **identity** key;
- each side validates against the key that corresponds to its *peer's* role.

`Link::validate_packet_proof` ignored that distinction: it verified every proof
against `peer_ed25519_pub` — the transient key — and returned `false` when that
key was absent. So a responder's spec-correct identity-signed proof failed
validation at the initiator, every time.

FoxHole is the **responder** when receiving inbound LXMF (a peer opens a link to
our `lxmf.delivery` destination), and the initiator when sending. The failure was
visible to us in the sending direction.

## What we got wrong

Our original report blamed the *signing* side — `LinkManager`'s DIRECT arm
(`crates/rns-runtime/src/link_manager.rs`) signing with the identity key — and
proposed replacing it with `prove_packet_with_link_key`, since every other proof
path in that file already used the link key.

That was the wrong side of the wire. The identity-key signing there was correct
*for a responder*; the uniformity we observed in the rest of the file reflected
those paths being initiator-side, not a convention the DIRECT arm was violating.
Had the proposed patch landed, FoxHole would have signed responder proofs with the
transient key and broken against spec-correct peers.

Lesson worth keeping: "this one call site disagrees with its neighbours" is a
weaker signal than it looks when the call sites differ by *role*. The validating
side is what defines correctness for a signature, and that is where we should
have started reading.

## The upstream fix

Entirely within `crates/rns-link/src/link.rs` — `link_manager.rs`, the file our
diff targeted, was not touched:

- a `LinkRole` enum (`Initiator` / `Responder`) with `Link::role()`;
- `peer_ed25519_pub` replaced by `peer_packet_proof_key`, the role-dependent key
  used to verify a peer's proofs, alongside a separate
  `responder_identity_signing_key`;
- `prove_packet_with_local_signer` signs with the one key the local role permits,
  with no role-incompatible fallback, and `prove_responder_packet_with` rejects an
  initiator caller with `PacketProofError::WrongRole`;
- `validate_peer_packet_proof` verifies against the role-dependent peer key.

## Consequences for this workspace

- **`v1.2.0` is the pin floor.** It is the first `rsReticulum` release containing
  `f7ae0274c`; anything at or below `v1.1.0` reintroduces the bug. There was a
  brief window where the only tag carrying the fix was the `ratspeak-v1.0.26n`
  app pre-release; `v1.2.0` superseded it, so a plain release tag is enough now.
- Bumping to `v1.2.0` needed no changes on our side — the role handling is
  internal to `rns-link`, and FoxHole never touched the proof API directly.
