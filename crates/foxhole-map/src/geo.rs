//! Geographic primitives shared across the map feature (and, via re-export, the
//! wider domain model — peer telemetry and the operator fix are [`GeoPos`]).

/// A geographic position in EPSG:4326 (WGS-84) decimal degrees — what the World
/// Map tool plots. The operator's own fix comes from `config`; peer fixes arrive
/// over LXMF telemetry (e.g. a Sideband contact sharing its location).
/// Constructed via [`GeoPos::new`], which clamps latitude to the poles and wraps
/// longitude into −180..=180 so out-of-range telemetry can never project off the
/// canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPos {
    /// Latitude in degrees, north positive (−90..=90).
    pub lat: f64,
    /// Longitude in degrees, east positive (−180..=180).
    pub lon: f64,
}

impl GeoPos {
    /// A position with latitude clamped to ±90 and longitude wrapped into
    /// −180..=180. Non-finite inputs collapse to `0.0` so a bad fix is plotted at
    /// the origin rather than corrupting the viewport.
    pub fn new(lat: f64, lon: f64) -> Self {
        let lat = if lat.is_finite() { lat } else { 0.0 };
        let lon = if lon.is_finite() { lon } else { 0.0 };
        Self {
            lat: lat.clamp(-90.0, 90.0),
            lon: wrap_lon(lon),
        }
    }
}

/// Wrap a longitude into the −180..=180 range so panning across the antimeridian
/// (or out-of-range telemetry) stays on the map.
pub fn wrap_lon(lon: f64) -> f64 {
    let mut l = (lon + 180.0).rem_euclid(360.0) - 180.0;
    // `rem_euclid` yields [0,360) → [−180,180); nudge the −180 edge to +180 so a
    // reset/center reads as the conventional dateline value.
    if l <= -180.0 {
        l += 360.0;
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_clamps_latitude_and_wraps_longitude() {
        let p = GeoPos::new(120.0, 200.0);
        assert_eq!(p.lat, 90.0, "latitude pins to the pole");
        assert_eq!(p.lon, -160.0, "longitude wraps past the dateline");
        // Non-finite inputs collapse to the origin rather than corrupting state.
        let bad = GeoPos::new(f64::NAN, f64::INFINITY);
        assert_eq!(bad, GeoPos { lat: 0.0, lon: 0.0 });
    }

    #[test]
    fn wrap_lon_normalises_the_dateline() {
        assert_eq!(wrap_lon(190.0), -170.0);
        assert_eq!(wrap_lon(-190.0), 170.0);
        // The −180 edge reads as the conventional +180.
        assert_eq!(wrap_lon(-180.0), 180.0);
        assert_eq!(wrap_lon(0.0), 0.0);
    }
}
