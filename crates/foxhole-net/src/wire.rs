//! Shared plumbing for FoxHole's encrypted on-disk stores.
//!
//! Both the conversation store and the intel store use the same recipe — a
//! hand-rolled versioned binary blob, authenticated-encrypted with
//! `rns_crypto::token` (AES-256-CBC + HMAC-SHA256, random IV), written through
//! [`foxhole_core::storage::atomic_write`] — and the same bounds-checked
//! reader/writer primitives for the blob itself. They live here so the two
//! stores can't drift apart on framing or on what "a corrupt file" means.

use std::io;
use std::path::Path;

use rns_crypto::token;

// --- Sealed files ---------------------------------------------------------------

/// What reading a sealed file produced.
pub(crate) enum Sealed {
    /// No file yet — a first run, not an error.
    Missing,
    /// Decrypted plaintext, ready to decode.
    Plain(Vec<u8>),
    /// Present but unreadable, undecryptable (foreign identity / tampered), or
    /// otherwise not ours. Callers skip it and carry on.
    Corrupt,
}

/// Encrypt `blob` under `key` and atomically replace `path` with it, creating
/// the parent directory if needed.
pub(crate) fn seal(path: &Path, key: &[u8; 64], blob: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let token = token::encrypt(blob, key).map_err(|e| io::Error::other(format!("encrypt: {e}")))?;
    foxhole_core::storage::atomic_write(path, &token)
}

/// Read and decrypt a sealed file. Never fails loudly: a missing file is a first
/// run and a bad one is [`Sealed::Corrupt`], so a damaged store can't stop the
/// terminal coming up.
pub(crate) fn unseal(path: &Path, key: &[u8; 64]) -> Sealed {
    let Ok(bytes) = std::fs::read(path) else {
        return Sealed::Missing;
    };
    match token::decrypt(&bytes, key) {
        Ok(plain) => Sealed::Plain(plain),
        Err(_) => Sealed::Corrupt,
    }
}

// --- Blob writers ---------------------------------------------------------------

/// `u16` length-prefixed string (short fields: peer hash, display name, uid).
pub(crate) fn put_str(b: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    b.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    b.extend_from_slice(bytes);
}

/// `u32` length-prefixed text (message bodies / remarks — may be long).
pub(crate) fn put_text(b: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    b.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    b.extend_from_slice(bytes);
}

pub(crate) fn put_f64(b: &mut Vec<u8>, v: f64) {
    b.extend_from_slice(&v.to_bits().to_be_bytes());
}

/// Presence byte (`0`/`1`) then, if present, the value — preserving `Option`.
pub(crate) fn put_opt_i64(b: &mut Vec<u8>, v: Option<i64>) {
    match v {
        Some(x) => {
            b.push(1);
            b.extend_from_slice(&x.to_be_bytes());
        }
        None => b.push(0),
    }
}

pub(crate) fn put_opt_f64(b: &mut Vec<u8>, v: Option<f64>) {
    match v {
        Some(x) => {
            b.push(1);
            put_f64(b, x);
        }
        None => b.push(0),
    }
}

pub(crate) fn put_opt_str(b: &mut Vec<u8>, v: Option<&str>) {
    match v {
        Some(s) => {
            b.push(1);
            put_str(b, s);
        }
        None => b.push(0),
    }
}

pub(crate) fn put_opt_text(b: &mut Vec<u8>, v: Option<&str>) {
    match v {
        Some(s) => {
            b.push(1);
            put_text(b, s);
        }
        None => b.push(0),
    }
}

// --- Blob reader ----------------------------------------------------------------

/// Bounds-checked sequential reader; any out-of-range read yields `None`, which
/// propagates up as "this blob is corrupt" rather than a panic.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    pub(crate) fn i64(&mut self) -> Option<i64> {
        Some(i64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    pub(crate) fn f64(&mut self) -> Option<f64> {
        Some(f64::from_bits(u64::from_be_bytes(
            self.take(8)?.try_into().ok()?,
        )))
    }

    pub(crate) fn str(&mut self) -> Option<String> {
        let len = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    pub(crate) fn text(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    pub(crate) fn opt_i64(&mut self) -> Option<Option<i64>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.i64()?)),
        }
    }

    pub(crate) fn opt_f64(&mut self) -> Option<Option<f64>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.f64()?)),
        }
    }

    pub(crate) fn opt_str(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.str()?)),
        }
    }

    pub(crate) fn opt_text(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.text()?)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writers_and_reader_round_trip_every_shape() {
        let mut b = Vec::new();
        put_str(&mut b, "peer");
        put_text(&mut b, "a long body\nwith a newline");
        put_f64(&mut b, -12.5);
        put_opt_i64(&mut b, Some(-7));
        put_opt_i64(&mut b, None);
        put_opt_f64(&mut b, Some(0.25));
        put_opt_f64(&mut b, None);
        put_opt_str(&mut b, Some("call"));
        put_opt_str(&mut b, None);
        put_opt_text(&mut b, Some("remarks"));
        put_opt_text(&mut b, None);

        let mut r = Reader::new(&b);
        assert_eq!(r.str().unwrap(), "peer");
        assert_eq!(r.text().unwrap(), "a long body\nwith a newline");
        assert_eq!(r.f64().unwrap(), -12.5);
        assert_eq!(r.opt_i64().unwrap(), Some(-7));
        assert_eq!(r.opt_i64().unwrap(), None);
        assert_eq!(r.opt_f64().unwrap(), Some(0.25));
        assert_eq!(r.opt_f64().unwrap(), None);
        assert_eq!(r.opt_str().unwrap(), Some("call".to_string()));
        assert_eq!(r.opt_str().unwrap(), None);
        assert_eq!(r.opt_text().unwrap(), Some("remarks".to_string()));
        assert_eq!(r.opt_text().unwrap(), None);
        // Reading past the end is None, never a panic.
        assert!(r.u8().is_none());
    }

    #[test]
    fn truncated_blob_reads_none_instead_of_panicking() {
        let mut b = Vec::new();
        put_text(&mut b, "hello");
        b.truncate(6); // length prefix says 5 bytes, only 2 remain
        assert!(Reader::new(&b).text().is_none());
    }

    #[test]
    fn sealed_round_trips_and_reports_missing_or_corrupt() {
        let mut path = std::env::temp_dir();
        path.push("foxhole_wire_seal_test.bin");
        let _ = std::fs::remove_file(&path);
        let key = [3u8; 64];

        assert!(matches!(unseal(&path, &key), Sealed::Missing));

        seal(&path, &key, b"payload").unwrap();
        match unseal(&path, &key) {
            Sealed::Plain(p) => assert_eq!(p, b"payload"),
            _ => panic!("should decrypt"),
        }

        // A different key can't authenticate it.
        assert!(matches!(unseal(&path, &[4u8; 64]), Sealed::Corrupt));

        std::fs::write(&path, b"not a token").unwrap();
        assert!(matches!(unseal(&path, &key), Sealed::Corrupt));

        let _ = std::fs::remove_file(&path);
    }
}
