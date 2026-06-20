//! World Map tool: the `App`-level binding for the map feature.
//!
//! The geometry, viewport, marker/zone/city types and the embedded gazetteer all
//! live in the standalone [`foxhole-map`](foxhole_map) crate (pure logic + data).
//! What stays here is the *integration*: deriving the [`MapMarker`] list from the
//! operator's own fix (`config`) and peer telemetry (each
//! [`Conversation`](super::Conversation)), and routing keypresses to the
//! [`MapView`] methods.

use super::*;
use crate::domain::now_secs;

impl App {
    /// The markers to plot, in selection order: the operator's own fix first (if
    /// configured), then every peer carrying a telemetry location, in roster
    /// order. `map_selected` indexes into this list.
    pub fn map_markers(&self) -> Vec<MapMarker> {
        let mut out = Vec::new();
        if let Some(pos) = self.config.operator_pos() {
            out.push(MapMarker {
                label: self.config.display_name.clone(),
                pos,
                kind: MarkerKind::Operator,
                intel_key: None,
            });
        }
        for c in &self.conversations {
            if let Some(pos) = c.location {
                out.push(MapMarker {
                    label: c.label(),
                    pos,
                    kind: MarkerKind::Peer,
                    intel_key: None,
                });
            }
        }
        // Live (non-expired) received/authored intel — markers and zones alike are
        // selectable (a zone's centre is its handle; its ring is drawn separately
        // by [`App::intel_zones`]). Selection cycling tours these with the peers.
        for r in self.live_intel_at(now_secs() as i64) {
            out.push(MapMarker {
                label: r.label(),
                pos: r.pos(),
                kind: MarkerKind::Intel(r.affiliation()),
                intel_key: Some((r.source.clone(), r.event.uid.clone())),
            });
        }
        out
    }

    /// World Map keys: arrows pan, `+`/`-` zoom, `Tab`/`[`/`]` cycle markers,
    /// `Enter`/`c` centre on the selected marker, `g` toggles the capitals/cities
    /// reference layer, `r` resets the view.
    pub(super) fn handle_map_key(&mut self, _ctrl: bool, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.map.pan_west(),
            KeyCode::Right => self.map.pan_east(),
            KeyCode::Up => self.map.pan_north(),
            KeyCode::Down => self.map.pan_south(),
            KeyCode::Char('+') | KeyCode::Char('=') => self.map.zoom_in(),
            KeyCode::Char('-') | KeyCode::Char('_') => self.map.zoom_out(),
            KeyCode::Tab | KeyCode::Char(']') => self.select_map_marker(1),
            KeyCode::BackTab | KeyCode::Char('[') => self.select_map_marker(-1),
            KeyCode::Enter | KeyCode::Char('c') => self.center_selected_marker(),
            KeyCode::Char('g') => self.map_cities = !self.map_cities,
            KeyCode::Char('r') => {
                self.map = MapView::default();
                self.map_selected = 0;
            }
            // Open the incoming-intel review list (staged CoT from unvetted peers).
            KeyCode::Char('i') => self.open_intel_review(),
            // Author a new intel object (marker/zone) at the map centre.
            KeyCode::Char('a') => self.open_author(false),
            // Edit the selected intel object in place.
            KeyCode::Char('e') => self.open_author(true),
            // Remove the selected intel object from the local map.
            KeyCode::Char('x') | KeyCode::Delete => self.remove_selected_intel(),
            _ => {}
        }
    }

    /// Move the marker selection by `delta` (wrapping), a no-op when there are no
    /// markers. Centres the view on the newly selected marker so cycling tours
    /// the located peers.
    fn select_map_marker(&mut self, delta: isize) {
        let n = self.map_markers().len();
        if n == 0 {
            self.map_selected = 0;
            return;
        }
        let cur = self.map_selected.min(n - 1) as isize;
        self.map_selected = (cur + delta).rem_euclid(n as isize) as usize;
        self.center_selected_marker();
    }

    /// Frame the viewport on the currently selected marker, if any. (The
    /// zoom-in-from-globe behaviour lives in [`MapView::frame_on`].)
    fn center_selected_marker(&mut self) {
        let markers = self.map_markers();
        if let Some(m) = markers.get(self.map_selected) {
            self.map.frame_on(m.pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn markers_list_operator_first_then_located_peers() {
        let mut app = App::new();
        // No fix, no peer telemetry → nothing to plot.
        assert!(app.map_markers().is_empty());

        app.config.lat = Some(48.0);
        app.config.lon = Some(2.0);
        app.config.display_name = "base".to_string();
        app.conversations[0].location = Some(GeoPos::new(51.5, -0.1));

        let markers = app.map_markers();
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0].kind, MarkerKind::Operator);
        assert_eq!(markers[0].label, "base");
        assert_eq!(markers[1].kind, MarkerKind::Peer);
    }

    #[test]
    fn telemetry_creates_a_peer_marker_and_refreshes_it() {
        let mut app = App::new();
        app.conversations.clear();
        let log_before = app.syslog.len();

        // A fix from a peer we have never messaged still plots (the conversation
        // is created on first contact) and is logged.
        app.set_location("a1b2c3", GeoPos::new(51.5, -0.12));
        assert_eq!(app.conversations.len(), 1);
        let markers = app.map_markers();
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].kind, MarkerKind::Peer);
        assert_eq!(markers[0].pos, GeoPos::new(51.5, -0.12));
        assert!(app.syslog.len() > log_before, "telemetry is logged");

        // A later fix refreshes the same peer in place (no duplicate).
        app.set_location("a1b2c3", GeoPos::new(40.0, -74.0));
        assert_eq!(app.conversations.len(), 1);
        assert_eq!(app.map_markers()[0].pos, GeoPos::new(40.0, -74.0));
    }

    #[test]
    fn empty_inbound_message_is_not_recorded() {
        let mut app = App::new();
        app.conversations.clear();
        // A body-less (telemetry/command-only) message creates nothing.
        app.deliver("peer", "");
        assert!(app.conversations.is_empty());
        // A real message is still delivered normally.
        app.deliver("peer", "hi");
        assert_eq!(app.conversations.len(), 1);
        assert_eq!(app.conversations[0].messages.len(), 1);
    }

    #[test]
    fn repeated_identical_telemetry_logs_only_once() {
        let mut app = App::new();
        app.conversations.clear();
        app.syslog.clear();
        let pos = GeoPos::new(51.5, -0.12);

        app.set_location("peer", pos);
        let after_first = app.syslog.len();
        assert_eq!(after_first, 1, "first fix logs");

        // A stationary peer re-sending the same fix adds no log line.
        app.set_location("peer", pos);
        app.set_location("peer", pos);
        assert_eq!(app.syslog.len(), after_first, "unchanged fixes don't churn");

        // Movement logs again.
        app.set_location("peer", GeoPos::new(52.0, -0.12));
        assert_eq!(app.syslog.len(), after_first + 1);
    }

    #[test]
    fn syslog_is_bounded() {
        let mut app = App::new();
        app.syslog.clear();
        // Far exceed the cap; the buffer must stay bounded and keep the newest.
        for i in 0..(crate::app::SYSLOG_MAX + 250) {
            app.push_log(format!("[SYS] line {i}"));
        }
        assert_eq!(app.syslog.len(), crate::app::SYSLOG_MAX);
        let last = crate::app::SYSLOG_MAX + 249;
        assert!(
            app.syslog
                .last()
                .unwrap()
                .text
                .contains(&format!("line {last}")),
            "the most recent line is retained"
        );
    }

    #[test]
    fn cycling_markers_wraps_and_recenters() {
        let mut app = App::new();
        app.config = Config::default();
        app.config.lat = Some(0.0);
        app.config.lon = Some(0.0);
        app.conversations[0].location = Some(GeoPos::new(10.0, 20.0));
        // Zoom in so the latitude band is wide enough to shift the centre onto a
        // marker (at full-globe zoom the band collapses to the equator).
        app.map.span = 40.0;

        // Two markers: operator (idx 0) then the located peer (idx 1).
        app.select_map_marker(1);
        assert_eq!(app.map_selected, 1);
        assert_eq!(app.map.center, GeoPos::new(10.0, 20.0));
        // Wrap back to the operator.
        app.select_map_marker(1);
        assert_eq!(app.map_selected, 0);
    }

    #[test]
    fn g_toggles_the_cities_layer() {
        let mut app = App::new();
        assert!(app.map_cities, "cities are shown by default");
        let g = KeyEvent::from(KeyCode::Char('g'));
        app.handle_map_key(false, g);
        assert!(!app.map_cities, "g hides the layer");
        app.handle_map_key(false, g);
        assert!(app.map_cities, "g shows it again");
        // Resetting the viewport leaves the display toggle alone.
        app.map_cities = false;
        app.handle_map_key(false, KeyEvent::from(KeyCode::Char('r')));
        assert!(!app.map_cities, "reset only touches the viewport");
    }

    #[test]
    fn centering_from_the_globe_zooms_in_to_frame() {
        let mut app = App::new();
        app.config = Config::default();
        app.conversations[0].location = Some(GeoPos::new(40.0, 30.0));
        // Whole-globe default view: centring would otherwise be a no-op, so it
        // zooms to a regional scale and the view jumps to the marker. (The 60°
        // regional span is `MapView`'s CENTER_ZOOM_SPAN.)
        assert_eq!(app.map.span, 360.0);
        app.center_selected_marker();
        assert_eq!(app.map.span, 60.0);
        assert_eq!(app.map.center, GeoPos::new(40.0, 30.0));

        // Once already zoomed in past the threshold, centring leaves zoom alone.
        app.map.span = 20.0;
        app.center_selected_marker();
        assert_eq!(app.map.span, 20.0);
    }
}
