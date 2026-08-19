//! Received-intel layer: ingest, trust-gate, stage, and expire CoT events
//! shared by peers (design note §6).
//!
//! A peer's CoT event arrives decoded as [`NetEvent::Cot`](crate::domain::NetEvent)
//! and is folded in by [`App::apply_cot`]. Provenance is the LXMF signature (the
//! `source` hash), so trust is keyed on the sending peer:
//!
//! - **Trusted** → applied straight to the live map layer ([`IntelState::live`]).
//! - **Unknown / Untrusted** → **staged** for operator review
//!   ([`IntelState::staged`]); accept promotes it, discard drops it. (A config
//!   toggle, [`Config::intel_auto_apply`](crate::config::Config), opts into
//!   auto-applying these too.)
//! - **Compromised** → dropped (logged, never shown).
//!
//! Objects are keyed by **`(source, uid)`** with newest-`time`-wins semantics; a
//! revocation removes them; and every record carries an effective `stale` (the
//! event's, or a configured default TTL for stale-less intel) that a periodic
//! [`App::sweep_intel`] enforces — the map can never fill with immortal markers.
//!
//! Scope: what *arrives*. The record itself is [`IntelRecord`] in
//! `crate::domain`, sending intel out is `app::share`, drawing it locally is
//! `app::author`, and the canvas/panel rendering is `foxhole-tui`.

use super::*;
use crate::domain::now_secs;
use foxhole_cot::{CotEvent, Kind};

/// Modal state for the "incoming intel" review list (design note §6): the staged
/// events from Unknown/Untrusted peers the operator accepts or discards.
pub struct IntelReview {
    /// Highlighted row within [`IntelState::staged`].
    pub selected: usize,
}

/// The received/authored intel layer: what is applied to the map, what is
/// waiting for the operator's verdict, and the three modals that act on it.
///
/// Kept a sibling of [`MapState`](super::MapState) rather than folded into it:
/// the map *draws* this layer, but the layer's lifetime is the network's — it
/// arrives from peers, is persisted encrypted by the binary, and is swept on a
/// timer, none of which the viewport has a say in.
#[derive(Default)]
pub struct IntelState {
    /// Live received CoT intel applied to the map (from Trusted peers, or all
    /// peers when `intel_auto_apply` is set, or operator-accepted). Keyed by
    /// `(source, uid)`; expired entries are swept.
    pub live: Vec<IntelRecord>,
    /// Received CoT intel from Unknown/Untrusted peers, staged for operator
    /// review (accept → `live`, or discard).
    pub staged: Vec<IntelRecord>,
    /// Set when the live/staged layer changed this iteration; `main` drains it
    /// and persists the encrypted intel store. Keeps `App` free of I/O.
    pub dirty: bool,
    /// When `Some`, the incoming-intel review modal is open (captures input).
    pub review: Option<IntelReview>,
    /// When `Some`, the share-zone picker is open (captures input).
    pub share_zone: Option<ShareZone>,
    /// When `Some`, the intel authoring form is open (captures input).
    pub author: Option<AuthorForm>,
}

impl App {
    /// Fold a decoded CoT event from `source` into the received-intel layer,
    /// applying trust gating, revocation, and newest-wins upsert. The entry point
    /// for [`NetEvent::Cot`](crate::domain::NetEvent).
    pub fn apply_cot(&mut self, source: String, event: CotEvent) {
        let who = crate::domain::short_hash(&source).to_string();

        // A revocation (stale ≤ time, or a delete type) removes the object from
        // both layers regardless of trust — the originator is taking it back.
        if event.is_revocation() {
            let removed = self.revoke_intel(&source, &event.uid);
            if removed {
                self.intel.dirty = true;
                self.push_log(format!("[SYS] intel: {who} revoked {}", event.uid));
            }
            return;
        }

        let record = IntelRecord {
            source,
            event,
            received_at: now_secs(),
        };

        match self.peer_trust(&record.source) {
            Trust::Compromised => {
                // Dropped — never shown — but logged so the operator knows hostile
                // traffic is being filtered.
                self.push_log(format!("[SYS] intel: dropped event from compromised {who}"));
            }
            Trust::Trusted => {
                let label = record.label();
                if upsert(&mut self.intel.live, record) {
                    self.intel.dirty = true;
                    self.push_log(format!("[SYS] intel: applied {label} from {who}"));
                }
            }
            // Unknown/Untrusted: stage for review unless the operator opted into
            // auto-applying all intel.
            Trust::Unknown | Trust::Untrusted => {
                if self.config.intel_auto_apply {
                    let label = record.label();
                    if upsert(&mut self.intel.live, record) {
                        self.intel.dirty = true;
                        self.push_log(format!("[SYS] intel: auto-applied {label} from {who}"));
                    }
                } else {
                    let label = record.label();
                    if upsert(&mut self.intel.staged, record) {
                        self.intel.dirty = true;
                        self.push_log(format!("[SYS] intel: staged {label} from {who} (review)"));
                    }
                }
            }
        }
    }

    /// The operator-assigned trust for a peer hash, defaulting to
    /// [`Trust::Unknown`] for a source we have no conversation with.
    fn peer_trust(&self, source: &str) -> Trust {
        self.conversations
            .iter()
            .find(|c| c.peer == source)
            .map(|c| c.trust)
            .unwrap_or(Trust::Unknown)
    }

    /// Remove any live or staged object matching `(source, uid)`. Returns whether
    /// anything was removed.
    fn revoke_intel(&mut self, source: &str, uid: &str) -> bool {
        let before = self.intel.live.len() + self.intel.staged.len();
        self.intel
            .live
            .retain(|r| !(r.source == source && r.event.uid == uid));
        self.intel
            .staged
            .retain(|r| !(r.source == source && r.event.uid == uid));
        self.clamp_intel_review();
        before != self.intel.live.len() + self.intel.staged.len()
    }

    /// Drop every expired object (live and staged) at `now`, given the configured
    /// default TTL. Returns how many were removed. Cheap to call often — `main`
    /// runs it as the periodic sweep §6 calls for.
    pub fn sweep_intel(&mut self, now: i64) -> usize {
        let ttl = self.config.intel_ttl_secs;
        let before = self.intel.live.len() + self.intel.staged.len();
        self.intel.live.retain(|r| !r.is_expired(now, ttl));
        self.intel.staged.retain(|r| !r.is_expired(now, ttl));
        self.clamp_intel_review();
        let removed = before - (self.intel.live.len() + self.intel.staged.len());
        if removed > 0 {
            self.intel.dirty = true;
        }
        removed
    }

    /// Live (applied, non-expired) intel at `now` — what the map layer plots.
    pub fn live_intel_at(&self, now: i64) -> Vec<&IntelRecord> {
        let ttl = self.config.intel_ttl_secs;
        self.intel
            .live
            .iter()
            .filter(|r| !r.is_expired(now, ttl))
            .collect()
    }

    /// Live intel as of the wall clock (renderer convenience).
    pub fn live_intel(&self) -> Vec<&IntelRecord> {
        self.live_intel_at(now_secs() as i64)
    }

    /// The live zone overlays (circular intel) to draw on the canvas.
    pub fn intel_zones(&self) -> Vec<IntelZone> {
        self.intel_zones_at(now_secs() as i64)
    }

    /// [`Self::intel_zones`] at an explicit `now` (testable without the clock).
    pub fn intel_zones_at(&self, now: i64) -> Vec<IntelZone> {
        self.live_intel_at(now)
            .into_iter()
            .filter(|r| r.kind() == Kind::Zone)
            .filter_map(|r| {
                r.radius_km().map(|radius_km| IntelZone {
                    label: r.label(),
                    center: r.pos(),
                    radius_km,
                    affiliation: r.affiliation(),
                })
            })
            .collect()
    }

    /// Open the incoming-intel review modal (no-op when nothing is staged).
    pub(super) fn open_intel_review(&mut self) {
        if !self.intel.staged.is_empty() {
            self.intel.review = Some(IntelReview { selected: 0 });
        }
    }

    /// Key handling while the incoming-intel review modal is open: Up/Down select,
    /// `a`/Enter accept (apply to the map), `x`/`d`/Delete discard, Esc close.
    pub(super) fn handle_intel_review_key(&mut self, key: KeyEvent) {
        let Some(selected) = self.intel.review.as_ref().map(|r| r.selected) else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.intel.review = None,
            KeyCode::Up => {
                if let Some(r) = self.intel.review.as_mut() {
                    r.selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if selected + 1 < self.intel.staged.len()
                    && let Some(r) = self.intel.review.as_mut()
                {
                    r.selected = selected + 1;
                }
            }
            KeyCode::Char('a') | KeyCode::Enter => self.accept_staged(selected),
            KeyCode::Char('x') | KeyCode::Char('d') | KeyCode::Delete => {
                self.discard_staged(selected)
            }
            _ => {}
        }
        // Close the modal once the queue is drained so it never lingers empty.
        if self.intel.staged.is_empty() {
            self.intel.review = None;
        }
    }

    /// Promote a staged object to the live map layer (operator vouches for it).
    pub fn accept_staged(&mut self, idx: usize) {
        if idx >= self.intel.staged.len() {
            return;
        }
        let record = self.intel.staged.remove(idx);
        let (label, who) = (
            record.label(),
            crate::domain::short_hash(&record.source).to_string(),
        );
        upsert(&mut self.intel.live, record);
        self.intel.dirty = true;
        self.push_log(format!("[SYS] intel: accepted {label} from {who}"));
        self.clamp_intel_review();
    }

    /// Discard a staged object without applying it.
    pub fn discard_staged(&mut self, idx: usize) {
        if idx >= self.intel.staged.len() {
            return;
        }
        let record = self.intel.staged.remove(idx);
        let (label, who) = (
            record.label(),
            crate::domain::short_hash(&record.source).to_string(),
        );
        self.intel.dirty = true;
        self.push_log(format!("[SYS] intel: discarded {label} from {who}"));
        self.clamp_intel_review();
    }

    /// Keep the review cursor within the staged list after a removal.
    fn clamp_intel_review(&mut self) {
        if let Some(review) = self.intel.review.as_mut() {
            review.selected = review
                .selected
                .min(self.intel.staged.len().saturating_sub(1));
        }
    }
}

/// Upsert a record into a layer keyed by `(source, uid)` with newest-`time`-wins
/// semantics. Returns whether the layer changed (a strictly older duplicate is
/// ignored, so a replayed event doesn't churn the map or the log).
pub(super) fn upsert(layer: &mut Vec<IntelRecord>, record: IntelRecord) -> bool {
    if let Some(existing) = layer.iter_mut().find(|r| r.key() == record.key()) {
        if record.time() >= existing.time() {
            *existing = record;
            return true;
        }
        return false;
    }
    layer.push(record);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{app_with_peer, event};

    #[test]
    fn trusted_source_is_applied_unknown_is_staged() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        assert_eq!(app.intel.live.len(), 1);
        assert!(app.intel.staged.is_empty());

        // A second, unknown peer's intel is staged for review, not applied.
        app.conversations.push(Conversation::new("bb")); // defaults to Unknown
        app.apply_cot("bb".into(), event("u2", "a-h-G", 1000));
        assert_eq!(app.intel.live.len(), 1);
        assert_eq!(app.intel.staged.len(), 1);
    }

    #[test]
    fn compromised_source_is_dropped() {
        let mut app = app_with_peer("aa", Trust::Compromised);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        assert!(app.intel.live.is_empty());
        assert!(app.intel.staged.is_empty());
    }

    #[test]
    fn auto_apply_bypasses_staging_for_unknown() {
        let mut app = app_with_peer("aa", Trust::Unknown);
        app.config.intel_auto_apply = true;
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        assert_eq!(app.intel.live.len(), 1);
        assert!(app.intel.staged.is_empty());
    }

    #[test]
    fn newest_time_wins_and_replays_are_ignored() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        // A newer event for the same (source, uid) replaces in place.
        let mut newer = event("u1", "a-h-G", 2000);
        newer.callsign = Some("MOVED".into());
        app.apply_cot("aa".into(), newer);
        assert_eq!(app.intel.live.len(), 1);
        assert_eq!(app.intel.live[0].label(), "MOVED");
        // An older replay is ignored (no churn).
        app.apply_cot("aa".into(), event("u1", "a-h-G", 500));
        assert_eq!(app.intel.live.len(), 1);
        assert_eq!(app.intel.live[0].label(), "MOVED");
        // The same uid from a *different* source is kept separately (attributed).
        let mut c = Conversation::new("bb");
        c.trust = Trust::Trusted;
        app.conversations.push(c);
        app.apply_cot("bb".into(), event("u1", "a-h-G", 1000));
        assert_eq!(app.intel.live.len(), 2);
    }

    #[test]
    fn revocation_removes_the_object() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        assert_eq!(app.intel.live.len(), 1);
        // stale <= time is a revoke for the same uid.
        let mut revoke = event("u1", "a-h-G", 3000);
        revoke.stale = Some(3000);
        app.apply_cot("aa".into(), revoke);
        assert!(app.intel.live.is_empty());
    }

    #[test]
    fn sweep_drops_expired_and_keeps_live() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000)); // stale at 4600
        // Before stale: nothing swept, and the live view shows it.
        assert_eq!(app.sweep_intel(2000), 0);
        assert_eq!(app.live_intel_at(2000).len(), 1);
        // After stale: the live view hides it and the sweep reclaims it.
        assert!(app.live_intel_at(5000).is_empty());
        assert_eq!(app.sweep_intel(5000), 1);
        assert!(app.intel.live.is_empty());
    }

    #[test]
    fn stale_less_event_uses_the_default_ttl() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        app.config.intel_ttl_secs = 100;
        let mut e = event("u1", "a-h-G", 1000);
        e.stale = None; // no stale → time + ttl = 1100
        app.apply_cot("aa".into(), e);
        assert_eq!(app.live_intel_at(1050).len(), 1);
        assert!(app.live_intel_at(1200).is_empty());
    }

    #[test]
    fn accept_and_discard_move_staged_records() {
        let mut app = app_with_peer("aa", Trust::Unknown);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        app.apply_cot("aa".into(), event("u2", "a-h-G", 1000));
        assert_eq!(app.intel.staged.len(), 2);

        app.accept_staged(0);
        assert_eq!(app.intel.live.len(), 1);
        assert_eq!(app.intel.staged.len(), 1);

        app.discard_staged(0);
        assert!(app.intel.staged.is_empty());
        assert_eq!(app.intel.live.len(), 1);
    }

    #[test]
    fn zones_overlay_only_includes_live_circular_intel() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        // A marker (no radius) and a zone (with radius).
        app.apply_cot("aa".into(), event("mk", "a-h-G", 1000));
        let mut zone = CotEvent::zone("z1", "AO ALPHA", 50.4, 30.5, 400_000.0, 1000, 1000 + 3600);
        zone.cot_type = "a-h-G-U-C".into(); // hostile zone
        app.apply_cot("aa".into(), zone);

        let zones = app.intel_zones_at(2000);
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].label, "AO ALPHA");
        assert_eq!(zones[0].radius_km, 400.0);
        assert_eq!(zones[0].affiliation, Affiliation::Hostile);
    }
}
