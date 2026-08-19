//! Transport introspection: operator path probes and interface statistics.
//!
//! Both are polled rather than pushed — the transport already keeps the path
//! table and per-interface counters, so a periodic read is cheaper (and simpler)
//! than another event stream.

use std::collections::HashMap;

use tokio::sync::mpsc;

use rns_runtime::reticulum;
use rns_transport::messages::{TransportQuery, TransportQueryResponse};

use foxhole_core::app::{Interface, NetEvent};

use super::now_secs;

/// After an operator path probe, wait this long before reading the path table so
/// the path response has a chance to arrive. Probed once more after a second
/// grace before reporting "no path".
const PROBE_GRACE: f64 = 1.5;

/// Operator path probes awaiting resolution: dest -> (due time, already re-armed).
#[derive(Default)]
pub(crate) struct PathProbes(HashMap<[u8; 16], (f64, bool)>);

impl PathProbes {
    /// Arm a probe for `dest`, to be read back after the grace window.
    pub(crate) fn arm(&mut self, dest: [u8; 16]) {
        self.0.insert(dest, (now_secs() + PROBE_GRACE, false));
    }

    /// Resolve any probes whose grace window has elapsed: read the transport's
    /// path table (`HopsTo` + `GetNextHopIfName`) and report the result
    /// (rnpath-style). A probe still unresolved on its first pass is re-armed
    /// once before being reported as "no path".
    pub(crate) async fn resolve(
        &mut self,
        handle: &reticulum::ReticulumHandle,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let now = now_secs();
        let due: Vec<[u8; 16]> = self
            .0
            .iter()
            .filter(|(_, (deadline, _))| *deadline <= now)
            .map(|(&dest, _)| dest)
            .collect();

        for dest in due {
            let hops = match handle.query_control(TransportQuery::GetPathTable).await {
                Some(TransportQueryResponse::PathTable(entries)) => {
                    entries.iter().find(|e| e.hash == dest).map(|e| e.hops)
                }
                _ => None,
            };

            // No path yet — give the path response one more grace window before
            // declaring it unreachable.
            if hops.is_none()
                && let Some(entry) = self.0.get_mut(&dest)
                && !entry.1
            {
                entry.0 = now + PROBE_GRACE;
                entry.1 = true;
                continue;
            }

            let iface = if hops.is_some() {
                match handle
                    .query_control(TransportQuery::GetNextHopIfName { dest })
                    .await
                {
                    Some(TransportQueryResponse::StringResult(s)) => s,
                    _ => None,
                }
            } else {
                None
            };

            self.0.remove(&dest);
            let _ = events
                .send(NetEvent::Path {
                    hash: hex::encode(dest),
                    hops,
                    iface,
                })
                .await;
        }
    }
}

/// Snapshot the transport's per-interface stats (and the active link count) and
/// forward them to the Interfaces tab. Modeled on the path/announce queries:
/// a fire-and-poll RPC whose result replaces the UI snapshot wholesale.
pub(crate) async fn interfaces(
    handle: &reticulum::ReticulumHandle,
    events: &mpsc::Sender<NetEvent>,
) {
    let Some(TransportQueryResponse::InterfaceStats(stats)) = handle
        .query_control(TransportQuery::GetInterfaceStats)
        .await
    else {
        return;
    };
    let links = match handle.query_control(TransportQuery::GetLinkCount).await {
        Some(TransportQueryResponse::IntResult(n)) => n.max(0) as u32,
        _ => 0,
    };
    let interfaces = stats
        .into_iter()
        .map(|s| Interface {
            name: s.name,
            online: s.online,
            bitrate: s.bitrate,
            rx_bytes: s.rx_bytes,
            tx_bytes: s.tx_bytes,
        })
        .collect();
    let _ = events
        .send(NetEvent::Interfaces { interfaces, links })
        .await;
}
