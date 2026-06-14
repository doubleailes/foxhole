# LXMF DIRECT delivery proof is signed with the identity key instead of the link key

## Summary

When application data arrives over an established link (the LXMF **DIRECT**
delivery path), `LinkManager` replies with a delivery proof signed with the
**identity** key. The initiating peer validates link-packet proofs against the
**link's ephemeral key** advertised at handshake (`peer_ed25519_pub`), not the
identity key, so the proof is rejected. The sender therefore **never confirms
delivery** — the message stays unproven indefinitely.

`prove_packet_with_link_key` already exists and is used on every other proof
path; only this one path was missed.

## Affected version

- Repo: `rsReticulum`
- Commit: `3b91b36f3ec7b3769327e0dd06003875953e81ee` (`main`, v1.0.1)
- File: `crates/rns-runtime/src/link_manager.rs`

## Impact

- **Direct (link) LXMF delivery is never acknowledged.** A sender that delivers
  opportunistically/over a link gets no proof back, so the message never reaches
  the `Delivered` state and senders may needlessly retry or fall back to
  propagation.
- Affects any embedder that relies on `LinkManager` to answer inbound link data
  with a valid proof (e.g. a delivery-destination node receiving DIRECT LXMF).

## Steps to reproduce

1. Node A opens a link to node B's `lxmf.delivery` destination and sends an LXMF
   message as link application data.
2. B decrypts and forwards the payload (works), then emits a delivery proof.
3. A receives the proof and validates it against B's **link** public key
   (`Link::validate_packet_proof` → `peer_ed25519_pub`).
4. **Observed:** validation fails (proof was signed with B's identity key); A
   records no delivery proof.
   **Expected:** the proof validates and A marks the message delivered.

## Root cause

In `LinkManager`, the inbound-link-data handler's `_ =>` arm ("Application data
on a link (LXMF DIRECT)") signs the proof with the identity key
(`prove_packet(&pkt_hash, &signing_key)`), with a fallible identity-sign
fallback. Both sign with the identity key, which the peer does not check for link
packets.

Every other proof emission in the same file already uses the link key
(`prove_packet_with_link_key`), e.g. around lines `888`, `3551`, `3620`, `3738` —
this DIRECT path (≈ line `1744`) is the outlier.

## Proposed fix

Sign the proof with the link's ephemeral key, like the other paths. The
identity-key locals in this arm become unused and can be removed.

```diff
             _ => {
                 // Application data on a link (LXMF DIRECT).
-                let identity_key_bytes = self.identity_key.as_ref().map(|key| key.to_bytes());
-                let identity_for_signing = if identity_key_bytes.is_none() {
-                    self.identity.clone()
-                } else {
-                    None
-                };
                 if let Some(active) = self.active_links.get_mut(&link_id) {
                     active.link.record_inbound();
                     active.link.record_rx(data.len());
                     if let Ok(plaintext) = active.link.decrypt(data) {
                         // …forward plaintext…

                         // Link proofs are unencrypted (Packet.py:198-200).
                         let pkt_hash =
                             rns_wire::hash::packet_hash(raw, header.flags.header_type);
-                        let proof = if let Some(key_bytes) = identity_key_bytes {
-                            let signing_key = Ed25519PrivateKey::from_bytes(&key_bytes);
-                            active.link.prove_packet(&pkt_hash, &signing_key)
-                        } else if let Some(identity) = identity_for_signing.as_ref() {
-                            active
-                                .link
-                                .prove_packet_with_fallible(&pkt_hash, |hash| identity.sign(hash))
-                        } else {
-                            Err(rns_link::encryption::LinkCryptoError::EncryptionFailed)
-                        };
+                        // Delivery proofs MUST be signed with the link's ephemeral
+                        // key: the peer validates against the key advertised at
+                        // handshake (`peer_ed25519_pub`, see
+                        // `Link::validate_packet_proof`), not the identity key —
+                        // otherwise the sender never confirms delivery.
+                        let proof = active.link.prove_packet_with_link_key(&pkt_hash);
                         match proof {
                             Ok(proof_data) => { /* …pack + send LinkProof… */ }
                             Err(_) => { /* …warn… */ }
                         }
                     }
                 }
             }
```

## Notes

- This is the v1.0.1 forward-port of a fix already validated on the 0.9.3 line
  (branch `fix_deliver_packet`, commit `b6ec454` "fix delivery emission packet").
  That branch predates the v1.0.1 API changes, so the fix is re-applied here
  against current `main` rather than rebased.
- No new API is required — `Link::prove_packet_with_link_key` already exists
  (`crates/rns-link/src/link.rs`).
