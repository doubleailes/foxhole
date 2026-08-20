//! Announce-learned peer cache: identity keys, hop counts, and path-request
//! throttling.
//!
//! Everything the outbound path needs to *reach* a destination is cached here:
//! the 64-byte public key (so a payload can be encrypted to it), the announced
//! hop count (so a link can be sized), and when we last asked the transport for
//! a path (so retries don't flood the mesh). Identity keys persist across
//! restarts — without them a freshly started terminal cannot reach a known peer
//! until it happens to re-hear an announce.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use rns_transport::messages::TransportMessage;

/// Cache of peer destination hash (hex) -> 64-byte public key, learned from
/// `lxmf.delivery` announces. Hex-keyed to match `lxmf_core`'s own convention
/// (`LinkDeliveryManager::drain_events` takes `&HashMap<String, [u8;64]>`).
pub(crate) type KnownKeys = HashMap<String, [u8; 64]>;

/// After a path request, wait this long before re-requesting / retrying a
/// delivery for the same destination. Bounds path-request traffic and defers the
/// message's next attempt so the router doesn't re-emit it every tick.
pub(crate) const PATH_REQUEST_WAIT: f64 = 30.0;

/// Upper bound on cached peers. Announces are free to mint, so an unbounded cache
/// is a memory-and-disk-exhaustion vector (the key map is persisted to
/// `known_identities`). Past this the least-recently-learned identity is evicted.
/// ~4k peers is far more than any real off-grid deployment while capping the map
/// (and its on-disk file) to a fraction of a megabyte.
pub(crate) const MAX_PEERS: usize = 4096;

/// What we know about how to reach our peers.
pub(crate) struct PeerCache {
    /// Identity keys, hex-keyed (the shape `lxmf_core` wants back).
    keys: KnownKeys,
    /// Announced hop count per destination; feeds Direct delivery planning.
    hops: HashMap<[u8; 16], u8>,
    /// Last path request per destination, for the [`PATH_REQUEST_WAIT`] throttle.
    last_request: HashMap<[u8; 16], f64>,
    /// Monotonic learn order per identity (hex), for least-recently-learned
    /// eviction once [`MAX_PEERS`] is reached. Not persisted — recency is only a
    /// runtime eviction hint.
    order: HashMap<String, u64>,
    /// Next value handed out to `order` on a learn.
    seq: u64,
    /// Backing file for `keys`, and whether it has diverged from disk.
    path: PathBuf,
    dirty: bool,
}

impl PeerCache {
    /// Load the persisted identity keys from `path` (empty if absent/corrupt).
    pub(crate) fn load(path: PathBuf) -> Self {
        let keys = load_known(&path);
        Self {
            keys,
            hops: HashMap::new(),
            last_request: HashMap::new(),
            order: HashMap::new(),
            seq: 0,
            path,
            dirty: false,
        }
    }

    /// Number of cached identity keys.
    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    /// The whole key map, as `lxmf_core`'s drain/tick APIs want it.
    pub(crate) fn keys(&self) -> &KnownKeys {
        &self.keys
    }

    /// Record an identity key, flagging the cache dirty only when it changed (so
    /// the periodic persist writes on real updates, not every re-announce).
    pub(crate) fn learn(&mut self, dest: [u8; 16], pk: [u8; 64]) {
        let hex = hex::encode(dest);
        if self.keys.insert(hex.clone(), pk) != Some(pk) {
            self.dirty = true;
        }
        // Refresh recency and evict the least-recently-learned identity once over
        // the cap, so an announce flood can't grow the (persisted) cache without
        // bound. Eviction also drops the matching transient hop entry.
        self.seq += 1;
        self.order.insert(hex, self.seq);
        while self.keys.len() > MAX_PEERS {
            let Some(victim) = self
                .order
                .iter()
                .min_by_key(|&(_, &s)| s)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.keys.remove(&victim);
            self.order.remove(&victim);
            if let Ok(bytes) = hex::decode(&victim)
                && let Ok(d) = <[u8; 16]>::try_from(bytes.as_slice())
            {
                self.hops.remove(&d);
            }
            self.dirty = true;
        }
    }

    /// Whether we hold an identity key for `dest` (i.e. can encrypt to it).
    pub(crate) fn knows(&self, dest: &[u8; 16]) -> bool {
        self.keys.contains_key(&hex::encode(dest))
    }

    /// The identity key for `dest`, if cached.
    pub(crate) fn key_for(&self, dest: &[u8; 16]) -> Option<&[u8; 64]> {
        self.keys.get(&hex::encode(dest))
    }

    /// Record an announced hop count.
    pub(crate) fn set_hops(&mut self, dest: [u8; 16], hops: u8) {
        self.hops.insert(dest, hops);
    }

    /// The announced hop count for `dest`, if heard.
    pub(crate) fn hops(&self, dest: &[u8; 16]) -> Option<u8> {
        self.hops.get(dest).copied()
    }

    /// Send a path request for `dest`, at most once per [`PATH_REQUEST_WAIT`].
    /// Returns true only when a request was actually sent, so callers log just
    /// once per window instead of every tick.
    pub(crate) fn request_path(
        &mut self,
        transport: &mpsc::Sender<TransportMessage>,
        dest: [u8; 16],
        now: f64,
    ) -> bool {
        if now - self.last_request.get(&dest).copied().unwrap_or(0.0) < PATH_REQUEST_WAIT {
            return false;
        }
        self.note_path_request(dest, now);
        let _ = transport.try_send(TransportMessage::RequestPath {
            destination_hash: dest,
        });
        true
    }

    /// Arm the throttle without sending — for an operator-initiated probe, which
    /// fires its own request directly and must not be suppressed by the window.
    pub(crate) fn note_path_request(&mut self, dest: [u8; 16], now: f64) {
        self.last_request.insert(dest, now);
        // Bound the throttle map (a probe can target any dest, learned or not).
        // Dropping the oldest entry only lifts a stale throttle, which is safe —
        // at worst one extra path request is allowed for that destination.
        while self.last_request.len() > MAX_PEERS {
            let Some(oldest) = self
                .last_request
                .iter()
                .min_by(|a, b| a.1.total_cmp(b.1))
                .map(|(k, _)| *k)
            else {
                break;
            };
            self.last_request.remove(&oldest);
        }
    }

    /// Persist the identity keys if they changed since the last flush.
    pub(crate) fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let _ = save_known(&self.path, &self.keys);
        self.dirty = false;
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
    foxhole_core::storage::atomic_write(path, s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> PeerCache {
        PeerCache::load(PathBuf::from("/nonexistent/foxhole-peer-cache"))
    }

    #[test]
    fn request_path_throttled_to_one_per_window() {
        let (tx, _rx) = mpsc::channel::<TransportMessage>(8);
        let mut peers = cache();
        let dest = [1u8; 16];
        assert!(peers.request_path(&tx, dest, 100.0), "first send");
        assert!(
            !peers.request_path(&tx, dest, 100.0 + PATH_REQUEST_WAIT - 1.0),
            "suppressed within the window"
        );
        assert!(
            peers.request_path(&tx, dest, 100.0 + PATH_REQUEST_WAIT + 1.0),
            "sent again after the window"
        );
    }

    #[test]
    fn learn_flags_dirty_only_on_change() {
        let mut peers = cache();
        peers.learn([1u8; 16], [2u8; 64]);
        assert!(peers.dirty, "new identity is a change");
        peers.dirty = false;
        peers.learn([1u8; 16], [2u8; 64]);
        assert!(!peers.dirty, "identical re-announce is not a change");
        peers.learn([1u8; 16], [3u8; 64]);
        assert!(peers.dirty, "rotated key is a change");
        assert!(peers.knows(&[1u8; 16]));
        assert_eq!(peers.key_for(&[1u8; 16]), Some(&[3u8; 64]));
    }

    #[test]
    fn learn_evicts_least_recently_learned_over_the_cap() {
        let mut peers = cache();
        let dest = |n: u32| {
            let mut d = [0u8; 16];
            d[..4].copy_from_slice(&n.to_be_bytes());
            d
        };
        // Fill to the cap, then learn one more: the map stays capped and the
        // first-learned identity is the one evicted.
        for n in 0..(MAX_PEERS as u32) {
            peers.learn(dest(n), [1u8; 64]);
        }
        assert_eq!(peers.len(), MAX_PEERS);
        peers.learn(dest(MAX_PEERS as u32), [1u8; 64]);
        assert_eq!(peers.len(), MAX_PEERS, "still capped after the extra learn");
        assert!(!peers.knows(&dest(0)), "the oldest identity was evicted");
        assert!(peers.knows(&dest(MAX_PEERS as u32)), "the newest is kept");
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

    #[test]
    fn flush_only_writes_when_dirty() {
        let mut path = std::env::temp_dir();
        path.push("foxhole_peer_cache_flush_test");
        let _ = std::fs::remove_file(&path);

        let mut peers = PeerCache::load(path.clone());
        peers.flush();
        assert!(!path.exists(), "a clean cache writes nothing");

        peers.learn([9u8; 16], [4u8; 64]);
        peers.flush();
        assert!(path.exists(), "a dirty cache is persisted");
        assert!(!peers.dirty, "flush clears the dirty flag");

        let _ = std::fs::remove_file(&path);
    }
}
