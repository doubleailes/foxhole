//! End-to-end check that the opt-in trace sink captures a real `tracing` event
//! shaped like the delivery-proof diagnostics we need to see.
//!
//! Its own integration binary on purpose: `trace::install` sets the *global*
//! dispatcher, which a process may do only once.

#[test]
fn captures_stack_events_to_the_trace_file() {
    let dir = std::env::temp_dir().join(format!("foxhole-trace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // Narrow to a target substring, so the filter is exercised rather than
    // bypassed by the catch-all.
    unsafe {
        std::env::set_var("FOXHOLE_TRACE", "trace_sink");
        std::env::set_var("FOXHOLE_TRACE_LEVEL", "debug");
    }
    let path = foxhole_net::trace::install(&dir).expect("sink installs");

    // The exact shape of rns-runtime's link-packet proof event.
    tracing::info!(
        link_id = "aabb",
        proof_len = 96,
        "delivery proof queued for link data packet (unencrypted)"
    );
    tracing::warn!(link_id = "aabb", "could not sign delivery proof");
    // Below the level cap → must not be recorded.
    tracing::trace!("noise that the level cap drops");

    let log = std::fs::read_to_string(&path).expect("trace file");
    assert!(
        log.contains("delivery proof queued for link data packet"),
        "the proof event must be captured, got:\n{log}"
    );
    assert!(log.contains("proof_len=96"), "fields render: {log}");
    assert!(log.contains("could not sign delivery proof"), "warn: {log}");
    assert!(
        !log.contains("noise that the level cap drops"),
        "FOXHOLE_TRACE_LEVEL must cap verbosity: {log}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
