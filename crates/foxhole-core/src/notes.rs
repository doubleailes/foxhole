//! Persistent note buffer: ten free-text scratch slots (port of ghostlink's
//! `notebuffer.py`).
//!
//! A "field shell" convenience: stash a hash, a grid reference, a frequency, or
//! any short string in one of slots `0`–`9` for quick recall without copy/paste,
//! surviving restarts. Like [`crate::config`], it is a small `key = value` text
//! file under [`config_dir`] written through [`crate::storage::atomic_write`], so
//! it persists offline too and is destroyed by a burn along with everything else
//! in the config dir. (Plaintext, matching the config posture — the encrypted
//! conversation store is the place for message content, not scratch notes.)

use std::io;
use std::path::PathBuf;

use crate::config::config_dir;

/// Number of slots, addressed `0`..`SLOTS-1`.
pub const SLOTS: usize = 10;

/// The ten note slots. A slot is empty when its string is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notes {
    slots: [String; SLOTS],
}

impl Default for Notes {
    fn default() -> Self {
        // `[String; N]` has no blanket `Default`; build it explicitly.
        Self {
            slots: std::array::from_fn(|_| String::new()),
        }
    }
}

impl Notes {
    /// Path to the note-buffer file within the config dir.
    fn path() -> PathBuf {
        config_dir().join("notes")
    }

    /// Load from disk, falling back to an empty buffer for a missing/unreadable
    /// file. Never fails — a corrupt line is ignored so the terminal comes up.
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    /// Parse the `N = value` body. Keys outside `0..SLOTS`, blank lines, and
    /// `#` comments are skipped; later duplicates win.
    fn parse(text: &str) -> Self {
        let mut notes = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if let Ok(slot) = key.trim().parse::<usize>()
                && slot < SLOTS
            {
                notes.slots[slot] = value.trim().to_string();
            }
        }
        notes
    }

    /// Serialize the non-empty slots to the `N = value` form written to disk.
    fn serialize(&self) -> String {
        let mut s = String::new();
        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.is_empty() {
                s.push_str(&format!("{i} = {slot}\n"));
            }
        }
        s
    }

    /// Atomically persist the buffer, creating the config dir if needed.
    pub fn save(&self) -> io::Result<()> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir)?;
        crate::storage::atomic_write(&Self::path(), self.serialize().as_bytes())
    }

    /// The slots, in order (for rendering).
    pub fn slots(&self) -> &[String; SLOTS] {
        &self.slots
    }

    /// Read one slot (empty string if out of range).
    pub fn get(&self, slot: usize) -> &str {
        self.slots.get(slot).map(String::as_str).unwrap_or("")
    }

    /// Append a character to a slot.
    pub fn push_char(&mut self, slot: usize, c: char) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.push(c);
        }
    }

    /// Delete the last character of a slot.
    pub fn pop_char(&mut self, slot: usize) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.pop();
        }
    }

    /// Empty a slot.
    pub fn clear(&mut self, slot: usize) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.clear();
        }
    }

    /// Number of non-empty slots.
    pub fn count(&self) -> usize {
        self.slots.iter().filter(|s| !s.is_empty()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_serialize_parse() {
        let mut n = Notes::default();
        n.slots[0] = "a1b2c3d4e5f600112233445566778899".to_string();
        n.slots[3] = "grid 38TKL 1234 5678".to_string();
        n.slots[9] = "146.520 MHz".to_string();
        assert_eq!(Notes::parse(&n.serialize()), n);
    }

    #[test]
    fn empty_slots_are_not_written() {
        let mut n = Notes::default();
        n.slots[2] = "x".to_string();
        let text = n.serialize();
        assert_eq!(text, "2 = x\n");
        assert_eq!(n.count(), 1);
    }

    #[test]
    fn parse_skips_junk_and_out_of_range_slots() {
        let n = Notes::parse("# note\n\nnonsense\n42 = too big\n1 = keep\n");
        assert_eq!(n.get(1), "keep");
        assert_eq!(n.count(), 1);
    }

    #[test]
    fn value_with_equals_is_preserved() {
        let n = Notes::parse("0 = key=value pair\n");
        assert_eq!(n.get(0), "key=value pair");
    }

    #[test]
    fn edit_helpers_mutate_the_right_slot() {
        let mut n = Notes::default();
        n.push_char(5, 'h');
        n.push_char(5, 'i');
        assert_eq!(n.get(5), "hi");
        n.pop_char(5);
        assert_eq!(n.get(5), "h");
        n.clear(5);
        assert_eq!(n.get(5), "");
        // Out-of-range is a no-op, not a panic.
        n.push_char(99, 'x');
        assert_eq!(n.count(), 0);
    }
}
