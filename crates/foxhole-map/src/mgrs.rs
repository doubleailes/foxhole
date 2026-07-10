//! Military Grid Reference System (MGRS) codec — convert between [`GeoPos`]
//! (WGS-84 decimal degrees) and MGRS grid references so the operator can reframe
//! the map onto, or designate a position by, a grid reference instead of raw
//! lat/lon.
//!
//! An MGRS reference is `<zone><band><square><easting><northing>`, e.g.
//! `18SUJ2348706483`: the UTM grid-zone designator (`18S`), the 100 km square id
//! (`UJ`), then an even split of digits giving easting then northing within that
//! square (here 5+5 digits → 1 m precision).
//!
//! The transform runs through UTM (Universal Transverse Mercator): lat/lon ↔ UTM
//! easting/northing on the WGS-84 ellipsoid (Snyder's series, mm-accurate in the
//! −80°..84° band), then UTM ↔ the MGRS square lettering. This is the same
//! algorithm the widely-used `mgrs`/`proj4js` libraries implement, reimplemented
//! here dependency-free. Polar regions (outside −80°..84°) use UPS, which foxhole
//! does not carry — [`format`] returns `None` there.

use crate::geo::GeoPos;

/// WGS-84 semi-major axis (metres).
const A: f64 = 6_378_137.0;
/// WGS-84 first eccentricity squared.
const ECC_SQ: f64 = 0.006_694_38;
/// UTM central-meridian scale factor.
const K0: f64 = 0.9996;

/// Default digits per axis used when the caller doesn't care: 5 → 1 m precision.
pub const DEFAULT_DIGITS: usize = 5;

// 100 km square lettering: the column/row origin letter for each of the six
// zone-cycle sets, and the ASCII bounds the I/O-skipping arithmetic clamps to.
const SET_ORIGIN_COLUMN: [u8; 6] = *b"AJSAJS";
const SET_ORIGIN_ROW: [u8; 6] = *b"AFAFAF";
const LETTER_A: i32 = b'A' as i32;
const LETTER_I: i32 = b'I' as i32;
const LETTER_O: i32 = b'O' as i32;
const LETTER_V: i32 = b'V' as i32;
const LETTER_Z: i32 = b'Z' as i32;

/// One position in UTM coordinates plus the grid-zone identity MGRS needs.
struct Utm {
    easting: f64,
    northing: f64,
    zone: u8,
    /// Latitude-band letter (C..X) — also our hemisphere flag on the way back.
    band: char,
}

/// Format `pos` as an MGRS grid reference at `digits` digits per axis (clamped to
/// 1..=5, i.e. 10 km..1 m precision). Returns `None` for latitudes outside UTM's
/// −80°..84° band, where MGRS needs the polar (UPS) grid foxhole doesn't carry.
pub fn format(pos: GeoPos, digits: usize) -> Option<String> {
    if !(-80.0..=84.0).contains(&pos.lat) {
        return None;
    }
    let digits = digits.clamp(1, 5);
    let utm = ll_to_utm(pos);
    let square = hundred_k_id(utm.easting, utm.northing, utm.zone)?;

    // The within-100km easting/northing are the low five digits of each; take the
    // leading `digits` of those (so fewer digits is a coarser fix, not a wrong one).
    let e = (utm.easting.round() as i64).rem_euclid(100_000);
    let n = (utm.northing.round() as i64).rem_euclid(100_000);
    let e = &format!("{e:05}")[..digits];
    let n = &format!("{n:05}")[..digits];
    Some(format!("{}{}{square}{e}{n}", utm.zone, utm.band))
}

/// Parse an MGRS grid reference into the **centre** of the grid square it names
/// (so a coarse reference still frames sensibly). Tolerant of spaces and
/// lower-case; returns `None` if the input isn't a well-formed reference.
pub fn parse(s: &str) -> Option<GeoPos> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let s = s.to_ascii_uppercase();
    let bytes = s.as_bytes();
    if bytes.len() < 5 {
        return None;
    }

    // Zone number: the leading one or two digits.
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 2 {
        return None;
    }
    let zone: u8 = s[..i].parse().ok()?;
    if !(1..=60).contains(&zone) {
        return None;
    }

    // Band letter, then the two-letter 100 km square id.
    let band = *bytes.get(i)? as char;
    if !valid_band(band) {
        return None;
    }
    i += 1;
    let col = *bytes.get(i)? as char;
    let row = *bytes.get(i + 1)? as char;
    i += 2;

    let set = set_for_zone(zone);
    let east_100k = easting_from_char(col, set)?;
    let mut north_100k = northing_from_char(row, set)?;
    // Lift the 100 km row into the band's northing range (the lettering repeats
    // every 2 000 km, so the band fixes which repetition we're in).
    let min_north = min_northing(band)?;
    while north_100k < min_north {
        north_100k += 2_000_000.0;
    }

    // The remaining digits split evenly into easting then northing.
    let rest = &bytes[i..];
    if !rest.len().is_multiple_of(2) || !rest.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let half = rest.len() / 2;
    let (mut easting, mut northing) = (east_100k, north_100k);
    // Square size for the precision given: 100 km / 10^half. With no digits the
    // reference is the whole 100 km square.
    let square = 100_000.0 / 10f64.powi(half as i32);
    if half > 0 {
        let e: f64 = s[i..i + half].parse().ok()?;
        let n: f64 = s[i + half..].parse().ok()?;
        easting += e * square;
        northing += n * square;
    }

    // Frame on the centre of the named square, not its south-west corner.
    let utm = Utm {
        easting: easting + square / 2.0,
        northing: northing + square / 2.0,
        zone,
        band,
    };
    Some(utm_to_ll(&utm))
}

/// Latitude bands run C..X (8° each) skipping I and O; A/B/Y/Z are the polar UPS
/// caps MGRS-via-UTM never produces.
fn valid_band(c: char) -> bool {
    matches!(c, 'C'..='H' | 'J'..='N' | 'P'..='X')
}

/// The latitude-band letter for `lat` (C..X), per the standard 8° banding (X is
/// the 12°-tall top band). Callers gate on the −80..84 range first.
fn band_letter(lat: f64) -> char {
    if (72.0..=84.0).contains(&lat) {
        return 'X';
    }
    const BANDS: &[u8] = b"CDEFGHJKLMNPQRSTUVWX";
    let idx = ((lat + 80.0) / 8.0).floor() as usize;
    BANDS[idx.min(BANDS.len() - 1)] as char
}

/// Which of the six 100 km lettering sets a UTM zone uses (cycles every 6 zones).
fn set_for_zone(zone: u8) -> usize {
    (zone as usize + 5) % 6 // zone%6 with 0→6, then to a 0-based index
}

/// Forward projection: WGS-84 lat/lon → UTM, with the Norway/Svalbard zone
/// exceptions so those references match standard output.
fn ll_to_utm(pos: GeoPos) -> Utm {
    let (lat, lon) = (pos.lat, pos.lon);
    let mut zone = ((lon + 180.0) / 6.0).floor() as i32 + 1;
    if lon >= 180.0 {
        zone = 60;
    }
    // South-west Norway shifts zone 31→32.
    if (56.0..64.0).contains(&lat) && (3.0..12.0).contains(&lon) {
        zone = 32;
    }
    // Svalbard's four widened zones.
    if (72.0..84.0).contains(&lat) {
        zone = match lon {
            l if (0.0..9.0).contains(&l) => 31,
            l if (9.0..21.0).contains(&l) => 33,
            l if (21.0..33.0).contains(&l) => 35,
            l if (33.0..42.0).contains(&l) => 37,
            _ => zone,
        };
    }

    let lat_rad = lat.to_radians();
    let lon_rad = lon.to_radians();
    let lon_origin = ((zone - 1) * 6 - 180 + 3) as f64; // central meridian
    let lon_origin_rad = lon_origin.to_radians();

    let ecc_prime = ECC_SQ / (1.0 - ECC_SQ);
    let n = A / (1.0 - ECC_SQ * lat_rad.sin().powi(2)).sqrt();
    let t = lat_rad.tan().powi(2);
    let c = ecc_prime * lat_rad.cos().powi(2);
    let a_ = lat_rad.cos() * (lon_rad - lon_origin_rad);
    let m = A
        * ((1.0 - ECC_SQ / 4.0 - 3.0 * ECC_SQ.powi(2) / 64.0 - 5.0 * ECC_SQ.powi(3) / 256.0)
            * lat_rad
            - (3.0 * ECC_SQ / 8.0 + 3.0 * ECC_SQ.powi(2) / 32.0 + 45.0 * ECC_SQ.powi(3) / 1024.0)
                * (2.0 * lat_rad).sin()
            + (15.0 * ECC_SQ.powi(2) / 256.0 + 45.0 * ECC_SQ.powi(3) / 1024.0)
                * (4.0 * lat_rad).sin()
            - (35.0 * ECC_SQ.powi(3) / 3072.0) * (6.0 * lat_rad).sin());

    let easting = K0
        * n
        * (a_
            + (1.0 - t + c) * a_.powi(3) / 6.0
            + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ecc_prime) * a_.powi(5) / 120.0)
        + 500_000.0;
    let mut northing = K0
        * (m + n
            * lat_rad.tan()
            * (a_.powi(2) / 2.0
                + (5.0 - t + 9.0 * c + 4.0 * c * c) * a_.powi(4) / 24.0
                + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ecc_prime) * a_.powi(6) / 720.0));
    if lat < 0.0 {
        northing += 10_000_000.0;
    }

    Utm {
        easting,
        northing,
        zone: zone as u8,
        band: band_letter(lat),
    }
}

/// Inverse projection: UTM → WGS-84 lat/lon (Snyder's inverse series). The band
/// letter only tells us the hemisphere (< 'N' → southern).
fn utm_to_ll(utm: &Utm) -> GeoPos {
    let x = utm.easting - 500_000.0;
    let mut y = utm.northing;
    if utm.band < 'N' {
        y -= 10_000_000.0;
    }
    let lon_origin = ((utm.zone as i32 - 1) * 6 - 180 + 3) as f64;
    let ecc_prime = ECC_SQ / (1.0 - ECC_SQ);
    let e1 = (1.0 - (1.0 - ECC_SQ).sqrt()) / (1.0 + (1.0 - ECC_SQ).sqrt());

    let m = y / K0;
    let mu =
        m / (A * (1.0 - ECC_SQ / 4.0 - 3.0 * ECC_SQ.powi(2) / 64.0 - 5.0 * ECC_SQ.powi(3) / 256.0));
    let phi1 = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1.powi(2) / 16.0 - 55.0 * e1.powi(4) / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1.powi(3) / 96.0) * (6.0 * mu).sin();

    let n1 = A / (1.0 - ECC_SQ * phi1.sin().powi(2)).sqrt();
    let t1 = phi1.tan().powi(2);
    let c1 = ecc_prime * phi1.cos().powi(2);
    let r1 = A * (1.0 - ECC_SQ) / (1.0 - ECC_SQ * phi1.sin().powi(2)).powf(1.5);
    let d = x / (n1 * K0);

    let lat = phi1
        - (n1 * phi1.tan() / r1)
            * (d * d / 2.0
                - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * ecc_prime) * d.powi(4)
                    / 24.0
                + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1
                    - 252.0 * ecc_prime
                    - 3.0 * c1 * c1)
                    * d.powi(6)
                    / 720.0);
    let lon = (d - (1.0 + 2.0 * t1 + c1) * d.powi(3) / 6.0
        + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * ecc_prime + 24.0 * t1 * t1)
            * d.powi(5)
            / 120.0)
        / phi1.cos();

    GeoPos::new(lat.to_degrees(), lon_origin + lon.to_degrees())
}

/// The two-letter 100 km square id for a UTM easting/northing in `zone`.
fn hundred_k_id(easting: f64, northing: f64, zone: u8) -> Option<String> {
    let set = set_for_zone(zone);
    let col = (easting / 100_000.0).floor() as i32;
    let row = ((northing / 100_000.0).floor() as i32).rem_euclid(20);

    let col_origin = SET_ORIGIN_COLUMN[set] as i32;
    let row_origin = SET_ORIGIN_ROW[set] as i32;

    let mut col_int = col_origin + col - 1;
    let mut row_int = row_origin + row;
    let mut rollover = false;
    if col_int > LETTER_Z {
        col_int = col_int - LETTER_Z + LETTER_A - 1;
        rollover = true;
    }
    if col_int == LETTER_I
        || (col_origin < LETTER_I && col_int > LETTER_I)
        || ((col_int > LETTER_I || col_origin < LETTER_I) && rollover)
    {
        col_int += 1;
    }
    if col_int == LETTER_O
        || (col_origin < LETTER_O && col_int > LETTER_O)
        || ((col_int > LETTER_O || col_origin < LETTER_O) && rollover)
    {
        col_int += 1;
        if col_int == LETTER_I {
            col_int += 1;
        }
    }
    if col_int > LETTER_Z {
        col_int = col_int - LETTER_Z + LETTER_A - 1;
    }

    rollover = false;
    if row_int > LETTER_V {
        row_int = row_int - LETTER_V + LETTER_A - 1;
        rollover = true;
    }
    if row_int == LETTER_I
        || (row_origin < LETTER_I && row_int > LETTER_I)
        || ((row_int > LETTER_I || row_origin < LETTER_I) && rollover)
    {
        row_int += 1;
    }
    if row_int == LETTER_O
        || (row_origin < LETTER_O && row_int > LETTER_O)
        || ((row_int > LETTER_O || row_origin < LETTER_O) && rollover)
    {
        row_int += 1;
        if row_int == LETTER_I {
            row_int += 1;
        }
    }
    if row_int > LETTER_V {
        row_int = row_int - LETTER_V + LETTER_A - 1;
    }

    let col_c = u8::try_from(col_int).ok()? as char;
    let row_c = u8::try_from(row_int).ok()? as char;
    Some(format!("{col_c}{row_c}"))
}

/// The 100 km easting (metres) for a column letter within a lettering set,
/// stepping the I/O-skipping alphabet from the set's origin.
fn easting_from_char(c: char, set: usize) -> Option<f64> {
    let target = c as i32;
    let mut cur = SET_ORIGIN_COLUMN[set] as i32;
    let mut easting = 100_000.0;
    let mut rewound = false;
    while cur != target {
        cur += 1;
        if cur == LETTER_I {
            cur += 1;
        }
        if cur == LETTER_O {
            cur += 1;
        }
        if cur > LETTER_Z {
            if rewound {
                return None;
            }
            cur = LETTER_A;
            rewound = true;
        }
        easting += 100_000.0;
    }
    Some(easting)
}

/// The 100 km northing (metres, pre band-lift) for a row letter within a set.
fn northing_from_char(c: char, set: usize) -> Option<f64> {
    if c > 'V' {
        return None;
    }
    let target = c as i32;
    let mut cur = SET_ORIGIN_ROW[set] as i32;
    let mut northing = 0.0;
    let mut rewound = false;
    while cur != target {
        cur += 1;
        if cur == LETTER_I {
            cur += 1;
        }
        if cur == LETTER_O {
            cur += 1;
        }
        if cur > LETTER_V {
            if rewound {
                return None;
            }
            cur = LETTER_A;
            rewound = true;
        }
        northing += 100_000.0;
    }
    Some(northing)
}

/// The minimum northing (metres) for a latitude band — used to lift a decoded
/// 100 km row into the right 2 000 km repetition.
fn min_northing(band: char) -> Option<f64> {
    let n = match band {
        'C' => 1_100_000.0,
        'D' => 2_000_000.0,
        'E' => 2_800_000.0,
        'F' => 3_700_000.0,
        'G' => 4_600_000.0,
        'H' => 5_500_000.0,
        'J' => 6_400_000.0,
        'K' => 7_300_000.0,
        'L' => 8_200_000.0,
        'M' => 9_100_000.0,
        'N' => 0.0,
        'P' => 800_000.0,
        'Q' => 1_700_000.0,
        'R' => 2_600_000.0,
        'S' => 3_500_000.0,
        'T' => 4_400_000.0,
        'U' => 5_300_000.0,
        'V' => 6_200_000.0,
        'W' => 7_000_000.0,
        'X' => 7_900_000.0,
        _ => return None,
    };
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two positions agree to within `eps` degrees on both axes.
    fn close(a: GeoPos, b: GeoPos, eps: f64) -> bool {
        (a.lat - b.lat).abs() < eps && (a.lon - b.lon).abs() < eps
    }

    #[test]
    fn formats_known_references() {
        // Equator/prime-meridian origin — the canonical GeographicLib worked
        // example for MGRS, exact to the metre.
        assert_eq!(
            format(GeoPos::new(0.0, 0.0), 5).as_deref(),
            Some("31NAA6602100000")
        );
        // Eiffel Tower, northern band U, zone 31.
        let eiffel = GeoPos::new(48.858_37, 2.294_48);
        let mgrs = format(eiffel, 5).unwrap();
        assert!(mgrs.starts_with("31U"), "got {mgrs}");
    }

    #[test]
    fn parses_to_the_square_centre() {
        // The origin reference decodes back to within a metre (~1e-4°).
        let p = parse("31N AA 66021 00000").unwrap();
        assert!(close(p, GeoPos::new(0.0, 0.0), 1e-3), "{p:?}");
        // Case- and space-insensitive.
        assert!(parse("31naa6602100000").is_some());
    }

    #[test]
    fn round_trips_across_the_globe() {
        // A spread of mid-latitude points survive ll→MGRS→ll within ~5 m.
        for &(lat, lon) in &[
            (0.0, 0.0),
            (51.5, -0.12),
            (-33.86, 151.2),
            (35.68, 139.69),
            (-23.55, -46.63),
            (64.13, -21.9),
        ] {
            let p = GeoPos::new(lat, lon);
            let s = format(p, 5).expect("in-band");
            let back = parse(&s).expect("re-parse");
            assert!(close(p, back, 1e-3), "{p:?} -> {s} -> {back:?}");
        }
    }

    #[test]
    fn coarser_precision_loses_resolution_but_stays_in_square() {
        let p = GeoPos::new(48.858_37, 2.294_48);
        // 1-digit precision is a 10 km square: still resolves near Paris.
        let coarse = parse(&format(p, 1).unwrap()).unwrap();
        assert!(close(p, coarse, 0.1), "{coarse:?}");
        // 3 digits (100 m) is much tighter.
        let mid = parse(&format(p, 3).unwrap()).unwrap();
        assert!(close(p, mid, 1e-2), "{mid:?}");
    }

    #[test]
    fn rejects_malformed_and_polar_input() {
        assert!(parse("").is_none());
        assert!(parse("garbage").is_none());
        assert!(parse("18S").is_none()); // no square
        assert!(parse("18SUJ123").is_none()); // odd digit count
        assert!(parse("99SUJ2348306482").is_none()); // zone out of range
        assert!(parse("18IUJ2348306482").is_none()); // invalid band letter
        // Antarctica / high Arctic are outside the UTM band.
        assert!(format(GeoPos::new(-88.0, 0.0), 5).is_none());
        assert!(format(GeoPos::new(85.0, 0.0), 5).is_none());
    }
}
