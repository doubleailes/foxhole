//! Atomic filesystem primitives.
//!
//! Every persistent-state mutation in FoxHole (identity keys, config, message
//! store) must funnel through `atomic_write`. The field constraint is hard: a
//! reader — or a power-loss recovery on a forward-deployed terminal — must see
//! either the previous file contents in full or the new contents in full, never
//! a torn write. We achieve this with the classic write-temp → fsync → rename
//! dance: a same-filesystem `rename` is atomic on POSIX.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// Atomically replace the contents of `path` with `bytes`.
///
/// The temp file is created as a hidden sibling in the *same* directory so the
/// final `rename` stays on one filesystem (cross-device renames are not atomic
/// and would fall back to a copy). On any error the temp file is cleaned up
/// best-effort and the original `path` is left untouched.
///
/// Currently unused — wired in ahead of the persistence layer so all future
/// state writes have a single durable path to go through.
#[allow(dead_code)]
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "target path has no file name")
    })?;

    // Hidden sibling temp file, e.g. `config.toml` -> `.config.toml.tmp`.
    let mut tmp = dir.to_path_buf();
    tmp.push(format!(".{}.tmp", file_name.to_string_lossy()));

    // Write + durably flush to disk before we expose the file via rename.
    let write_res = (|| -> io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?; // fsync: contents on stable storage before the rename
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp); // best-effort; ignore secondary failure
        return Err(e);
    }

    // The rename itself is the atomic swap.
    fs::rename(&tmp, path)?;

    // Best-effort durability of the rename: fsync the containing directory so
    // the new dir entry survives a crash. Not all platforms permit opening a
    // directory for this, hence best-effort.
    if let Ok(dir_handle) = File::open(dir) {
        let _ = dir_handle.sync_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_and_replaces() {
        // Use a unique subdir under the OS temp dir.
        let mut path = std::env::temp_dir();
        path.push("foxhole_atomic_write_test.bin");
        let _ = fs::remove_file(&path);

        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");

        atomic_write(&path, b"second-longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second-longer");

        // No stray temp file left behind.
        let tmp = path.with_file_name(".foxhole_atomic_write_test.bin.tmp");
        assert!(!tmp.exists());

        let _ = fs::remove_file(&path);
    }
}
