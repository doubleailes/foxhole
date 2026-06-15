//! World Map tool: plot the operator and any located peers on an
//! equirectangular world map, with keyboard pan/zoom and marker selection.
//!
//! All geography lives here as pure state (`MapView`) and a derived marker list
//! ([`App::map_markers`]); the actual canvas drawing is in `foxhole-tui`. Markers
//! come from two sources — the operator's own fix in `config` and peer fixes
//! arriving over LXMF telemetry (stored on each [`Conversation`](super::Conversation)).

use super::*;
use crate::domain::{GeoPos, now_secs, wrap_lon};
use foxhole_cot::Affiliation;

/// What a plotted marker represents — drives its glyph and colour in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    /// This node's own position, from `config`'s `lat`/`lon`.
    Operator,
    /// A peer whose position we learned over LXMF telemetry.
    Peer,
    /// A received CoT intel point marker, tinted by its affiliation.
    Intel(Affiliation),
}

/// A single thing plotted on the world map: a label, where it is, and what it is.
/// Built on demand from `App` state (see [`App::map_markers`]).
#[derive(Clone, Debug, PartialEq)]
pub struct MapMarker {
    /// Display label (operator display name, or peer label).
    pub label: String,
    /// Where to plot it.
    pub pos: GeoPos,
    /// What it is (drives glyph/colour).
    pub kind: MarkerKind,
}

/// The map viewport: a centre point plus how many degrees of longitude span its
/// width (the zoom level). Pan/zoom mutate these; the renderer turns them into
/// canvas `x_bounds`/`y_bounds`. Latitude span is half the longitude span, which
/// keeps the conventional 2:1 equirectangular aspect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapView {
    /// Centre of the viewport.
    pub center: GeoPos,
    /// Longitude degrees across the viewport width. Smaller = more zoomed in.
    pub span: f64,
}

/// Tightest zoom (degrees of longitude across the width) — roughly city scale.
const MIN_SPAN: f64 = 4.0;
/// Widest zoom — the whole globe.
const MAX_SPAN: f64 = 360.0;
/// Pan distance per keypress, as a fraction of the current span (so panning
/// feels constant on screen at every zoom level).
const PAN_STEP: f64 = 0.2;
/// Span a `center` snaps to when the view is wider than this — so framing a
/// marker from the whole-globe view actually zooms in to a regional scale.
const CENTER_ZOOM_SPAN: f64 = 60.0;

impl Default for MapView {
    /// The whole globe, centred on the origin.
    fn default() -> Self {
        Self {
            center: GeoPos { lat: 0.0, lon: 0.0 },
            span: MAX_SPAN,
        }
    }
}

impl MapView {
    /// Half the viewport width, in longitude degrees.
    pub fn half_lon(&self) -> f64 {
        self.span / 2.0
    }

    /// Half the viewport height, in latitude degrees (2:1 lon:lat aspect).
    pub fn half_lat(&self) -> f64 {
        self.span / 4.0
    }

    /// Longitude bounds `[west, east]` for the canvas.
    pub fn x_bounds(&self) -> [f64; 2] {
        [
            self.center.lon - self.half_lon(),
            self.center.lon + self.half_lon(),
        ]
    }

    /// Latitude bounds `[south, north]` for the canvas.
    pub fn y_bounds(&self) -> [f64; 2] {
        [
            self.center.lat - self.half_lat(),
            self.center.lat + self.half_lat(),
        ]
    }

    /// Project a longitude into the viewport's x-range, handling the
    /// antimeridian. When the centre sits near ±180 the `x_bounds()` run past the
    /// canonical edge (e.g. `[150, 190]`), but every feature is stored wrapped
    /// into −180..=180 by [`GeoPos::new`](crate::domain::GeoPos::new) — so a point
    /// at −175 (the same place as 185) only falls inside such a window once shifted
    /// by +360. Returns the input shifted by `0`/`±360` so it lands within
    /// `x_bounds()`, or `None` when the feature is off-screen horizontally.
    /// Outside the dateline case this is just the identity for in-view points.
    pub fn project_lon(&self, lon: f64) -> Option<f64> {
        let [west, east] = self.x_bounds();
        [-360.0, 0.0, 360.0]
            .into_iter()
            .map(|shift| lon + shift)
            .find(|&l| l >= west && l <= east)
    }

    /// Multiply the span by `factor`, clamped to the zoom limits, then re-clamp
    /// the centre so the viewport never runs off the poles.
    fn zoom(&mut self, factor: f64) {
        self.span = (self.span * factor).clamp(MIN_SPAN, MAX_SPAN);
        self.clamp_center();
    }

    /// Pan by fractions of the current span: `dlon`/`dlat` in `[-1, 1]`-ish units
    /// where 1.0 is a full viewport width/height. Longitude wraps; latitude is
    /// clamped to keep the view on the map.
    fn pan(&mut self, dlon: f64, dlat: f64) {
        self.center.lon = wrap_lon(self.center.lon + dlon * self.span);
        // Latitude span is half the longitude span (2:1 aspect), so a full
        // viewport height is `span / 2`, not `span`.
        self.center.lat += dlat * (self.span / 2.0);
        self.clamp_center();
    }

    /// Snap the viewport centre onto a position, then clamp to the poles.
    fn center_on(&mut self, pos: GeoPos) {
        self.center = pos;
        self.clamp_center();
    }

    /// Keep the latitude centre within the poles given the current zoom, so the
    /// top/bottom edges stay on the map. At the widest zoom the band collapses
    /// and the centre pins to the equator.
    fn clamp_center(&mut self) {
        let margin = self.half_lat().min(90.0);
        let limit = (90.0 - margin).max(0.0);
        self.center.lat = self.center.lat.clamp(-limit, limit);
    }
}

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
            });
        }
        for c in &self.conversations {
            if let Some(pos) = c.location {
                out.push(MapMarker {
                    label: c.label(),
                    pos,
                    kind: MarkerKind::Peer,
                });
            }
        }
        // Received intel point markers (live, non-expired); zones are drawn as
        // circles separately (see [`App::intel_zones`]). Selection cycling
        // (`map_selected`) tours these alongside the peers.
        for r in self.live_intel_at(now_secs() as i64) {
            if r.kind() == crate::app::CotKind::Marker {
                out.push(MapMarker {
                    label: r.label(),
                    pos: r.pos(),
                    kind: MarkerKind::Intel(r.affiliation()),
                });
            }
        }
        out
    }

    /// World Map keys: arrows pan, `+`/`-` zoom, `Tab`/`[`/`]` cycle markers,
    /// `Enter`/`c` centre on the selected marker, `r` resets the view.
    pub(super) fn handle_map_key(&mut self, _ctrl: bool, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.map.pan(-PAN_STEP, 0.0),
            KeyCode::Right => self.map.pan(PAN_STEP, 0.0),
            KeyCode::Up => self.map.pan(0.0, PAN_STEP),
            KeyCode::Down => self.map.pan(0.0, -PAN_STEP),
            KeyCode::Char('+') | KeyCode::Char('=') => self.map.zoom(0.5),
            KeyCode::Char('-') | KeyCode::Char('_') => self.map.zoom(2.0),
            KeyCode::Tab | KeyCode::Char(']') => self.select_map_marker(1),
            KeyCode::BackTab | KeyCode::Char('[') => self.select_map_marker(-1),
            KeyCode::Enter | KeyCode::Char('c') => self.center_selected_marker(),
            KeyCode::Char('r') => {
                self.map = MapView::default();
                self.map_selected = 0;
            }
            // Open the incoming-intel review list (staged CoT from unvetted peers).
            KeyCode::Char('i') => self.open_intel_review(),
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

    /// Centre the viewport on the currently selected marker, if any. Framing a
    /// marker from the whole-globe view is meaningless (it already fills the
    /// screen), so a centre from far out also zooms in to a regional scale — the
    /// view visibly jumps to the position.
    fn center_selected_marker(&mut self) {
        let markers = self.map_markers();
        if let Some(m) = markers.get(self.map_selected) {
            let pos = m.pos;
            if self.map.span > CENTER_ZOOM_SPAN {
                self.map.span = CENTER_ZOOM_SPAN;
            }
            self.map.center_on(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn default_view_is_the_whole_globe() {
        let v = MapView::default();
        assert_eq!(v.x_bounds(), [-180.0, 180.0]);
        assert_eq!(v.y_bounds(), [-90.0, 90.0]);
    }

    #[test]
    fn zoom_clamps_and_keeps_view_on_the_map() {
        let mut v = MapView::default();
        // Zoom all the way in: span bottoms out at MIN_SPAN.
        for _ in 0..20 {
            v.zoom(0.5);
        }
        assert_eq!(v.span, MIN_SPAN);
        // Zoom all the way out: span caps at MAX_SPAN.
        for _ in 0..20 {
            v.zoom(2.0);
        }
        assert_eq!(v.span, MAX_SPAN);
    }

    #[test]
    fn pan_wraps_longitude_and_clamps_latitude() {
        let mut v = MapView {
            center: GeoPos {
                lat: 0.0,
                lon: 170.0,
            },
            span: 40.0,
        };
        // Pan east across the antimeridian — longitude wraps into range.
        v.pan(1.0, 0.0);
        assert!(v.center.lon >= -180.0 && v.center.lon <= 180.0);
        // Pan hard north — latitude centre stays within the visible band.
        for _ in 0..50 {
            v.pan(0.0, 1.0);
        }
        let limit = 90.0 - v.half_lat();
        assert!(v.center.lat <= limit + f64::EPSILON);
    }

    #[test]
    fn pan_vertical_moves_by_the_visible_latitude_height() {
        let mut v = MapView {
            center: GeoPos { lat: 0.0, lon: 0.0 },
            span: 40.0,
        };
        // A full-height pan shifts the centre by the visible latitude span,
        // which is `span / 2` (== `half_lat * 2`), not the longitude `span`.
        v.pan(0.0, 1.0);
        assert_eq!(v.center.lat, v.span / 2.0);
        assert_eq!(v.center.lat, 20.0);
    }

    #[test]
    fn project_lon_handles_the_antimeridian() {
        // Centre near +180 with a small span: the window straddles the dateline.
        let v = MapView {
            center: GeoPos {
                lat: 0.0,
                lon: 178.0,
            },
            span: 20.0,
        };
        assert_eq!(v.x_bounds(), [168.0, 188.0]);
        // A marker at −179 (== 181) is wrapped out of the raw window, but a +360
        // shift brings it inside, so it stays visible.
        assert_eq!(v.project_lon(-179.0), Some(181.0));
        // A point already inside the window projects to itself.
        assert_eq!(v.project_lon(170.0), Some(170.0));
        // Something on the far side of the globe is genuinely off-screen.
        assert_eq!(v.project_lon(-10.0), None);
    }

    #[test]
    fn project_lon_is_identity_for_in_view_points() {
        // An ordinary, non-dateline window: in-view points are unchanged and
        // out-of-view ones drop out.
        let v = MapView {
            center: GeoPos { lat: 0.0, lon: 0.0 },
            span: 40.0,
        };
        assert_eq!(v.project_lon(10.0), Some(10.0));
        assert_eq!(v.project_lon(-10.0), Some(-10.0));
        assert_eq!(v.project_lon(120.0), None);
    }

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
    fn centering_from_the_globe_zooms_in_to_frame() {
        let mut app = App::new();
        app.config = Config::default();
        app.conversations[0].location = Some(GeoPos::new(40.0, 30.0));
        // Whole-globe default view: centring would otherwise be a no-op, so it
        // zooms to a regional scale and the view jumps to the marker.
        assert_eq!(app.map.span, 360.0);
        app.center_selected_marker();
        assert_eq!(app.map.span, CENTER_ZOOM_SPAN);
        assert_eq!(app.map.center, GeoPos::new(40.0, 30.0));

        // Once already zoomed in past the threshold, centring leaves zoom alone.
        app.map.span = 20.0;
        app.center_selected_marker();
        assert_eq!(app.map.span, 20.0);
    }
}
