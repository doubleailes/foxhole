//! Live LXMF / Reticulum networking (compiled only under the `net` feature).
//!
//! This is FoxHole's analogue of Ratspeak's `ratspeak-runtime`: it owns the
//! identity, brings up the Reticulum transport against a public TCP hub, and
//! registers the `lxmf.delivery` destination plus announce handlers. Inbound
//! traffic and discovered peers are forwarded to the UI as [`NetEvent`]s; the
//! UI's compose queue arrives back here over a channel.
//!
//! See `docs/lxmf-integration.md` for the full binding rationale.
//!
//! Scope (Phase 2): transport bring-up, peer discovery via announces, and
//! receipt of **opportunistic** (single-packet) LXMF messages — decoded exactly
//! as `lxmd`'s `handle_inbound_packet` does. Outbound sending and link-backed
//! (Direct) delivery are Phase 3; `outbound_rx` is drained with an interim
//! notice until then.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use lxmf_core::constants::DeliveryMethod;
use lxmf_core::link_delivery::{DeliveryResult, LinkDeliveryManager};
use lxmf_core::propagation_client::{PropagationClient, PropagationClientState};
use lxmf_core::message::{LxMessage, MessageError};
use lxmf_core::router::{
    DirectDeliveryPlanInput, DirectReusableLinkState, DirectRouteSnapshot, LxmRouter, OutboundAction,
    RouterConfig,
};
use rns_identity::destination::{DestType, Destination, Direction};
use rns_identity::identity::Identity;
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::link_manager::LinkManager;
use rns_runtime::reticulum;
use rns_transport::link_messages::DestinationEvent;
use rns_transport::messages::{
    AnnounceHandlerEvent, OutboundRequest, TransportMessage, TransportQuery, TransportQueryResponse,
};

use crate::app::{NetCommand, NetEvent, Outbound, PeerKind};
use crate::config::{Config, config_dir};

/// Cache of peer destination hash (hex) -> 64-byte public key, learned from
/// `lxmf.delivery` announces. Hex-keyed to match `lxmf_core`'s own convention
/// (`LinkDeliveryManager::drain_events` takes `&HashMap<String, [u8;64]>`).
type KnownKeys = HashMap<String, [u8; 64]>;

/// Cache of peer destination hash -> hop count, learned from announces; feeds
/// the router's Direct delivery planning.
type HopCache = HashMap<[u8; 16], u8>;

/// Fallback port when `FOXHOLE_HUB` gives a bare host with no `:port`.
const DEFAULT_HUB_PORT: u16 = 4242;

/// LXMF inbox aspect — the full dotted destination name.
const LXMF_DELIVERY: &str = "lxmf.delivery";
const LXMF_PROPAGATION: &str = "lxmf.propagation";

/// Re-announce our delivery destination on this cadence so peers keep a path.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(300);

/// Cadence for draining the router's outbound queue (retries, deferred stamps,
/// sends unblocked by a freshly learned key/path).
const SEND_INTERVAL: Duration = Duration::from_secs(1);

/// After a path request, wait this long before re-requesting / retrying a
/// delivery for the same destination. Bounds path-request traffic and defers the
/// message's next attempt so the router doesn't re-emit it every tick.
const PATH_REQUEST_WAIT: f64 = 30.0;

/// Entry point spawned from `main`. Runs until the transport shuts down or a
/// fatal bring-up error occurs; either way it reports through `events` so the
/// operator sees what happened in the Log tab.
pub async fn run(
    events: mpsc::Sender<NetEvent>,
    outbound_rx: mpsc::Receiver<Outbound>,
    command_rx: mpsc::Receiver<NetCommand>,
    config: Config,
) {
    if let Err(e) = run_inner(&events, outbound_rx, command_rx, config).await {
        let _ = events.send(NetEvent::Sys(format!("[SYS] net: {e}"))).await;
    }
}

/// Current Unix time as fractional seconds (the form announces/messages want).
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The default Reticulum INI written into `{cfgdir}/config` on first run (only
/// if no config exists — a hand-edited one is left untouched). It always
/// includes an `AutoInterface` for zero-config LAN discovery, and adds a
/// `TCPClientInterface` when a hub is supplied (via `FOXHOLE_HUB`). Format
/// mirrors the parser's own fixtures (`rns-runtime/src/config.rs`).
fn rns_config(hub: Option<(&str, u16)>) -> String {
    let mut s = String::from(
        "[reticulum]\n\
         share_instance = no\n\
         enable_transport = no\n\
         \n\
         [interfaces]\n\
         [[Auto]]\n\
         type = AutoInterface\n\
         enabled = yes\n",
    );
    if let Some((host, port)) = hub {
        s.push_str(&format!(
            "[[Hub]]\n\
             type = TCPClientInterface\n\
             enabled = yes\n\
             target_host = {host}\n\
             target_port = {port}\n"
        ));
    }
    s
}

async fn run_inner(
    events: &mpsc::Sender<NetEvent>,
    mut outbound_rx: mpsc::Receiver<Outbound>,
    mut command_rx: mpsc::Receiver<NetCommand>,
    config: Config,
) -> Result<(), String> {
    let sys = |m: String| {
        let tx = events.clone();
        async move {
            let _ = tx.send(NetEvent::Sys(m)).await;
        }
    };

    // --- Identity ---------------------------------------------------------------
    let cfg = config_dir();
    std::fs::create_dir_all(&cfg).map_err(|e| format!("config dir: {e}"))?;
    let id_path = cfg.join("identity");
    let identity = if id_path.exists() {
        Identity::from_file(&id_path).map_err(|e| format!("load identity: {e:?}"))?
    } else {
        let id = Identity::new();
        id.to_file(&id_path)
            .map_err(|e| format!("save identity: {e:?}"))?;
        id
    };
    sys(format!("[SYS] identity {}", hex::encode(identity.hash))).await;

    // Hand the conversation-store key to `main` early (before any traffic), so it
    // can decrypt history before live messages start appending.
    if let Some(key) = crate::store::derive_key(&identity) {
        let _ = events.send(NetEvent::StoreKey(key)).await;
    }

    // --- LXMF delivery destination ---------------------------------------------
    let mut delivery = Destination::new(
        Some(&identity),
        Direction::In,
        DestType::Single,
        LXMF_DELIVERY,
    )
    .map_err(|e| format!("delivery destination: {e:?}"))?;
    let lxmf_hash = delivery.hash;

    // --- Transport bring-up -----------------------------------------------------
    // Respect a hand-edited config; only synthesize a default on first run.
    // The hub comes from `FOXHOLE_HUB=host[:port]` (env wins) or the config file;
    // with neither we run LAN-only via AutoInterface (no public hub needed — the
    // project testnet is decommissioned, so there is no safe baked-in default).
    let hub = std::env::var("FOXHOLE_HUB")
        .ok()
        .or_else(|| config.hub.clone())
        .map(|s| parse_hostport(&s));
    let rns_dir = cfg.join("reticulum");
    std::fs::create_dir_all(&rns_dir).map_err(|e| format!("rns dir: {e}"))?;
    let cfg_file = rns_dir.join("config");
    if cfg_file.exists() {
        sys(format!("[SYS] using existing RNS config at {}", cfg_file.display())).await;
    } else {
        let ini = rns_config(hub.as_ref().map(|(h, p)| (h.as_str(), *p)));
        std::fs::write(&cfg_file, ini).map_err(|e| format!("write rns config: {e}"))?;
        match &hub {
            Some((h, p)) => sys(format!("[SYS] interfaces: AutoInterface (LAN) + TCP hub {h}:{p}")).await,
            None => {
                sys("[SYS] interfaces: AutoInterface (LAN only)".to_string()).await;
                sys("[SYS] set FOXHOLE_HUB=host:port for an internet hub, or edit the RNS config".to_string()).await;
            }
        }
    }

    sys("[SYS] bringing up transport ...".to_string()).await;
    let shutdown = ShutdownSignal::new();
    let handle = reticulum::init(
        rns_dir.to_str(),
        None,
        shutdown.clone(),
        Arc::new(AtomicBool::new(true)),
    )
    .await
    .map_err(|e| format!("reticulum init: {e:?}"))?;
    let transport = handle.transport_tx.clone();
    handle
        .enable_on_network_discovery(Arc::new(
            lxmf_core::discovery_stamper::LxmfDiscoveryStamper::default(),
        ))
        .await;
    sys("[SYS] transport online".to_string()).await;

    // --- Register inbox + announce handlers ------------------------------------
    let (delivery_tx, delivery_rx) = mpsc::channel::<DestinationEvent>(256);
    transport
        .send(TransportMessage::RegisterDestination {
            hash: lxmf_hash,
            app_name: LXMF_DELIVERY.to_string(),
            delivery_tx: Some(delivery_tx),
        })
        .await
        .map_err(|_| "transport closed".to_string())?;

    let (peer_tx, mut peer_rx) = mpsc::channel::<AnnounceHandlerEvent>(256);
    transport
        .send(TransportMessage::RegisterAnnounceHandler {
            aspect_filter: Some(LXMF_DELIVERY.to_string()),
            receive_path_responses: true,
            callback_tx: peer_tx,
        })
        .await
        .map_err(|_| "transport closed".to_string())?;

    let (node_tx, mut node_rx) = mpsc::channel::<AnnounceHandlerEvent>(256);
    transport
        .send(TransportMessage::RegisterAnnounceHandler {
            aspect_filter: Some(LXMF_PROPAGATION.to_string()),
            receive_path_responses: true,
            callback_tx: node_tx,
        })
        .await
        .map_err(|_| "transport closed".to_string())?;

    sys(format!("[SYS] {LXMF_DELIVERY} {} registered", hex::encode(lxmf_hash))).await;
    let _ = events.send(NetEvent::Local(hex::encode(lxmf_hash))).await;

    // --- Inbound link manager ---------------------------------------------------
    // rns-runtime's LinkManager takes ownership of our destination's event stream
    // and performs the inbound link handshake (Direct delivery — what nomadnet
    // uses), handing us decrypted payloads. Opportunistic (non-link) packets come
    // back raw on `inbound_raw`; link messages on `link_packet`; large messages
    // (resources) on `resource`. Mirrors lxmd's wiring.
    let link_signing_key = identity
        .get_signing_key()
        .ok_or_else(|| "identity has no signing key".to_string())?;
    let mut link_mgr =
        LinkManager::with_destination(transport.clone(), delivery_rx, &identity, LXMF_DELIVERY, link_signing_key);
    let (inbound_raw_tx, mut inbound_raw_rx) = mpsc::channel::<Vec<u8>>(256);
    let (link_packet_tx, mut link_packet_rx) = mpsc::channel::<(Vec<u8>, [u8; 16])>(256);
    let (resource_tx, mut resource_rx) = mpsc::channel::<(Vec<u8>, [u8; 16])>(64);
    let (link_up_tx, mut link_up_rx) = mpsc::channel::<[u8; 16]>(64);
    let (link_ident_tx, mut link_ident_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(64);
    link_mgr.set_inbound_raw_channel(inbound_raw_tx);
    link_mgr.set_link_packet_channel(link_packet_tx);
    link_mgr.set_resource_completed_channel(resource_tx);
    link_mgr.set_link_established_channel(link_up_tx);
    link_mgr.set_link_identified_channel(link_ident_tx);
    tokio::spawn(link_mgr.run());

    // --- Outbound router + link delivery ---------------------------------------
    let mut router = LxmRouter::new(RouterConfig::default());
    router.set_transport(transport.clone());
    let mut link_delivery = LinkDeliveryManager::new(
        transport.clone(),
        Some(identity.get_public_key()),
        identity.get_signing_key(),
    );
    let mut prop_client = PropagationClient::new(
        transport.clone(),
        Some(identity.get_public_key()),
        identity.get_signing_key(),
    );
    // Identities persist across restarts so we don't have to re-hear an announce
    // before we can reach a known peer/node (the cause of the post-restart
    // "identity unknown" loop). `hops`/stamp costs are re-learned cheaply.
    let known_path = cfg.join("known_identities");
    let mut known: KnownKeys = load_known(&known_path);
    let mut known_dirty = false;
    let mut hops: HopCache = HashMap::new();
    let mut last_path_request: HashMap<[u8; 16], f64> = HashMap::new();
    let mut ticks: u32 = 0;
    let mut syncing = false;

    // Seed identities/hops from the transport's own recent-announce cache too —
    // it may already know peers/nodes we haven't re-heard this session.
    if let Some(TransportQueryResponse::Announces(entries)) =
        handle.query_control(TransportQuery::GetRecentAnnounces).await
    {
        for e in entries {
            if let Some(pk) = e.public_key {
                learn(&mut known, &mut known_dirty, e.dest_hash, pk);
            }
            hops.insert(e.dest_hash, e.hops);
            if let Some(cost) = e
                .app_data
                .as_deref()
                .and_then(lxmf_core::handlers::pn_stamp_cost_from_app_data)
            {
                router.set_stamp_cost(e.dest_hash, cost);
            }
        }
    }
    if !known.is_empty() {
        sys(format!("[SYS] {} known identities loaded", known.len())).await;
    }

    // Apply the persisted propagation node, if any.
    if let Some(node) = config.propagation_node.as_deref().and_then(|s| parse_hash(s).ok()) {
        router.set_outbound_propagation_node(Some(node));
        prop_client.set_propagation_node(node);
        sys(format!("[SYS] propagation node {} (from config)", hex::encode(node))).await;
    }

    // Announce ourselves now and on a timer so peers learn a path to us.
    announce(&transport, &mut delivery, &identity, &config.display_name, events).await;
    let mut announce_tick = tokio::time::interval(ANNOUNCE_INTERVAL);
    announce_tick.tick().await; // consume the immediate first tick
    let mut send_tick = tokio::time::interval(SEND_INTERVAL);
    send_tick.tick().await; // consume the immediate first tick

    // --- Event loop -------------------------------------------------------------
    loop {
        tokio::select! {
            // Opportunistic (non-link) inbound: decrypt with our identity.
            Some(raw) = inbound_raw_rx.recv() => {
                let decoded = decode_inbound(&identity, &lxmf_hash, &raw);
                deliver_inbound(events, "opportunistic", raw.len(), decoded).await;
            }
            // Direct (link) inbound: already decrypted by the link manager.
            Some((data, _link)) = link_packet_rx.recv() => {
                let decoded = decode_link_payload(&lxmf_hash, &data);
                deliver_inbound(events, "direct", data.len(), decoded).await;
            }
            // Large messages arriving as a completed resource over a link.
            Some((data, _link)) = resource_rx.recv() => {
                let decoded = decode_link_payload(&lxmf_hash, &data);
                deliver_inbound(events, "direct(resource)", data.len(), decoded).await;
            }
            Some(_link) = link_up_rx.recv() => {
                let _ = events.send(NetEvent::Sys("[SYS] inbound link established".to_string())).await;
            }
            Some((_link, ident)) = link_ident_rx.recv() => {
                let _ = events.send(NetEvent::Sys(format!(
                    "[SYS] peer {}\u{2026} identified on inbound link",
                    &hex::encode(ident)[..16]
                ))).await;
            }
            Some(ev) = peer_rx.recv() => {
                // Cache the peer's key + hop count so we can reach it later (path
                // responses carry these too, hence no is_path_response guard here).
                if let Some(pk) = ev.public_key {
                    learn(&mut known, &mut known_dirty, ev.destination_hash, pk);
                }
                hops.insert(ev.destination_hash, ev.hops);
                if !ev.is_path_response {
                    let name = ev.app_data.as_deref()
                        .and_then(lxmf_core::handlers::display_name_from_app_data);
                    let _ = events.send(NetEvent::Peer {
                        kind: PeerKind::Delivery,
                        hash: hex::encode(ev.destination_hash),
                        name,
                    }).await;
                }
            }
            Some(ev) = node_rx.recv() => {
                // Cache what we need to deposit to / sync from this node later.
                if let Some(pk) = ev.public_key {
                    learn(&mut known, &mut known_dirty, ev.destination_hash, pk);
                }
                hops.insert(ev.destination_hash, ev.hops);
                if let Some(cost) = ev
                    .app_data
                    .as_deref()
                    .and_then(lxmf_core::handlers::pn_stamp_cost_from_app_data)
                {
                    router.set_stamp_cost(ev.destination_hash, cost);
                }
                if !ev.is_path_response {
                    let name = ev.app_data.as_deref()
                        .and_then(lxmf_core::handlers::pn_name_from_app_data);
                    let _ = events.send(NetEvent::Peer {
                        kind: PeerKind::Propagation,
                        hash: hex::encode(ev.destination_hash),
                        name,
                    }).await;
                }
            }
            Some(out) = outbound_rx.recv() => {
                match build_message(&identity, &lxmf_hash, &out) {
                    Ok(msg) => {
                        router.send(msg);
                        dispatch(&mut router, &mut link_delivery, &known, &hops, &mut last_path_request, &transport, events).await;
                    }
                    Err(e) => {
                        let _ = events.send(NetEvent::Sys(format!("[SYS] send: {e}"))).await;
                    }
                }
            }
            Some(cmd) = command_rx.recv() => {
                match cmd {
                    NetCommand::SetPropagationNode(node) => {
                        let parsed = node.as_deref().and_then(|s| parse_hash(s).ok());
                        router.set_outbound_propagation_node(parsed);
                        match parsed {
                            Some(n) => {
                                prop_client.set_propagation_node(n);
                                sys(format!("[SYS] propagation node set to {}", hex::encode(n))).await;
                            }
                            None => sys("[SYS] propagation node cleared".to_string()).await,
                        }
                    }
                    NetCommand::SyncNow => {
                        try_sync(&mut router, &mut prop_client, &known, &mut last_path_request, &transport, events).await;
                    }
                }
            }
            _ = send_tick.tick() => {
                ticks = ticks.wrapping_add(1);

                // Advance in-flight link deliveries.
                link_delivery.drain_events(&known);
                for result in link_delivery.tick() {
                    handle_delivery_result(&mut router, &known, &transport, events, result).await;
                }

                // Advance an in-progress propagation sync + surface downloads.
                // Sync is on-demand only (no automatic polling — bandwidth is
                // precious off-grid); it is started by Ctrl+R / the Network tab.
                prop_client.drain_events(&known);
                prop_client.tick();
                for data in prop_client.take_received_messages() {
                    let decoded = decode_propagation_payload(&identity, &lxmf_hash, &data);
                    deliver_inbound(events, "propagation", data.len(), decoded).await;
                }
                // Drive the sync-progress pop-up from the client's live state.
                let phase = sync_phase(prop_client.state);
                match phase {
                    Some(p) => {
                        let spin = ['|', '/', '-', '\\'][(ticks % 4) as usize];
                        let _ = events
                            .send(NetEvent::Sync(Some(format!("{spin} {p}\u{2026}"))))
                            .await;
                        syncing = true;
                    }
                    None if syncing => {
                        syncing = false;
                        let _ = events.send(NetEvent::Sync(None)).await;
                        sys("[SYS] propagation sync finished".to_string()).await;
                    }
                    None => {}
                }

                // Persist newly learned identities periodically (debounced).
                if known_dirty && ticks.is_multiple_of(30) {
                    let _ = save_known(&known_path, &known);
                    known_dirty = false;
                }

                dispatch(&mut router, &mut link_delivery, &known, &hops, &mut last_path_request, &transport, events).await;
            }
            _ = announce_tick.tick() => {
                announce(&transport, &mut delivery, &identity, &config.display_name, events).await;
            }
            _ = shutdown.wait() => break,
            else => break,
        }
    }

    Ok(())
}

/// Decode one inbound opportunistic LXMF packet. Mirrors `lxmd`'s
/// `handle_inbound_packet` + `decrypt_inbound`: strip the Reticulum header,
/// decrypt with our identity, re-prepend the dest hash (Python strips it for
/// opportunistic delivery), then unpack. Returns `None` for anything that
/// isn't a decodable LXMF message (e.g. link packets) — those are ignored.
fn decode_inbound(identity: &Identity, my_hash: &[u8; 16], raw: &[u8]) -> Option<LxMessage> {
    let (_header, header_len) = rns_wire::header::PacketHeader::unpack(raw).ok()?;
    let payload = raw.get(header_len..)?;
    if payload.is_empty() {
        return None;
    }
    let plaintext = identity.decrypt(payload, None, false).ok()?;
    let unpack_data = if plaintext.len() >= 16 && plaintext[..16] == *my_hash {
        plaintext
    } else {
        let mut d = my_hash.to_vec();
        d.extend_from_slice(&plaintext);
        d
    };
    LxMessage::unpack(&unpack_data).ok()
}

/// Decode an LXMF payload delivered over a link (already decrypted by the link
/// manager). Mirrors lxmd's `handle_link_delivered_data`: re-prepend the dest
/// hash if the sender stripped it, then unpack.
fn decode_link_payload(my_hash: &[u8; 16], data: &[u8]) -> Option<LxMessage> {
    let unpack_data = if data.len() >= 16 && data[..16] == *my_hash {
        data.to_vec()
    } else {
        let mut d = my_hash.to_vec();
        d.extend_from_slice(data);
        d
    };
    LxMessage::unpack(&unpack_data).ok()
}

/// Forward a decoded inbound message to the UI.
async fn emit_message(events: &mpsc::Sender<NetEvent>, msg: LxMessage) {
    let _ = events
        .send(NetEvent::Message {
            source: hex::encode(msg.source_hash),
            title: msg.title,
            content: msg.content,
        })
        .await;
}

/// Decode an inbound payload via `decode`, deliver it to the right thread, and
/// log the path/size/outcome so inbound traffic is observable (a payload that
/// fails to decode is otherwise dropped silently — invisible when debugging).
async fn deliver_inbound(
    events: &mpsc::Sender<NetEvent>,
    label: &str,
    raw_len: usize,
    decoded: Option<LxMessage>,
) {
    match decoded {
        Some(msg) => {
            let _ = events
                .send(NetEvent::Sys(format!(
                    "[SYS] {label} message from {} -> thread ({raw_len} B)",
                    hex::encode(msg.source_hash)
                )))
                .await;
            emit_message(events, msg).await;
        }
        None => {
            let _ = events
                .send(NetEvent::Sys(format!(
                    "[SYS] {label} data not decodable as LXMF ({raw_len} B)"
                )))
                .await;
        }
    }
}

/// Build a signed LXMF message from a UI compose entry, preferring **Direct**
/// (link) delivery — the priority the user wants and the method nomadnet uses.
/// The router falls back to Opportunistic only as a last resort (see
/// `handle_delivery_result`). The compose target (`out.peer`) is a hex hash.
fn build_message(identity: &Identity, source: &[u8; 16], out: &Outbound) -> Result<LxMessage, String> {
    let dest = parse_hash(&out.peer)?;
    let mut msg = LxMessage::new(dest, *source, "", &out.body, DeliveryMethod::Direct);
    let signing_key = identity
        .get_signing_key()
        .ok_or_else(|| "identity has no signing key".to_string())?;
    msg.sign(&signing_key).map_err(|e| format!("sign: {e}"))?;
    msg.compute_hash().map_err(|e| format!("hash: {e}"))?;
    Ok(msg)
}

/// Decode a 32-char hex destination hash into 16 bytes.
fn parse_hash(s: &str) -> Result<[u8; 16], String> {
    let bytes = hex::decode(s).map_err(|_| format!("bad hash: {s}"))?;
    bytes
        .try_into()
        .map_err(|_| format!("hash must be 16 bytes: {s}"))
}

/// Send a path request for `dest`, at most once per [`PATH_REQUEST_WAIT`].
/// Returns true only when a request was actually sent, so callers log just once
/// per window instead of every tick.
fn request_path(
    transport: &mpsc::Sender<TransportMessage>,
    last_path_request: &mut HashMap<[u8; 16], f64>,
    dest: [u8; 16],
    now: f64,
) -> bool {
    if now - last_path_request.get(&dest).copied().unwrap_or(0.0) < PATH_REQUEST_WAIT {
        return false;
    }
    last_path_request.insert(dest, now);
    let _ = transport.try_send(TransportMessage::RequestPath {
        destination_hash: dest,
    });
    true
}

/// Re-queue a message after requesting a path: defer its next attempt by
/// [`PATH_REQUEST_WAIT`] (so the router doesn't re-emit — and re-request — every
/// tick) and request a path (throttled), logging at most once per window.
async fn requeue_after_path_request(
    router: &mut LxmRouter,
    transport: &mpsc::Sender<TransportMessage>,
    last_path_request: &mut HashMap<[u8; 16], f64>,
    mut message: LxMessage,
    request_hash: [u8; 16],
    events: &mpsc::Sender<NetEvent>,
    note: &str,
) {
    let now = now_secs();
    message.last_delivery_attempt = now;
    message.next_delivery_attempt = now + PATH_REQUEST_WAIT;
    if request_path(transport, last_path_request, request_hash, now) {
        let _ = events
            .send(NetEvent::Sys(format!(
                "[SYS] {note} {} — requesting path (retry in {}s)",
                hex::encode(request_hash),
                PATH_REQUEST_WAIT as u64
            )))
            .await;
    }
    router.send(message);
}

/// Insert a learned identity, flagging the cache dirty only when it changed (so
/// the periodic persist writes on real updates, not every re-announce).
fn learn(known: &mut KnownKeys, dirty: &mut bool, dest: [u8; 16], pk: [u8; 64]) {
    if known.insert(hex::encode(dest), pk) != Some(pk) {
        *dirty = true;
    }
}

/// Load persisted identities — `<dest_hex> <pubkey_hex>` per line. Missing or
/// malformed entries are skipped; a bad file just yields an empty cache.
fn load_known(path: &Path) -> KnownKeys {
    let mut map = KnownKeys::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if let (Some(d), Some(p)) = (it.next(), it.next())
            && d.len() == 32
            && let Ok(bytes) = hex::decode(p)
            && let Ok(pk) = <[u8; 64]>::try_from(bytes.as_slice())
        {
            map.insert(d.to_string(), pk);
        }
    }
    map
}

/// Atomically persist learned identities to disk.
fn save_known(path: &Path, known: &KnownKeys) -> std::io::Result<()> {
    let mut s = String::new();
    for (dest, pk) in known {
        s.push_str(&format!("{dest} {}\n", hex::encode(pk)));
    }
    crate::storage::atomic_write(path, s.as_bytes())
}

/// Drain the router's outbound queue and act on each decision. Direct messages
/// are handed to the link-delivery manager; if that can't start (no path yet) we
/// request a path and re-queue. Opportunistic is the single-packet last resort.
async fn dispatch(
    router: &mut LxmRouter,
    link_delivery: &mut LinkDeliveryManager,
    known: &KnownKeys,
    hops: &HopCache,
    last_path_request: &mut HashMap<[u8; 16], f64>,
    transport: &mpsc::Sender<TransportMessage>,
    events: &mpsc::Sender<NetEvent>,
) {
    // The router's Direct planning needs to know, per message, whether we have
    // the peer's identity and a route. We supply both from our announce caches.
    let actions = router.process_outbound_with_direct(|message, _now| {
        let dest = message.destination_hash;
        DirectDeliveryPlanInput {
            identity_known: known.contains_key(&hex::encode(dest)),
            route: hops.get(&dest).map(|&h| DirectRouteSnapshot {
                destination_hash: dest,
                hops: h,
                interface_name: None,
                learned_at: None,
                expires_at: None,
            }),
            reusable_link: DirectReusableLinkState::None,
        }
    });

    for action in actions {
        match action {
            OutboundAction::DeliverDirect { message, dest_hash }
            | OutboundAction::PlanDirect {
                message, dest_hash, ..
            } => {
                let hop = hops.get(&dest_hash).copied().unwrap_or(1);
                match link_delivery.start_delivery(message, dest_hash, hop) {
                    Ok(_link_id) => {
                        let _ = events
                            .send(NetEvent::Sys(format!(
                                "[SYS] opening link to {} ...",
                                hex::encode(dest_hash)
                            )))
                            .await;
                    }
                    Err(fail) => {
                        // No path/identity yet: request a path (throttled) and
                        // defer the next attempt so we don't loop every tick.
                        requeue_after_path_request(
                            router,
                            transport,
                            last_path_request,
                            *fail.message,
                            dest_hash,
                            events,
                            "no path to peer",
                        )
                        .await;
                    }
                }
            }
            OutboundAction::DeliverOpportunistic { message, dest_hash } => {
                send_opportunistic(router, known, transport, events, message, dest_hash).await;
            }
            OutboundAction::DeliverPropagated {
                mut message,
                prop_hash,
            } => {
                if !known.contains_key(&hex::encode(prop_hash)) {
                    // Need the node's identity (from its announce) before we can
                    // link. Request a path (throttled) and defer the retry.
                    requeue_after_path_request(
                        router,
                        transport,
                        last_path_request,
                        message,
                        prop_hash,
                        events,
                        "propagation node identity unknown",
                    )
                    .await;
                    continue;
                }
                let target_cost = router.get_stamp_cost(&prop_hash).unwrap_or(0);
                let recipient = message.destination_hash;
                let packed = message.pack_propagated_encrypted_with_stamp(
                    |plaintext| encrypt_to_recipient(known, &recipient, plaintext),
                    target_cost,
                );
                match packed {
                    Ok((wrapper, _tid, _value)) => {
                        let hop = hops.get(&prop_hash).copied().unwrap_or(4);
                        match link_delivery
                            .start_packed_delivery(message, prop_hash, hop, wrapper, false)
                        {
                            Ok(_) => {
                                let _ = events
                                    .send(NetEvent::Sys(format!(
                                        "[SYS] depositing to propagation node {} ...",
                                        hex::encode(prop_hash)
                                    )))
                                    .await;
                            }
                            Err(fail) => {
                                requeue_after_path_request(
                                    router,
                                    transport,
                                    last_path_request,
                                    *fail.message,
                                    prop_hash,
                                    events,
                                    "no path to propagation node",
                                )
                                .await;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = events
                            .send(NetEvent::Sys(format!("[SYS] propagation pack failed: {e}")))
                            .await;
                    }
                }
            }
            OutboundAction::Failed(m) | OutboundAction::Expired(m) => {
                let _ = events
                    .send(NetEvent::Sys(format!(
                        "[SYS] delivery to {} failed",
                        hex::encode(m.destination_hash)
                    )))
                    .await;
            }
        }
    }
    router.process_deferred_stamps();
}

/// Act on a completed link delivery. On terminal failure we fall back to
/// Opportunistic — making it the genuine last resort behind Direct.
async fn handle_delivery_result(
    router: &mut LxmRouter,
    known: &KnownKeys,
    transport: &mpsc::Sender<TransportMessage>,
    events: &mpsc::Sender<NetEvent>,
    result: DeliveryResult,
) {
    match result {
        DeliveryResult::Complete { msg_hash, .. } => {
            // Remove it from the router's queue, or it re-emits every retry
            // window (the repeated "opening link → delivered" loop).
            if let Some(h) = msg_hash {
                router.mark_outbound_delivered(&h);
            }
            let _ = events
                .send(NetEvent::Sys("[SYS] delivered (direct)".to_string()))
                .await;
        }
        DeliveryResult::Rejected {
            message,
            dest_hash,
            reason,
            msg_hash,
            ..
        }
        | DeliveryResult::Failed {
            message,
            dest_hash,
            reason,
            msg_hash,
            ..
        } => {
            // Drop the failed Direct attempt from the queue before cascading, so
            // the router doesn't also keep retrying it in parallel.
            if let Some(h) = msg_hash {
                router.mark_outbound_failed(&h);
            }
            // Cascade DIRECT -> PROPAGATED -> OPPORTUNISTIC. A failed Direct with a
            // propagation node configured re-queues as Propagated; a failed
            // Propagated (or no node) falls to a single opportunistic packet.
            let mut message = message;
            let try_propagated = message.method == DeliveryMethod::Direct
                && router.outbound_propagation_node.is_some();
            if try_propagated {
                let _ = events
                    .send(NetEvent::Sys(format!(
                        "[SYS] direct to {} failed ({reason}) — trying propagation",
                        hex::encode(dest_hash)
                    )))
                    .await;
                message.method = DeliveryMethod::Propagated;
                router.send(message);
            } else {
                let _ = events
                    .send(NetEvent::Sys(format!(
                        "[SYS] {} delivery failed ({reason}) — trying opportunistic",
                        hex::encode(dest_hash)
                    )))
                    .await;
                send_opportunistic(router, known, transport, events, message, dest_hash).await;
            }
        }
    }
}

/// Encrypt `plaintext` to the final recipient identity (cached from a delivery
/// announce). Shared by opportunistic and propagated packing.
fn encrypt_to_recipient(
    known: &KnownKeys,
    recipient: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, MessageError> {
    match known.get(&hex::encode(recipient)) {
        Some(pk) => Identity::from_public_key(pk)
            .ok()
            .and_then(|id| id.encrypt(plaintext, None).ok())
            .ok_or_else(|| MessageError::PackFailed("encrypt failed".to_string())),
        None => Err(MessageError::PackFailed("no identity key".to_string())),
    }
}

/// Decode an LXMF message downloaded from a propagation node. Mirrors lxmd's
/// `handle_propagation_downloaded_data`: if the blob is addressed to us, strip
/// the dest hash and decrypt with our identity; then unpack.
fn decode_propagation_payload(
    identity: &Identity,
    my_hash: &[u8; 16],
    data: &[u8],
) -> Option<LxMessage> {
    if data.len() < 16 {
        return None;
    }
    let unpack_data = if data[..16] == *my_hash {
        match identity.decrypt(&data[16..], None, false) {
            Ok(plaintext) => {
                let mut full = my_hash.to_vec();
                full.extend_from_slice(&plaintext);
                full
            }
            Err(_) => data.to_vec(),
        }
    } else {
        data.to_vec()
    };
    LxMessage::unpack(&unpack_data).ok()
}

/// Human label for an in-progress sync state (drives the pop-up). Idle /
/// Complete / Failed map to `None` — i.e. no pop-up.
fn sync_phase(state: PropagationClientState) -> Option<&'static str> {
    match state {
        PropagationClientState::LinkEstablishing => Some("contacting node"),
        PropagationClientState::LinkEstablished => Some("link established"),
        PropagationClientState::ListRequested => Some("requesting message list"),
        PropagationClientState::GetRequested => Some("downloading messages"),
        PropagationClientState::PurgeRequested => Some("finalizing"),
        _ => None,
    }
}

/// Start a sync from the configured propagation node, if one is set, idle, and
/// its identity is cached; otherwise request a path or report why not.
async fn try_sync(
    router: &mut LxmRouter,
    prop_client: &mut PropagationClient,
    known: &KnownKeys,
    last_path_request: &mut HashMap<[u8; 16], f64>,
    transport: &mpsc::Sender<TransportMessage>,
    events: &mpsc::Sender<NetEvent>,
) {
    let Some(node) = router.outbound_propagation_node else {
        let _ = events
            .send(NetEvent::Sys(
                "[SYS] no propagation node set (Network tab: Enter on one)".to_string(),
            ))
            .await;
        return;
    };
    if prop_client.state != PropagationClientState::Idle {
        return; // a sync is already running
    }
    if known.contains_key(&hex::encode(node)) {
        if prop_client.start_download() {
            let _ = events
                .send(NetEvent::Sys(format!("[SYS] syncing from {} ...", hex::encode(node))))
                .await;
        }
    } else if request_path(transport, last_path_request, node, now_secs()) {
        let _ = events
            .send(NetEvent::Sys(format!(
                "[SYS] propagation node {} identity unknown — requesting path (retry in {}s)",
                hex::encode(node),
                PATH_REQUEST_WAIT as u64
            )))
            .await;
    }
}

/// Encrypt, frame, and transmit one opportunistic LXMF packet — mirroring
/// `lxmd`'s opportunistic path. If the peer's key isn't cached yet, request a
/// path and re-queue so a later tick retries once an announce arrives.
async fn send_opportunistic(
    router: &mut LxmRouter,
    known: &KnownKeys,
    transport: &mpsc::Sender<TransportMessage>,
    events: &mpsc::Sender<NetEvent>,
    message: LxMessage,
    dest_hash: [u8; 16],
) {
    let mut missing = false;
    let packed = message.pack_opportunistic_encrypted(|plaintext| match known.get(&hex::encode(dest_hash)) {
        Some(pk) => Identity::from_public_key(pk)
            .ok()
            .and_then(|peer| peer.encrypt(plaintext, None).ok())
            .ok_or_else(|| MessageError::PackFailed("encrypt failed".to_string())),
        None => {
            missing = true;
            Err(MessageError::PackFailed("no identity key".to_string()))
        }
    });

    let payload = match packed {
        Ok(p) => p,
        Err(_) if missing => {
            let _ = transport
                .send(TransportMessage::RequestPath {
                    destination_hash: dest_hash,
                })
                .await;
            router.send(message); // retried by a later tick once the key arrives
            let _ = events
                .send(NetEvent::Sys(format!(
                    "[SYS] no key for {} yet — requested path, will retry",
                    hex::encode(dest_hash)
                )))
                .await;
            return;
        }
        Err(e) => {
            let _ = events
                .send(NetEvent::Sys(format!("[SYS] pack failed: {e}")))
                .await;
            return;
        }
    };

    let header = rns_wire::header::PacketHeader {
        flags: rns_wire::flags::PacketFlags {
            header_type: rns_wire::flags::HeaderType::Header1,
            context_flag: false,
            transport_type: rns_wire::flags::TransportType::Broadcast,
            destination_type: rns_wire::flags::DestinationType::Single,
            packet_type: rns_wire::flags::PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: dest_hash,
        context: rns_wire::context::PacketContext::None,
    };
    let mut raw = header.pack();
    raw.extend_from_slice(&payload);

    if raw.len() > rns_wire::constants::MTU {
        let _ = events
            .send(NetEvent::Sys(format!(
                "[SYS] message to {} too large for opportunistic (link delivery is Phase 4)",
                hex::encode(dest_hash)
            )))
            .await;
        return;
    }

    let _ = transport
        .send(TransportMessage::Outbound(OutboundRequest {
            raw: bytes::Bytes::from(raw),
            destination_hash: dest_hash,
        }))
        .await;
    let _ = events
        .send(NetEvent::Sys(format!("[SYS] sent to {}", hex::encode(dest_hash))))
        .await;
}

/// Build and transmit an announce for our delivery destination.
async fn announce(
    transport: &mpsc::Sender<TransportMessage>,
    delivery: &mut Destination,
    identity: &Identity,
    display_name: &str,
    events: &mpsc::Sender<NetEvent>,
) {
    let app_data = lxmf_core::handlers::get_announce_app_data(Some(display_name), None);
    match delivery.announce_packet(identity, Some(&app_data), None, false, None, now_secs()) {
        Ok(raw) => {
            let _ = transport
                .send(TransportMessage::Outbound(OutboundRequest {
                    raw: bytes::Bytes::from(raw),
                    destination_hash: delivery.hash,
                }))
                .await;
            let _ = events.send(NetEvent::Sys("[SYS] announced".to_string())).await;
        }
        Err(e) => {
            let _ = events
                .send(NetEvent::Sys(format!("[SYS] announce failed: {e:?}")))
                .await;
        }
    }
}

/// Split a `host:port` string, defaulting to [`DEFAULT_HUB_PORT`] if the port
/// is absent or non-numeric.
fn parse_hostport(s: &str) -> (String, u16) {
    match s.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(DEFAULT_HUB_PORT)),
        None => (s.to_string(), DEFAULT_HUB_PORT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rns_runtime::config::Config;

    #[test]
    fn generated_config_parses() {
        // The INI we hand-write must satisfy Reticulum's own parser, or
        // `reticulum::init` would reject it at startup.

        // LAN-only default: just the AutoInterface.
        let lan = Config::parse(&rns_config(None)).expect("LAN config must parse");
        let subs = lan.subsections("interfaces");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].1.get("type"), Some("AutoInterface"));

        // With a hub: AutoInterface + the TCP client.
        let with_hub = Config::parse(&rns_config(Some(("example.net", 4965))))
            .expect("hub config must parse");
        let subs = with_hub.subsections("interfaces");
        assert_eq!(subs.len(), 2);
        let hub = with_hub
            .subsection("interfaces", "Hub")
            .expect("Hub interface present");
        assert_eq!(hub.get("type"), Some("TCPClientInterface"));
        assert_eq!(hub.get("target_host"), Some("example.net"));
        assert_eq!(hub.get("target_port"), Some("4965"));
    }

    #[test]
    fn parse_hash_accepts_16_bytes_only() {
        assert_eq!(
            parse_hash("00112233445566778899aabbccddeeff").unwrap(),
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
        assert!(parse_hash("abcd").is_err(), "too short");
        assert!(parse_hash("zz").is_err(), "not hex");
    }

    #[test]
    fn hostport_split() {
        assert_eq!(parse_hostport("host:1234"), ("host".to_string(), 1234));
        assert_eq!(
            parse_hostport("host"),
            ("host".to_string(), DEFAULT_HUB_PORT)
        );
        assert_eq!(
            parse_hostport("bad:port"),
            ("bad".to_string(), DEFAULT_HUB_PORT),
            "non-numeric port falls back to default"
        );
    }

    #[test]
    fn request_path_throttled_to_one_per_window() {
        let (tx, _rx) = mpsc::channel::<TransportMessage>(8);
        let mut last = HashMap::new();
        let dest = [1u8; 16];
        assert!(request_path(&tx, &mut last, dest, 100.0), "first send");
        assert!(
            !request_path(&tx, &mut last, dest, 100.0 + PATH_REQUEST_WAIT - 1.0),
            "suppressed within the window"
        );
        assert!(
            request_path(&tx, &mut last, dest, 100.0 + PATH_REQUEST_WAIT + 1.0),
            "sent again after the window"
        );
    }

    #[test]
    fn learn_flags_dirty_only_on_change() {
        let mut known = KnownKeys::new();
        let mut dirty = false;
        learn(&mut known, &mut dirty, [1u8; 16], [2u8; 64]);
        assert!(dirty, "new identity is a change");
        dirty = false;
        learn(&mut known, &mut dirty, [1u8; 16], [2u8; 64]);
        assert!(!dirty, "identical re-announce is not a change");
        learn(&mut known, &mut dirty, [1u8; 16], [3u8; 64]);
        assert!(dirty, "rotated key is a change");
    }

    #[test]
    fn sync_phase_maps_active_states_only() {
        assert!(sync_phase(PropagationClientState::Idle).is_none());
        assert!(sync_phase(PropagationClientState::Complete).is_none());
        assert!(sync_phase(PropagationClientState::Failed).is_none());
        assert_eq!(
            sync_phase(PropagationClientState::GetRequested),
            Some("downloading messages")
        );
    }

    #[test]
    fn known_identities_round_trip() {
        let mut path = std::env::temp_dir();
        path.push("foxhole_known_identities_test");
        let _ = std::fs::remove_file(&path);

        let mut known = KnownKeys::new();
        known.insert("aa".repeat(16), [7u8; 64]);
        save_known(&path, &known).unwrap();
        assert_eq!(load_known(&path), known);

        let _ = std::fs::remove_file(&path);
    }
}
