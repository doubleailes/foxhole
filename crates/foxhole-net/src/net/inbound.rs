//! Inbound delivery: turning a decoded [`LxMessage`] into the [`NetEvent`]s the
//! UI folds into state (a thread entry, a map fix, an intel event) plus the log
//! lines that make inbound traffic observable.

use tokio::sync::mpsc;

use lxmf_core::message::LxMessage;

use foxhole_core::app::NetEvent;

use super::codec::{cot_payload, has_text_body};
use super::telemetry;

/// Deliver a decoded inbound payload to the right thread and log the
/// path/size/outcome so inbound traffic is observable (a payload that fails to
/// decode is otherwise dropped silently — invisible when debugging).
pub(crate) async fn deliver(
    events: &mpsc::Sender<NetEvent>,
    label: &str,
    raw_len: usize,
    decoded: Option<LxMessage>,
) {
    match decoded {
        Some(msg) => {
            // A message with no text is telemetry/command only — note that in the
            // log rather than claiming it went to a thread it never reaches.
            let dest = if has_text_body(&msg) {
                "-> thread"
            } else {
                "(no text)"
            };
            let _ = events
                .send(NetEvent::Sys(format!(
                    "[SYS] {label} message from {} {dest} ({raw_len} B)",
                    hex::encode(msg.source_hash)
                )))
                .await;
            emit(events, msg).await;
        }
        None => {
            let _ = events
                .send(NetEvent::Sys(format!(
                    "[SYS] {label} data not decodable as LXMF ({raw_len} B)"
                )))
                .await;
        }
    }
}

/// Forward a decoded inbound message to the UI, plus any location telemetry it
/// carries (so the World Map can plot the sender) — a single fix, a relayed
/// stream of them, or both. A telemetry-only / command-only message has no text,
/// so no conversation entry is emitted for it — only its telemetry (if any) is
/// surfaced.
async fn emit(events: &mpsc::Sender<NetEvent>, msg: LxMessage) {
    debug_dump_fields(events, &msg).await;
    let source = hex::encode(msg.source_hash);
    let location = telemetry::location(&msg);
    let stream = telemetry::stream(&msg);
    let cot = cot_payload(&msg);
    if has_text_body(&msg) {
        let _ = events
            .send(NetEvent::Message {
                source: source.clone(),
                title: msg.title,
                content: msg.content,
            })
            .await;
    }
    if let Some((lat, lon)) = location {
        let _ = events
            .send(NetEvent::Telemetry {
                source: source.clone(),
                lat,
                lon,
            })
            .await;
    }
    // A collector-enabled Sideband answers a telemetry request with a *stream*
    // instead of a single fix, relaying other objects' positions alongside its
    // own — so each entry is attributed to its own source, falling back to the
    // sender when an entry carries none.
    for (entry_source, lat, lon) in stream {
        let _ = events
            .send(NetEvent::Telemetry {
                source: entry_source.map_or_else(|| source.clone(), hex::encode),
                lat,
                lon,
            })
            .await;
    }
    // CoT intel (markers / hazard zones) carried in the LXMF custom field. Parsed
    // here so the UI only ever sees a validated event; a malformed/oversized/XXE
    // payload is logged and dropped (never fatal) — see docs/intel-sharing.md §9.
    if let Some(xml) = cot {
        match foxhole_cot::parse(&xml) {
            Ok(event) => {
                let _ = events.send(NetEvent::Cot { source, event }).await;
            }
            Err(e) => {
                let _ = events
                    .send(NetEvent::Sys(format!(
                        "[SYS] intel: dropped malformed CoT ({e:?})"
                    )))
                    .await;
            }
        }
    }
}

/// Opt-in diagnostic (set `FOXHOLE_DEBUG_TELEMETRY`): log the inbound message's
/// LXMF field ids and the raw hex of any telemetry field(s), so the exact
/// Sideband payload layout can be inspected when a fix fails to decode. Off by
/// default — it would otherwise dump field bytes into the Log for every message.
async fn debug_dump_fields(events: &mpsc::Sender<NetEvent>, msg: &LxMessage) {
    if std::env::var_os("FOXHOLE_DEBUG_TELEMETRY").is_none() {
        return;
    }
    let ids: Vec<String> = msg.fields.keys().map(|k| format!("{k:#04x}")).collect();
    let _ = events
        .send(NetEvent::Sys(format!(
            "[SYS] msg fields: [{}]",
            ids.join(", ")
        )))
        .await;
    for fid in [
        lxmf_core::constants::FIELD_TELEMETRY,
        lxmf_core::constants::FIELD_TELEMETRY_STREAM,
        lxmf_core::constants::FIELD_COMMANDS,
    ] {
        if let Some(bytes) = msg.get_field(fid) {
            let _ = events
                .send(NetEvent::Sys(format!(
                    "[SYS] field {fid:#04x} raw: {}",
                    hex::encode(bytes)
                )))
                .await;
        }
    }
}
