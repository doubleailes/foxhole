//! In-app intel authoring: place, edit, or remove a marker or zone directly on
//! the World Map (P4 of the intel-sharing plan).
//!
//! Authored objects go straight into the live intel layer as locally attributed
//! records, so the operator can annotate the map without a network round-trip —
//! and can clear a received report the same way. The form carries the position
//! twice, as lat/lon and as an MGRS grid reference, kept in step so either can
//! be typed; the map bindings are `a` (place), `e` (edit), `x` (remove).

use super::intel::upsert;
use super::*;
use crate::domain::{GeoPos, now_secs};
use foxhole_cot::{Affiliation, CotEvent, Kind};

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
    /// The position as an MGRS grid reference — two-way synced with Lat/Lon so the
    /// operator can designate a point either way.
    Mgrs,
    Radius,
    Remarks,
}

impl AuthorField {
    /// Field order for Up/Down / Tab cycling.
    const ORDER: [AuthorField; 8] = [
        AuthorField::Kind,
        AuthorField::Affiliation,
        AuthorField::Callsign,
        AuthorField::Lat,
        AuthorField::Lon,
        AuthorField::Mgrs,
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
    /// MGRS mirror of `lat`/`lon`; kept in sync as either side is edited.
    pub mgrs: String,
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
    /// The `(source, uid)` of the currently selected map marker, if it is an
    /// intel object (operator/peer markers return `None`).
    pub(super) fn selected_intel_key(&self) -> Option<(String, String)> {
        self.map_markers()
            .into_iter()
            .nth(self.map.selected)
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
                .live
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
                mgrs: mgrs_for(r.event.point.lat, r.event.point.lon),
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
                lat: fmt_coord(self.map.view.center.lat),
                lon: fmt_coord(self.map.view.center.lon),
                mgrs: mgrs_for(self.map.view.center.lat, self.map.view.center.lon),
                radius_km: "10".to_string(),
                remarks: String::new(),
                field: AuthorField::Kind,
                edit_key: None,
                error: None,
            }
        };
        self.intel.author = Some(form);
    }

    /// Key handling while the authoring form is open: Up/Down move fields,
    /// Left/Right cycle the Kind/Affiliation toggles, typing edits text fields,
    /// Enter commits, Esc cancels.
    pub(super) fn handle_author_key(&mut self, key: KeyEvent) {
        let Some(form) = self.intel.author.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.intel.author = None,
            KeyCode::Enter => self.commit_author(),
            KeyCode::Up | KeyCode::BackTab => form.field = form.field.step(-1),
            KeyCode::Down | KeyCode::Tab => form.field = form.field.step(1),
            KeyCode::Left => cycle_field(form, -1),
            KeyCode::Right => cycle_field(form, 1),
            KeyCode::Backspace => {
                form.error = None;
                let changed = form.field;
                if let Some(buf) = text_field(form) {
                    buf.pop();
                }
                sync_author_coords(form, changed);
            }
            KeyCode::Char(c) => {
                form.error = None;
                // On the toggle fields, Space cycles instead of typing.
                if c == ' ' && matches!(form.field, AuthorField::Kind | AuthorField::Affiliation) {
                    cycle_field(form, 1);
                } else {
                    let changed = form.field;
                    if let Some(buf) = text_field(form) {
                        buf.push(c);
                    }
                    sync_author_coords(form, changed);
                }
            }
            _ => {}
        }
    }

    /// Validate the form and commit it to the live intel layer as a local-authored
    /// object (or update the edited one in place).
    fn commit_author(&mut self) {
        let Some(form) = self.intel.author.as_ref() else {
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
            &mut self.intel.live,
            IntelRecord {
                source,
                event,
                received_at: now as u64,
            },
        );
        self.intel.dirty = true;
        self.intel.author = None;
        let verb = if edit_key.is_some() {
            "edited"
        } else {
            "authored"
        };
        self.push_log(format!("[SYS] intel: {verb} {label}"));
    }

    /// Flag a validation error on the open form (keeps it open).
    fn author_error(&mut self, msg: &'static str) {
        if let Some(form) = self.intel.author.as_mut() {
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
        let before = self.intel.live.len();
        self.intel
            .live
            .retain(|r| !(r.source == source && r.event.uid == uid));
        if self.intel.live.len() != before {
            self.intel.dirty = true;
            self.map.selected = 0;
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
        AuthorField::Mgrs => Some(&mut form.mgrs),
        AuthorField::Radius => Some(&mut form.radius_km),
        AuthorField::Remarks => Some(&mut form.remarks),
        AuthorField::Kind | AuthorField::Affiliation => None,
    }
}

/// Keep the Lat/Lon and MGRS representations of the position in step after one
/// side is edited: typing MGRS rewrites Lat/Lon (when it parses), and typing
/// either coordinate rewrites the MGRS mirror. Unparseable partial input is left
/// alone so the operator can keep typing.
fn sync_author_coords(form: &mut AuthorForm, changed: AuthorField) {
    match changed {
        AuthorField::Mgrs => {
            if let Some(p) = foxhole_map::mgrs::parse(&form.mgrs) {
                form.lat = fmt_coord(p.lat);
                form.lon = fmt_coord(p.lon);
            }
        }
        AuthorField::Lat | AuthorField::Lon => {
            if let (Ok(lat), Ok(lon)) = (
                form.lat.trim().parse::<f64>(),
                form.lon.trim().parse::<f64>(),
            ) && let Some(s) = mgrs_for_opt(lat, lon)
            {
                form.mgrs = s;
            }
        }
        _ => {}
    }
}

/// MGRS for a lat/lon at the default precision, or empty for an out-of-band
/// (polar) position — for prefilling the author form's mirror.
fn mgrs_for(lat: f64, lon: f64) -> String {
    mgrs_for_opt(lat, lon).unwrap_or_default()
}

/// MGRS for a lat/lon at the default precision, `None` outside UTM's band.
fn mgrs_for_opt(lat: f64, lon: f64) -> Option<String> {
    foxhole_map::mgrs::format(GeoPos::new(lat, lon), foxhole_map::mgrs::DEFAULT_DIGITS)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authoring_a_marker_adds_a_live_intel_record() {
        let mut app = App::new();
        app.intel.live.clear();
        app.open_author(false);
        let form = app.intel.author.as_mut().unwrap();
        form.affiliation = Affiliation::Hostile;
        form.callsign = "SNIPER".to_string();
        form.lat = "50.5".to_string();
        form.lon = "30.1".to_string();
        app.commit_author();

        assert!(app.intel.author.is_none(), "form closes on commit");
        assert_eq!(app.intel.live.len(), 1);
        assert!(app.intel.dirty);
        let e = &app.intel.live[0].event;
        assert_eq!(e.cot_type, "a-h-G");
        assert_eq!(e.affiliation(), Affiliation::Hostile);
        assert_eq!(e.callsign.as_deref(), Some("SNIPER"));
        assert_eq!(app.intel.live[0].kind(), Kind::Marker);
    }

    #[test]
    fn authoring_a_zone_tints_by_affiliation() {
        let mut app = App::new();
        app.intel.live.clear();
        app.open_author(false);
        let form = app.intel.author.as_mut().unwrap();
        form.kind = AuthorKind::Zone;
        form.affiliation = Affiliation::Hostile;
        form.lat = "0".into();
        form.lon = "0".into();
        form.radius_km = "250".into();
        app.commit_author();

        let e = &app.intel.live[0].event;
        assert_eq!(e.kind(), Kind::Zone);
        assert_eq!(e.radius_m, Some(250_000.0));
        assert_eq!(e.affiliation(), Affiliation::Hostile);
    }

    #[test]
    fn author_form_syncs_mgrs_and_latlon_both_ways() {
        let mut app = App::new();
        app.open_author(false);
        // Editing the MGRS field rewrites Lat/Lon. Drive it through the key path
        // by replacing the buffer then nudging it so the sync fires.
        {
            let f = app.intel.author.as_mut().unwrap();
            f.field = AuthorField::Mgrs;
            f.mgrs = "31NAA660210000".to_string();
        }
        // Type the final digit so handle_author_key runs the sync.
        app.handle_author_key(KeyEvent::from(KeyCode::Char('0')));
        let f = app.intel.author.as_ref().unwrap();
        assert_eq!(f.mgrs, "31NAA6602100000");
        assert!(
            f.lat.parse::<f64>().unwrap().abs() < 0.01,
            "lat synced: {}",
            f.lat
        );
        assert!(
            f.lon.parse::<f64>().unwrap().abs() < 0.01,
            "lon synced: {}",
            f.lon
        );

        // Editing a coordinate rewrites the MGRS mirror.
        {
            let f = app.intel.author.as_mut().unwrap();
            f.field = AuthorField::Lat;
            f.lat = "48.8583".to_string();
            f.lon = "2.2944".to_string();
        }
        app.handle_author_key(KeyEvent::from(KeyCode::Backspace)); // edits lat
        let f = app.intel.author.as_ref().unwrap();
        assert!(f.mgrs.starts_with("31U"), "mgrs mirror updated: {}", f.mgrs);
    }

    #[test]
    fn invalid_coordinates_keep_the_form_open_with_an_error() {
        let mut app = App::new();
        app.intel.live.clear();
        app.open_author(false);
        app.intel.author.as_mut().unwrap().lat = "north".to_string();
        app.commit_author();
        assert!(app.intel.author.is_some(), "form stays open on bad input");
        assert!(app.intel.author.as_ref().unwrap().error.is_some());
        assert!(app.intel.live.is_empty());
    }

    #[test]
    fn editing_updates_in_place_via_selection() {
        let mut app = App::new();
        app.conversations.clear();
        app.intel.live.clear();
        app.config.lat = None; // no operator marker, so index 0 is the intel one
        app.config.lon = None;
        // Author one marker, then edit it in place.
        app.open_author(false);
        {
            let f = app.intel.author.as_mut().unwrap();
            f.callsign = "OP".into();
            f.lat = "10".into();
            f.lon = "20".into();
        }
        app.commit_author();
        let key = app.intel.live[0].key();
        let key = (key.0.to_string(), key.1.to_string());

        app.map.selected = 0; // the sole marker
        app.open_author(true);
        assert_eq!(
            app.intel.author.as_ref().unwrap().edit_key,
            Some(key.clone())
        );
        app.intel.author.as_mut().unwrap().callsign = "OP-MOVED".into();
        app.commit_author();

        assert_eq!(
            app.intel.live.len(),
            1,
            "edit updates in place, no duplicate"
        );
        assert_eq!(app.intel.live[0].event.uid, key.1, "same uid kept");
        assert_eq!(
            app.intel.live[0].event.callsign.as_deref(),
            Some("OP-MOVED")
        );
    }

    #[test]
    fn remove_selected_intel_drops_it_locally() {
        let mut app = App::new();
        app.conversations.clear();
        app.intel.live.clear();
        app.config.lat = None;
        app.config.lon = None;
        app.open_author(false);
        app.intel.author.as_mut().unwrap().lat = "1".into();
        app.intel.author.as_mut().unwrap().lon = "2".into();
        app.commit_author();
        assert_eq!(app.intel.live.len(), 1);

        app.map.selected = 0;
        app.intel.dirty = false;
        app.remove_selected_intel();
        assert!(app.intel.live.is_empty());
        assert!(app.intel.dirty, "removal flags persistence");
    }
}
