//! The received-intel record: one shared CoT object plus the provenance and
//! bookkeeping FoxHole needs to gate, attribute, and expire it.
//!
//! Pure data and derived queries, with no notion of *where* the object came from
//! or what the operator may do with it — the trust gating, staging, and sweeping
//! live in `app::intel`, the rendering in `foxhole-tui`, and the encrypted
//! persistence in `foxhole-net`. All three agree on the shape defined here.

use foxhole_cot::{Affiliation, CotEvent, Kind};

use super::GeoPos;

/// Hard ceiling on how long any received object stays valid, measured from when
/// we ingested it (30 days). A peer's CoT `stale` is otherwise honoured verbatim,
/// so a hostile (or buggy) far-future `stale`/`time` — the ISO-8601 codec accepts
/// up to year 9999 — would pin a marker on the map forever and defeat the periodic
/// sweep. Clamping `effective_stale` to `received_at + this` guarantees every
/// record eventually expires regardless of what the wire claims. Well above any
/// legitimate validity window (operator-shared intel defaults to a 6-hour TTL).
pub const MAX_INTEL_LIFETIME_SECS: i64 = 30 * 24 * 3600;

/// One received CoT object plus the provenance and bookkeeping foxhole needs to
/// gate, attribute, and expire it.
#[derive(Clone, Debug, PartialEq)]
pub struct IntelRecord {
    /// Sender's hex destination hash — the cryptographic origin (LXMF signature)
    /// the trust gating keys on. Half of the `(source, uid)` identity.
    pub source: String,
    /// The decoded CoT event.
    pub event: CotEvent,
    /// When we ingested it (Unix epoch **seconds**, UTC) — the fallback clock for
    /// a stale-less / time-less event.
    pub received_at: u64,
}

impl IntelRecord {
    /// The object identity for upsert/revoke: `(source, uid)`.
    pub fn key(&self) -> (&str, &str) {
        (self.source.as_str(), self.event.uid.as_str())
    }

    /// Event time for newest-wins ordering: the CoT `time`, else `start`, else the
    /// receipt time (so a time-less event still orders sensibly).
    pub fn time(&self) -> i64 {
        self.event
            .time
            .or(self.event.start)
            .unwrap_or(self.received_at as i64)
    }

    /// When this object stops being valid: the CoT `stale`, or `time + ttl` for a
    /// stale-less event so map-flooding intel still expires (`ttl` is the
    /// configured default). Capped at `received_at + `[`MAX_INTEL_LIFETIME_SECS`]
    /// so an attacker-supplied far-future `stale`/`time` can't defeat the sweep.
    pub fn effective_stale(&self, ttl: u64) -> i64 {
        let raw = self.event.stale.unwrap_or_else(|| self.time() + ttl as i64);
        let ceiling = (self.received_at as i64).saturating_add(MAX_INTEL_LIFETIME_SECS);
        raw.min(ceiling)
    }

    /// Whether the object has expired at `now` (epoch seconds), given the default
    /// `ttl` for stale-less events.
    pub fn is_expired(&self, now: i64, ttl: u64) -> bool {
        now >= self.effective_stale(ttl)
    }

    /// Seconds until the object goes stale at `now` (negative once expired).
    pub fn seconds_to_stale(&self, now: i64, ttl: u64) -> i64 {
        self.effective_stale(ttl) - now
    }

    /// Affiliation read from the CoT `type` (drives the tint/glyph).
    pub fn affiliation(&self) -> Affiliation {
        self.event.affiliation()
    }

    /// The object's map kind (marker / zone / route / other).
    pub fn kind(&self) -> Kind {
        self.event.kind()
    }

    /// Where to plot it.
    pub fn pos(&self) -> GeoPos {
        GeoPos::new(self.event.point.lat, self.event.point.lon)
    }

    /// Circular-zone radius in kilometres, if this object is a zone.
    pub fn radius_km(&self) -> Option<f64> {
        self.event.radius_m.map(|m| m / 1000.0)
    }

    /// What to show in the roster / on the map: the callsign, else a short uid.
    pub fn label(&self) -> String {
        match &self.event.callsign {
            Some(cs) if !cs.is_empty() => cs.clone(),
            _ if !self.event.uid.is_empty() => self.event.uid.clone(),
            _ => "(intel)".to_string(),
        }
    }
}

/// A live intel zone ready to draw on the map canvas — the circular overlay plus
/// the facets the renderer tints/labels it with. Built by [`App::intel_zones`].
#[derive(Clone, Debug, PartialEq)]
pub struct IntelZone {
    pub label: String,
    pub center: GeoPos,
    pub radius_km: f64,
    pub affiliation: Affiliation,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record whose event carries an explicit `stale`.
    fn record(stale: Option<i64>, received_at: u64) -> IntelRecord {
        let mut event = CotEvent::marker("u1", Affiliation::Hostile, "AO", 50.4, 30.5, 1_000, 0);
        event.stale = stale;
        IntelRecord {
            source: "aa".to_string(),
            event,
            received_at,
        }
    }

    #[test]
    fn effective_stale_falls_back_to_the_default_ttl() {
        // With an explicit stale, that wins.
        assert_eq!(record(Some(5_000), 0).effective_stale(3_600), 5_000);
        // Without one, it is `time + ttl` — a stale-less event still expires.
        assert_eq!(record(None, 0).effective_stale(3_600), 1_000 + 3_600);
    }

    #[test]
    fn effective_stale_is_capped_at_the_max_lifetime() {
        // A hostile far-future stale can't outlive received_at + the ceiling, so
        // the sweep can always reclaim it.
        let r = record(Some(253_402_300_799), 1_000); // year-9999 stale
        assert_eq!(
            r.effective_stale(3_600),
            1_000 + MAX_INTEL_LIFETIME_SECS,
            "far-future stale is clamped to the ceiling"
        );
        assert!(r.is_expired(1_000 + MAX_INTEL_LIFETIME_SECS, 3_600));
    }

    #[test]
    fn expiry_and_countdown_track_the_effective_stale() {
        let r = record(Some(5_000), 0);
        assert!(!r.is_expired(4_999, 3_600));
        assert!(r.is_expired(5_000, 3_600), "expiry is inclusive");
        assert_eq!(r.seconds_to_stale(4_900, 3_600), 100);
        assert!(r.seconds_to_stale(5_100, 3_600) < 0);
    }

    #[test]
    fn time_falls_back_through_start_to_receipt() {
        let mut r = record(None, 42);
        assert_eq!(r.time(), 1_000, "the event time wins");
        r.event.time = None;
        r.event.start = Some(900);
        assert_eq!(r.time(), 900, "then start");
        r.event.start = None;
        assert_eq!(r.time(), 42, "then the receipt time");
    }

    #[test]
    fn label_prefers_callsign_then_uid() {
        let mut r = record(None, 0);
        assert_eq!(r.label(), "AO");
        r.event.callsign = None;
        assert_eq!(r.label(), "u1");
        r.event.uid = String::new();
        assert_eq!(r.label(), "(intel)");
    }

    #[test]
    fn radius_is_reported_in_kilometres() {
        let mut r = record(None, 0);
        assert_eq!(r.radius_km(), None);
        r.event.radius_m = Some(2_500.0);
        assert_eq!(r.radius_km(), Some(2.5));
    }
}
