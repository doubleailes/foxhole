//! Sideband-compatible location telemetry: decode an inbound fix, detect a
//! telemetry request, and pack our own position for the reply.
//!
//! The wire shapes here are all reverse-engineered from a live Sideband handset
//! (see the per-item docs), so they are kept together and covered by fixtures
//! taken from real payloads. Pure functions only — the reply *send* lives in
//! [`super::outbound`].

use lxmf_core::message::LxMessage;

/// Sideband telemetry sensor id for the location sensor (`sense.py`
/// `Sensor.SID_LOCATION`). The telemetry field is a msgpack map keyed by sensor
/// id; this is the entry carrying latitude/longitude. (Confirmed against a live
/// Sideband handset's payload — the time sensor is `0x01`, location `0x02`.)
pub(crate) const SID_LOCATION: u8 = 0x02;

/// Sideband command id for a telemetry request (`Commands.TELEMETRY_REQUEST`).
/// Confirmed from a live handset's `FIELD_COMMANDS` payload.
const COMMAND_TELEMETRY_REQUEST: u8 = 0x01;

/// Extract a `(lat, lon)` fix from a message's Sideband-style telemetry field,
/// if present and plausible.
pub(crate) fn location(msg: &LxMessage) -> Option<(f64, f64)> {
    let bytes = msg.get_field(lxmf_core::constants::FIELD_TELEMETRY)?;
    parse_location(bytes)
}

/// Pure decode half of [`location`] (split out so it is unit-testable without a
/// whole `LxMessage`). The `FIELD_TELEMETRY` value is a msgpack map
/// `{ sensor_id: value }`; the location sensor ([`SID_LOCATION`]) packs an array
/// whose first two elements are latitude and longitude. Sideband encodes each as
/// a 4-byte big-endian signed integer (degrees ×1e6) wrapped in a msgpack binary;
/// see [`coord`] for the bare integer/float forms we also accept.
/// Implausible coordinates are rejected so a misparse plots nothing rather than
/// noise.
pub(crate) fn parse_location(bytes: &[u8]) -> Option<(f64, f64)> {
    let value = rmpv::decode::read_value(&mut &bytes[..]).ok()?;
    let map = value.as_map()?;
    let location = map.iter().find_map(|(k, v)| {
        let id = k.as_u64().or_else(|| k.as_i64().map(|i| i as u64));
        (id == Some(u64::from(SID_LOCATION))).then_some(v)
    })?;
    let arr = location.as_array()?;
    let lat = coord(arr.first()?)?;
    let lon = coord(arr.get(1)?)?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// One coordinate from a telemetry array, scaled to degrees. Sideband packs each
/// coordinate as a big-endian signed integer of `degrees ×1e6` wrapped in a
/// msgpack **binary** (4 bytes for the i32 form, 8 for an i64) — this is what a
/// live handset sends. As a fallback we also accept a bare msgpack integer
/// (×1e6 fixed-point) or a float (already degrees), to tolerate other encoders.
/// Integers are handled before `as_f64`, which would otherwise coerce them and
/// leave the fixed-point value unscaled.
fn coord(v: &rmpv::Value) -> Option<f64> {
    if let rmpv::Value::Binary(bytes) = v {
        return match bytes.len() {
            4 => Some(i64::from(i32::from_be_bytes(bytes[..4].try_into().ok()?)) as f64 / 1e6),
            8 => Some(i64::from_be_bytes(bytes[..8].try_into().ok()?) as f64 / 1e6),
            _ => None,
        };
    }
    if let Some(i) = v.as_i64() {
        Some(i as f64 / 1e6)
    } else if let Some(u) = v.as_u64() {
        Some(u as f64 / 1e6)
    } else {
        v.as_f64()
    }
}

/// Whether an inbound message is Sideband's "Request telemetry". The
/// `FIELD_COMMANDS` (0x09) value is a msgpack array of commands, each a map
/// `{ command_id: params }`; a telemetry request carries
/// [`COMMAND_TELEMETRY_REQUEST`] (its params are `[timebase, …]`, which we
/// ignore — we just answer with our current position).
pub(crate) fn is_requested(msg: &LxMessage) -> bool {
    msg.get_field(lxmf_core::constants::FIELD_COMMANDS)
        .is_some_and(|bytes| parse_request(bytes))
}

/// Parse a `FIELD_COMMANDS` payload and report whether any command is a telemetry
/// request. Malformed payloads decode to `false` (never a panic).
fn parse_request(bytes: &[u8]) -> bool {
    let Ok(value) = rmpv::decode::read_value(&mut &bytes[..]) else {
        return false;
    };
    let Some(commands) = value.as_array() else {
        return false;
    };
    commands.iter().any(|cmd| {
        cmd.as_map().is_some_and(|entries| {
            entries.iter().any(|(k, _)| {
                let id = k.as_u64().or_else(|| k.as_i64().map(|i| i as u64));
                id == Some(u64::from(COMMAND_TELEMETRY_REQUEST))
            })
        })
    })
}

/// Pack a position into Sideband's telemetry format (the inverse of
/// [`parse_location`]): a msgpack map `{ 0x01: <time>, 0x02: [lat, lon, alt,
/// speed, bearing, accuracy, ts] }` with each coordinate a 4-byte big-endian
/// signed int of `degrees ×1e6` wrapped in a msgpack binary.
pub(crate) fn pack_location(lat: f64, lon: f64, time: u64) -> Vec<u8> {
    let micro = |deg: f64| rmpv::Value::Binary(((deg * 1e6).round() as i32).to_be_bytes().to_vec());
    let zero4 = rmpv::Value::Binary(0i32.to_be_bytes().to_vec());
    let location = rmpv::Value::Array(vec![
        micro(lat),
        micro(lon),
        zero4.clone(),                         // altitude
        zero4.clone(),                         // speed
        zero4,                                 // bearing
        rmpv::Value::Binary(vec![0x00, 0x00]), // accuracy (2 bytes, as Sideband)
        rmpv::Value::from(time),               // last_update
    ]);
    let map = rmpv::Value::Map(vec![
        (rmpv::Value::from(0x01_u8), rmpv::Value::from(time)), // SID_TIME
        (rmpv::Value::from(SID_LOCATION), location),
    ]);
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &map).expect("encode telemetry");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxmf_core::constants::DeliveryMethod;

    /// Build a `FIELD_TELEMETRY` payload: a msgpack `{ sensor_id: value }` map
    /// with a single location sensor carrying `coords`.
    fn telemetry_blob(coords: Vec<rmpv::Value>) -> Vec<u8> {
        let map = rmpv::Value::Map(vec![(
            rmpv::Value::from(SID_LOCATION),
            rmpv::Value::Array(coords),
        )]);
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, &map).expect("encode");
        buf
    }

    #[test]
    fn packs_telemetry_that_round_trips_through_the_decoder() {
        // What we send back to a requester must decode to the same position.
        let blob = pack_location(48.5342, 3.8325, 1_781_467_583);
        let (lat, lon) = parse_location(&blob).expect("round-trips");
        assert!((lat - 48.5342).abs() < 1e-6, "lat was {lat}");
        assert!((lon - 3.8325).abs() < 1e-6, "lon was {lon}");

        // Negative (west/south) coordinates survive the i32 two's-complement pack.
        let blob = pack_location(-33.86, -70.65, 0);
        let (lat, lon) = parse_location(&blob).expect("round-trips");
        assert!((lat + 33.86).abs() < 1e-6);
        assert!((lon + 70.65).abs() < 1e-6);
    }

    /// A `FIELD_COMMANDS` payload carrying a single command with `command_id`.
    fn commands_blob(command_id: u8) -> Vec<u8> {
        let cmds = rmpv::Value::Array(vec![rmpv::Value::Map(vec![(
            rmpv::Value::from(command_id),
            rmpv::Value::Array(vec![
                rmpv::Value::from(0.0_f64),
                rmpv::Value::Boolean(false),
            ]),
        )])]);
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, &cmds).expect("encode");
        buf
    }

    #[test]
    fn detects_telemetry_request_command() {
        let mut msg = LxMessage::new([0u8; 16], [1u8; 16], "", "", DeliveryMethod::Direct);
        // A plain message is not a request.
        assert!(!is_requested(&msg));

        // The exact FIELD_COMMANDS payload captured from a live Sideband handset:
        // `[ { 0x01: [<float64 timebase>, false] } ]` — command 0x01 is the
        // telemetry request.
        let real = hex::decode("91810192cb41da89756a3cdf3bc2").expect("valid hex");
        assert!(parse_request(&real));
        msg.set_field(lxmf_core::constants::FIELD_COMMANDS, real);
        assert!(is_requested(&msg));

        // A different command (not 0x01) is not answered.
        assert!(parse_request(&commands_blob(0x01)));
        assert!(!parse_request(&commands_blob(0x02)));
        // Junk never panics.
        assert!(!parse_request(&[0xff, 0x00]));
    }

    #[test]
    fn parses_real_sideband_location_payload() {
        // Captured from a live Sideband handset (the FIELD_TELEMETRY value):
        // `{ 0x01: <time>, 0x04: nil, 0x02: [lat, lon, alt, speed, bearing,
        // accuracy, ts] }`, with lat/lon as 4-byte big-endian signed ints ×1e6
        // wrapped in msgpack binaries. lat 0x02e492b8 = 48.534200,
        // lon 0x003a7ab4 = 3.832500.
        let bytes = hex::decode(
            "8301ce6a2f09bf04c00297c40402e492b8c404003a7ab4c40400001e14\
             c40400000000c40400000000c4020001ce6a2f09b1",
        )
        .expect("valid hex");
        let (lat, lon) = parse_location(&bytes).expect("a fix");
        assert!((lat - 48.534200).abs() < 1e-6, "lat was {lat}");
        assert!((lon - 3.832500).abs() < 1e-6, "lon was {lon}");
    }

    #[test]
    fn parses_binary_packed_negative_coordinate() {
        // West longitudes are negative i32s in two's complement: -74.0 ×1e6.
        let micro = (-74_000_000_i32).to_be_bytes().to_vec();
        let blob = telemetry_blob(vec![
            rmpv::Value::Binary(40_000_000_i32.to_be_bytes().to_vec()),
            rmpv::Value::Binary(micro),
        ]);
        let (lat, lon) = parse_location(&blob).expect("a fix");
        assert!((lat - 40.0).abs() < 1e-6);
        assert!((lon + 74.0).abs() < 1e-6);
    }

    #[test]
    fn parses_float_encoded_location_telemetry() {
        let blob = telemetry_blob(vec![
            rmpv::Value::from(48.85_f64),
            rmpv::Value::from(2.35_f64),
            rmpv::Value::from(0.0_f64), // altitude, ignored
        ]);
        let (lat, lon) = parse_location(&blob).expect("a fix");
        assert!((lat - 48.85).abs() < 1e-9);
        assert!((lon - 2.35).abs() < 1e-9);
    }

    #[test]
    fn parses_scaled_integer_location_telemetry() {
        // Sideband's ×1e6 fixed-point encoding.
        let blob = telemetry_blob(vec![
            rmpv::Value::from(48_850_000_i64),
            rmpv::Value::from(-2_350_000_i64),
        ]);
        let (lat, lon) = parse_location(&blob).expect("a fix");
        assert!((lat - 48.85).abs() < 1e-6);
        assert!((lon + 2.35).abs() < 1e-6);
    }

    #[test]
    fn parses_unsigned_integer_location_telemetry() {
        // Positive fixed-point coordinates may be encoded as msgpack unsigned
        // ints; they must still be scaled by 1e6, not treated as raw degrees.
        let blob = telemetry_blob(vec![
            rmpv::Value::from(48_850_000_u64),
            rmpv::Value::from(2_350_000_u64),
        ]);
        let (lat, lon) = parse_location(&blob).expect("a fix");
        assert!((lat - 48.85).abs() < 1e-6);
        assert!((lon - 2.35).abs() < 1e-6);
    }

    #[test]
    fn rejects_non_location_and_implausible_telemetry() {
        // A non-location sensor only (battery 0x04) → no fix.
        let only_battery = {
            let map =
                rmpv::Value::Map(vec![(rmpv::Value::from(0x04_u8), rmpv::Value::from(95_u8))]);
            let mut buf = Vec::new();
            rmpv::encode::write_value(&mut buf, &map).expect("encode");
            buf
        };
        assert!(parse_location(&only_battery).is_none());

        // Out-of-range coordinates are rejected rather than clamped to noise.
        let bogus = telemetry_blob(vec![
            rmpv::Value::from(999.0_f64),
            rmpv::Value::from(0.0_f64),
        ]);
        assert!(parse_location(&bogus).is_none());

        // Not even msgpack → no panic, no fix.
        assert!(parse_location(&[0xff, 0x00, 0x13]).is_none());
    }
}
