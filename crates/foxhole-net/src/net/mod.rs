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
//! This module is the *wiring*: bring-up, then a single `select!` loop that
//! routes each source to the piece that owns it. The pieces themselves are
//! split by concern:
//!
//! - [`endpoint`] — our identity, inbox destination, and message framing.
//! - [`peers`] — announce-learned identity keys, hop counts, path throttling.
//! - [`outbound`] — router + link delivery + propagation, and delivery status.
//! - [`inbound`] — decoded messages → UI events (thread, telemetry, intel).
//! - [`nomad`] — Nomad Network node discovery and page fetching.
//! - [`discovery`] — operator path probes and interface statistics.
//! - [`codec`] / [`telemetry`] — pure wire-format helpers.

mod codec;
mod discovery;
mod endpoint;
mod inbound;
mod nomad;
mod outbound;
mod peers;
mod telemetry;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use rns_identity::identity::Identity;
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::link_client::LinkClient;
use rns_runtime::link_manager::LinkManager;
use rns_runtime::reticulum;
use rns_transport::link_messages::DestinationEvent;
use rns_transport::messages::{
    AnnounceHandlerEvent, TransportMessage, TransportQuery, TransportQueryResponse,
};

use foxhole_core::app::{NetCommand, NetEvent, Outbound, PeerKind};
use foxhole_core::config::{Config, config_dir};

use codec::{parse_hash, parse_hostport};
use discovery::PathProbes;
use endpoint::{Endpoint, LXMF_DELIVERY};
use nomad::Nomad;
use outbound::Dispatcher;
use peers::PeerCache;

/// LXMF propagation-node aspect — the destination that stores messages for
/// offline peers.
const LXMF_PROPAGATION: &str = "lxmf.propagation";

/// Re-announce our delivery destination on this cadence so peers keep a path.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(300);

/// Cadence for draining the router's outbound queue (retries, deferred stamps,
/// sends unblocked by a freshly learned key/path).
const SEND_INTERVAL: Duration = Duration::from_secs(1);

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

/// The five inbound streams the link manager feeds us.
struct Inbox {
    /// Opportunistic (non-link) packets, still encrypted.
    raw: mpsc::Receiver<Vec<u8>>,
    /// Direct (link) payloads, already decrypted by the link manager.
    ///
    /// Unbounded because `set_link_packet_channel` demands it: the link manager
    /// pushes decrypted link data from inside its own select loop and cannot
    /// park on a full queue without stalling every other link.
    packets: mpsc::UnboundedReceiver<(Vec<u8>, [u8; 16])>,
    /// Large messages arriving as a completed resource over a link.
    resources: mpsc::Receiver<(Vec<u8>, [u8; 16])>,
    /// Inbound links reaching the established state.
    links_up: mpsc::Receiver<[u8; 16]>,
    /// Peers identifying themselves on an inbound link.
    identified: mpsc::Receiver<([u8; 16], [u8; 16])>,
}

/// The propagation-sync progress pop-up's state.
#[derive(Default)]
struct SyncBanner {
    /// Whether the pop-up is currently shown.
    showing: bool,
    /// Set when the operator dismisses an in-progress sync (`CancelSync`): keeps
    /// the pop-up hidden while the client winds down on its own watchdog,
    /// clearing once it returns to Idle so a later sync shows normally.
    suppressed: bool,
}

impl SyncBanner {
    /// Drive the pop-up from the client's live phase (`None` = not syncing).
    async fn update(&mut self, phase: Option<&str>, ticks: u32, events: &mpsc::Sender<NetEvent>) {
        match phase {
            // Operator dismissed this run: stay quiet until it winds down.
            Some(_) if self.suppressed => {}
            Some(p) => {
                let spin = ['|', '/', '-', '\\'][(ticks % 4) as usize];
                let _ = events
                    .send(NetEvent::Sync(Some(format!("{spin} {p}\u{2026}"))))
                    .await;
                self.showing = true;
            }
            None if self.showing => {
                self.showing = false;
                let _ = events.send(NetEvent::Sync(None)).await;
                let _ = events
                    .send(NetEvent::Sys("[SYS] propagation sync finished".to_string()))
                    .await;
            }
            // Client back to Idle — re-arm so the next sync shows again.
            None => self.suppressed = false,
        }
    }

    /// Drop the pop-up at once and stop re-asserting it; the client
    /// finishes/aborts in the background (its own timeout).
    async fn cancel(&mut self, phase: Option<&str>, events: &mpsc::Sender<NetEvent>) {
        if !self.showing && phase.is_none() {
            return;
        }
        self.suppressed = true;
        self.showing = false;
        let _ = events.send(NetEvent::Sync(None)).await;
        let _ = events
            .send(NetEvent::Sys(
                "[SYS] propagation sync canceled by operator".to_string(),
            ))
            .await;
    }
}

async fn run_inner(
    events: &mpsc::Sender<NetEvent>,
    mut outbound_rx: mpsc::Receiver<Outbound>,
    mut command_rx: mpsc::Receiver<NetCommand>,
    config: Config,
) -> Result<(), String> {
    // --- Identity + inbox destination ------------------------------------------
    let cfg = config_dir();
    std::fs::create_dir_all(&cfg).map_err(|e| format!("config dir: {e}"))?;
    let id_path = cfg.join("identity");
    let mut endpoint = Endpoint::open(&id_path, &config.display_name)?;
    sys(
        events,
        format!("[SYS] identity {}", hex::encode(endpoint.identity().hash)),
    )
    .await;

    // Hand the conversation-store key to `main` early (before any traffic), so it
    // can decrypt history before live messages start appending.
    if let Some(key) = crate::store::derive_key(endpoint.identity()) {
        let _ = events.send(NetEvent::StoreKey(key)).await;
    }

    // --- Transport bring-up -----------------------------------------------------
    let rns_dir = cfg.join("reticulum");
    prepare_rns_config(&rns_dir, &config, events).await?;

    sys(events, "[SYS] bringing up transport ...".to_string()).await;
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
    // Initiator-side client for Nomad Network page fetches. `Identity` is not
    // `Clone`, but `LinkClient` is — so reload a second identity copy here and
    // clone the client into each fetch task. Built from the same on-disk file.
    let mut nomad = Nomad::new(LinkClient::new(
        handle.transport_tx.clone(),
        Identity::from_file(&id_path).map_err(|e| format!("load identity (links): {e:?}"))?,
    ));
    handle
        .enable_on_network_discovery(Arc::new(
            lxmf_core::discovery_stamper::LxmfDiscoveryStamper::default(),
        ))
        .await;
    sys(events, "[SYS] transport online".to_string()).await;

    // --- Register inbox + announce handlers ------------------------------------
    let delivery_rx = register_destination(&transport, endpoint.hash).await?;
    let mut peer_rx = register_announces(&transport, LXMF_DELIVERY).await?;
    let mut node_rx = register_announces(&transport, LXMF_PROPAGATION).await?;

    sys(
        events,
        format!(
            "[SYS] {LXMF_DELIVERY} {} registered",
            hex::encode(endpoint.hash)
        ),
    )
    .await;
    let _ = events
        .send(NetEvent::Local(hex::encode(endpoint.hash)))
        .await;

    let mut rx = spawn_link_manager(&transport, delivery_rx, &endpoint)?;

    // --- Outbound router + link delivery ---------------------------------------
    // Identities persist across restarts so we don't have to re-hear an announce
    // before we can reach a known peer/node (the cause of the post-restart
    // "identity unknown" loop). `hops`/stamp costs are re-learned cheaply.
    let peers = PeerCache::load(cfg.join("known_identities"));
    let mut tx = Dispatcher::new(
        endpoint.identity(),
        peers,
        transport.clone(),
        events.clone(),
    );
    let mut probes = PathProbes::default();
    let mut banner = SyncBanner::default();
    let mut ticks: u32 = 0;

    seed_from_announce_cache(&handle, &mut tx).await;
    if tx.peers.len() > 0 {
        sys(
            events,
            format!("[SYS] {} known identities loaded", tx.peers.len()),
        )
        .await;
    }

    // Apply the persisted propagation node, if any.
    if let Some(node) = config
        .propagation_node
        .as_deref()
        .and_then(|s| parse_hash(s).ok())
    {
        tx.set_propagation_node(Some(node));
        sys(
            events,
            format!("[SYS] propagation node {} (from config)", hex::encode(node)),
        )
        .await;
    }

    // Announce ourselves now and on a timer so peers learn a path to us.
    endpoint.announce(&transport, events).await;
    let mut announce_tick = tokio::time::interval(ANNOUNCE_INTERVAL);
    announce_tick.tick().await; // consume the immediate first tick
    let mut send_tick = tokio::time::interval(SEND_INTERVAL);
    send_tick.tick().await; // consume the immediate first tick

    // --- Event loop -------------------------------------------------------------
    loop {
        tokio::select! {
            // Opportunistic (non-link) inbound: decrypt with our identity.
            Some(raw) = rx.raw.recv() => {
                let decoded = endpoint.decode_opportunistic(&raw);
                tx.answer_telemetry(&endpoint, &config, decoded.as_ref()).await;
                inbound::deliver(events, "opportunistic", raw.len(), decoded).await;
            }
            // Direct (link) inbound: already decrypted by the link manager.
            Some((data, _link)) = rx.packets.recv() => {
                let decoded = endpoint.decode_link(&data);
                tx.answer_telemetry(&endpoint, &config, decoded.as_ref()).await;
                inbound::deliver(events, "direct", data.len(), decoded).await;
            }
            // Large messages arriving as a completed resource over a link.
            Some((data, _link)) = rx.resources.recv() => {
                let decoded = endpoint.decode_link(&data);
                tx.answer_telemetry(&endpoint, &config, decoded.as_ref()).await;
                inbound::deliver(events, "direct(resource)", data.len(), decoded).await;
            }
            Some(_link) = rx.links_up.recv() => {
                sys(events, "[SYS] inbound link established".to_string()).await;
            }
            Some((_link, ident)) = rx.identified.recv() => {
                sys(events, format!(
                    "[SYS] peer {}\u{2026} identified on inbound link",
                    &hex::encode(ident)[..16]
                )).await;
            }
            Some(ev) = peer_rx.recv() => {
                // Cache the peer's key + hop count so we can reach it later (path
                // responses carry these too, hence no is_path_response guard here).
                learn_announce(&mut tx, &ev);
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
                learn_announce(&mut tx, &ev);
                if let Some(cost) = ev
                    .app_data
                    .as_deref()
                    .and_then(lxmf_core::handlers::pn_stamp_cost_from_app_data)
                {
                    tx.set_stamp_cost(ev.destination_hash, cost);
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
                tx.send(&endpoint, &out).await;
            }
            Some(cmd) = command_rx.recv() => {
                handle_command(cmd, &endpoint, &mut tx, &mut probes, &nomad, &mut banner, events).await;
            }
            _ = send_tick.tick() => {
                ticks = ticks.wrapping_add(1);

                // Advance in-flight link deliveries.
                tx.tick_links().await;

                // Advance an in-progress propagation sync + surface downloads.
                // Sync is on-demand only (no automatic polling — bandwidth is
                // precious off-grid); it is started by Ctrl+R / the Network tab.
                for data in tx.tick_sync() {
                    let decoded = endpoint.decode_propagated(&data);
                    inbound::deliver(events, "propagation", data.len(), decoded).await;
                }
                banner.update(tx.sync_phase(), ticks, events).await;

                // Persist newly learned identities periodically (debounced).
                if ticks.is_multiple_of(30) {
                    tx.peers.flush();
                }

                probes.resolve(&handle, events).await;

                // Poll the recent-announce cache for Nomad Network nodes (~10 s).
                if ticks.is_multiple_of(10) {
                    nomad.discover(&handle, events).await;
                }

                // Refresh the Interfaces tab from the transport's interface
                // stats (rnstatus-style) every ~2 s — a cheap in-process query.
                if ticks.is_multiple_of(2) {
                    discovery::interfaces(&handle, events).await;
                }

                tx.dispatch().await;
            }
            _ = announce_tick.tick() => {
                endpoint.announce(&transport, events).await;
            }
            _ = shutdown.wait() => break,
            else => break,
        }
    }

    Ok(())
}

/// Push one line to the operator's Log tab.
async fn sys(events: &mpsc::Sender<NetEvent>, line: String) {
    let _ = events.send(NetEvent::Sys(line)).await;
}

/// Fold an announce (or path response) into the peer cache.
fn learn_announce(tx: &mut Dispatcher, ev: &AnnounceHandlerEvent) {
    if let Some(pk) = ev.public_key {
        tx.peers.learn(ev.destination_hash, pk);
    }
    tx.peers.set_hops(ev.destination_hash, ev.hops);
}

/// Write the default Reticulum INI on first run, respecting a hand-edited one.
/// The hub comes from `FOXHOLE_HUB=host[:port]` (env wins) or the config file;
/// with neither we run LAN-only via AutoInterface (no public hub needed — the
/// project testnet is decommissioned, so there is no safe baked-in default).
async fn prepare_rns_config(
    rns_dir: &std::path::Path,
    config: &Config,
    events: &mpsc::Sender<NetEvent>,
) -> Result<(), String> {
    std::fs::create_dir_all(rns_dir).map_err(|e| format!("rns dir: {e}"))?;
    let cfg_file = rns_dir.join("config");
    if cfg_file.exists() {
        sys(
            events,
            format!("[SYS] using existing RNS config at {}", cfg_file.display()),
        )
        .await;
        return Ok(());
    }

    let hub = std::env::var("FOXHOLE_HUB")
        .ok()
        .or_else(|| config.hub.clone())
        .map(|s| parse_hostport(&s));
    let ini = rns_config(hub.as_ref().map(|(h, p)| (h.as_str(), *p)));
    std::fs::write(&cfg_file, ini).map_err(|e| format!("write rns config: {e}"))?;
    match &hub {
        Some((h, p)) => {
            sys(
                events,
                format!("[SYS] interfaces: AutoInterface (LAN) + TCP hub {h}:{p}"),
            )
            .await;
        }
        None => {
            sys(
                events,
                "[SYS] interfaces: AutoInterface (LAN only)".to_string(),
            )
            .await;
            sys(
                events,
                "[SYS] set FOXHOLE_HUB=host:port for an internet hub, or edit the RNS config"
                    .to_string(),
            )
            .await;
        }
    }
    Ok(())
}

/// Register our LXMF inbox with the transport and take its event stream.
async fn register_destination(
    transport: &mpsc::Sender<TransportMessage>,
    hash: [u8; 16],
) -> Result<mpsc::Receiver<DestinationEvent>, String> {
    let (delivery_tx, delivery_rx) = mpsc::channel::<DestinationEvent>(256);
    transport
        .send(TransportMessage::RegisterDestination {
            hash,
            app_name: LXMF_DELIVERY.to_string(),
            delivery_tx: Some(delivery_tx),
        })
        .await
        .map_err(|_| "transport closed".to_string())?;
    Ok(delivery_rx)
}

/// Subscribe to announces (and path responses) for one destination aspect.
async fn register_announces(
    transport: &mpsc::Sender<TransportMessage>,
    aspect: &str,
) -> Result<mpsc::Receiver<AnnounceHandlerEvent>, String> {
    let (tx, rx) = mpsc::channel::<AnnounceHandlerEvent>(256);
    transport
        .send(TransportMessage::RegisterAnnounceHandler {
            aspect_filter: Some(aspect.to_string()),
            receive_path_responses: true,
            callback_tx: tx,
        })
        .await
        .map_err(|_| "transport closed".to_string())?;
    Ok(rx)
}

/// Hand our destination's event stream to `rns-runtime`'s link manager, which
/// performs the inbound link handshake (Direct delivery — what nomadnet uses)
/// and hands us decrypted payloads. Mirrors lxmd's wiring.
fn spawn_link_manager(
    transport: &mpsc::Sender<TransportMessage>,
    delivery_rx: mpsc::Receiver<DestinationEvent>,
    endpoint: &Endpoint,
) -> Result<Inbox, String> {
    let mut link_mgr = LinkManager::with_destination(
        transport.clone(),
        delivery_rx,
        endpoint.identity(),
        LXMF_DELIVERY,
        Some(endpoint.signing_key()?),
    );
    let (raw_tx, raw) = mpsc::channel::<Vec<u8>>(256);
    let (packet_tx, packets) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
    let (resource_tx, resources) = mpsc::channel::<(Vec<u8>, [u8; 16])>(64);
    let (up_tx, links_up) = mpsc::channel::<[u8; 16]>(64);
    let (ident_tx, identified) = mpsc::channel::<([u8; 16], [u8; 16])>(64);
    link_mgr.set_inbound_raw_channel(raw_tx);
    link_mgr.set_link_packet_channel(packet_tx);
    link_mgr.set_resource_completed_channel(resource_tx);
    link_mgr.set_link_established_channel(up_tx);
    link_mgr.set_link_identified_channel(ident_tx);
    tokio::spawn(link_mgr.run());
    Ok(Inbox {
        raw,
        packets,
        resources,
        links_up,
        identified,
    })
}

/// Seed identities/hops/stamp costs from the transport's own recent-announce
/// cache — it may already know peers/nodes we haven't re-heard this session.
async fn seed_from_announce_cache(handle: &reticulum::ReticulumHandle, tx: &mut Dispatcher) {
    let Some(TransportQueryResponse::Announces(entries)) = handle
        .query_control(TransportQuery::GetRecentAnnounces)
        .await
    else {
        return;
    };
    for e in entries {
        if let Some(pk) = e.public_key {
            tx.peers.learn(e.dest_hash, pk);
        }
        tx.peers.set_hops(e.dest_hash, e.hops);
        if let Some(cost) = e
            .app_data
            .as_deref()
            .and_then(lxmf_core::handlers::pn_stamp_cost_from_app_data)
        {
            tx.set_stamp_cost(e.dest_hash, cost);
        }
    }
}

/// Act on one command from the UI.
async fn handle_command(
    cmd: NetCommand,
    endpoint: &Endpoint,
    tx: &mut Dispatcher,
    probes: &mut PathProbes,
    nomad: &Nomad,
    banner: &mut SyncBanner,
    events: &mpsc::Sender<NetEvent>,
) {
    match cmd {
        NetCommand::SetPropagationNode(node) => {
            let parsed = node.as_deref().and_then(|s| parse_hash(s).ok());
            tx.set_propagation_node(parsed);
            match parsed {
                Some(n) => {
                    sys(
                        events,
                        format!("[SYS] propagation node set to {}", hex::encode(n)),
                    )
                    .await;
                }
                None => sys(events, "[SYS] propagation node cleared".to_string()).await,
            }
        }
        NetCommand::SyncNow => tx.try_sync().await,
        NetCommand::CancelSync => banner.cancel(tx.sync_phase(), events).await,
        NetCommand::RequestTelemetry(peer) => tx.request_telemetry(endpoint, &peer).await,
        NetCommand::RequestPath(hex) => match parse_hash(&hex) {
            Ok(dest) => {
                // Operator-initiated: fire the path request directly (bypass the
                // background per-window throttle), then resolve on a later tick
                // once the response is in.
                tx.probe_path(dest);
                probes.arm(dest);
            }
            Err(e) => sys(events, format!("[SYS] path probe: bad address: {e}")).await,
        },
        NetCommand::FetchPage {
            identity,
            path,
            fields,
        } => nomad.fetch(identity, path, fields, events).await,
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
        let with_hub =
            Config::parse(&rns_config(Some(("example.net", 4965)))).expect("hub config must parse");
        let subs = with_hub.subsections("interfaces");
        assert_eq!(subs.len(), 2);
        let hub = with_hub
            .subsection("interfaces", "Hub")
            .expect("Hub interface present");
        assert_eq!(hub.get("type"), Some("TCPClientInterface"));
        assert_eq!(hub.get("target_host"), Some("example.net"));
        assert_eq!(hub.get("target_port"), Some("4965"));
    }
}
