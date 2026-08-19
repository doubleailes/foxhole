//! Sharing local intel outward: the zone picker and the CoT events it puts on
//! the wire (P3 of the intel-sharing plan).
//!
//! Sharing is always an explicit operator action — FoxHole never relays what it
//! receives. A local `zones.conf` hazard area becomes a produced `u-d-c-c` CoT
//! event addressed to one peer, echoed into that peer's thread so the operator
//! can see what left; a revoke re-sends the same deterministic `uid` with
//! `stale == time`, which the receiver's `apply_cot` decodes as "drop this".

use super::*;
use crate::domain::now_secs;
use foxhole_cot::CotEvent;

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

impl App {
    /// Open the "share zone" picker for the active conversation (P3). No-op when
    /// there is no selected peer or no local zone to share.
    pub(super) fn open_share_zone(&mut self) {
        if self.map.zones.is_empty() {
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
                if selected + 1 < self.map.zones.len()
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
        let Some(zone) = self.map.zones.get(zone_idx) else {
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
        let Some(zone) = self.map.zones.get(zone_idx) else {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use foxhole_cot::Kind;

    #[test]
    fn share_zone_enqueues_a_cot_event_and_echoes() {
        let mut app = App::new();
        app.conversations.clear();
        app.conversations.push(Conversation::new("aa11"));
        app.selected = 0;
        app.map.zones = vec![crate::domain::Zone::new("AO ALPHA", 50.4, 30.5, 400.0)];

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
        sender.map.zones = vec![crate::domain::Zone::new("AO ALPHA", 50.4, 30.5, 400.0)];
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
        app.map.zones.clear();
        // No zones → no picker (logs a hint instead).
        app.open_share_zone();
        assert!(app.share_zone.is_none());

        app.map.zones = vec![crate::domain::Zone::new("AO", 0.0, 0.0, 10.0)];
        // No conversation selected → still no picker.
        app.open_share_zone();
        assert!(app.share_zone.is_none());

        app.conversations.push(Conversation::new("bb22"));
        app.selected = 0;
        app.open_share_zone();
        assert!(app.share_zone.is_some());
        assert_eq!(app.share_zone.as_ref().unwrap().peer, "bb22");
    }
}
