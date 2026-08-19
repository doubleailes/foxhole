//! The outbound half of the stack: the LXMF router, link delivery, the
//! propagation client, and the caches they all consult.
//!
//! These pieces are inseparable — planning a Direct send needs the peer cache,
//! a failed link cascades into propagation and then a single opportunistic
//! packet, and every outcome has to reach the UI as a status update. Bundling
//! them in one [`Dispatcher`] keeps that shared state in a single owner instead
//! of threading a dozen `&mut` caches through every call.

use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc;

use lxmf_core::constants::DeliveryMethod;
use lxmf_core::link_delivery::{DeliveryResult, LinkDeliveryManager};
use lxmf_core::message::{LxMessage, MessageError};
use lxmf_core::propagation_client::{PropagationClient, PropagationClientState};
use lxmf_core::router::{
    DirectDeliveryPlanInput, DirectReusableLinkState, DirectRouteSnapshot, LxmRouter,
    OutboundAction, RouterConfig,
};
use rns_identity::identity::Identity;
use rns_transport::messages::{OutboundRequest, TransportMessage};

use foxhole_core::app::{MsgStatus, NetEvent, Outbound};
use foxhole_core::config::Config;

use super::endpoint::Endpoint;
use super::now_secs;
use super::peers::{PATH_REQUEST_WAIT, PeerCache};
use super::telemetry;

/// Tracks outbound messages in flight so delivery outcomes can be reported back
/// to the UI by the message's correlation id. Keyed by the LXMF message hash
/// (which the router/link-delivery results also carry).
#[derive(Default)]
struct StatusTracker {
    /// msg hash -> the UI entry id that should reflect its status.
    ids: HashMap<[u8; 32], u64>,
    /// Hashes being delivered via a propagation node (so `Complete` reads as
    /// `Propagated` rather than `Delivered`).
    propagated: HashSet<[u8; 32]>,
}

impl StatusTracker {
    /// The entry id for a message hash, if we're tracking it.
    fn id_for(&self, hash: Option<[u8; 32]>) -> Option<u64> {
        hash.and_then(|h| self.ids.get(&h).copied())
    }

    /// Stop tracking a now-terminal message.
    fn forget(&mut self, hash: &[u8; 32]) {
        self.ids.remove(hash);
        self.propagated.remove(hash);
    }
}

/// Everything needed to get a message out and report what became of it.
pub(crate) struct Dispatcher {
    router: LxmRouter,
    links: LinkDeliveryManager,
    prop: PropagationClient,
    /// Announce-learned identity keys, hop counts, and path-request throttle.
    pub(crate) peers: PeerCache,
    tracker: StatusTracker,
    transport: mpsc::Sender<TransportMessage>,
    events: mpsc::Sender<NetEvent>,
}

impl Dispatcher {
    /// Wire up the router, link delivery, and propagation client against our
    /// identity and the live transport.
    pub(crate) fn new(
        identity: &Identity,
        peers: PeerCache,
        transport: mpsc::Sender<TransportMessage>,
        events: mpsc::Sender<NetEvent>,
    ) -> Self {
        let mut router = LxmRouter::new(RouterConfig::default());
        router.set_transport(transport.clone());
        let links = LinkDeliveryManager::new(
            transport.clone(),
            Some(identity.get_public_key()),
            identity.get_signing_key(),
        );
        let prop = PropagationClient::new(
            transport.clone(),
            Some(identity.get_public_key()),
            identity.get_signing_key(),
        );
        Self {
            router,
            links,
            prop,
            peers,
            tracker: StatusTracker::default(),
            transport,
            events,
        }
    }

    // --- Configuration ---------------------------------------------------------

    /// Record a propagation node's advertised stamp cost (from its announce).
    pub(crate) fn set_stamp_cost(&mut self, dest: [u8; 16], cost: u8) {
        self.router.set_stamp_cost(dest, cost);
    }

    /// Point outbound propagation at `node` (or clear it), for both the router's
    /// planning and the sync client.
    pub(crate) fn set_propagation_node(&mut self, node: Option<[u8; 16]>) {
        self.router.set_outbound_propagation_node(node);
        if let Some(n) = node {
            self.prop.set_propagation_node(n);
        }
    }

    /// Fire an operator-initiated path request immediately, bypassing the
    /// background per-window throttle (but arming it, so the background path
    /// doesn't pile another request on top).
    pub(crate) fn probe_path(&mut self, dest: [u8; 16]) {
        let _ = self.transport.try_send(TransportMessage::RequestPath {
            destination_hash: dest,
        });
        self.peers.note_path_request(dest, now_secs());
    }

    // --- Sending ---------------------------------------------------------------

    /// Build, queue, and dispatch one composed message, correlating its hash with
    /// the UI entry id so status updates can find their way back.
    pub(crate) async fn send(&mut self, endpoint: &Endpoint, out: &Outbound) {
        match endpoint.build_message(out) {
            Ok(msg) => {
                if let Some(h) = msg.hash {
                    self.tracker.ids.insert(h, out.id);
                }
                self.route(msg).await;
                self.dispatch().await;
            }
            Err(e) => self.sys(format!("[SYS] send: {e}")).await,
        }
    }

    /// If `decoded` is a telemetry request and the operator has a configured
    /// position, send our location back to the requester. No-op otherwise (e.g. a
    /// non-request message, or no `lat`/`lon` set in the config). Runs in the
    /// inbound path, so the same dispatch machinery as a normal send is reused.
    pub(crate) async fn answer_telemetry(
        &mut self,
        endpoint: &Endpoint,
        config: &Config,
        decoded: Option<&LxMessage>,
    ) {
        let Some(msg) = decoded else { return };
        if !telemetry::is_requested(msg) {
            return;
        }
        let Some(pos) = config.operator_pos() else {
            self.sys(
                "[SYS] telemetry request ignored (no operator position — set lat/lon in foxhole.conf)"
                    .to_string(),
            )
            .await;
            return;
        };
        match endpoint.build_telemetry_reply(msg.source_hash, pos.lat, pos.lon) {
            Ok(reply) => {
                let dest = hex::encode(reply.destination_hash);
                self.route(reply).await;
                self.dispatch().await;
                self.sys(format!("[SYS] answered telemetry request from {dest}"))
                    .await;
            }
            Err(e) => self.sys(format!("[SYS] telemetry reply: {e}")).await,
        }
    }

    /// Queue a message on the router, reporting an immediate routing rejection.
    ///
    /// `try_send` refuses a message it cannot route at all — currently a
    /// `Propagated` send with no propagation node configured, or a failed ticket
    /// preparation — and has already marked it failed and fired its callback
    /// before returning. So there is nothing to retry here; the operator just
    /// needs to see why it never left, instead of the silent drop the deprecated
    /// `send` gave us.
    async fn route(&mut self, message: LxMessage) {
        if let Err(e) = self.router.try_send(message) {
            self.sys(format!("[ERR] not routable: {e}")).await;
        }
    }

    /// Drain the router's outbound queue and act on each decision. Direct
    /// messages are handed to the link-delivery manager; if that can't start (no
    /// path yet) we request a path and re-queue. Opportunistic is the
    /// single-packet last resort.
    pub(crate) async fn dispatch(&mut self) {
        // The router's Direct planning needs to know, per message, whether we have
        // the peer's identity and a route. We supply both from our announce caches.
        let peers = &self.peers;
        let actions = self.router.process_outbound_with_direct(|message, _now| {
            let dest = message.destination_hash;
            DirectDeliveryPlanInput {
                identity_known: peers.knows(&dest),
                route: peers.hops(&dest).map(|h| DirectRouteSnapshot {
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
            self.apply(action).await;
        }
        self.router.process_deferred_stamps();
    }

    /// Carry out one router decision.
    async fn apply(&mut self, action: OutboundAction) {
        match action {
            OutboundAction::DeliverDirect { message, dest_hash }
            | OutboundAction::PlanDirect {
                message, dest_hash, ..
            } => self.deliver_direct(message, dest_hash).await,
            OutboundAction::DeliverOpportunistic { message, dest_hash } => {
                self.send_opportunistic(message, dest_hash).await;
            }
            OutboundAction::DeliverPropagated { message, prop_hash } => {
                self.deposit(message, prop_hash).await
            }
            OutboundAction::Failed(m) | OutboundAction::Expired(m) => {
                self.finish(m.hash, MsgStatus::Failed).await;
                self.sys(format!(
                    "[SYS] delivery to {} failed",
                    hex::encode(m.destination_hash)
                ))
                .await;
            }
        }
    }

    /// Open a link to the peer and deliver over it (the preferred method).
    async fn deliver_direct(&mut self, message: LxMessage, dest_hash: [u8; 16]) {
        let hop = self.peers.hops(&dest_hash).unwrap_or(1);
        match self.links.start_delivery(message, dest_hash, hop) {
            Ok(_link_id) => {
                self.sys(format!(
                    "[SYS] opening link to {} ...",
                    hex::encode(dest_hash)
                ))
                .await;
            }
            // No path/identity yet: request a path (throttled) and defer the next
            // attempt so we don't loop every tick.
            Err(fail) => {
                self.requeue_after_path_request(*fail.message, dest_hash, "no path to peer")
                    .await;
            }
        }
    }

    /// Deposit a message with a propagation node for later pickup. Needs both the
    /// node's identity (to link) and the recipient's (a deposit is still
    /// end-to-end encrypted); a missing key defers rather than drops.
    async fn deposit(&mut self, message: LxMessage, prop_hash: [u8; 16]) {
        if !self.peers.knows(&prop_hash) {
            self.requeue_after_path_request(
                message,
                prop_hash,
                "propagation node identity unknown",
            )
            .await;
            return;
        }
        let recipient = message.destination_hash;
        if !self.peers.knows(&recipient) {
            // If we have never heard their announce, ask for a path and retry —
            // don't drop the message (which would strand it at `[sending]`).
            self.requeue_after_path_request(
                message,
                recipient,
                "recipient identity unknown — can't encrypt",
            )
            .await;
            return;
        }

        let mut message = message;
        let target_cost = self.router.get_stamp_cost(&prop_hash).unwrap_or(0);
        let msg_hash = message.hash;
        let packed = message.pack_propagated_encrypted_with_stamp(
            |plaintext| encrypt_to(&self.peers, &recipient, plaintext),
            target_cost,
        );
        let wrapper = match packed {
            Ok((wrapper, _tid, _value)) => wrapper,
            Err(e) => {
                // Recipient key was present, so this is a genuine packing error
                // (not a transient unknown-identity) → terminal.
                self.finish(msg_hash, MsgStatus::Failed).await;
                self.sys(format!("[SYS] propagation pack failed: {e}"))
                    .await;
                return;
            }
        };

        let hop = self.peers.hops(&prop_hash).unwrap_or(4);
        match self
            .links
            .start_packed_delivery(message, prop_hash, hop, wrapper, false)
        {
            Ok(_) => {
                // Mark the hash so its Complete reads as Propagated.
                if let Some(h) = msg_hash {
                    self.tracker.propagated.insert(h);
                }
                self.sys(format!(
                    "[SYS] depositing to propagation node {} ...",
                    hex::encode(prop_hash)
                ))
                .await;
            }
            Err(fail) => {
                self.requeue_after_path_request(
                    *fail.message,
                    prop_hash,
                    "no path to propagation node",
                )
                .await;
            }
        }
    }

    /// Encrypt, frame, and transmit one opportunistic LXMF packet — mirroring
    /// `lxmd`'s opportunistic path. If the peer's key isn't cached yet, request a
    /// path and re-queue so a later tick retries once an announce arrives.
    async fn send_opportunistic(&mut self, message: LxMessage, dest_hash: [u8; 16]) {
        let msg_hash = message.hash;
        let mut missing = false;
        let packed = message.pack_opportunistic_encrypted(|plaintext| {
            if !self.peers.knows(&dest_hash) {
                missing = true;
            }
            encrypt_to(&self.peers, &dest_hash, plaintext)
        });

        let payload = match packed {
            Ok(p) => p,
            Err(_) if missing => {
                let _ = self
                    .transport
                    .send(TransportMessage::RequestPath {
                        destination_hash: dest_hash,
                    })
                    .await;
                // Retried by a later tick once the key arrives.
                self.route(message).await;
                self.sys(format!(
                    "[SYS] no key for {} yet — requested path, will retry",
                    hex::encode(dest_hash)
                ))
                .await;
                return;
            }
            Err(e) => {
                self.sys(format!("[SYS] pack failed: {e}")).await;
                return;
            }
        };

        let mut raw = opportunistic_header(dest_hash).pack();
        raw.extend_from_slice(&payload);

        if raw.len() > rns_wire::constants::MTU {
            self.sys(format!(
                "[SYS] message to {} too large for opportunistic (link delivery is Phase 4)",
                hex::encode(dest_hash)
            ))
            .await;
            return;
        }

        let _ = self
            .transport
            .send(TransportMessage::Outbound(OutboundRequest {
                raw: bytes::Bytes::from(raw),
                destination_hash: dest_hash,
            }))
            .await;
        // Opportunistic has no proof, so this is the terminal state for it: Sent.
        self.finish(msg_hash, MsgStatus::Sent).await;
        self.sys(format!("[SYS] sent to {}", hex::encode(dest_hash)))
            .await;
    }

    /// Re-queue a message after requesting a path: defer its next attempt by
    /// [`PATH_REQUEST_WAIT`] (so the router doesn't re-emit — and re-request —
    /// every tick) and request a path (throttled), logging at most once per
    /// window.
    async fn requeue_after_path_request(
        &mut self,
        mut message: LxMessage,
        request_hash: [u8; 16],
        note: &str,
    ) {
        let now = now_secs();
        // Count the attempt so a never-reachable peer eventually expires to Failed
        // (the router emits OutboundAction::Failed once attempts exceed its max),
        // rather than re-queuing — and showing `[sending]` — forever.
        message.delivery_attempts += 1;
        message.last_delivery_attempt = now;
        message.next_delivery_attempt = now + PATH_REQUEST_WAIT;
        if self.peers.request_path(&self.transport, request_hash, now) {
            self.sys(format!(
                "[SYS] {note} {} — requesting path (retry in {}s)",
                hex::encode(request_hash),
                PATH_REQUEST_WAIT as u64
            ))
            .await;
        }
        self.route(message).await;
    }

    // --- Ticking ---------------------------------------------------------------

    /// Advance in-flight link deliveries and act on any that completed.
    pub(crate) async fn tick_links(&mut self) {
        self.links.drain_events(self.peers.keys());
        for result in self.links.tick() {
            self.handle_delivery_result(result).await;
        }
    }

    /// Act on a completed link delivery. On terminal failure we fall back to
    /// Opportunistic — making it the genuine last resort behind Direct.
    async fn handle_delivery_result(&mut self, result: DeliveryResult) {
        match result {
            DeliveryResult::Complete { msg_hash, .. } => {
                // Remove it from the router's queue, or it re-emits every retry
                // window (the repeated "opening link → delivered" loop).
                if let Some(h) = msg_hash {
                    self.router.mark_outbound_delivered(&h);
                }
                // A propagation deposit reads as Propagated; a peer link as
                // Delivered.
                let propagated = msg_hash.is_some_and(|h| self.tracker.propagated.contains(&h));
                let status = if propagated {
                    MsgStatus::Propagated
                } else {
                    MsgStatus::Delivered
                };
                self.finish(msg_hash, status).await;
                let label = if propagated {
                    "deposited to propagation node"
                } else {
                    "delivered (direct)"
                };
                self.sys(format!("[SYS] {label}")).await;
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
                    self.router.mark_outbound_failed(&h);
                }
                // Cascade DIRECT -> PROPAGATED -> OPPORTUNISTIC. A failed Direct with
                // a propagation node configured re-queues as Propagated; a failed
                // Propagated (or no node) falls to a single opportunistic packet.
                let mut message = message;
                let try_propagated = message.method == DeliveryMethod::Direct
                    && self.router.outbound_propagation_node.is_some();
                if try_propagated {
                    self.sys(format!(
                        "[SYS] direct to {} failed ({reason}) — trying propagation",
                        hex::encode(dest_hash)
                    ))
                    .await;
                    message.method = DeliveryMethod::Propagated;
                    self.route(message).await;
                } else {
                    self.sys(format!(
                        "[SYS] {} delivery failed ({reason}) — trying opportunistic",
                        hex::encode(dest_hash)
                    ))
                    .await;
                    self.send_opportunistic(message, dest_hash).await;
                }
            }
        }
    }

    // --- Propagation sync ------------------------------------------------------

    /// Advance an in-progress propagation sync and return any message blobs it
    /// downloaded this tick (still encrypted — the caller decodes them).
    pub(crate) fn tick_sync(&mut self) -> Vec<Vec<u8>> {
        self.prop.drain_events(self.peers.keys());
        self.prop.tick();
        self.prop.take_received_messages()
    }

    /// Human label for an in-progress sync (drives the pop-up), or `None` when
    /// the client is Idle / Complete / Failed — i.e. no pop-up.
    pub(crate) fn sync_phase(&self) -> Option<&'static str> {
        sync_phase(self.prop.state())
    }

    /// Start a sync from the configured propagation node, if one is set, idle,
    /// and its identity is cached; otherwise request a path or report why not.
    pub(crate) async fn try_sync(&mut self) {
        let Some(node) = self.router.outbound_propagation_node else {
            self.sys("[SYS] no propagation node set (Network tab: Enter on one)".to_string())
                .await;
            return;
        };
        if self.prop.state() != PropagationClientState::Idle {
            return; // a sync is already running
        }
        if self.peers.knows(&node) {
            if self.prop.start_download() {
                self.sys(format!("[SYS] syncing from {} ...", hex::encode(node)))
                    .await;
            }
        } else if self.peers.request_path(&self.transport, node, now_secs()) {
            self.sys(format!(
                "[SYS] propagation node {} identity unknown — requesting path (retry in {}s)",
                hex::encode(node),
                PATH_REQUEST_WAIT as u64
            ))
            .await;
        }
    }

    // --- Reporting -------------------------------------------------------------

    /// Report a message's terminal status to the UI and stop tracking it.
    async fn finish(&mut self, hash: Option<[u8; 32]>, status: MsgStatus) {
        self.emit_status(hash, status).await;
        if let Some(h) = hash {
            self.tracker.forget(&h);
        }
    }

    /// Emit a status update for `hash`'s message, if it's being tracked.
    ///
    /// Takes `&mut self` although it only reads: `LxmRouter` is `!Sync`, so a
    /// shared `&Dispatcher` held across an `.await` would make the whole
    /// networking future non-`Send` and un-spawnable.
    async fn emit_status(&mut self, hash: Option<[u8; 32]>, status: MsgStatus) {
        if let Some(id) = self.tracker.id_for(hash) {
            let _ = self.events.send(NetEvent::MsgStatus { id, status }).await;
        }
    }

    /// Push one line to the operator's Log tab. `&mut self` for the same
    /// `!Sync` reason as [`Dispatcher::emit_status`].
    async fn sys(&mut self, line: String) {
        let _ = self.events.send(NetEvent::Sys(line)).await;
    }
}

/// Encrypt `plaintext` to a destination whose identity we have cached (from its
/// announce). Shared by opportunistic and propagated packing.
fn encrypt_to(
    peers: &PeerCache,
    recipient: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, MessageError> {
    match peers.key_for(recipient) {
        Some(pk) => Identity::from_public_key(pk)
            .ok()
            .and_then(|id| id.encrypt(plaintext, None).ok())
            .ok_or_else(|| MessageError::PackFailed("encrypt failed".to_string())),
        None => Err(MessageError::PackFailed("no identity key".to_string())),
    }
}

/// The Reticulum header for a single-packet (opportunistic) LXMF delivery.
fn opportunistic_header(dest_hash: [u8; 16]) -> rns_wire::header::PacketHeader {
    rns_wire::header::PacketHeader {
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
    }
}

/// Human label for an in-progress sync state. Idle / Complete / Failed map to
/// `None` — i.e. no pop-up.
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn tracker_forgets_terminal_messages() {
        let mut tracker = StatusTracker::default();
        tracker.ids.insert([1u8; 32], 42);
        tracker.propagated.insert([1u8; 32]);
        assert_eq!(tracker.id_for(Some([1u8; 32])), Some(42));
        assert_eq!(tracker.id_for(Some([2u8; 32])), None);
        assert_eq!(tracker.id_for(None), None);
        tracker.forget(&[1u8; 32]);
        assert_eq!(tracker.id_for(Some([1u8; 32])), None);
        assert!(tracker.propagated.is_empty());
    }
}
