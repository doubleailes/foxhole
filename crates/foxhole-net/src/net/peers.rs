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

/// What we know about how to reach our peers.
pub(crate) struct PeerCache {
    /// Identity keys, hex-keyed (the shape `lxmf_core` wants back).
    keys: KnownKeys,
    /// Announced hop count per destination; feeds Direct delivery planning.
    hops: HashMap<[u8; 16], u8>,
    /// Last path request per destination, for the [`PATH_REQUEST_WAIT`] throttle.
    last_request: HashMap<[u8; 16], f64>,
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
        if self.keys.insert(hex::encode(dest), pk) != Some(pk) {
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
