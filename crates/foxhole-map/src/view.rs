//! The World Map viewport ([`MapView`]) and what it plots ([`MapMarker`]).
//!
//! All geometry — pan/zoom limits, the antimeridian projection, the
//! frame-on-marker behaviour — lives here as pure methods on [`MapView`], so the
//! `App` layer only has to route a keypress to an intent-named call
//! (`pan_east`, `zoom_in`, `frame_on`, …) and never touches the magic numbers.

use crate::geo::{GeoPos, wrap_lon};
use foxhole_cot::Affiliation;

/// What a plotted marker represents — drives its glyph and colour in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    /// This node's own position, from `config`'s `lat`/`lon`.
    Operator,
    /// A peer whose position we learned over LXMF telemetry.
    Peer,
    /// A received/authored CoT intel point marker, tinted by its affiliation.
    Intel(Affiliation),
}

/// A single thing plotted on the world map: a label, where it is, and what it
/// is. Built on demand from `App` state (see `App::map_markers`).
#[derive(Clone, Debug, PartialEq)]
pub struct MapMarker {
    /// Display label (operator display name, or peer label).
    pub label: String,
    /// Where to plot it.
    pub pos: GeoPos,
    /// What it is (drives glyph/colour).
    pub kind: MarkerKind,
    /// For an intel marker, its `(source, uid)` key — lets map actions (edit /
    /// local remove) resolve the selected marker back to its `IntelRecord`.
    /// `None` for the operator/peer markers.
    pub intel_key: Option<(String, String)>,
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
    /// into −180..=180 by [`GeoPos::new`] — so a point at −175 (the same place as
    /// 185) only falls inside such a window once shifted by +360. Returns the
    /// input shifted by `0`/`±360` so it lands within `x_bounds()`, or `None`
    /// when the feature is off-screen horizontally. Outside the dateline case
    /// this is just the identity for in-view points.
    pub fn project_lon(&self, lon: f64) -> Option<f64> {
        let [west, east] = self.x_bounds();
        [-360.0, 0.0, 360.0]
            .into_iter()
            .map(|shift| lon + shift)
            .find(|&l| l >= west && l <= east)
    }

    /// Pan one step west / east / north / south. The step is a fixed fraction of
    /// the current span, so it feels constant on screen at any zoom.
    pub fn pan_west(&mut self) {
        self.pan(-PAN_STEP, 0.0);
    }
    pub fn pan_east(&mut self) {
        self.pan(PAN_STEP, 0.0);
    }
    pub fn pan_north(&mut self) {
        self.pan(0.0, PAN_STEP);
    }
    pub fn pan_south(&mut self) {
        self.pan(0.0, -PAN_STEP);
    }

    /// Zoom in / out one step, clamped to the zoom limits.
    pub fn zoom_in(&mut self) {
        self.zoom(0.5);
    }
    pub fn zoom_out(&mut self) {
        self.zoom(2.0);
    }

    /// Frame a position. Framing from the whole-globe view is meaningless (the
    /// marker already fills the screen), so from far out this also zooms in to a
    /// regional scale before centring — the view visibly jumps to the position.
    pub fn frame_on(&mut self, pos: GeoPos) {
        if self.span > CENTER_ZOOM_SPAN {
            self.span = CENTER_ZOOM_SPAN;
        }
        self.center_on(pos);
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

#[cfg(test)]
mod tests {
    use super::*;

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
            v.zoom_in();
        }
        assert_eq!(v.span, MIN_SPAN);
        // Zoom all the way out: span caps at MAX_SPAN.
        for _ in 0..20 {
            v.zoom_out();
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
        v.pan_east();
        assert!(v.center.lon >= -180.0 && v.center.lon <= 180.0);
        // Pan hard north — latitude centre stays within the visible band.
        for _ in 0..50 {
            v.pan_north();
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
        // which is `span / 2` (== `half_lat * 2`), not the longitude `span`. One
        // `pan_north` is PAN_STEP (0.2) of that.
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
    fn frame_on_zooms_in_from_the_globe_then_holds_zoom() {
        let mut v = MapView::default();
        assert_eq!(v.span, MAX_SPAN);
        // From the whole-globe view, framing zooms to a regional scale and jumps.
        v.frame_on(GeoPos::new(40.0, 30.0));
        assert_eq!(v.span, CENTER_ZOOM_SPAN);
        assert_eq!(v.center, GeoPos::new(40.0, 30.0));
        // Once already zoomed in past the threshold, framing leaves zoom alone.
        v.span = 20.0;
        v.frame_on(GeoPos::new(-10.0, -20.0));
        assert_eq!(v.span, 20.0);
        assert_eq!(v.center, GeoPos::new(-10.0, -20.0));
    }
}
