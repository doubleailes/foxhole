//! Minimal ISO-8601 ↔ Unix-epoch conversion, dependency-free.
//!
//! CoT stamps every event with `time`/`start`/`stale` as ISO-8601 instants
//! (e.g. `2026-06-15T07:00:00.000Z`). The rest of foxhole speaks Unix epoch
//! **seconds** (see `foxhole-core`'s `now_secs`), so the codec converts at the
//! boundary: [`parse`] turns a CoT timestamp into epoch seconds for the
//! lifecycle logic (newest-`time`-wins, `stale` expiry), and [`format`] renders
//! epoch seconds back to the canonical Zulu form for generation.
//!
//! The civil-date arithmetic is Howard Hinnant's well-known
//! `days_from_civil`/`civil_from_days` algorithms (public domain), which are
//! exact for the proleptic Gregorian calendar and need no leap-table.

/// Days from 1970-01-01 to the given civil date (Gregorian). Month is 1..=12,
/// day 1..=31; out-of-range inputs are the caller's responsibility (validated in
/// [`parse`]).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Days in `m` of year `y` (proleptic Gregorian), for day-of-month validation.
fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
            if leap { 29 } else { 28 }
        }
    }
}

/// Inverse of [`days_from_civil`]: civil `(year, month, day)` for a day count
/// since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse a CoT/ISO-8601 instant into Unix epoch **seconds** (UTC).
///
/// Accepts `YYYY-MM-DDThh:mm:ss` with an optional fractional-seconds suffix
/// (`.sss`, ignored) and an optional zone designator: `Z`/`z` (or none) for UTC,
/// or a numeric offset `±hh:mm` / `±hhmm` / `±hh`. The date/time separator may be
/// `T`, `t`, or a space. Returns `None` for anything it cannot read — callers
/// treat a missing/invalid timestamp leniently (e.g. apply a default TTL).
pub fn parse(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', 't', ' '])?;

    let mut dp = date.split('-');
    let y: i64 = dp.next()?.trim().parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    // Bounding the year keeps the calendar arithmetic overflow-free (CoT stamps
    // are 4-digit years anyway); the day must exist in that month/year, so
    // 2023-02-30 is rejected rather than rolled over into March.
    if dp.next().is_some()
        || !(0..=9999).contains(&y)
        || !(1..=12).contains(&mo)
        || !(1..=days_in_month(y, mo)).contains(&d)
    {
        return None;
    }

    // Peel off the timezone designator, leaving bare `hh:mm:ss[.fff]`.
    let (hms, offset) = split_offset(rest)?;
    // Drop any fractional-seconds component — we resolve to whole seconds.
    let hms = hms.split('.').next().unwrap_or(hms);

    let mut tp = hms.split(':');
    let h: i64 = tp.next()?.trim().parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    let se: i64 = tp.next().unwrap_or("0").parse().ok()?;
    if tp.next().is_some()
        || !(0..=23).contains(&h)
        || !(0..=59).contains(&mi)
        || !(0..=60).contains(&se)
    {
        return None;
    }

    let days = days_from_civil(y, mo, d);
    Some(days * 86_400 + h * 3_600 + mi * 60 + se - offset)
}

/// Split a `hh:mm:ss[.fff][zone]` string into `(time, offset_seconds)` where
/// `offset_seconds` is what must be **subtracted** to reach UTC. `None` if a
/// present numeric offset is malformed or outside `±23:59`.
fn split_offset(t: &str) -> Option<(&str, i64)> {
    let t = t.trim();
    if let Some(rest) = t.strip_suffix(['Z', 'z']) {
        return Some((rest, 0));
    }
    // A `+`/`-` after the time (never at index 0) introduces a numeric offset.
    if let Some(idx) = t.rfind(['+', '-']).filter(|&i| i > 0) {
        let (hms, off) = t.split_at(idx);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let off = &off[1..];
        let (oh, om) = match off.split_once(':') {
            Some((a, b)) => (a, b),
            // `±hhmm` — only byte-split all-ASCII input, so a multi-byte char
            // in the offset (`+€1`) fails the parse below instead of panicking
            // on a char boundary.
            None if off.len() >= 4 && off.is_ascii() => off.split_at(2),
            None => (off, "0"), // ±hh
        };
        let oh: i64 = oh.parse().ok()?;
        let om: i64 = om.parse().ok()?;
        // No real zone lies outside ±23:59; the bound also keeps this (and the
        // caller's epoch arithmetic) overflow-free on long digit strings.
        if !(0..=23).contains(&oh) || !(0..=59).contains(&om) {
            return None;
        }
        return Some((hms, sign * (oh * 3_600 + om * 60)));
    }
    Some((t, 0)) // no designator → assume UTC
}

/// Render Unix epoch seconds as a canonical CoT Zulu timestamp
/// (`YYYY-MM-DDThh:mm:ss.000Z`). The `.000` keeps the shape ATAK emits.
pub fn format(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, mi, se) = (tod / 3_600, (tod % 3_600) / 60, tod % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{se:02}.000Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_canonical_form() {
        // 2026-06-15T07:00:00Z
        let secs = parse("2026-06-15T07:00:00.000Z").unwrap();
        assert_eq!(format(secs), "2026-06-15T07:00:00.000Z");
    }

    #[test]
    fn known_epoch_values() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("2000-01-01T00:00:00Z"), Some(946_684_800));
        // A leap year's Feb 29 must resolve.
        assert_eq!(parse("2024-02-29T12:00:00Z"), Some(1_709_208_000));
    }

    #[test]
    fn tolerates_separators_fractions_and_offsets() {
        let z = parse("2026-06-15T07:00:00.000Z").unwrap();
        assert_eq!(parse("2026-06-15t07:00:00Z"), Some(z));
        assert_eq!(parse("2026-06-15 07:00:00"), Some(z)); // bare → UTC
        assert_eq!(parse("2026-06-15T07:00:00.123456Z"), Some(z));
        // 09:00+02:00 is the same instant as 07:00Z.
        assert_eq!(parse("2026-06-15T09:00:00+02:00"), Some(z));
        assert_eq!(parse("2026-06-15T05:30:00-0130"), Some(z));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("not-a-date"), None);
        assert_eq!(parse("2026-13-01T00:00:00Z"), None); // month 13
        assert_eq!(parse("2026-06-32T00:00:00Z"), None); // day 32
        assert_eq!(parse("2026-06-15T24:00:00Z"), None); // hour 24
        assert_eq!(parse("2026-06-15"), None); // no time
    }

    #[test]
    fn malformed_offsets_fail_instead_of_panicking() {
        // Multi-byte char where `±hhmm` would byte-split (the old code panicked
        // on the char boundary — reachable from any peer's CoT `time`).
        assert_eq!(parse("2026-06-15T00:00:00+€1"), None);
        assert_eq!(parse("2026-06-15T00:00:00-€€€€"), None);
        // Digit strings long enough to overflow the `* 3_600` epoch arithmetic.
        assert_eq!(parse("2026-06-15T00:00:00+999999999999999999"), None);
        assert_eq!(parse("2026-06-15T00:00:00+99:0000000000000000"), None);
        // Out-of-range but well-formed offsets.
        assert_eq!(parse("2026-06-15T00:00:00+24:00"), None);
        assert_eq!(parse("2026-06-15T00:00:00+02:60"), None);
    }

    #[test]
    fn rejects_out_of_range_years() {
        // An 18-digit year parses as i64 but would overflow the calendar
        // arithmetic (debug panic, silent wrap in release).
        assert_eq!(parse("999999999999999999-01-01T00:00:00Z"), None);
        assert_eq!(parse("10000-01-01T00:00:00Z"), None);
        assert!(parse("9999-12-31T23:59:59Z").is_some());
        assert!(parse("0000-01-01T00:00:00Z").is_some());
    }

    #[test]
    fn rejects_days_that_do_not_exist_in_the_month() {
        assert_eq!(parse("2023-02-30T00:00:00Z"), None); // used to roll into March
        assert_eq!(parse("2023-02-29T00:00:00Z"), None); // not a leap year
        assert_eq!(parse("2023-04-31T00:00:00Z"), None); // April has 30 days
        assert_eq!(parse("2100-02-29T00:00:00Z"), None); // century non-leap
        assert!(parse("2000-02-29T00:00:00Z").is_some()); // 400-year leap
    }
}
