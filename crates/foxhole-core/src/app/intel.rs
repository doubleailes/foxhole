//! Received-intel layer: ingest, trust-gate, stage, and expire CoT events
//! shared by peers (design note §6).
//!
//! A peer's CoT event arrives decoded as [`NetEvent::Cot`](crate::domain::NetEvent)
//! and is folded in by [`App::apply_cot`]. Provenance is the LXMF signature (the
//! `source` hash), so trust is keyed on the sending peer:
//!
//! - **Trusted** → applied straight to the live map layer ([`App::intel`]).
//! - **Unknown / Untrusted** → **staged** for operator review
//!   ([`App::intel_staged`]); accept promotes it, discard drops it. (A config
//!   toggle, [`Config::intel_auto_apply`](crate::config::Config), opts into
//!   auto-applying these too.)
//! - **Compromised** → dropped (logged, never shown).
//!
//! Objects are keyed by **`(source, uid)`** with newest-`time`-wins semantics; a
//! revocation removes them; and every record carries an effective `stale` (the
//! event's, or a configured default TTL for stale-less intel) that a periodic
//! [`App::sweep_intel`] enforces — the map can never fill with immortal markers.
//! This module is pure state; the canvas/panel rendering lives in `foxhole-tui`.

use super::*;
use crate::domain::{GeoPos, now_secs};
use foxhole_cot::{Affiliation, CotEvent, Kind};

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
    /// configured default).
    pub fn effective_stale(&self, ttl: u64) -> i64 {
        self.event.stale.unwrap_or_else(|| self.time() + ttl as i64)
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

/// Modal state for the "incoming intel" review list (design note §6): the staged
/// events from Unknown/Untrusted peers the operator accepts or discards.
pub struct IntelReview {
    /// Highlighted row within [`App::intel_staged`].
    pub selected: usize,
}

/// Modal state for sharing a local hazard zone as CoT to the active peer (P3):
/// pick which `zones.conf` zone to send. The recipient is captured at open time
/// (the selected conversation), so the picker only chooses the zone.
pub struct ShareZone {
    /// Highlighted row within [`App::zones`].
    pub selected: usize,
    /// Recipient peer key (hex hash / display key) the zone will be sent to.
    pub peer: String,
    /// Human-friendly recipient label for the modal header.
    pub peer_label: String,
}

/// What an authored object is — a point marker or a circular area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorKind {
    Marker,
    Zone,
}

/// The field the author form is editing (Up/Down moves between them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorField {
    Kind,
    Affiliation,
    Callsign,
    Lat,
    Lon,
    Radius,
    Remarks,
}

impl AuthorField {
    /// Field order for Up/Down / Tab cycling.
    const ORDER: [AuthorField; 7] = [
        AuthorField::Kind,
        AuthorField::Affiliation,
        AuthorField::Callsign,
        AuthorField::Lat,
        AuthorField::Lon,
        AuthorField::Radius,
        AuthorField::Remarks,
    ];

    fn step(self, delta: isize) -> Self {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0) as isize;
        let n = Self::ORDER.len() as isize;
        Self::ORDER[(i + delta).rem_euclid(n) as usize]
    }
}

/// In-app intel authoring form (P4): place/edit a marker or zone of any
/// affiliation, committed to the live intel layer as a local-authored object.
/// Opened fresh (`edit_key == None`) or prefilled from a selected object (edit).
pub struct AuthorForm {
    pub kind: AuthorKind,
    pub affiliation: Affiliation,
    pub callsign: String,
    pub lat: String,
    pub lon: String,
    pub radius_km: String,
    pub remarks: String,
    /// Which field is focused.
    pub field: AuthorField,
    /// `Some((source, uid))` when editing in place; `None` when creating.
    pub edit_key: Option<(String, String)>,
    /// Set when the last commit attempt failed validation; cleared on edit.
    pub error: Option<&'static str>,
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
                self.intel_dirty = true;
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
                if upsert(&mut self.intel, record) {
                    self.intel_dirty = true;
                    self.push_log(format!("[SYS] intel: applied {label} from {who}"));
                }
            }
            // Unknown/Untrusted: stage for review unless the operator opted into
            // auto-applying all intel.
            Trust::Unknown | Trust::Untrusted => {
                if self.config.intel_auto_apply {
                    let label = record.label();
                    if upsert(&mut self.intel, record) {
                        self.intel_dirty = true;
                        self.push_log(format!("[SYS] intel: auto-applied {label} from {who}"));
                    }
                } else {
                    let label = record.label();
                    if upsert(&mut self.intel_staged, record) {
                        self.intel_dirty = true;
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
        let before = self.intel.len() + self.intel_staged.len();
        self.intel
            .retain(|r| !(r.source == source && r.event.uid == uid));
        self.intel_staged
            .retain(|r| !(r.source == source && r.event.uid == uid));
        self.clamp_intel_review();
        before != self.intel.len() + self.intel_staged.len()
    }

    /// Drop every expired object (live and staged) at `now`, given the configured
    /// default TTL. Returns how many were removed. Cheap to call often — `main`
    /// runs it as the periodic sweep §6 calls for.
    pub fn sweep_intel(&mut self, now: i64) -> usize {
        let ttl = self.config.intel_ttl_secs;
        let before = self.intel.len() + self.intel_staged.len();
        self.intel.retain(|r| !r.is_expired(now, ttl));
        self.intel_staged.retain(|r| !r.is_expired(now, ttl));
        self.clamp_intel_review();
        let removed = before - (self.intel.len() + self.intel_staged.len());
        if removed > 0 {
            self.intel_dirty = true;
        }
        removed
    }

    /// Live (applied, non-expired) intel at `now` — what the map layer plots.
    pub fn live_intel_at(&self, now: i64) -> Vec<&IntelRecord> {
        let ttl = self.config.intel_ttl_secs;
        self.intel
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
        if !self.intel_staged.is_empty() {
            self.intel_review = Some(IntelReview { selected: 0 });
        }
    }

    /// Key handling while the incoming-intel review modal is open: Up/Down select,
    /// `a`/Enter accept (apply to the map), `x`/`d`/Delete discard, Esc close.
    pub(super) fn handle_intel_review_key(&mut self, key: KeyEvent) {
        let Some(selected) = self.intel_review.as_ref().map(|r| r.selected) else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.intel_review = None,
            KeyCode::Up => {
                if let Some(r) = self.intel_review.as_mut() {
                    r.selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if selected + 1 < self.intel_staged.len()
                    && let Some(r) = self.intel_review.as_mut()
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
        if self.intel_staged.is_empty() {
            self.intel_review = None;
        }
    }

    /// Promote a staged object to the live map layer (operator vouches for it).
    pub fn accept_staged(&mut self, idx: usize) {
        if idx >= self.intel_staged.len() {
            return;
        }
        let record = self.intel_staged.remove(idx);
        let (label, who) = (
            record.label(),
            crate::domain::short_hash(&record.source).to_string(),
        );
        upsert(&mut self.intel, record);
        self.intel_dirty = true;
        self.push_log(format!("[SYS] intel: accepted {label} from {who}"));
        self.clamp_intel_review();
    }

    /// Discard a staged object without applying it.
    pub fn discard_staged(&mut self, idx: usize) {
        if idx >= self.intel_staged.len() {
            return;
        }
        let record = self.intel_staged.remove(idx);
        let (label, who) = (
            record.label(),
            crate::domain::short_hash(&record.source).to_string(),
        );
        self.intel_dirty = true;
        self.push_log(format!("[SYS] intel: discarded {label} from {who}"));
        self.clamp_intel_review();
    }

    /// Keep the review cursor within the staged list after a removal.
    fn clamp_intel_review(&mut self) {
        if let Some(review) = self.intel_review.as_mut() {
            review.selected = review
                .selected
                .min(self.intel_staged.len().saturating_sub(1));
        }
    }

    /// Open the "share zone" picker for the active conversation (P3). No-op when
    /// there is no selected peer or no local zone to share.
    pub(super) fn open_share_zone(&mut self) {
        if self.zones.is_empty() {
            self.push_log("[SYS] intel: no local zones to share (add to zones.conf)".to_string());
            return;
        }
        let Some(conv) = self.conversations.get(self.selected) else {
            return;
        };
        self.share_zone = Some(ShareZone {
            selected: 0,
            peer: conv.peer.clone(),
            peer_label: conv.label(),
        });
    }

    /// Key handling while the share-zone picker is open: Up/Down select, Enter/`s`
    /// share the highlighted zone, `r` revoke it on the peer, Esc cancel.
    pub(super) fn handle_share_zone_key(&mut self, key: KeyEvent) {
        let Some(state) = self.share_zone.as_ref() else {
            return;
        };
        let selected = state.selected;
        match key.code {
            KeyCode::Esc => self.share_zone = None,
            KeyCode::Up => {
                if let Some(s) = self.share_zone.as_mut() {
                    s.selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if selected + 1 < self.zones.len()
                    && let Some(s) = self.share_zone.as_mut()
                {
                    s.selected = selected + 1;
                }
            }
            KeyCode::Enter | KeyCode::Char('s') => {
                if let Some(state) = self.share_zone.take() {
                    self.share_zone(state.selected, &state.peer);
                }
            }
            // Revoke the highlighted zone on the peer (withdraw a prior share).
            KeyCode::Char('r') => {
                if let Some(state) = self.share_zone.take() {
                    self.revoke_shared_zone(state.selected, &state.peer);
                }
            }
            _ => {}
        }
    }

    /// Produce a CoT `u-d-c-c` hazard-zone event from local zone `zone_idx` and
    /// enqueue it for transmission to `peer` (with a human-readable summary body
    /// for graceful degradation). The wire generation is `foxhole-cot`'s producer
    /// side — "today's `Zone` becomes a produced `u-d-c-c`" (design note §4).
    pub fn share_zone(&mut self, zone_idx: usize, peer: &str) {
        let Some(zone) = self.zones.get(zone_idx) else {
            return;
        };
        let (label, lat, lon, radius_km) = (
            zone.label.clone(),
            zone.center.lat,
            zone.center.lon,
            zone.radius_km,
        );
        let now = now_secs() as i64;
        let stale = now + self.config.intel_ttl_secs as i64;
        let uid = self.zone_uid(&label);
        let event = CotEvent::zone(uid, &label, lat, lon, radius_km * 1000.0, now, stale);
        let summary = event.summary();
        let xml = event.to_xml();

        let id = self.next_id();
        // Echo into the recipient's thread so the operator sees what was shared.
        if let Some(conv) = self.conversations.iter_mut().find(|c| c.peer == peer) {
            let mut entry = Entry::now(format!("[TX] shared intel: {label}"));
            entry.id = id;
            entry.status = MsgStatus::Sending;
            conv.messages.push(entry);
        }
        self.outbound.push_back(Outbound {
            id,
            peer: peer.to_string(),
            title: String::new(),
            body: summary,
            cot_xml: Some(xml),
        });
        self.mark_dirty(peer);
        self.push_log(format!(
            "[SYS] intel: shared {label} to {}",
            crate::domain::short_hash(peer)
        ));
    }

    /// Revoke a previously-shared zone: send a CoT revocation (`stale == time`,
    /// same `uid`) to `peer` so its `apply_cot` revoke path drops the object from
    /// the map. The local `zones.conf` entry is untouched — this only withdraws
    /// the copy the peer holds (design note §6; no auto-relay, an explicit action).
    pub fn revoke_shared_zone(&mut self, zone_idx: usize, peer: &str) {
        let Some(zone) = self.zones.get(zone_idx) else {
            return;
        };
        let (label, lat, lon, radius_km) = (
            zone.label.clone(),
            zone.center.lat,
            zone.center.lon,
            zone.radius_km,
        );
        let now = now_secs() as i64;
        let uid = self.zone_uid(&label);
        // `stale == time` is CoT's "this object is no longer valid" idiom, which
        // the receiver decodes via `CotEvent::is_revocation`.
        let event = CotEvent::zone(uid, &label, lat, lon, radius_km * 1000.0, now, now);
        let xml = event.to_xml();

        let id = self.next_id();
        if let Some(conv) = self.conversations.iter_mut().find(|c| c.peer == peer) {
            let mut entry = Entry::now(format!("[TX] revoked intel: {label}"));
            entry.id = id;
            entry.status = MsgStatus::Sending;
            conv.messages.push(entry);
        }
        self.outbound.push_back(Outbound {
            id,
            peer: peer.to_string(),
            title: String::new(),
            body: format!("REVOKE: {label} \u{2014} no longer valid"),
            cot_xml: Some(xml),
        });
        self.mark_dirty(peer);
        self.push_log(format!(
            "[SYS] intel: revoked {label} to {}",
            crate::domain::short_hash(peer)
        ));
    }

    /// The deterministic CoT `uid` for one of our shared zones: our short identity
    /// (or `foxhole` offline) + the zone label. Stable across sessions, so a later
    /// share *updates* and a revoke *matches* the object on the receiver.
    fn zone_uid(&self, label: &str) -> String {
        let origin = self
            .local_address
            .as_deref()
            .map(crate::domain::short_hash)
            .unwrap_or("foxhole");
        format!("{origin}-{}", label.replace(' ', "-"))
    }

    /// The `(source, uid)` of the currently selected map marker, if it is an
    /// intel object (operator/peer markers return `None`).
    pub(super) fn selected_intel_key(&self) -> Option<(String, String)> {
        self.map_markers()
            .into_iter()
            .nth(self.map_selected)
            .and_then(|m| m.intel_key)
    }

    /// Open the intel authoring form. `edit` prefills from the selected intel
    /// object (in-place edit); otherwise a fresh marker is placed at the map
    /// centre for the operator to adjust.
    pub(super) fn open_author(&mut self, edit: bool) {
        let form = if edit {
            let Some(key) = self.selected_intel_key() else {
                self.push_log("[SYS] intel: select an intel marker to edit".to_string());
                return;
            };
            let Some(r) = self
                .intel
                .iter()
                .find(|r| r.key() == (key.0.as_str(), key.1.as_str()))
            else {
                return;
            };
            let kind = if r.kind() == Kind::Zone {
                AuthorKind::Zone
            } else {
                AuthorKind::Marker
            };
            AuthorForm {
                kind,
                affiliation: r.affiliation(),
                callsign: r.event.callsign.clone().unwrap_or_default(),
                lat: fmt_coord(r.event.point.lat),
                lon: fmt_coord(r.event.point.lon),
                radius_km: r.radius_km().map(fmt_coord).unwrap_or_else(|| "10".into()),
                remarks: r.event.remarks.clone().unwrap_or_default(),
                field: AuthorField::Kind,
                edit_key: Some((r.source.clone(), r.event.uid.clone())),
                error: None,
            }
        } else {
            AuthorForm {
                kind: AuthorKind::Marker,
                affiliation: Affiliation::Unknown,
                callsign: String::new(),
                lat: fmt_coord(self.map.center.lat),
                lon: fmt_coord(self.map.center.lon),
                radius_km: "10".to_string(),
                remarks: String::new(),
                field: AuthorField::Kind,
                edit_key: None,
                error: None,
            }
        };
        self.author = Some(form);
    }

    /// Key handling while the authoring form is open: Up/Down move fields,
    /// Left/Right cycle the Kind/Affiliation toggles, typing edits text fields,
    /// Enter commits, Esc cancels.
    pub(super) fn handle_author_key(&mut self, key: KeyEvent) {
        let Some(form) = self.author.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.author = None,
            KeyCode::Enter => self.commit_author(),
            KeyCode::Up | KeyCode::BackTab => form.field = form.field.step(-1),
            KeyCode::Down | KeyCode::Tab => form.field = form.field.step(1),
            KeyCode::Left => cycle_field(form, -1),
            KeyCode::Right => cycle_field(form, 1),
            KeyCode::Backspace => {
                form.error = None;
                if let Some(buf) = text_field(form) {
                    buf.pop();
                }
            }
            KeyCode::Char(c) => {
                form.error = None;
                // On the toggle fields, Space cycles instead of typing.
                if c == ' ' && matches!(form.field, AuthorField::Kind | AuthorField::Affiliation) {
                    cycle_field(form, 1);
                } else if let Some(buf) = text_field(form) {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    /// Validate the form and commit it to the live intel layer as a local-authored
    /// object (or update the edited one in place).
    fn commit_author(&mut self) {
        let Some(form) = self.author.as_ref() else {
            return;
        };
        let lat = match form.lat.trim().parse::<f64>() {
            Ok(v) if v.is_finite() => v,
            _ => return self.author_error("latitude must be a number"),
        };
        let lon = match form.lon.trim().parse::<f64>() {
            Ok(v) if v.is_finite() => v,
            _ => return self.author_error("longitude must be a number"),
        };
        let radius_km = if form.kind == AuthorKind::Zone {
            match form.radius_km.trim().parse::<f64>() {
                Ok(v) if v.is_finite() && v > 0.0 => v,
                _ => return self.author_error("zone needs a radius (km) > 0"),
            }
        } else {
            0.0
        };

        // Snapshot the form, then drop the borrow before mutating `self`.
        let (kind, affiliation, callsign, remarks, edit_key) = (
            form.kind,
            form.affiliation,
            form.callsign.trim().to_string(),
            form.remarks.trim().to_string(),
            form.edit_key.clone(),
        );

        let now = now_secs() as i64;
        let stale = now + self.config.intel_ttl_secs as i64;
        // Editing keeps the object's identity; a new object is attributed to us.
        let (source, uid) = match &edit_key {
            Some((src, uid)) => (src.clone(), uid.clone()),
            None => {
                let origin = self
                    .local_address
                    .as_deref()
                    .map(crate::domain::short_hash)
                    .unwrap_or("foxhole");
                (
                    self.local_address.clone().unwrap_or_else(|| "local".into()),
                    format!("{origin}-a{now}"),
                )
            }
        };

        let mut event = match kind {
            AuthorKind::Marker => {
                CotEvent::marker(uid, affiliation, callsign.clone(), lat, lon, now, stale)
            }
            AuthorKind::Zone => {
                let mut e = CotEvent::zone(
                    uid,
                    callsign.clone(),
                    lat,
                    lon,
                    radius_km * 1000.0,
                    now,
                    stale,
                );
                // Tint the zone by affiliation when one is chosen; otherwise leave
                // it as a plain `u-d-c-c` hazard area.
                if affiliation != Affiliation::Unknown {
                    e.cot_type = format!("a-{}-G", affiliation.token());
                }
                e
            }
        };
        if !remarks.is_empty() {
            event.remarks = Some(remarks);
        }
        let label = event.callsign.clone().unwrap_or_else(|| event.uid.clone());

        upsert(
            &mut self.intel,
            IntelRecord {
                source,
                event,
                received_at: now as u64,
            },
        );
        self.intel_dirty = true;
        self.author = None;
        let verb = if edit_key.is_some() {
            "edited"
        } else {
            "authored"
        };
        self.push_log(format!("[SYS] intel: {verb} {label}"));
    }

    /// Flag a validation error on the open form (keeps it open).
    fn author_error(&mut self, msg: &'static str) {
        if let Some(form) = self.author.as_mut() {
            form.error = Some(msg);
        }
    }

    /// Remove the selected intel object from the local map (no network — receiving
    /// foxhole does not rebroadcast, so this just withdraws our own copy). Works
    /// on authored *and* received objects.
    pub(super) fn remove_selected_intel(&mut self) {
        let Some((source, uid)) = self.selected_intel_key() else {
            return;
        };
        let before = self.intel.len();
        self.intel
            .retain(|r| !(r.source == source && r.event.uid == uid));
        if self.intel.len() != before {
            self.intel_dirty = true;
            self.map_selected = 0;
            self.push_log(format!("[SYS] intel: removed {uid} (local)"));
        }
    }
}

/// Cycle the focused toggle field (Kind / Affiliation) by `delta`; a no-op on a
/// text field.
fn cycle_field(form: &mut AuthorForm, delta: isize) {
    match form.field {
        AuthorField::Kind => {
            form.kind = match form.kind {
                AuthorKind::Marker => AuthorKind::Zone,
                AuthorKind::Zone => AuthorKind::Marker,
            };
        }
        AuthorField::Affiliation => form.affiliation = cycle_affiliation(form.affiliation, delta),
        _ => {}
    }
}

/// The editable text buffer for the focused field, or `None` on a toggle field.
fn text_field(form: &mut AuthorForm) -> Option<&mut String> {
    match form.field {
        AuthorField::Callsign => Some(&mut form.callsign),
        AuthorField::Lat => Some(&mut form.lat),
        AuthorField::Lon => Some(&mut form.lon),
        AuthorField::Radius => Some(&mut form.radius_km),
        AuthorField::Remarks => Some(&mut form.remarks),
        AuthorField::Kind | AuthorField::Affiliation => None,
    }
}

/// Cycle through the four affiliations in a stable order.
fn cycle_affiliation(a: Affiliation, delta: isize) -> Affiliation {
    const ORDER: [Affiliation; 4] = [
        Affiliation::Friendly,
        Affiliation::Hostile,
        Affiliation::Neutral,
        Affiliation::Unknown,
    ];
    let i = ORDER.iter().position(|x| *x == a).unwrap_or(3) as isize;
    ORDER[(i + delta).rem_euclid(4) as usize]
}

/// Format a coordinate/number for prefilling a form field (trim trailing zeros
/// to a tidy default like `50.4`, not `50.400000`).
fn fmt_coord(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// Upsert a record into a layer keyed by `(source, uid)` with newest-`time`-wins
/// semantics. Returns whether the layer changed (a strictly older duplicate is
/// ignored, so a replayed event doesn't churn the map or the log).
fn upsert(layer: &mut Vec<IntelRecord>, record: IntelRecord) -> bool {
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

    /// A test event at a fixed instant; `stale` is `time + 3600` (one hour out).
    fn event(uid: &str, cot_type: &str, time: i64) -> CotEvent {
        let mut e = CotEvent::marker(
            uid,
            Affiliation::Hostile,
            "AO",
            50.4,
            30.5,
            time,
            time + 3600,
        );
        e.cot_type = cot_type.to_string();
        e
    }

    /// An app with a single peer at the given trust, no demo intel.
    fn app_with_peer(hash: &str, trust: Trust) -> App {
        let mut app = App::new();
        app.conversations.clear();
        app.intel.clear();
        app.intel_staged.clear();
        let mut c = Conversation::new(hash);
        c.trust = trust;
        app.conversations.push(c);
        app
    }

    #[test]
    fn trusted_source_is_applied_unknown_is_staged() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        assert_eq!(app.intel.len(), 1);
        assert!(app.intel_staged.is_empty());

        // A second, unknown peer's intel is staged for review, not applied.
        app.conversations.push(Conversation::new("bb")); // defaults to Unknown
        app.apply_cot("bb".into(), event("u2", "a-h-G", 1000));
        assert_eq!(app.intel.len(), 1);
        assert_eq!(app.intel_staged.len(), 1);
    }

    #[test]
    fn compromised_source_is_dropped() {
        let mut app = app_with_peer("aa", Trust::Compromised);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        assert!(app.intel.is_empty());
        assert!(app.intel_staged.is_empty());
    }

    #[test]
    fn auto_apply_bypasses_staging_for_unknown() {
        let mut app = app_with_peer("aa", Trust::Unknown);
        app.config.intel_auto_apply = true;
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        assert_eq!(app.intel.len(), 1);
        assert!(app.intel_staged.is_empty());
    }

    #[test]
    fn newest_time_wins_and_replays_are_ignored() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        // A newer event for the same (source, uid) replaces in place.
        let mut newer = event("u1", "a-h-G", 2000);
        newer.callsign = Some("MOVED".into());
        app.apply_cot("aa".into(), newer);
        assert_eq!(app.intel.len(), 1);
        assert_eq!(app.intel[0].label(), "MOVED");
        // An older replay is ignored (no churn).
        app.apply_cot("aa".into(), event("u1", "a-h-G", 500));
        assert_eq!(app.intel.len(), 1);
        assert_eq!(app.intel[0].label(), "MOVED");
        // The same uid from a *different* source is kept separately (attributed).
        let mut c = Conversation::new("bb");
        c.trust = Trust::Trusted;
        app.conversations.push(c);
        app.apply_cot("bb".into(), event("u1", "a-h-G", 1000));
        assert_eq!(app.intel.len(), 2);
    }

    #[test]
    fn revocation_removes_the_object() {
        let mut app = app_with_peer("aa", Trust::Trusted);
        app.apply_cot("aa".into(), event("u1", "a-h-G", 1000));
        assert_eq!(app.intel.len(), 1);
        // stale <= time is a revoke for the same uid.
        let mut revoke = event("u1", "a-h-G", 3000);
        revoke.stale = Some(3000);
        app.apply_cot("aa".into(), revoke);
        assert!(app.intel.is_empty());
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
        assert!(app.intel.is_empty());
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
        assert_eq!(app.intel_staged.len(), 2);

        app.accept_staged(0);
        assert_eq!(app.intel.len(), 1);
        assert_eq!(app.intel_staged.len(), 1);

        app.discard_staged(0);
        assert!(app.intel_staged.is_empty());
        assert_eq!(app.intel.len(), 1);
    }

    #[test]
    fn share_zone_enqueues_a_cot_event_and_echoes() {
        let mut app = App::new();
        app.conversations.clear();
        app.conversations.push(Conversation::new("aa11"));
        app.selected = 0;
        app.zones = vec![crate::domain::Zone::new("AO ALPHA", 50.4, 30.5, 400.0)];

        app.share_zone(0, "aa11");

        // One outbound carrying the CoT XML + a summary body, echoed in the thread.
        assert_eq!(app.outbound.len(), 1);
        let out = &app.outbound[0];
        assert_eq!(out.peer, "aa11");
        assert!(out.body.contains("AO ALPHA"), "summary body");
        let xml = out.cot_xml.as_ref().expect("cot xml attached");

        // The produced event is a u-d-c-c hazard zone the codec round-trips.
        let event = foxhole_cot::parse(xml).unwrap();
        assert_eq!(event.cot_type, "u-d-c-c");
        assert_eq!(event.kind(), Kind::Zone);
        assert_eq!(event.radius_m, Some(400_000.0));
        assert_eq!(event.point.lat, 50.4);
        assert!(
            app.conversations[0]
                .messages
                .last()
                .unwrap()
                .text
                .contains("shared intel"),
            "thread echo"
        );
    }

    #[test]
    fn revoke_shared_zone_sends_a_revocation_that_drops_on_the_receiver() {
        // Sender: build a revocation for a local zone addressed to peer "aa11".
        let mut sender = App::new();
        sender.conversations.clear();
        sender.conversations.push(Conversation::new("aa11"));
        sender.selected = 0;
        sender.zones = vec![crate::domain::Zone::new("AO ALPHA", 50.4, 30.5, 400.0)];
        sender.revoke_shared_zone(0, "aa11");

        let out = sender.outbound.front().expect("revocation enqueued");
        assert!(out.body.contains("REVOKE"), "human body marks a revoke");
        let xml = out.cot_xml.as_ref().expect("cot xml attached");
        let event = foxhole_cot::parse(xml).unwrap();
        assert!(event.is_revocation(), "stale<=time is a revocation");
        let revoke_uid = event.uid.clone();
        assert!(
            sender.conversations[0]
                .messages
                .last()
                .unwrap()
                .text
                .contains("revoked intel"),
            "thread echo"
        );

        // Receiver: first holds the shared object (same source+uid), then the
        // revocation removes it via apply_cot's revoke path.
        let mut rx = App::new();
        rx.conversations.clear();
        rx.intel.clear();
        let mut trusted = Conversation::new("sender-hash");
        trusted.trust = Trust::Trusted;
        rx.conversations.push(trusted);
        let mut shared = CotEvent::zone(
            &revoke_uid,
            "AO ALPHA",
            50.4,
            30.5,
            400_000.0,
            1000,
            1000 + 3600,
        );
        shared.cot_type = "u-d-c-c".into();
        rx.apply_cot("sender-hash".into(), shared);
        assert_eq!(rx.intel.len(), 1, "object applied");

        let revoke = foxhole_cot::parse(xml).unwrap();
        rx.apply_cot("sender-hash".into(), revoke);
        assert!(rx.intel.is_empty(), "revocation dropped the object");
    }

    #[test]
    fn share_picker_opens_only_with_a_peer_and_zone() {
        let mut app = App::new();
        app.conversations.clear();
        app.zones.clear();
        // No zones → no picker (logs a hint instead).
        app.open_share_zone();
        assert!(app.share_zone.is_none());

        app.zones = vec![crate::domain::Zone::new("AO", 0.0, 0.0, 10.0)];
        // No conversation selected → still no picker.
        app.open_share_zone();
        assert!(app.share_zone.is_none());

        app.conversations.push(Conversation::new("bb22"));
        app.selected = 0;
        app.open_share_zone();
        assert!(app.share_zone.is_some());
        assert_eq!(app.share_zone.as_ref().unwrap().peer, "bb22");
    }

    #[test]
    fn authoring_a_marker_adds_a_live_intel_record() {
        let mut app = App::new();
        app.intel.clear();
        app.open_author(false);
        let form = app.author.as_mut().unwrap();
        form.affiliation = Affiliation::Hostile;
        form.callsign = "SNIPER".to_string();
        form.lat = "50.5".to_string();
        form.lon = "30.1".to_string();
        app.commit_author();

        assert!(app.author.is_none(), "form closes on commit");
        assert_eq!(app.intel.len(), 1);
        assert!(app.intel_dirty);
        let e = &app.intel[0].event;
        assert_eq!(e.cot_type, "a-h-G");
        assert_eq!(e.affiliation(), Affiliation::Hostile);
        assert_eq!(e.callsign.as_deref(), Some("SNIPER"));
        assert_eq!(app.intel[0].kind(), Kind::Marker);
    }

    #[test]
    fn authoring_a_zone_tints_by_affiliation() {
        let mut app = App::new();
        app.intel.clear();
        app.open_author(false);
        let form = app.author.as_mut().unwrap();
        form.kind = AuthorKind::Zone;
        form.affiliation = Affiliation::Hostile;
        form.lat = "0".into();
        form.lon = "0".into();
        form.radius_km = "250".into();
        app.commit_author();

        let e = &app.intel[0].event;
        assert_eq!(e.kind(), Kind::Zone);
        assert_eq!(e.radius_m, Some(250_000.0));
        assert_eq!(e.affiliation(), Affiliation::Hostile);
    }

    #[test]
    fn invalid_coordinates_keep_the_form_open_with_an_error() {
        let mut app = App::new();
        app.intel.clear();
        app.open_author(false);
        app.author.as_mut().unwrap().lat = "north".to_string();
        app.commit_author();
        assert!(app.author.is_some(), "form stays open on bad input");
        assert!(app.author.as_ref().unwrap().error.is_some());
        assert!(app.intel.is_empty());
    }

    #[test]
    fn editing_updates_in_place_via_selection() {
        let mut app = App::new();
        app.conversations.clear();
        app.intel.clear();
        app.config.lat = None; // no operator marker, so index 0 is the intel one
        app.config.lon = None;
        // Author one marker, then edit it in place.
        app.open_author(false);
        {
            let f = app.author.as_mut().unwrap();
            f.callsign = "OP".into();
            f.lat = "10".into();
            f.lon = "20".into();
        }
        app.commit_author();
        let key = app.intel[0].key();
        let key = (key.0.to_string(), key.1.to_string());

        app.map_selected = 0; // the sole marker
        app.open_author(true);
        assert_eq!(app.author.as_ref().unwrap().edit_key, Some(key.clone()));
        app.author.as_mut().unwrap().callsign = "OP-MOVED".into();
        app.commit_author();

        assert_eq!(app.intel.len(), 1, "edit updates in place, no duplicate");
        assert_eq!(app.intel[0].event.uid, key.1, "same uid kept");
        assert_eq!(app.intel[0].event.callsign.as_deref(), Some("OP-MOVED"));
    }

    #[test]
    fn remove_selected_intel_drops_it_locally() {
        let mut app = App::new();
        app.conversations.clear();
        app.intel.clear();
        app.config.lat = None;
        app.config.lon = None;
        app.open_author(false);
        app.author.as_mut().unwrap().lat = "1".into();
        app.author.as_mut().unwrap().lon = "2".into();
        app.commit_author();
        assert_eq!(app.intel.len(), 1);

        app.map_selected = 0;
        app.intel_dirty = false;
        app.remove_selected_intel();
        assert!(app.intel.is_empty());
        assert!(app.intel_dirty, "removal flags persistence");
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
