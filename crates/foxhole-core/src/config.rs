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
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Name announced on our `lxmf.delivery` destination.
    pub display_name: String,
    /// TCP hub `host[:port]` to dial out to. `None` = LAN-only (AutoInterface).
    pub hub: Option<String>,
    /// Active propagation node, as a hex destination hash.
    pub propagation_node: Option<String>,
    /// Operator's own latitude in decimal degrees (north positive), if set.
    /// Paired with [`Config::lon`] to plot this node on the World Map tool.
    pub lat: Option<f64>,
    /// Operator's own longitude in decimal degrees (east positive), if set.
    pub lon: Option<f64>,
    /// Auto-apply received CoT intel from *every* peer, bypassing the staging
    /// review for Unknown/Untrusted sources (Trusted is always auto-applied,
    /// Compromised always dropped — see the intel trust gating). Off by default
    /// so unvetted intel is staged for the operator (design note §6).
    pub intel_auto_apply: bool,
    /// Default time-to-live (seconds) applied to a received CoT event that
    /// carries no usable `stale`, so map-flooding stale-less intel still expires
    /// (§6 / §9). Defaults to [`DEFAULT_INTEL_TTL_SECS`].
    pub intel_ttl_secs: u64,
}

/// Fallback validity window for a stale-less CoT event: 6 hours, matching the
/// reference injector's default `--stale`.
pub const DEFAULT_INTEL_TTL_SECS: u64 = 6 * 3600;

impl Default for Config {
    fn default() -> Self {
        Self {
            display_name: "foxhole".to_string(),
            hub: None,
            propagation_node: None,
            lat: None,
            lon: None,
            intel_auto_apply: false,
            intel_ttl_secs: DEFAULT_INTEL_TTL_SECS,
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
                // A blank or unparseable coordinate clears to `None` so a junk
                // edit never plots the operator at a bogus spot.
                "lat" => cfg.lat = parse_coord(value),
                "lon" => cfg.lon = parse_coord(value),
                "intel_auto_apply" => cfg.intel_auto_apply = parse_bool(value),
                // A blank/unparseable value falls back to the default TTL so a
                // junk edit never disables expiry.
                "intel_ttl_secs" => {
                    cfg.intel_ttl_secs = value
                        .parse()
                        .ok()
                        .filter(|&n| n > 0)
                        .unwrap_or(DEFAULT_INTEL_TTL_SECS)
                }
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
        if let Some(lat) = self.lat {
            s.push_str(&format!("lat = {lat}\n"));
        }
        if let Some(lon) = self.lon {
            s.push_str(&format!("lon = {lon}\n"));
        }
        if self.intel_auto_apply {
            s.push_str("intel_auto_apply = true\n");
        }
        if self.intel_ttl_secs != DEFAULT_INTEL_TTL_SECS {
            s.push_str(&format!("intel_ttl_secs = {}\n", self.intel_ttl_secs));
        }
        s
    }

    /// The operator's own position, when both coordinates are configured. Fed to
    /// the World Map tool as the `Operator` marker.
    pub fn operator_pos(&self) -> Option<crate::domain::GeoPos> {
        match (self.lat, self.lon) {
            (Some(lat), Some(lon)) => Some(crate::domain::GeoPos::new(lat, lon)),
            _ => None,
        }
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

/// Parse a decimal-degree coordinate, ignoring blanks and junk (which clear the
/// field to `None`). Only finite values are accepted.
fn parse_coord(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse a boolean flag: `true`/`yes`/`on`/`1` (case-insensitive) are true,
/// anything else false.
fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
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
            lat: Some(48.8566),
            lon: Some(2.3522),
            intel_auto_apply: true,
            intel_ttl_secs: 3600,
        };
        assert_eq!(Config::parse(&cfg.serialize()), cfg);
    }

    #[test]
    fn parses_coordinates_and_ignores_junk() {
        let cfg = Config::parse("lat = 51.5\nlon = -0.12\n");
        assert_eq!(cfg.lat, Some(51.5));
        assert_eq!(cfg.lon, Some(-0.12));
        assert!(cfg.operator_pos().is_some());

        // A blank or unparseable coordinate clears the field; one coordinate
        // alone is not a usable fix.
        let cfg = Config::parse("lat = north\nlon =\n");
        assert!(cfg.lat.is_none() && cfg.lon.is_none());
        assert!(cfg.operator_pos().is_none());
        assert!(Config::parse("lat = 10\n").operator_pos().is_none());
    }

    #[test]
    fn parses_intel_knobs_and_falls_back_on_junk() {
        let cfg = Config::parse("intel_auto_apply = yes\nintel_ttl_secs = 1800\n");
        assert!(cfg.intel_auto_apply);
        assert_eq!(cfg.intel_ttl_secs, 1800);

        // Defaults: staging on, the standard TTL.
        let cfg = Config::default();
        assert!(!cfg.intel_auto_apply);
        assert_eq!(cfg.intel_ttl_secs, DEFAULT_INTEL_TTL_SECS);

        // A junk or zero TTL clears back to the default (expiry never disabled).
        let cfg = Config::parse("intel_auto_apply = nope\nintel_ttl_secs = 0\n");
        assert!(!cfg.intel_auto_apply);
        assert_eq!(cfg.intel_ttl_secs, DEFAULT_INTEL_TTL_SECS);
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
