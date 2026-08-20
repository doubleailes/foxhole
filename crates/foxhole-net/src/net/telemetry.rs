//! Sideband-compatible location telemetry: decode an inbound fix (single or
//! streamed), detect a telemetry request, pack our own position for the reply,
//! and pack a request of our own.
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

/// Length of an LXMF destination hash — the width of a stream entry's source
/// field. Skipping binaries of exactly this width when hunting for the entry's
/// packed fix costs nothing: a packed location map spends more than 16 bytes on
/// its two 4-byte coordinate binaries alone, so it can never be this short.
const DESTINATION_LENGTH: usize = 16;

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

/// Extract every `(source, lat, lon)` fix from a message's *streamed* telemetry
/// field, if present.
///
/// Sideband answers a telemetry request from a collector-enabled handset with
/// `FIELD_TELEMETRY_STREAM` rather than `FIELD_TELEMETRY`
/// (`core.py::create_telemetry_collector_response`), so a terminal that only
/// reads the single-fix field sees nothing at all from such a peer. The stream
/// relays *other* objects' telemetry too, hence the per-entry source hash.
pub(crate) fn stream(msg: &LxMessage) -> Vec<(Option<[u8; 16]>, f64, f64)> {
    match msg.get_field(lxmf_core::constants::FIELD_TELEMETRY_STREAM) {
        Some(bytes) => parse_stream(bytes),
        None => Vec::new(),
    }
}

/// Pure decode half of [`stream`]. The `FIELD_TELEMETRY_STREAM` value is a
/// msgpack **array** of entries, each `[source_hash, timestamp, packed_telemetry,
/// appearance]` — the shape Sideband builds in
/// `create_telemetry_collector_response`. `packed_telemetry` is the very payload
/// [`parse_location`] already reads, so each entry is decoded with it and
/// attributed to its own `source_hash` (16 bytes) rather than to the peer that
/// relayed it.
///
/// Deliberately lenient: entries that carry no source, no usable fix, or an
/// older two-element `[timestamp, packed]` shape neither abort the batch nor
/// panic — every entry that *does* decode is returned.
pub(crate) fn parse_stream(bytes: &[u8]) -> Vec<(Option<[u8; 16]>, f64, f64)> {
    let Ok(value) = rmpv::decode::read_value(&mut &bytes[..]) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let fields = entry.as_array()?;
            let source = fields.iter().find_map(entry_source);
            // The packed blob is whichever element decodes as a location fix —
            // robust against the extra `appearance` element and the older
            // source-less entry shape alike. See [`DESTINATION_LENGTH`] for why
            // skipping a source-width binary can never skip the fix.
            let (lat, lon) = fields.iter().find_map(|f| match f {
                rmpv::Value::Binary(b) if b.len() != DESTINATION_LENGTH => parse_location(b),
                _ => None,
            })?;
            Some((source, lat, lon))
        })
        .collect()
}

/// A stream entry's originating destination hash: a msgpack binary of exactly
/// [`DESTINATION_LENGTH`] bytes.
fn entry_source(field: &rmpv::Value) -> Option<[u8; 16]> {
    match field {
        rmpv::Value::Binary(b) => b.as_slice().try_into().ok(),
        _ => None,
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

/// Pack a "Request telemetry" command for a peer — the mirror of
/// [`is_requested`], so a Sideband handset answers us the same way we answer it.
///
/// `FIELD_COMMANDS` is a msgpack array of `{ command_id: params }` maps; the
/// telemetry request's params are `[timebase, collector_request]`. `timebase`
/// bounds how far back a collector may reach; `collector_request` is left
/// `false` on purpose — a collector-enabled peer would otherwise dump telemetry
/// for every object it knows, where all we asked for is the peer's own position
/// (`core.py::handle_commands`).
pub(crate) fn pack_request(timebase: f64) -> Vec<u8> {
    let cmd = rmpv::Value::Map(vec![(
        rmpv::Value::from(COMMAND_TELEMETRY_REQUEST),
        rmpv::Value::Array(vec![
            rmpv::Value::from(timebase),
            rmpv::Value::Boolean(false),
        ]),
    )]);
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &rmpv::Value::Array(vec![cmd]))
        .expect("encode telemetry request");
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

    /// One `FIELD_TELEMETRY_STREAM` entry as Sideband builds it in
    /// `create_telemetry_collector_response`: `[source, timestamp, packed, appearance]`.
    fn stream_entry(source: [u8; 16], packed: Vec<u8>) -> rmpv::Value {
        rmpv::Value::Array(vec![
            rmpv::Value::Binary(source.to_vec()),
            rmpv::Value::from(1_781_467_583_u64),
            rmpv::Value::Binary(packed),
            // The appearance element: `[icon, fg, bg]`, which must not be
            // mistaken for a packed fix.
            rmpv::Value::Array(vec![
                rmpv::Value::from("account"),
                rmpv::Value::Binary(vec![0x00, 0x7f, 0xff]),
                rmpv::Value::Binary(vec![0x11, 0x22, 0x33]),
            ]),
        ])
    }

    fn stream_blob(entries: Vec<rmpv::Value>) -> Vec<u8> {
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, &rmpv::Value::Array(entries)).expect("encode");
        buf
    }

    #[test]
    fn parses_streamed_telemetry_attributed_to_each_entrys_source() {
        // A collector-enabled Sideband answers a telemetry request with a stream
        // rather than a single fix, relaying other objects' positions too — each
        // must be attributed to its own source hash, not to the relaying peer.
        let own = [0xAAu8; 16];
        let relayed = [0xBBu8; 16];
        let blob = stream_blob(vec![
            stream_entry(own, pack_location(48.5342, 3.8325, 1_781_467_583)),
            stream_entry(relayed, pack_location(-33.86, -70.65, 1_781_467_500)),
        ]);

        let fixes = parse_stream(&blob);
        assert_eq!(fixes.len(), 2, "both entries decode");
        assert_eq!(fixes[0].0, Some(own));
        assert!((fixes[0].1 - 48.5342).abs() < 1e-6);
        assert!((fixes[0].2 - 3.8325).abs() < 1e-6);
        assert_eq!(fixes[1].0, Some(relayed));
        assert!((fixes[1].1 + 33.86).abs() < 1e-6);
        assert!((fixes[1].2 + 70.65).abs() < 1e-6);

        // And it is reachable through the field, which lxmf-core hands back
        // msgpack-re-encoded because the value is an array, not a `bin`.
        let mut msg = LxMessage::new([0u8; 16], own, "", "", DeliveryMethod::Direct);
        assert!(stream(&msg).is_empty(), "no stream field, no fixes");
        msg.set_field(lxmf_core::constants::FIELD_TELEMETRY_STREAM, blob);
        assert_eq!(stream(&msg).len(), 2);
    }

    #[test]
    fn stream_tolerates_partial_and_malformed_entries() {
        let good = [0xCCu8; 16];
        let blob = stream_blob(vec![
            // Source-less two-element entry (the older shape).
            rmpv::Value::Array(vec![
                rmpv::Value::from(1_781_467_583_u64),
                rmpv::Value::Binary(pack_location(51.5, -0.12, 0)),
            ]),
            // An entry whose telemetry carries no location sensor at all.
            stream_entry(good, vec![0x81, 0x04, 0x5f]),
            // Not an array.
            rmpv::Value::from(7_u8),
            stream_entry(good, pack_location(40.0, -74.0, 0)),
        ]);

        let fixes = parse_stream(&blob);
        assert_eq!(fixes.len(), 2, "only the decodable entries survive");
        assert_eq!(fixes[0].0, None, "a source-less entry still yields its fix");
        assert!((fixes[0].1 - 51.5).abs() < 1e-6);
        assert_eq!(fixes[1].0, Some(good));
        assert!((fixes[1].2 + 74.0).abs() < 1e-6);

        // Junk never panics and never invents a fix.
        assert!(parse_stream(&[0xff, 0x00, 0x13]).is_empty());
        assert!(parse_stream(&[]).is_empty());
    }

    #[test]
    fn packs_a_request_the_detector_recognises() {
        // Our request must have the shape we already detect inbound — and the
        // shape a live handset sent us (`91 81 01 92 cb… c2`): a one-element
        // array holding `{ 0x01: [<float64 timebase>, false] }`.
        let blob = pack_request(1_781_424_383.0);
        assert!(parse_request(&blob), "round-trips through the detector");
        assert_eq!(blob[0], 0x91, "array(1)");
        assert_eq!(blob[1], 0x81, "map(1)");
        assert_eq!(blob[2], 0x01, "command id 0x01");
        assert_eq!(blob[3], 0x92, "params array(2)");
        assert_eq!(blob[4], 0xcb, "float64 timebase");
        assert_eq!(
            *blob.last().expect("non-empty"),
            0xc2,
            "collector_request = false, so a collector answers with its own \
             position rather than dumping every object it knows"
        );
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
