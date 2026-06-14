# LXMF / Reticulum integration — architecture binding

This document records **how the Ratspeak reference client binds to the
`rsReticulum` and `rsLXMF` library crates**, and how FoxHole applies the same
pattern. It exists because the libraries expose a non-obvious split: the
*reusable* surface is small, and the integration "glue" that the daemon
(`lxmd-rs`) implements has to be rebuilt by any embedding app.

Source studied: [`github.com/ratspeak/Ratspeak`](https://github.com/ratspeak/Ratspeak),
crate `ratspeak-runtime` (commit on `main`, mid-2026), plus the local checkouts
at `../rsReticulum` and `../rsLXMF`.

> **Licensing.** `rns-*` and `lxmf-core` are **AGPL-3.0-or-later**. Linking them
> makes a distributed FoxHole binary a combined AGPL work, including the
> network-use clause. This is a deliberate downstream obligation, not an
> accident — decide before shipping binaries.

---

## 1. Crate graph

Ratspeak is a Cargo workspace; **all** Reticulum/LXMF binding lives in one member
crate, `ratspeak-runtime` (the others are UI/db/tauri). It path-depends the
libraries as sibling checkouts. FoxHole binds the same library surface but pulls
the crates from git, pinned by commit (behind the `net` feature):

```toml
# FoxHole Cargo.toml — net-only deps, pinned by rev
rns-crypto    = { git = "https://github.com/doubleailes/rsReticulum", rev = "<rev>", optional = true }
# …rns-wire / rns-identity / rns-transport / rns-interface / rns-runtime, same rev…
lxmf-core     = { git = "https://github.com/doubleailes/rsLXMF",      rev = "<rev>", optional = true }
```

All crates are **edition 2024, tokio-based, AGPL-3.0**. `rsLXMF` itself depends on
`rsReticulum` via `[workspace.dependencies]` using sibling-checkout *paths*
(`../rsReticulum/...`), which don't exist when `rsLXMF` is fetched from git — so
FoxHole's root manifest carries a `[patch."…/rsLXMF"]` block redirecting those
`rns-*` names to the same pinned `rsReticulum` rev, unifying the whole stack on
one source. (Ratspeak's sibling-path layout avoids the patch by keeping both
repos side-by-side on disk.)

**Reusable vs. not.** `lxmf_core::message::LxMessage` (pack/unpack/sign) and the
`rns-*` primitives are cleanly reusable. `lxmf_core::router::LxmRouter` is a
*synchronous state machine* — it does not own I/O. The genuinely hard runtime
(inbound decrypt, link-backed delivery, resource transfer, announce handling,
propagation sync) is **not** in the library; `lxmd-rs` implements it in ~2500
lines, and Ratspeak re-implements an equivalent inside `ratspeak-runtime`. The
one heavy piece the library *does* hand you is
`lxmf_core::link_delivery::LinkDeliveryManager`.

Key `ratspeak-runtime/src/` files:

| File | Responsibility |
|------|----------------|
| `rns.rs` | Reticulum transport bring-up; owns the `ReticulumHandle`. |
| `rns_config.rs` | Writes INI interface blocks (`add_tcp_client`). |
| `lxmf.rs` | `LxmfManager`: identity, router, destinations, send. |
| `messaging.rs` | Send entry points / conversation glue. |
| `announce_handlers.rs` | Announce-driven peer & propagation-node discovery. |
| `propagation.rs`, `state.rs`, `vault.rs` | Propagation, shared state, identity-at-rest. |
| `ratspeak-db/{static_nodes.rs,nodes.json}` | Seed propagation-node metadata. |

---

## 2. Transport bring-up (`rns.rs`)

Ratspeak does **not** construct transport actors or spawn interfaces by hand. It
writes a Reticulum INI config (one `TCPClientInterface` block per hub, via
`rns_config::add_tcp_client(config_dir, name, host, port)`), then lets the
runtime parse it:

```rust
let handle = rns_runtime::reticulum::init(
    Some(config_dir),   // dir containing the INI we just wrote
    socket_dir,         // Option<PathBuf>; None for a standalone client
    shutdown.clone(),   // rns_runtime::lifecycle::ShutdownSignal
    is_foreground,      // Arc<AtomicBool>
).await?;

handle.enable_on_network_discovery(
    Arc::new(lxmf_core::discovery_stamper::LxmfDiscoveryStamper::default()),
).await;
```

`init` parses the INI, spawns the transport actor + interfaces, and returns a
`ReticulumHandle`. From there the **entire app talks to the stack through two
members of that handle**:

- `handle.transport_tx: mpsc::Sender<TransportMessage>` — fire-and-forget
  (register destinations/handlers, send outbound packets, request paths).
- `handle.query_control(TransportQuery).await` — request/response (interface
  stats, recent announces, next-hop lookups).

The handle is wrapped in an `RnsManager { handle, shutdown }` kept in shared
state. Shutdown is `shutdown.trigger()` then `shutdown.wait().await`.

> **Hubs / connectivity.** `nodes.json` holds **propagation-node hashes**, not
> TCP endpoints. Reachability comes from interfaces in the INI. The project's
> developer testnet (incl. `amsterdam.connect.reticulum.network`) has been
> **decommissioned**, so FoxHole ships **no hard-coded hub**:
> - On first run it writes a config with an **`AutoInterface`** (zero-config LAN
>   multicast discovery — finds nomadnet/sideband/another foxhole on the same
>   network with no hub).
> - `FOXHOLE_HUB=host[:port]` adds a `TCPClientInterface` to that default for
>   internet reach. Find a current public hub via the community directories
>   (`directory.rns.recipes`, `rmap.world`) or run your own `rnsd`/transport node.
> - A hand-edited `{config}/reticulum/config` is **respected** (never
>   overwritten), so you can define any interface(s) yourself.

---

## 3. LXMF manager: router + destinations (`lxmf.rs`)

`LxmfManager::load_or_create(data_dir, preferred_identity_hash, hw_pin)` loads or
creates the `rns_identity::Identity`, then builds the router and registers the
delivery destination with the transport:

```rust
let mut router = LxmRouter::new(RouterConfig::default());
router.set_transport(transport_tx.clone());

// Destination hash for this identity's inbox:
//   Destination::hash_from_name_and_identity("lxmf.delivery", Some(&identity.hash))
transport_tx.try_send(TransportMessage::RegisterDestination {
    hash: self.lxmf_dest_hash,
    app_name: "lxmf.delivery".to_string(),
    delivery_tx: Some(delivery_tx.clone()),   // mpsc::Sender<DestinationEvent>
});
```

Link-backed delivery is delegated to lxmf-core rather than hand-rolled:

```rust
self.link_delivery = Some(lxmf_core::link_delivery::LinkDeliveryManager::new(
    transport_tx.clone(),
    Some(identity.get_public_key()),
    identity.get_signing_key(),
));
```

`LxmfManager` (abridged) holds: `identity`, `lxmf_dest_hash`,
`propagation_dest_hash`, `router`, `link_delivery`, a `delivery_tx` for the
inbound channel, and a crypto cache `known_identities: HashMap<hex, [u8;64]>`
(peer public keys, populated from announces).

---

## 4. Send path (`lxmf.rs::send_message_with_method_internal`)

```rust
let mut msg = LxMessage::new(dest_hash, self.lxmf_dest_hash, title, content, method);

// Sign with the Ed25519 seed = identity private key bytes [32..64]
let mut ed_seed = [0u8; 32];
ed_seed.copy_from_slice(&identity.get_private_key().unwrap()[32..64]);
let signing_key = rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(&ed_seed);
msg.sign(&signing_key)?;
msg.compute_hash()?;

self.router.send(msg);
```

`router.process_outbound()` (driven from the periodic tick) yields
`OutboundAction`s. **Opportunistic** sends are turned into a wire packet
(`PacketHeader` + payload encrypted to the peer's identity) and pushed as
`TransportMessage::Outbound`; **Direct** sends are handed to the
`LinkDeliveryManager`; **Propagated** sends go to a propagation node. Outbound
encryption needs the peer's public key — obtained from announces (§6).

---

## 5. Receive path

The `delivery_tx`/`delivery_rx` pair registered in §3 carries
`DestinationEvent::InboundPacket`. A spawned task:

1. decrypts the payload (`Destination::decrypt` / `Identity::decrypt`),
2. `LxMessage::unpack(bytes)` → struct with `destination_hash`, `source_hash`,
   `title`, `content`, `timestamp`, `signature`,
3. validates the signature against the source identity,
4. surfaces `{source_hash, title, content, timestamp}` upward (Ratspeak → SQLite
   + a UI emitter; FoxHole → a `NetEvent` channel into the conversation store).

`LxMessage::pack`/`unpack` are pure and transport-free — the same call reads the
`.lxm` files `lxmd-rs` writes to disk, so the wire and at-rest formats match.

---

## 6. Peer discovery via announces (`announce_handlers.rs`)

Discovery is **announce-driven**; there is no bootstrap peer database (static
`nodes.json` seeds only propagation nodes). Register one handler per aspect:

```rust
transport_tx.send(TransportMessage::RegisterAnnounceHandler {
    aspect_filter: Some("lxmf.delivery".to_string()),   // also "lxmf.propagation"
    receive_path_responses: true,
    callback_tx,   // mpsc::Sender<AnnounceHandlerEvent>
}).await;
```

Each `AnnounceHandlerEvent` carries `destination_hash: [u8;16]`, `public_key`,
`identity_hash`, `app_data`, `hops`, `ratchet`, `is_path_response`. From it:

- **display name** = `extract_display_name(app_data)`,
- **stamp cost** = `lxmf_core::handlers::stamp_cost_from_app_data(app_data)`,
- **propagation-node fields** = `lxmf_core::handlers::parse_pn_announce_data(app_data)`
  (only for `lxmf.propagation`; failure means "not a PN announce", dropped),
- **public key** → cached in `known_identities` keyed by hex hash; this is what
  later encrypts outbound messages (`Identity::from_public_key(public_key)`).

`lxmf.delivery` announces are messaging peers (→ conversation/peer list);
`lxmf.propagation` announces are relay nodes (→ a separate node registry). Path
responses (`is_path_response == true`) refresh routes without counting as
"last heard".

---

## 7. How FoxHole applies this

FoxHole adds `src/net.rs` — its analogue of `ratspeak-runtime` — behind the
`net` Cargo feature. It owns the identity, `ReticulumHandle`, `LxmRouter`, and
the announce/delivery tasks, and exposes a tiny surface so `main.rs` stays a thin
event loop:

- **net → UI**: `enum NetEvent { Sys(String), Peer { hash:[u8;16], name:Option<String> }, Message { source:[u8;16], title:String, content:String } }`.
- **UI → net**: the existing `app::Outbound { peer, body }`, where `peer` is the
  hex destination hash; `main.rs` drains `app.outbound` after each key event.

Conversations re-key on the hex destination hash with a display name shown when
known. The **Network** tab renders discovered delivery peers + propagation nodes
from the same store. The non-`net` build keeps the seeded demo peers and the
stub network task untouched.

Status: (1) deps + `--features net` compiles ✅; (2) bring-up + discovery +
**receive** ✅ — opportunistic (raw, decrypted with our identity) *and* Direct
over a link via **`rns_runtime::link_manager::LinkManager`** (it owns the
destination's event stream and hands back decrypted link payloads/resources);
(3) **send** ✅ — Direct-first via `lxmf_core::link_delivery::LinkDeliveryManager`
(driven by `process_outbound_with_direct` with announce-derived identity/hops),
falling back to Opportunistic; this node's own LXMF address is surfaced in the
Network tab + status bar. (4) **Propagated** ✅ — send via
`pack_propagated_encrypted_with_stamp` (encrypting to the recipient) +
`start_packed_delivery` to the node; receive via
`lxmf_core::propagation_client::PropagationClient`, completing the **DIRECT →
PROPAGATED → OPPORTUNISTIC** cascade. Sync is **on-demand only** (no automatic
polling — radio bandwidth is precious off-grid): `Ctrl+R` from Conversations or
`s` in the Network tab, with a live progress pop-up driven by the client's
state. The active propagation node is chosen in the Network tab (Up/Down +
Enter) and persisted by a small `Config` (`src/config.rs`, `{cfgdir}/foxhole.conf`,
also holds display name + hub). Learned peer/node identities persist to
`{cfgdir}/known_identities` so they survive restarts. Remaining: ratchets and
delivery-proof surfacing.
