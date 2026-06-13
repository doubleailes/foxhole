//! Persistent application configuration.
//!
//! A deliberately small `key = value` file (no serde/TOML dependency, so the
//! default build stays lean and the file stays hand-editable). It is the home
//! for settings that must survive restarts — starting with the operator's
//! display name, the TCP hub, and the active LXMF propagation node. Writes go
//! through [`crate::storage::atomic_write`] so a power-loss never leaves a torn
//! config on a field terminal.

use std::path::PathBuf;

/// Where FoxHole keeps its identity, Reticulum config, and this file.
/// Overridable with `FOXHOLE_CONFIG_DIR`; defaults to `~/.config/foxhole`.
pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("FOXHOLE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("foxhole")
}

/// Persisted operator settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Name announced on our `lxmf.delivery` destination.
    pub display_name: String,
    /// TCP hub `host[:port]` to dial out to. `None` = LAN-only (AutoInterface).
    pub hub: Option<String>,
    /// Active propagation node, as a hex destination hash.
    pub propagation_node: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            display_name: "foxhole".to_string(),
            hub: None,
            propagation_node: None,
        }
    }
}

impl Config {
    /// Path to the config file within the config dir.
    fn path() -> PathBuf {
        config_dir().join("foxhole.conf")
    }

    /// Load from disk, falling back to defaults for a missing file, unreadable
    /// file, or any unrecognized/blank field. Never fails — a corrupt line is
    /// simply ignored so the terminal always comes up.
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    /// Parse the `key = value` body. Unknown keys and blank/comment (`#`) lines
    /// are skipped; later duplicates win.
    fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "display_name" if !value.is_empty() => cfg.display_name = value.to_string(),
                "hub" => cfg.hub = non_empty(value),
                "propagation_node" => cfg.propagation_node = non_empty(value),
                _ => {}
            }
        }
        cfg
    }

    /// Serialize to the `key = value` form written to disk.
    fn serialize(&self) -> String {
        let mut s = format!("display_name = {}\n", self.display_name);
        if let Some(ref hub) = self.hub {
            s.push_str(&format!("hub = {hub}\n"));
        }
        if let Some(ref node) = self.propagation_node {
            s.push_str(&format!("propagation_node = {node}\n"));
        }
        s
    }

    /// Atomically persist the config, creating the config dir if needed.
    pub fn save(&self) -> std::io::Result<()> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir)?;
        crate::storage::atomic_write(&Self::path(), self.serialize().as_bytes())
    }
}

/// `Some(trimmed)` unless the value is empty.
fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_serialize_parse() {
        let cfg = Config {
            display_name: "rat-six".to_string(),
            hub: Some("hub.example:4242".to_string()),
            propagation_node: Some("00112233445566778899aabbccddeeff".to_string()),
        };
        assert_eq!(Config::parse(&cfg.serialize()), cfg);
    }

    #[test]
    fn defaults_when_empty_and_skips_junk() {
        let cfg = Config::parse("# a comment\n\nnonsense line\nunknown = x\n");
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.display_name, "foxhole");
        assert!(cfg.hub.is_none() && cfg.propagation_node.is_none());
    }

    #[test]
    fn blank_optional_clears_to_none() {
        let cfg = Config::parse("display_name = me\nhub =\npropagation_node =\n");
        assert_eq!(cfg.display_name, "me");
        assert!(cfg.hub.is_none());
        assert!(cfg.propagation_node.is_none());
    }
}
