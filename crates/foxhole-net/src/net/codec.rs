//! Small, pure wire-format helpers shared across the networking layer.
//!
//! Everything here is free of transport state and side effects — parsing an
//! address, reading an LXMF custom field, encoding a Nomad Network form — so it
//! is unit-testable without bringing up a stack.

use lxmf_core::message::LxMessage;

/// Fallback port when a hub is given as a bare host with no `:port`.
pub(crate) const DEFAULT_HUB_PORT: u16 = 4242;

/// Decode a 32-char hex destination hash into 16 bytes.
pub(crate) fn parse_hash(s: &str) -> Result<[u8; 16], String> {
    let bytes = hex::decode(s).map_err(|_| format!("bad hash: {s}"))?;
    bytes
        .try_into()
        .map_err(|_| format!("hash must be 16 bytes: {s}"))
}

/// Split a `host:port` string, defaulting to [`DEFAULT_HUB_PORT`] if the port
/// is absent or non-numeric.
pub(crate) fn parse_hostport(s: &str) -> (String, u16) {
    match s.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(DEFAULT_HUB_PORT)),
        None => (s.to_string(), DEFAULT_HUB_PORT),
    }
}

/// Extract the CoT-XML payload from an inbound message's custom fields, if it is
/// tagged `cot/xml` (`FIELD_CUSTOM_TYPE` = `0xFB`) and carries data
/// (`FIELD_CUSTOM_DATA` = `0xFC`). The wire framing is the design note's §5.
pub(crate) fn cot_payload(msg: &LxMessage) -> Option<String> {
    let tag = msg.get_field(lxmf_core::constants::FIELD_CUSTOM_TYPE)?;
    if custom_field_text(tag) != foxhole_cot::CONTENT_TAG_XML {
        return None;
    }
    let data = msg.get_field(lxmf_core::constants::FIELD_CUSTOM_DATA)?;
    Some(custom_field_text(data))
}

/// Decode an LXMF custom-field value to text, tolerating both encodings the
/// stack can hand back: a msgpack `bin` arrives as raw bytes (so XML/`"cot/xml"`
/// is already UTF-8), while a msgpack `str` arrives re-serialized (so it must be
/// msgpack-decoded). Try the raw bytes first, then fall back to a msgpack decode.
pub(crate) fn custom_field_text(bytes: &[u8]) -> String {
    // CoT XML and the `cot/xml` tag are both plain UTF-8 when sent as a bin.
    let raw = String::from_utf8_lossy(bytes);
    if raw.trim_start().starts_with('<') {
        return raw.into_owned();
    }
    if let Ok(value) = rmpv::decode::read_value(&mut &bytes[..]) {
        if let Some(s) = value.as_str() {
            return s.to_string();
        }
        if let rmpv::Value::Binary(b) = value {
            return String::from_utf8_lossy(&b).into_owned();
        }
    }
    raw.into_owned()
}

/// Whether a decoded message carries any text the operator should see in the
/// thread. Telemetry-only / command-only messages have neither a title nor a
/// body, so they are not delivered as conversation entries (an empty `[RX]` line
/// conveys nothing).
pub(crate) fn has_text_body(msg: &LxMessage) -> bool {
    !msg.title.is_empty() || !msg.content.is_empty()
}

/// Encode a Nomad Network form submission as the msgpack map the node expects —
/// `{ field_<name>: value, var_<key>: value }`. Empty input → empty bytes (a
/// plain GET, matching `link.request(path, data=None)`).
pub(crate) fn encode_form(fields: &[(String, String)]) -> Vec<u8> {
    if fields.is_empty() {
        return Vec::new();
    }
    let map = rmpv::Value::Map(
        fields
            .iter()
            .map(|(k, v)| (rmpv::Value::from(k.as_str()), rmpv::Value::from(v.as_str())))
            .collect(),
    );
    let mut buf = Vec::new();
    let _ = rmpv::encode::write_value(&mut buf, &map);
    buf
}

/// Best-effort node name from a `nomadnetwork.node` announce's app data (UTF-8,
/// trimmed). Returns `None` when empty or unprintable. (Calibrate against real
/// announces if a node encodes its name differently.)
pub(crate) fn nomad_name_from_app_data(data: Option<&[u8]>) -> Option<String> {
    let bytes = data?;
    let s = String::from_utf8_lossy(bytes);
    let t = s.trim();
    if t.is_empty() || t.chars().any(|c| c.is_control()) {
        None
    } else {
        Some(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_form_round_trips_through_msgpack() {
        // Empty submission → no payload (a plain GET).
        assert!(encode_form(&[]).is_empty());

        let bytes = encode_form(&[
            ("field_q".to_string(), "hi".to_string()),
            ("var_p".to_string(), "2".to_string()),
        ]);
        let val = rmpv::decode::read_value(&mut &bytes[..]).expect("valid msgpack");
        let map = val.as_map().expect("a map");
        assert_eq!(map.len(), 2);
        assert_eq!(map[0].0.as_str(), Some("field_q"));
        assert_eq!(map[0].1.as_str(), Some("hi"));
        assert_eq!(map[1].0.as_str(), Some("var_p"));
        assert_eq!(map[1].1.as_str(), Some("2"));
    }

    #[test]
    fn parse_hash_accepts_16_bytes_only() {
        assert_eq!(
            parse_hash("00112233445566778899aabbccddeeff").unwrap(),
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
        assert!(parse_hash("abcd").is_err(), "too short");
        assert!(parse_hash("zz").is_err(), "not hex");
    }

    #[test]
    fn hostport_split() {
        assert_eq!(parse_hostport("host:1234"), ("host".to_string(), 1234));
        assert_eq!(
            parse_hostport("host"),
            ("host".to_string(), DEFAULT_HUB_PORT)
        );
        assert_eq!(
            parse_hostport("bad:port"),
            ("bad".to_string(), DEFAULT_HUB_PORT),
            "non-numeric port falls back to default"
        );
    }

    #[test]
    fn custom_field_text_reads_both_encodings() {
        // Raw UTF-8 XML (msgpack `bin` handed back as bytes).
        assert_eq!(custom_field_text(b"<event/>"), "<event/>");
        // A msgpack `str` must be decoded rather than read as bytes.
        let mut packed = Vec::new();
        rmpv::encode::write_value(&mut packed, &rmpv::Value::from("cot/xml")).unwrap();
        assert_eq!(custom_field_text(&packed), "cot/xml");
    }

    #[test]
    fn nomad_name_rejects_empty_and_control_bytes() {
        assert_eq!(
            nomad_name_from_app_data(Some(b"  relay-one  ")),
            Some("relay-one".to_string())
        );
        assert_eq!(nomad_name_from_app_data(None), None);
        assert_eq!(nomad_name_from_app_data(Some(b"   ")), None);
        assert_eq!(nomad_name_from_app_data(Some(b"a\x07b")), None);
    }
}
