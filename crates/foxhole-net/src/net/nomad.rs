//! Nomad Network: discovering nodes that serve micron pages, and fetching those
//! pages over a Reticulum link.
//!
//! Discovery is pull-based — a poll of the transport's recent-announce cache —
//! because `LinkClient::query` deregisters announce handlers mid-fetch, so a
//! push-based handler would go deaf exactly while a page is loading. The hop
//! counts learned here size the link a fetch opens.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;

use rns_crypto::sha::{name_hash, truncated_hash};
use rns_identity::destination::Destination;
use rns_runtime::link_client::LinkClient;
use rns_runtime::reticulum;
use rns_transport::messages::{TransportQuery, TransportQueryResponse};

use foxhole_core::app::NetEvent;

use super::codec::{encode_form, nomad_name_from_app_data};

/// Nomad Network node aspect — the destination that serves micron pages.
pub(crate) const NOMAD_NODE: &str = "nomadnetwork.node";

/// Overall timeout for one page fetch (link + request + response).
const PAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on an accepted page body. A hostile `nomadnetwork.node` could
/// otherwise serve a multi-hundred-MB page that we copy into a `String` and hand
/// to the renderer every frame. 512 KiB is far above any real micron page; a
/// larger response is rejected rather than truncated (a truncated page could cut
/// a multi-byte sequence or a markup token and is not worth rendering).
const MAX_PAGE_BYTES: usize = 512 * 1024;

/// Hop count used for a page link when the node's announce hop count is unknown.
const DEFAULT_PAGE_HOPS: u8 = 8;

/// Upper bound on the per-node hop cache. Node announces are free to mint, so this
/// stops the (transient) map growing without limit; an evicted entry just falls
/// back to [`DEFAULT_PAGE_HOPS`] on the next fetch, so eviction is harmless.
const MAX_NODES: usize = 4096;

/// The Browser tab's half of the stack: known Nomad Network nodes and the link
/// client that fetches their pages.
pub(crate) struct Nomad {
    /// Initiator-side client for page fetches, cloned into each fetch task.
    client: LinkClient,
    /// The `nomadnetwork.node` aspect's name hash, to filter announces.
    name_hash: [u8; 10],
    /// Last-known hop count per node identity (hex), for sizing page links.
    hops: HashMap<String, u8>,
}

impl Nomad {
    pub(crate) fn new(client: LinkClient) -> Self {
        Self {
            client,
            name_hash: name_hash(NOMAD_NODE),
            hops: HashMap::new(),
        }
    }

    /// Scan the transport's recent-announce cache for Nomad Network nodes and
    /// report each (deduped UI-side) with its announce timestamp. The node's
    /// identity hash — `sha256(public_key)[..16]`, what a page fetch addresses —
    /// is derived from the announced public key; its hop count is cached for
    /// sizing page links.
    pub(crate) async fn discover(
        &mut self,
        handle: &reticulum::ReticulumHandle,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let Some(TransportQueryResponse::Announces(entries)) = handle
            .query_control(TransportQuery::GetRecentAnnounces)
            .await
        else {
            return;
        };
        for e in entries {
            if e.name_hash != self.name_hash {
                continue;
            }
            let Some(pk) = e.public_key else { continue };
            let id_hash = truncated_hash(&pk);
            let identity = hex::encode(id_hash);
            let name = nomad_name_from_app_data(e.app_data.as_deref());
            // Bound the transient hop cache against a node-announce flood; an
            // evicted entry just re-defaults on its next fetch.
            if self.hops.len() >= MAX_NODES
                && !self.hops.contains_key(&identity)
                && let Some(k) = self.hops.keys().next().cloned()
            {
                self.hops.remove(&k);
            }
            // Log the first time we see each node.
            if self.hops.insert(identity.clone(), e.hops).is_none() {
                // Cross-check: the destination a fetch addresses should equal the
                // announced one. A mismatch (never expected) means a derivation bug.
                let derived = Destination::hash_from_name_and_identity(NOMAD_NODE, Some(&id_hash));
                if derived != e.dest_hash {
                    let _ = events
                        .send(NetEvent::Sys(format!(
                            "[SYS] WARN nomad {}.. derived dest {}.. != announced {}..",
                            &identity[..8],
                            &hex::encode(derived)[..8],
                            &hex::encode(e.dest_hash)[..8],
                        )))
                        .await;
                }
                let _ = events
                    .send(NetEvent::Sys(format!(
                        "[SYS] nomad node {}.. dest {}.. ({} hops) {}",
                        &identity[..8],
                        &hex::encode(e.dest_hash)[..8],
                        e.hops,
                        name.as_deref().unwrap_or("?"),
                    )))
                    .await;
            }
            let _ = events
                .send(NetEvent::NomadNode {
                    identity,
                    dest: hex::encode(e.dest_hash),
                    name,
                    last_seen: e.timestamp as u64,
                })
                .await;
        }
    }

    /// Fetch one page (or submit one form) from `identity`, off the event loop —
    /// the query blocks for up to [`PAGE_FETCH_TIMEOUT`], which must not stall
    /// inbound traffic. The result comes back as a [`NetEvent::Page`] either way.
    pub(crate) async fn fetch(
        &self,
        identity: String,
        path: String,
        fields: Vec<(String, String)>,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let id = match super::codec::parse_hash(&identity) {
            Ok(id) => id,
            Err(e) => {
                let _ = events
                    .send(NetEvent::Page {
                        identity,
                        path,
                        body: Err(format!("bad node address: {e}")),
                    })
                    .await;
                return;
            }
        };

        let hops = self
            .hops
            .get(&identity)
            .copied()
            .unwrap_or(DEFAULT_PAGE_HOPS);
        let id8 = identity.get(..8).unwrap_or(&identity);
        let nfields = fields.len();
        let suffix = if nfields > 0 {
            format!(", {nfields} field(s)")
        } else {
            String::new()
        };
        let _ = events
            .send(NetEvent::Sys(format!(
                "[SYS] page fetch {path} from {id8}.. ({hops} hops{suffix})"
            )))
            .await;

        // The msgpack request data (`{field_…/var_…: value}`), empty for a GET.
        let payload = encode_form(&fields);
        let client = self.client.clone();
        let events = events.clone();
        tokio::spawn(async move {
            // `query` discovers the node's pubkey via its announce before opening
            // the link, so no cached key is needed.
            let result = client
                .query(id, NOMAD_NODE, &path, payload, hops, PAGE_FETCH_TIMEOUT)
                .await;
            let log = match &result {
                Ok(b) => format!("[SYS] page fetch {path}: ok, {} bytes", b.len()),
                Err(e) => format!("[SYS] page fetch {path}: FAILED — {e}"),
            };
            let _ = events.send(NetEvent::Sys(log)).await;
            let body = result.map_err(|e| e.to_string()).and_then(|bytes| {
                if bytes.len() > MAX_PAGE_BYTES {
                    Err(format!(
                        "page too large: {} bytes (limit {MAX_PAGE_BYTES})",
                        bytes.len()
                    ))
                } else {
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                }
            });
            let _ = events
                .send(NetEvent::Page {
                    identity,
                    path,
                    body,
                })
                .await;
        });
    }
}
