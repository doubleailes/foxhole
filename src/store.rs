//! Durable, encrypted, atomic conversation store (behind the `net` feature).
//!
//! Each conversation is serialized to a small versioned binary blob,
//! authenticated-encrypted with `rns_crypto::token` (AES-256-CBC + HMAC-SHA256,
//! random IV), and written to its own file via [`crate::storage::atomic_write`]
//! (write-temp → fsync → rename). Properties:
//!   * **Atomic** — a crash mid-write leaves the previous file intact.
//!   * **Authenticated** — corruption or tampering fails the HMAC, so a bad file
//!     is skipped rather than loaded as garbage.
//!   * **Isolated** — one file per conversation bounds the blast radius.
//!   * **Forgiving** — any unreadable/foreign/old file is skipped; the app still
//!     comes up.
//!
//! The encryption key is derived (HKDF) from the Reticulum identity, so history
//! is readable only with that identity present.

use std::io;
use std::path::{Path, PathBuf};

use rns_crypto::{hkdf, sha, token};
use rns_identity::identity::Identity;

use crate::app::{Conversation, Entry};
use crate::config::config_dir;

/// File-format magic + version. Bump the version when the layout changes.
const MAGIC: &[u8; 4] = b"FXC1";
const VERSION: u8 = 1;

/// Domain-separation salt for the store-key derivation.
const KEY_SALT: &[u8] = b"foxhole.conversations.v1";

/// Derive the 64-byte store key (32 AES + 32 HMAC) from the identity's private
/// key. `None` for a public-only identity (nothing to derive from).
pub fn derive_key(identity: &Identity) -> Option<[u8; 64]> {
    let private = identity.get_private_key()?;
    hkdf::derive_key_64(&private[..], KEY_SALT).ok()
}

/// Directory holding the per-conversation files.
fn conversations_dir() -> PathBuf {
    config_dir().join("conversations")
}

/// Content-addressed file name for a peer key: `hex(sha256(peer)[..16]).lxmc`.
/// Always filesystem-safe and collision-free; the real peer lives inside the
/// (encrypted) blob, so the name needn't be reversible.
fn file_for(dir: &Path, peer: &str) -> PathBuf {
    let digest = sha::sha256(peer.as_bytes());
    dir.join(format!("{}.lxmc", hex::encode(&digest[..16])))
}

/// Save one conversation: serialize → encrypt → atomic write.
pub fn save(key: &[u8; 64], conv: &Conversation) -> io::Result<()> {
    save_to(&conversations_dir(), key, conv)
}

/// Load every conversation in the store. Returns the decoded conversations plus
/// the count of files that were skipped (corrupt / foreign identity / old).
pub fn load_all(key: &[u8; 64]) -> (Vec<Conversation>, usize) {
    load_all_from(&conversations_dir(), key)
}

fn save_to(dir: &Path, key: &[u8; 64], conv: &Conversation) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let blob = serialize(conv);
    let token =
        token::encrypt(&blob, key).map_err(|e| io::Error::other(format!("encrypt: {e}")))?;
    crate::storage::atomic_write(&file_for(dir, &conv.peer), &token)
}

fn load_all_from(dir: &Path, key: &[u8; 64]) -> (Vec<Conversation>, usize) {
    let mut loaded = Vec::new();
    let mut skipped = 0usize;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (loaded, 0); // no store yet
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lxmc") {
            continue;
        }
        match std::fs::read(&path)
            .ok()
            .and_then(|bytes| token::decrypt(&bytes, key).ok())
            .and_then(|plain| deserialize(&plain))
        {
            Some(conv) => loaded.push(conv),
            None => skipped += 1,
        }
    }
    (loaded, skipped)
}

// --- Wire format ---------------------------------------------------------------

fn serialize(conv: &Conversation) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(MAGIC);
    b.push(VERSION);
    put_str(&mut b, &conv.peer);
    put_str(&mut b, conv.display_name.as_deref().unwrap_or(""));
    b.extend_from_slice(&(conv.unread as u32).to_be_bytes());
    b.extend_from_slice(&(conv.messages.len() as u32).to_be_bytes());
    for m in &conv.messages {
        b.extend_from_slice(&m.at.to_be_bytes());
        put_text(&mut b, &m.text);
    }
    b
}

fn deserialize(data: &[u8]) -> Option<Conversation> {
    let mut r = Reader::new(data);
    if r.take(4)? != MAGIC || r.u8()? != VERSION {
        return None;
    }
    let peer = r.str()?;
    let name = r.str()?;
    let unread = r.u32()? as usize;
    let count = r.u32()? as usize;

    let mut messages = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let at = r.u64()?;
        let text = r.text()?;
        messages.push(Entry { at, text });
    }

    let mut conv = Conversation::new(peer);
    conv.display_name = if name.is_empty() { None } else { Some(name) };
    conv.unread = unread;
    conv.messages = messages;
    Some(conv)
}

/// `u16` length-prefixed string (peer / display name).
fn put_str(b: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    b.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    b.extend_from_slice(bytes);
}

/// `u32` length-prefixed text (message body — may be long / multi-line).
fn put_text(b: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    b.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    b.extend_from_slice(bytes);
}

/// Bounds-checked sequential reader; any out-of-range read yields `None`.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    fn str(&mut self) -> Option<String> {
        let len = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn text(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Conversation {
        let mut c = Conversation::new("a1b2c3d4e5f600112233445566778899");
        c.display_name = Some("rat-six".to_string());
        c.unread = 3;
        c.draft = "unsaved".to_string(); // must NOT survive a round-trip
        c.messages = vec![
            Entry {
                at: 1_700_000_000,
                text: "[RX] line one\nline two".to_string(), // multi-line
            },
            Entry {
                at: 1_700_000_050,
                text: "[TX] reply".to_string(),
            },
        ];
        c
    }

    fn same(a: &Conversation, b: &Conversation) {
        assert_eq!(a.peer, b.peer);
        assert_eq!(a.display_name, b.display_name);
        assert_eq!(a.unread, b.unread);
        assert_eq!(a.messages, b.messages);
    }

    #[test]
    fn serialize_round_trip_preserves_messages() {
        let c = sample();
        let back = deserialize(&serialize(&c)).expect("decode");
        same(&c, &back);
        assert!(back.draft.is_empty(), "drafts are not persisted");
    }

    #[test]
    fn bad_blob_is_rejected() {
        assert!(deserialize(b"nope").is_none());
        assert!(deserialize(b"").is_none());
        // Right magic, truncated body.
        assert!(deserialize(b"FXC1").is_none());
    }

    #[test]
    fn save_then_load_round_trips_through_disk() {
        let dir = std::env::temp_dir().join("foxhole_store_rt");
        let _ = std::fs::remove_dir_all(&dir);
        let key = [7u8; 64];

        let c = sample();
        save_to(&dir, &key, &c).unwrap();
        let (loaded, skipped) = load_all_from(&dir, &key);
        assert_eq!(skipped, 0);
        assert_eq!(loaded.len(), 1);
        same(&c, &loaded[0]);

        // Wrong key (different identity) → file skipped, not decoded.
        let (loaded, skipped) = load_all_from(&dir, &[8u8; 64]);
        assert!(loaded.is_empty());
        assert_eq!(skipped, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_skipped_others_survive() {
        let dir = std::env::temp_dir().join("foxhole_store_corrupt");
        let _ = std::fs::remove_dir_all(&dir);
        let key = [5u8; 64];

        let mut a = sample();
        a.peer = "aaaa0000aaaa0000aaaa0000aaaa0000".to_string();
        let mut b = sample();
        b.peer = "bbbb1111bbbb1111bbbb1111bbbb1111".to_string();
        save_to(&dir, &key, &a).unwrap();
        save_to(&dir, &key, &b).unwrap();

        // Corrupt a's file by flipping a byte.
        let path = file_for(&dir, &a.peer);
        let mut bytes = std::fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        let (loaded, skipped) = load_all_from(&dir, &key);
        assert_eq!(skipped, 1, "the corrupt file is skipped");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].peer, b.peer, "the intact conversation survives");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
