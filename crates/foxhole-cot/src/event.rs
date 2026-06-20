//! The foxhole CoT subset event model and its XML codec.
//!
//! [`CotEvent`] is the decoded form of one CoT `<event>` — exactly the handful
//! of facets foxhole renders ([design note §4]): the point, the `type` (read as
//! affiliation + kind), the validity window, and the `detail` bits
//! (`contact/@callsign`, `remarks`, a circular `shape`). [`parse`] decodes the
//! hardened-XML subset into it leniently (unknown elements/attributes ignored,
//! never fatal); [`CotEvent::to_xml`] generates a standards-shaped document for
//! sharing. [`CotEvent::marker`]/[`CotEvent::zone`] are the producer side — the
//! latter is how today's `Zone` "folds in as a produced `u-d-c-c`".

use crate::affiliation::{Affiliation, Kind};
use crate::iso8601;
use crate::xml::{self, Token, XmlError};

/// CoT's sentinel for "error unknown" on a `point`'s `ce`/`le`.
const UNKNOWN_ERROR: f64 = 9_999_999.0;
/// Cap on `callsign`/`remarks` length (chars) — truncate attacker text (§9).
const MAX_TEXT: usize = 1_024;

/// A decoded CoT event, or a decode failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CotError {
    /// The XML reader rejected the document (size/depth/XXE/malformed).
    Xml(XmlError),
    /// No `<event>` element was present.
    NoEvent,
    /// The `<event>` had no usable `<point>` (missing/invalid lat or lon).
    NoPoint,
}

impl From<XmlError> for CotError {
    fn from(e: XmlError) -> Self {
        CotError::Xml(e)
    }
}

/// A CoT `<point>`: position plus error estimates. Latitude is clamped to the
/// poles and longitude wrapped into −180..=180 on construction (mirroring
/// `foxhole-core`'s `GeoPos`), so out-of-range input from a peer can never
/// project off the map (§9 "validate/clamp every point").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Latitude, degrees north (−90..=90).
    pub lat: f64,
    /// Longitude, degrees east (−180..=180).
    pub lon: f64,
    /// Height above ellipsoid, metres (`0.0` when unknown).
    pub hae: f64,
    /// Circular (horizontal) error, metres; [`UNKNOWN_ERROR`] = unknown.
    pub ce: f64,
    /// Linear (vertical) error, metres; [`UNKNOWN_ERROR`] = unknown.
    pub le: f64,
}

impl Point {
    /// A point with latitude clamped, longitude wrapped, and non-finite numbers
    /// collapsed to safe defaults.
    pub fn new(lat: f64, lon: f64) -> Self {
        Self {
            lat: finite(lat, 0.0).clamp(-90.0, 90.0),
            lon: wrap_lon(finite(lon, 0.0)),
            hae: 0.0,
            ce: UNKNOWN_ERROR,
            le: UNKNOWN_ERROR,
        }
    }
}

/// One CoT event in foxhole's subset. `time`/`start`/`stale` are Unix epoch
/// **seconds** (UTC), parsed from the wire's ISO-8601; `None` means the field was
/// absent or unreadable (the ingest layer then applies a default TTL).
#[derive(Clone, Debug, PartialEq)]
pub struct CotEvent {
    /// Object id — newest `time` for a given `uid` supersedes the prior event.
    pub uid: String,
    /// The raw `type` (MIL-STD-2525 code); interpreted via [`Self::affiliation`]
    /// / [`Self::kind`] but stored verbatim for round-trip fidelity.
    pub cot_type: String,
    /// Provenance hint (`how`); preserved, not interpreted.
    pub how: String,
    /// Event time (epoch seconds).
    pub time: Option<i64>,
    /// Validity start (epoch seconds).
    pub start: Option<i64>,
    /// Validity end — when the object stops being valid (epoch seconds).
    pub stale: Option<i64>,
    /// Position and error estimates.
    pub point: Point,
    /// `detail/contact/@callsign`, truncated to [`MAX_TEXT`].
    pub callsign: Option<String>,
    /// `detail/remarks`, truncated to [`MAX_TEXT`].
    pub remarks: Option<String>,
    /// Circular-zone radius in metres, from `detail/shape/ellipse/@major`
    /// (`minor` is ignored — foxhole renders circles). `None` for a bare point.
    pub radius_m: Option<f64>,
}

impl CotEvent {
    /// Affiliation read from the `type` (the tint/glyph facet).
    pub fn affiliation(&self) -> Affiliation {
        Affiliation::from_type(&self.cot_type)
    }

    /// Which map layer this object renders on.
    pub fn kind(&self) -> Kind {
        Kind::classify(&self.cot_type, self.radius_m.is_some())
    }

    /// Whether the event is no longer valid at `now` (epoch seconds): its `stale`
    /// has passed. An event with no `stale` is never stale by this test — the
    /// ingest layer is responsible for applying a default TTL.
    pub fn is_stale(&self, now: i64) -> bool {
        matches!(self.stale, Some(s) if now >= s)
    }

    /// A revoke event — `stale ≤ time` (or ≤ `start`), CoT's idiom for "remove
    /// this object" (alongside an explicit `t-x-d-d` delete type).
    pub fn is_revocation(&self) -> bool {
        if self.cot_type.starts_with("t-x-d-d") {
            return true;
        }
        match (self.stale, self.time.or(self.start)) {
            (Some(s), Some(t)) => s <= t,
            _ => false,
        }
    }

    /// Build a point marker of the given affiliation: `type` = `a-{aff}-G`.
    /// `time`/`stale` are epoch seconds.
    pub fn marker(
        uid: impl Into<String>,
        affiliation: Affiliation,
        callsign: impl Into<String>,
        lat: f64,
        lon: f64,
        time: i64,
        stale: i64,
    ) -> Self {
        Self {
            uid: uid.into(),
            cot_type: format!("a-{}-G", affiliation.token()),
            how: "h-g-i-g-o".into(), // human-entered, GPS-derived: operator-authored
            time: Some(time),
            start: Some(time),
            stale: Some(stale),
            point: Point::new(lat, lon),
            callsign: clamp_text(callsign.into()),
            remarks: None,
            radius_m: None,
        }
    }

    /// Build a circular hazard zone — `type` = `u-d-c-c` with a `shape/ellipse`
    /// of `radius_m`. This is how a foxhole `Zone` is produced on the wire.
    pub fn zone(
        uid: impl Into<String>,
        callsign: impl Into<String>,
        lat: f64,
        lon: f64,
        radius_m: f64,
        time: i64,
        stale: i64,
    ) -> Self {
        Self {
            uid: uid.into(),
            cot_type: "u-d-c-c".into(),
            how: "h-g-i-g-o".into(),
            time: Some(time),
            start: Some(time),
            stale: Some(stale),
            point: Point::new(lat, lon),
            callsign: clamp_text(callsign.into()),
            remarks: None,
            radius_m: Some(finite(radius_m, 0.0).max(0.0)),
        }
    }

    /// A one-line, human-readable summary for the LXMF message body, so a
    /// non-foxhole/non-TAK client still shows something legible (§5 graceful
    /// degradation), e.g. `INTEL: AO ALPHA (HOSTILE) @ 50.4000,30.5000 r400km,
    /// stale 13:00Z`.
    pub fn summary(&self) -> String {
        let who = self.callsign.as_deref().unwrap_or(&self.uid);
        let mut s = format!(
            "INTEL: {who} ({}) @ {:.4},{:.4}",
            self.affiliation().label(),
            self.point.lat,
            self.point.lon,
        );
        if let Some(r) = self.radius_m {
            s.push_str(&format!(" r{}km", (r / 1000.0).round() as i64));
        }
        if let Some(stale) = self.stale {
            let tod = stale.rem_euclid(86_400);
            s.push_str(&format!(
                ", stale {:02}:{:02}Z",
                tod / 3_600,
                (tod % 3_600) / 60
            ));
        }
        s
    }

    /// Serialize to a standards-shaped CoT XML document (one `<event>`), ready
    /// for the `FIELD_CUSTOM_DATA` payload.
    pub fn to_xml(&self) -> String {
        let iso = |t: Option<i64>| iso8601::format(t.unwrap_or(0));
        let mut detail = String::new();
        if let Some(cs) = &self.callsign {
            detail.push_str(&format!("<contact callsign={}/>", attr(cs)));
        }
        if let Some(r) = &self.remarks {
            detail.push_str(&format!("<remarks>{}</remarks>", escape(r)));
        }
        if let Some(radius) = self.radius_m {
            detail.push_str(&format!(
                "<shape><ellipse major=\"{radius}\" minor=\"{radius}\" angle=\"0\"/></shape>"
            ));
        }
        format!(
            "<?xml version=\"1.0\" standalone=\"yes\"?>\
             <event version=\"2.0\" uid={} type={} how={} time=\"{}\" start=\"{}\" stale=\"{}\">\
             <point lat=\"{:.6}\" lon=\"{:.6}\" hae=\"{}\" ce=\"{}\" le=\"{}\"/>\
             <detail>{detail}</detail></event>",
            attr(&self.uid),
            attr(&self.cot_type),
            attr(&self.how),
            iso(self.time),
            iso(self.start),
            iso(self.stale),
            self.point.lat,
            self.point.lon,
            self.point.hae,
            self.point.ce,
            self.point.le,
        )
    }
}

/// Decode one CoT `<event>` from XML into the foxhole subset. Lenient: unknown
/// elements and attributes are ignored, missing optional fields become `None`,
/// and only a missing `<event>` or an unusable `<point>` is an error.
pub fn parse(input: &str) -> Result<CotEvent, CotError> {
    let tokens = xml::tokenize(input)?;

    let mut event_attrs: Option<Vec<(String, String)>> = None;
    let mut point: Option<Point> = None;
    let mut callsign = None;
    let mut remarks = None;
    let mut radius_m = None;
    let mut in_remarks = false;

    for tok in &tokens {
        match tok {
            Token::Open { name, attrs } | Token::Empty { name, attrs } => match name.as_str() {
                "event" if event_attrs.is_none() => event_attrs = Some(attrs.clone()),
                "point" if point.is_none() => point = read_point(attrs),
                "contact" => {
                    if let Some(cs) = get(attrs, "callsign") {
                        callsign = clamp_text(cs.to_string());
                    }
                }
                "ellipse" => {
                    // A circular zone: prefer `major` (== `minor` for a circle).
                    if let Some(r) = get(attrs, "major").and_then(|v| v.trim().parse::<f64>().ok())
                    {
                        radius_m = Some(finite(r, 0.0).max(0.0));
                    }
                }
                "remarks" => in_remarks = matches!(tok, Token::Open { .. }),
                _ => {}
            },
            Token::Close { name } if name == "remarks" => in_remarks = false,
            Token::Text(t) if in_remarks => remarks = clamp_text(t.clone()),
            _ => {}
        }
    }

    let attrs = event_attrs.ok_or(CotError::NoEvent)?;
    let point = point.ok_or(CotError::NoPoint)?;

    Ok(CotEvent {
        uid: get(&attrs, "uid").unwrap_or_default().to_string(),
        cot_type: get(&attrs, "type").unwrap_or_default().to_string(),
        how: get(&attrs, "how").unwrap_or_default().to_string(),
        time: get(&attrs, "time").and_then(iso8601::parse),
        start: get(&attrs, "start").and_then(iso8601::parse),
        stale: get(&attrs, "stale").and_then(iso8601::parse),
        point,
        callsign,
        remarks,
        radius_m,
    })
}

/// Read a `<point>`'s attributes into a [`Point`]. Both `lat` and `lon` must be
/// present and finite, else the point is unusable (caller → [`CotError::NoPoint`]);
/// `hae`/`ce`/`le` are optional and default to the CoT sentinels.
fn read_point(attrs: &[(String, String)]) -> Option<Point> {
    let num = |k| get(attrs, k).and_then(|v| v.trim().parse::<f64>().ok());
    let (lat, lon) = (num("lat")?, num("lon")?);
    if !lat.is_finite() || !lon.is_finite() {
        return None;
    }
    let mut p = Point::new(lat, lon);
    if let Some(h) = num("hae") {
        p.hae = finite(h, 0.0);
    }
    if let Some(ce) = num("ce") {
        p.ce = finite(ce, UNKNOWN_ERROR);
    }
    if let Some(le) = num("le") {
        p.le = finite(le, UNKNOWN_ERROR);
    }
    Some(p)
}

/// Look up an attribute value by key.
fn get<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Replace a non-finite number with `fallback`.
fn finite(v: f64, fallback: f64) -> f64 {
    if v.is_finite() { v } else { fallback }
}

/// Truncate operator/attacker text to [`MAX_TEXT`] chars; empty → `None`.
fn clamp_text(mut s: String) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    if s.chars().count() > MAX_TEXT {
        s = s.chars().take(MAX_TEXT).collect();
    }
    Some(s)
}

/// Wrap a longitude into −180..=180 (mirrors `foxhole-core`'s `wrap_lon`).
fn wrap_lon(lon: f64) -> f64 {
    let mut l = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if l <= -180.0 {
        l += 360.0;
    }
    l
}

/// Escape character data for XML text content.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape and double-quote a string as an XML attribute value.
fn attr(s: &str) -> String {
    format!("\"{}\"", escape(s).replace('"', "&quot;"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact XML the reference injector (`cot_inject.py --dry-run`) emits for
    /// a hostile hazard zone — the capture-as-fixture discipline from the design
    /// note's Appendix A.
    const INJECTOR_ZONE: &str = "<?xml version=\"1.0\" standalone=\"yes\"?>\
        <event version=\"2.0\" uid=\"foxhole-AO ALPHA-1718000000\" type=\"a-h-G-U-C\" \
        how=\"h-g-i-g-o\" time=\"2026-06-15T07:00:00.000Z\" start=\"2026-06-15T07:00:00.000Z\" \
        stale=\"2026-06-15T13:00:00.000Z\">\
        <point lat=\"50.400000\" lon=\"30.500000\" hae=\"0.0\" ce=\"9999999.0\" le=\"9999999.0\"/>\
        <detail><contact callsign=\"AO ALPHA\"/><remarks>shelling reported</remarks>\
        <shape><ellipse major=\"400000\" minor=\"400000\" angle=\"0\"/></shape></detail></event>";

    #[test]
    fn parses_the_injector_fixture() {
        let e = parse(INJECTOR_ZONE).unwrap();
        assert_eq!(e.uid, "foxhole-AO ALPHA-1718000000");
        assert_eq!(e.cot_type, "a-h-G-U-C");
        assert_eq!(e.affiliation(), Affiliation::Hostile);
        assert_eq!(e.kind(), Kind::Zone); // radius promotes it
        assert_eq!(e.point.lat, 50.4);
        assert_eq!(e.point.lon, 30.5);
        assert_eq!(e.callsign.as_deref(), Some("AO ALPHA"));
        assert_eq!(e.remarks.as_deref(), Some("shelling reported"));
        assert_eq!(e.radius_m, Some(400_000.0));
        assert_eq!(e.time, iso8601::parse("2026-06-15T07:00:00Z"));
        assert_eq!(e.stale, iso8601::parse("2026-06-15T13:00:00Z"));
        assert!(!e.is_stale(e.time.unwrap()));
        assert!(e.is_stale(e.stale.unwrap()));
    }

    #[test]
    fn round_trips_through_xml() {
        let t = iso8601::parse("2026-06-15T07:00:00Z").unwrap();
        let stale = iso8601::parse("2026-06-15T13:00:00Z").unwrap();
        let z = CotEvent::zone("J-OP-1", "AO ALPHA", 50.4, 30.5, 400_000.0, t, stale);
        let back = parse(&z.to_xml()).unwrap();
        assert_eq!(back.uid, z.uid);
        assert_eq!(back.cot_type, "u-d-c-c");
        assert_eq!(back.point.lat, 50.4);
        assert_eq!(back.radius_m, Some(400_000.0));
        assert_eq!(back.time, z.time);
        assert_eq!(back.stale, z.stale);
    }

    #[test]
    fn marker_round_trips_with_affiliation() {
        let m = CotEvent::marker("M-1", Affiliation::Friendly, "OP-1", 48.86, 2.29, 100, 200);
        assert_eq!(m.cot_type, "a-f-G");
        let back = parse(&m.to_xml()).unwrap();
        assert_eq!(back.affiliation(), Affiliation::Friendly);
        assert_eq!(back.kind(), Kind::Marker);
        assert_eq!(back.callsign.as_deref(), Some("OP-1"));
        assert_eq!(back.radius_m, None);
    }

    #[test]
    fn lenient_about_unknown_elements_and_missing_detail() {
        let xml = "<event uid=\"x\" type=\"a-u-G\">\
            <point lat=\"0\" lon=\"0\"/><detail><__group__ name=\"g\"/>\
            <unknownthing>ignored</unknownthing></detail></event>";
        let e = parse(xml).unwrap();
        assert_eq!(e.uid, "x");
        assert_eq!(e.callsign, None);
        assert_eq!(e.remarks, None);
        assert_eq!(e.affiliation(), Affiliation::Unknown);
    }

    #[test]
    fn rejects_missing_event_or_point() {
        assert_eq!(parse("<detail/>"), Err(CotError::NoEvent));
        assert_eq!(
            parse("<event uid=\"x\" type=\"a-u-G\"/>"),
            Err(CotError::NoPoint)
        );
        assert_eq!(
            parse("<event uid=\"x\"><point hae=\"0\"/></event>"),
            Err(CotError::NoPoint)
        );
    }

    #[test]
    fn propagates_xxe_rejection() {
        let xxe = "<!DOCTYPE e [<!ENTITY x SYSTEM \"file:///etc/passwd\">]>\
            <event uid=\"x\" type=\"a-u-G\"><point lat=\"0\" lon=\"0\"/></event>";
        assert_eq!(parse(xxe), Err(CotError::Xml(XmlError::Dtd)));
    }

    #[test]
    fn clamps_out_of_range_coordinates() {
        let e = parse("<event uid=\"x\" type=\"a-u-G\"><point lat=\"99\" lon=\"540\"/></event>")
            .unwrap();
        assert_eq!(e.point.lat, 90.0); // clamped to the pole
        assert_eq!(e.point.lon, 180.0); // 540 wraps to 180
    }

    #[test]
    fn truncates_oversized_text() {
        let big = "A".repeat(MAX_TEXT + 50);
        let xml = format!(
            "<event uid=\"x\" type=\"a-u-G\"><point lat=\"0\" lon=\"0\"/>\
             <detail><contact callsign=\"{big}\"/></detail></event>"
        );
        let e = parse(&xml).unwrap();
        assert_eq!(e.callsign.unwrap().chars().count(), MAX_TEXT);
    }

    #[test]
    fn detects_revocation() {
        let t = 1000;
        // stale <= time is a revoke.
        let mut e = CotEvent::zone("z", "AO", 0.0, 0.0, 1000.0, t, t);
        assert!(e.is_revocation());
        e.stale = Some(t + 1);
        assert!(!e.is_revocation());
        e.cot_type = "t-x-d-d".into();
        assert!(e.is_revocation());
    }

    #[test]
    fn summary_is_legible() {
        let t = iso8601::parse("2026-06-15T07:00:00Z").unwrap();
        let stale = iso8601::parse("2026-06-15T13:00:00Z").unwrap();
        let mut z = CotEvent::zone("z", "AO ALPHA", 50.4, 30.5, 400_000.0, t, stale);
        z.cot_type = "a-h-G-U-C".into(); // hostile zone, as the injector sends
        assert_eq!(
            z.summary(),
            "INTEL: AO ALPHA (HOSTILE) @ 50.4000,30.5000 r400km, stale 13:00Z"
        );
    }
}
