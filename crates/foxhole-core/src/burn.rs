//! Emergency data destruction — the "burn notice".
//!
//! Shreds FoxHole's entire config directory (identity, known peers, encrypted
//! conversation history, Reticulum state, settings) so the operator can destroy
//! everything tying a session to them before the hardware is lost. Each file is
//! overwritten with zeros and `fsync`ed before being unlinked, then the tree is
//! removed.
//!
//! Honest scope: a zero-overwrite is **not** a guarantee against filesystem
//! forensics — journaling, copy-on-write, and SSD wear-levelling can retain old
//! blocks. The real guarantee is cryptographic: the conversation stores are
//! AES-256 sealed under a key HKDF-derived from the `identity` file, so once the
//! identity is destroyed the ciphertext is unrecoverable regardless.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Outcome of a burn: how much was destroyed, and anything that resisted.
#[derive(Default)]
pub struct BurnReport {
    /// Files shredded and removed.
    pub files: usize,
    /// Total bytes overwritten.
    pub bytes: u64,
    /// Paths that could not be fully destroyed, with the reason.
    pub errors: Vec<String>,
}

impl BurnReport {
    /// A one-paragraph confirmation for the operator on exit.
    pub fn render(&self) -> String {
        let mut out = String::from("\n████ DATA BURNED ████\n");
        out.push_str(&format!(
            "  {} file(s), {} bytes overwritten — identity, peers, and conversations destroyed.\n",
            self.files, self.bytes,
        ));
        if !self.errors.is_empty() {
            out.push_str(&format!(
                "  WARNING: {} item(s) resisted:\n",
                self.errors.len()
            ));
            for e in &self.errors {
                out.push_str(&format!("    - {e}\n"));
            }
        }
        out
    }
}

/// Shred everything under `dir` and remove it. A missing `dir` is a no-op
/// (nothing to burn). Best-effort: failures are collected, not fatal, so a
/// stubborn file never blocks destroying the rest.
pub fn execute(dir: &Path) -> BurnReport {
    let mut report = BurnReport::default();
    if !dir.exists() {
        return report;
    }
    burn_dir(dir, &mut report);
    // Drop the (now-empty) tree, including any dirs we couldn't recurse.
    if let Err(e) = fs::remove_dir_all(dir)
        && dir.exists()
    {
        report.errors.push(format!("{}: {e}", dir.display()));
    }
    report
}

/// Recursively shred files in `dir` (directories are removed afterwards by
/// `remove_dir_all`).
fn burn_dir(dir: &Path, report: &mut BurnReport) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            report.errors.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => burn_dir(&path, report),
            Ok(_) => match shred_file(&path) {
                Ok(n) => {
                    report.files += 1;
                    report.bytes += n;
                }
                Err(e) => report.errors.push(format!("{}: {e}", path.display())),
            },
            Err(e) => report.errors.push(format!("{}: {e}", path.display())),
        }
    }
}

/// Overwrite a file's contents with zeros (flushed + `fsync`ed) and unlink it.
/// Returns the number of bytes overwritten.
fn shred_file(path: &Path) -> io::Result<u64> {
    let len = fs::metadata(path)?.len();
    if len > 0 {
        let mut f = fs::OpenOptions::new().write(true).open(path)?;
        let zeros = [0u8; 64 * 1024];
        let mut remaining = len;
        while remaining > 0 {
            let n = remaining.min(zeros.len() as u64) as usize;
            f.write_all(&zeros[..n])?;
            remaining -= n as u64;
        }
        f.flush()?;
        f.sync_all()?;
    }
    fs::remove_file(path)?;
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp directory for a test (no external deps).
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("foxhole_burn_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn burns_nested_tree_and_removes_root() {
        let dir = temp_dir("nested");
        fs::create_dir_all(dir.join("conversations")).unwrap();
        fs::write(dir.join("identity"), b"secret-key-material").unwrap();
        fs::write(dir.join("known_identities"), b"peer-keys").unwrap();
        fs::write(dir.join("conversations").join("a.lxmc"), b"sealed").unwrap();
        fs::write(dir.join("empty"), b"").unwrap();

        let report = execute(&dir);

        assert_eq!(report.files, 4, "every file shredded");
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);
        assert!(!dir.exists(), "the config dir is gone");
    }

    #[test]
    fn missing_dir_is_a_noop() {
        let dir = temp_dir("missing"); // never created
        let report = execute(&dir);
        assert_eq!(report.files, 0);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn shred_file_removes_it() {
        let dir = temp_dir("shred");
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x");
        fs::write(&f, b"abcdef").unwrap();
        let n = shred_file(&f).unwrap();
        assert_eq!(n, 6);
        assert!(!f.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
