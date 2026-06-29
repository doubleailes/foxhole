//! Durable, encrypted, atomic store for the received-intel layer (behind the
//! `net` feature) — P4 of the intel-sharing plan.
//!
//! The live and staged CoT intel ([`App::intel`]/[`App::intel_staged`]) is
//! serialized to one versioned binary blob, authenticated-encrypted with
//! `rns_crypto::token` (AES-256-CBC + HMAC-SHA256, random IV), and written via
//! [`foxhole_core::storage::atomic_write`]. It reuses the identity-derived store key (so
//! intel is readable only with that identity present) and lives under
//! [`config_dir`], so a BURN wipes it with everything else.
//!
//! Unlike the per-conversation store, intel is a small flat list keyed by
//! `(source, uid)`, so a single file is simpler; a corrupt/foreign/old file is
//! skipped wholesale (the terminal still comes up with an empty intel layer).
//! Crucially the structured `Option` timestamps are preserved verbatim — a
//! stale-less event reloads as stale-less (default-TTL'd), not mis-expired.

use std::io;
use std::path::{Path, PathBuf};

use foxhole_cot::{CotEvent, Point};
use rns_crypto::token;

use foxhole_core::app::IntelRecord;
use foxhole_core::config::config_dir;

/// File-format magic + version.
const MAGIC: &[u8; 4] = b"FXI1";
const VERSION: u8 = 1;

/// File holding the encrypted intel blob.
fn intel_file() -> PathBuf {
    config_dir().join("intel.lxmi")
}

/// Save the whole intel layer: serialize → encrypt → atomic write. `live` and
/// `staged` are persisted with their review state so the staging queue survives
/// a restart too.
pub fn save(key: &[u8; 64], live: &[IntelRecord], staged: &[IntelRecord]) -> io::Result<()> {
    save_to(&intel_file(), key, live, staged)
}

/// Load the intel layer: `(live, staged)`. Both empty for a missing file; a
/// corrupt/foreign/old file decodes to empty (and the second tuple element of
/// [`load_from`] reports it was skipped).
pub fn load(key: &[u8; 64]) -> (Vec<IntelRecord>, Vec<IntelRecord>) {
    let ((live, staged), _skipped) = load_from(&intel_file(), key);
    (live, staged)
}

fn save_to(
    path: &Path,
    key: &[u8; 64],
    live: &[IntelRecord],
    staged: &[IntelRecord],
) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let blob = serialize(live, staged);
    let token =
        token::encrypt(&blob, key).map_err(|e| io::Error::other(format!("encrypt: {e}")))?;
    foxhole_core::storage::atomic_write(path, &token)
}

/// Returns `((live, staged), skipped)` where `skipped` is true if a present file
/// could not be read/decrypted/decoded (so the caller can log it).
fn load_from(path: &Path, key: &[u8; 64]) -> ((Vec<IntelRecord>, Vec<IntelRecord>), bool) {
    let Ok(bytes) = std::fs::read(path) else {
        return ((Vec::new(), Vec::new()), false); // no store yet
    };
    match token::decrypt(&bytes, key)
        .ok()
        .and_then(|plain| deserialize(&plain))
    {
        Some(lists) => (lists, false),
        None => ((Vec::new(), Vec::new()), true),
    }
}

// --- Wire format ---------------------------------------------------------------

fn serialize(live: &[IntelRecord], staged: &[IntelRecord]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(MAGIC);
    b.push(VERSION);
    b.extend_from_slice(&(live.len() as u32).to_be_bytes());
    b.extend_from_slice(&(staged.len() as u32).to_be_bytes());
    for r in live {
        put_record(&mut b, r);
    }
    for r in staged {
        put_record(&mut b, r);
    }
    b
}

fn deserialize(data: &[u8]) -> Option<(Vec<IntelRecord>, Vec<IntelRecord>)> {
    let mut r = Reader::new(data);
    if r.take(4)? != MAGIC || r.u8()? != VERSION {
        return None;
    }
    let n_live = r.u32()? as usize;
    let n_staged = r.u32()? as usize;
    // Bound allocation against a corrupt count (the file is small in practice).
    let mut live = Vec::with_capacity(n_live.min(4096));
    for _ in 0..n_live {
        live.push(get_record(&mut r)?);
    }
    let mut staged = Vec::with_capacity(n_staged.min(4096));
    for _ in 0..n_staged {
        staged.push(get_record(&mut r)?);
    }
    Some((live, staged))
}

fn put_record(b: &mut Vec<u8>, r: &IntelRecord) {
    put_str(b, &r.source);
    b.extend_from_slice(&r.received_at.to_be_bytes());
    let e = &r.event;
    put_str(b, &e.uid);
    put_str(b, &e.cot_type);
    put_str(b, &e.how);
    put_opt_i64(b, e.time);
    put_opt_i64(b, e.start);
    put_opt_i64(b, e.stale);
    put_f64(b, e.point.lat);
    put_f64(b, e.point.lon);
    put_f64(b, e.point.hae);
    put_f64(b, e.point.ce);
    put_f64(b, e.point.le);
    put_opt_str(b, e.callsign.as_deref());
    put_opt_text(b, e.remarks.as_deref());
    put_opt_f64(b, e.radius_m);
}

fn get_record(r: &mut Reader) -> Option<IntelRecord> {
    let source = r.str()?;
    let received_at = r.u64()?;
    let event = CotEvent {
        uid: r.str()?,
        cot_type: r.str()?,
        how: r.str()?,
        time: r.opt_i64()?,
        start: r.opt_i64()?,
        stale: r.opt_i64()?,
        point: Point {
            lat: r.f64()?,
            lon: r.f64()?,
            hae: r.f64()?,
            ce: r.f64()?,
            le: r.f64()?,
        },
        callsign: r.opt_str()?,
        remarks: r.opt_text()?,
        radius_m: r.opt_f64()?,
    };
    Some(IntelRecord {
        source,
        event,
        received_at,
    })
}

/// `u16` length-prefixed string.
fn put_str(b: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    b.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    b.extend_from_slice(bytes);
}

/// `u32` length-prefixed text (remarks may be long).
fn put_text(b: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    b.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    b.extend_from_slice(bytes);
}

fn put_f64(b: &mut Vec<u8>, v: f64) {
    b.extend_from_slice(&v.to_bits().to_be_bytes());
}

/// Presence byte (`0`/`1`) then, if present, the value — preserving `Option`.
fn put_opt_i64(b: &mut Vec<u8>, v: Option<i64>) {
    match v {
        Some(x) => {
            b.push(1);
            b.extend_from_slice(&x.to_be_bytes());
        }
        None => b.push(0),
    }
}

fn put_opt_f64(b: &mut Vec<u8>, v: Option<f64>) {
    match v {
        Some(x) => {
            b.push(1);
            put_f64(b, x);
        }
        None => b.push(0),
    }
}

fn put_opt_str(b: &mut Vec<u8>, v: Option<&str>) {
    match v {
        Some(s) => {
            b.push(1);
            put_str(b, s);
        }
        None => b.push(0),
    }
}

fn put_opt_text(b: &mut Vec<u8>, v: Option<&str>) {
    match v {
        Some(s) => {
            b.push(1);
            put_text(b, s);
        }
        None => b.push(0),
    }
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

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_bits(u64::from_be_bytes(
            self.take(8)?.try_into().ok()?,
        )))
    }

    fn str(&mut self) -> Option<String> {
        let len = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn text(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn opt_i64(&mut self) -> Option<Option<i64>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.i64()?)),
        }
    }

    fn opt_f64(&mut self) -> Option<Option<f64>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.f64()?)),
        }
    }

    fn opt_str(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.str()?)),
        }
    }

    fn opt_text(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.text()?)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foxhole_cot::Affiliation;

    fn record(uid: &str, source: &str, staged_marker: bool) -> IntelRecord {
        let mut event = if staged_marker {
            CotEvent::marker(uid, Affiliation::Friendly, "OP-1", 48.86, 2.29, 1000, 4600)
        } else {
            CotEvent::zone(uid, "AO ALPHA", 50.4, 30.5, 400_000.0, 1000, 4600)
        };
        event.remarks = Some("shelling reported\nsecond line".to_string());
        IntelRecord {
            source: source.to_string(),
            event,
            received_at: 1_700_000_000,
        }
    }

    fn same(a: &IntelRecord, b: &IntelRecord) {
        assert_eq!(a.source, b.source);
        assert_eq!(a.received_at, b.received_at);
        assert_eq!(a.event, b.event);
    }

    #[test]
    fn round_trips_live_and_staged() {
        let live = vec![record("z1", "aa", false)];
        let staged = vec![record("m1", "bb", true), record("m2", "cc", true)];
        let (l2, s2) = deserialize(&serialize(&live, &staged)).expect("decode");
        assert_eq!(l2.len(), 1);
        assert_eq!(s2.len(), 2);
        same(&live[0], &l2[0]);
        same(&staged[1], &s2[1]);
    }

    #[test]
    fn preserves_none_timestamps() {
        // A stale-less event must reload stale-less (else it would be wrongly
        // expired on the next sweep).
        let mut r = record("z1", "aa", false);
        r.event.stale = None;
        r.event.start = None;
        let (live, _) = deserialize(&serialize(std::slice::from_ref(&r), &[])).expect("decode");
        assert_eq!(live[0].event.stale, None);
        assert_eq!(live[0].event.start, None);
        assert_eq!(live[0].event.time, Some(1000));
    }

    #[test]
    fn bad_blob_is_rejected() {
        assert!(deserialize(b"nope").is_none());
        assert!(deserialize(b"").is_none());
        assert!(deserialize(b"FXI1").is_none()); // magic only, truncated
    }

    #[test]
    fn save_then_load_round_trips_through_disk() {
        let path = std::env::temp_dir().join("foxhole_intel_rt.lxmi");
        let _ = std::fs::remove_file(&path);
        let key = [9u8; 64];

        let live = vec![record("z1", "aa", false)];
        let staged = vec![record("m1", "bb", true)];
        save_to(&path, &key, &live, &staged).unwrap();

        let ((l, s), skipped) = load_from(&path, &key);
        assert!(!skipped);
        same(&live[0], &l[0]);
        same(&staged[0], &s[0]);

        // Wrong key (different identity) → skipped, empty.
        let ((l, s), skipped) = load_from(&path, &[1u8; 64]);
        assert!(skipped && l.is_empty() && s.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_empty_not_skipped() {
        let path = std::env::temp_dir().join("foxhole_intel_absent.lxmi");
        let _ = std::fs::remove_file(&path);
        let ((l, s), skipped) = load_from(&path, &[2u8; 64]);
        assert!(!skipped && l.is_empty() && s.is_empty());
    }
}
