//! Hazard-area / area-of-operations overlay for the World Map: the [`Zone`]
//! model plus the parser and demo set that feed it.
//!
//! Zones come from a small, hand-editable `key = value`-style file under the
//! config dir (no serde/TOML), one hazard area per line:
//!
//! ```text
//! # label = lat, lon, radius_km
//! AO ALPHA = 50.4, 30.5, 400
//! ```
//!
//! Blank lines and `#` comments are skipped, and any malformed row is ignored so
//! a typo never stops the terminal coming up. The labels and coordinates are
//! operator intel — the built-in [`demo`] set uses generic `AO …` callsigns and
//! makes no claim about any real-world conflict; it just makes the overlay
//! visible offline, exactly as the seeded demo peers do.
//!
//! This module stays free of I/O: [`parse`] and [`demo`] are pure functions, and
//! the actual `zones.conf` read lives in the root binary (see `src/main.rs`).

use crate::geo::GeoPos;

/// A hazard area overlaid on the World Map — a named region with a danger
/// radius, drawn as a red circle. Sourced from the operator's hand-editable
/// `zones.conf` (situational intel) or the offline demo set; e.g. an active
/// conflict "area of operations" the operator wants kept in view.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    /// Short label (e.g. `AO ALPHA`), shown on the map and in the roster.
    pub label: String,
    /// Centre of the hazard area.
    pub center: GeoPos,
    /// Danger radius in kilometres.
    pub radius_km: f64,
}

impl Zone {
    /// A zone with its centre normalised and a non-negative radius.
    pub fn new(label: impl Into<String>, lat: f64, lon: f64, radius_km: f64) -> Self {
        Self {
            label: label.into(),
            center: GeoPos::new(lat, lon),
            radius_km: if radius_km.is_finite() {
                radius_km.max(0.0)
            } else {
                0.0
            },
        }
    }

    /// Radius in latitude degrees (~111 km per degree) for the canvas circle,
    /// floored so a small zone still renders as a visible ring.
    pub fn radius_deg(&self) -> f64 {
        (self.radius_km / 111.0).max(0.3)
    }
}

/// Parse the `label = lat, lon, radius_km` body. Rows missing the `=`, lacking
/// three numeric fields, or carrying non-finite numbers are skipped.
pub fn parse(text: &str) -> Vec<Zone> {
    let mut zones = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((label, rest)) = line.split_once('=') else {
            continue;
        };
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        let nums: Vec<f64> = rest
            .split(',')
            .filter_map(|p| p.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .collect();
        if let [lat, lon, radius_km] = nums[..] {
            zones.push(Zone::new(label, lat, lon, radius_km));
        }
    }
    zones
}

/// Illustrative demo zones for the offline build. Generic area-of-operations
/// callsigns at recognisable but unlabelled coordinates — placeholder intel the
/// operator replaces via `zones.conf`, not a statement about any real conflict.
pub fn demo() -> Vec<Zone> {
    vec![
        Zone::new("AO ALPHA", 49.0, 32.0, 450.0),
        Zone::new("AO BRAVO", 33.3, 38.0, 350.0),
        Zone::new("AO CHARLIE", 15.0, 44.5, 300.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows_and_skips_junk() {
        let zones = parse(
            "# hazard areas\n\
             AO ALPHA = 50.4, 30.5, 400\n\
             malformed line\n\
             NO NUMS = a, b, c\n\
             SHORT = 1, 2\n\
             = 1, 2, 3\n\
             AO BRAVO = 33.3, 38.0, 350\n",
        );
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].label, "AO ALPHA");
        assert_eq!(zones[0].center.lat, 50.4);
        assert_eq!(zones[0].radius_km, 400.0);
        assert_eq!(zones[1].label, "AO BRAVO");
    }

    #[test]
    fn radius_degrees_has_a_floor() {
        // A tiny radius still renders as a visible ring.
        assert_eq!(Zone::new("pin", 0.0, 0.0, 1.0).radius_deg(), 0.3);
        // A large radius scales by ~111 km/deg.
        assert!((Zone::new("ao", 0.0, 0.0, 444.0).radius_deg() - 4.0).abs() < 0.01);
    }

    #[test]
    fn demo_set_is_non_empty_and_well_formed() {
        let zones = demo();
        assert!(!zones.is_empty());
        assert!(
            zones
                .iter()
                .all(|z| z.radius_km > 0.0 && !z.label.is_empty())
        );
    }
}
