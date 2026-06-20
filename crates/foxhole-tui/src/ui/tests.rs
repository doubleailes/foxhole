//! Unit tests for the pure rendering helpers (styling, wrapping, layout math).

use super::network::signal_meter;
use super::style::{fmt_time, leading_tag, line_style, status_token, sys_category, tag_style};
use super::widgets::{centered_rect, wrapped_height};
use crate::app::MsgStatus;
use ratatui::layout::Rect;
use ratatui::text::Line;

#[test]
fn wrapped_height_counts_wrapped_rows() {
    let lines = [Line::raw("ab"), Line::raw("abcdef"), Line::raw("")];
    // width 3: "ab"→1, "abcdef"→2, ""→1  = 4 visual rows.
    assert_eq!(wrapped_height(&lines, 3), 4);
    // width 0 falls back to the raw line count.
    assert_eq!(wrapped_height(&lines, 0), 3);
}

#[test]
fn status_tokens_map_to_palette() {
    assert!(status_token(MsgStatus::None).is_none());
    assert_eq!(status_token(MsgStatus::Sending), Some(("[sending]", "OPS")));
    assert_eq!(
        status_token(MsgStatus::Delivered),
        Some(("[delivered]", "DLV"))
    );
    assert_eq!(
        status_token(MsgStatus::Propagated),
        Some(("[propagated]", "CFG"))
    );
    assert_eq!(status_token(MsgStatus::Failed), Some(("[failed]", "ERR")));
}

#[test]
fn leading_tag_extracts_bracket() {
    assert_eq!(leading_tag("[RX] hi"), Some("RX"));
    assert_eq!(leading_tag("  [SYS] x"), Some("SYS"));
    assert_eq!(leading_tag("no tag"), None);
}

#[test]
fn rx_tx_colour_by_tag() {
    assert_eq!(line_style("[RX] hello"), tag_style("RX"));
    assert_eq!(line_style("[TX] hello"), tag_style("TX"));
}

#[test]
fn system_lines_classified_by_keyword() {
    assert_eq!(sys_category("[SYS] delivered (direct)"), "DLV");
    assert_eq!(sys_category("[SYS] delivery to X failed (timeout)"), "ERR");
    assert_eq!(
        sys_category("[SYS] direct data not decodable as LXMF"),
        "WRN"
    );
    assert_eq!(
        sys_category("[SYS] no key for X yet — requesting path"),
        "RT"
    );
    assert_eq!(sys_category("[SYS] sent to X"), "OPS");
    assert_eq!(
        sys_category("[SYS] peer X identified on inbound link"),
        "ID"
    );
    assert_eq!(sys_category("[SYS] inbound link established"), "LNK");
    assert_eq!(sys_category("[SYS] using existing RNS config"), "CFG");
    assert_eq!(sys_category("[SYS] something else entirely"), "SYS");
    // `[SYS]` lines route through sys_category.
    assert_eq!(line_style("[SYS] delivered (direct)"), tag_style("DLV"));
}

#[test]
fn formats_utc_hms() {
    assert_eq!(fmt_time(0), "--:--:--", "unknown time");
    assert_eq!(fmt_time(3661), "01:01:01");
    assert_eq!(
        fmt_time(86_400 + 3661),
        "01:01:01",
        "time-of-day wraps daily"
    );
    assert_eq!(fmt_time(1_700_000_000), "22:13:20", "known UTC instant");
}

#[test]
fn signal_meter_strength_falls_off_with_hops() {
    // Nearer peers read stronger; each cell is one of `▰` (lit) / `▱` (dim).
    assert_eq!(signal_meter(Some(0)), "▰▰▰▰");
    assert_eq!(signal_meter(Some(1)), "▰▰▰▰");
    assert_eq!(signal_meter(Some(2)), "▰▰▰▱");
    assert_eq!(signal_meter(Some(4)), "▰▱▱▱");
    // Distant (>=5 hops) and known-but-pathless (`None`) both read empty.
    assert_eq!(signal_meter(Some(9)), "▱▱▱▱");
    assert_eq!(signal_meter(None), "▱▱▱▱");
    // Every meter is exactly four cells wide so rows stay aligned.
    for h in [None, Some(0), Some(3), Some(50)] {
        assert_eq!(signal_meter(h).chars().count(), 4);
    }
}

#[test]
fn centered_rect_centers_and_clamps() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let r = centered_rect(48, 4, area);
    assert_eq!((r.x, r.y, r.width, r.height), (16, 10, 48, 4));

    // Clamp to a tiny area without overflow.
    let tiny = Rect {
        x: 0,
        y: 0,
        width: 10,
        height: 2,
    };
    let r2 = centered_rect(48, 4, tiny);
    assert_eq!((r2.width, r2.height), (10, 2));
}

/// An `App` parked on the World Map tab with the operator fixed at Paris and one
/// peer (London) carrying telemetry — the shape the map renders.
fn map_app() -> crate::app::App {
    use crate::app::{AppState, GeoPos, Tool};
    let mut app = crate::app::App::new();
    // Force the console (the splash owns the frame otherwise; under workspace
    // feature unification core's `cfg!(test)` is false here, so it boots Splash).
    app.state = AppState::Running;
    app.active = Tool::WorldMap;
    app.config.display_name = "base".to_string();
    app.config.lat = Some(48.85);
    app.config.lon = Some(2.35);
    app.conversations[0].display_name = Some("london".to_string());
    app.conversations[0].location = Some(GeoPos::new(51.5, -0.12));
    app
}

#[test]
fn world_map_renders_world_markers_and_roster() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let app = map_app();
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    let text = term.backend().to_string();

    // The tab strip carries the new tool, the canvas + roster panels are titled,
    // both marker labels are plotted, and the key legend is present.
    assert!(text.contains("Map"), "tab strip lists the Map tool");
    assert!(text.contains("WORLD MAP"), "canvas panel title");
    assert!(text.contains("POSITIONS"), "roster panel title");
    assert!(text.contains("base"), "operator marker label");
    assert!(text.contains("london"), "peer marker label");
    // The seeded demo hazard zones overlay in the generalized INTEL panel:
    // panel title + an AO callsign (local zones are tagged LOCAL).
    assert!(text.contains("INTEL"), "intel panel title");
    assert!(text.contains("AO ALPHA"), "demo hazard zone listed");
    // The braille world outline drew at least some land cells.
    assert!(
        text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
        "braille map cells were drawn"
    );
}

#[test]
fn world_map_renders_received_intel_and_review_modal() {
    use crate::app::{Affiliation, CotEvent, IntelReview, Trust};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = map_app();
    // A trusted peer's hostile zone is applied (live); an unknown peer's marker is
    // staged for review. Far-future stale so the sweep/render keeps them.
    let stale = super_now() + 36_000;
    app.conversations[0].peer = "aa11bb22cc33".to_string();
    app.conversations[0].trust = Trust::Trusted;
    app.apply_cot(
        "aa11bb22cc33".to_string(),
        CotEvent::zone("z1", "AO INTEL", 50.0, 10.0, 200_000.0, super_now(), stale),
    );
    app.conversations
        .push(crate::app::Conversation::new("dead00beef11")); // Unknown
    app.apply_cot(
        "dead00beef11".to_string(),
        CotEvent::marker(
            "m1",
            Affiliation::Friendly,
            "SCOUT",
            49.0,
            9.0,
            super_now(),
            stale,
        ),
    );

    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    let text = term.backend().to_string();
    // The applied zone shows in the INTEL panel; the staged marker is flagged.
    assert!(
        text.contains("AO INTEL"),
        "received intel listed in INTEL panel"
    );
    assert!(text.contains("staged"), "staged-intel review hint shown");

    // Opening the review modal lists the staged event with accept/discard keys.
    app.intel_review = Some(IntelReview { selected: 0 });
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    let text = term.backend().to_string();
    assert!(text.contains("INCOMING INTEL"), "review modal title");
    assert!(text.contains("SCOUT"), "staged event shown in the modal");
    assert!(text.contains("accept"), "accept/discard legend shown");
}

#[test]
fn share_zone_modal_lists_local_zones_for_the_peer() {
    use crate::app::{ShareZone, Tool};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = map_app();
    app.active = Tool::Conversations;
    app.conversations[0].display_name = Some("kilo".to_string());
    app.share_zone = Some(ShareZone {
        selected: 0,
        peer: app.conversations[0].peer.clone(),
        peer_label: "kilo".to_string(),
    });

    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    let text = term.backend().to_string();
    assert!(text.contains("SHARE INTEL"), "share modal title");
    assert!(text.contains("kilo"), "recipient named in the header");
    assert!(text.contains("AO ALPHA"), "a local zone is listed to share");
    assert!(text.contains("share"), "share/cancel legend shown");
}

#[test]
fn author_form_renders_fields_and_toggles() {
    use crate::app::{Affiliation, AuthorField, AuthorForm, AuthorKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = map_app();
    app.author = Some(AuthorForm {
        kind: AuthorKind::Zone,
        affiliation: Affiliation::Hostile,
        callsign: "AO ZULU".to_string(),
        lat: "50.0".to_string(),
        lon: "30.0".to_string(),
        mgrs: "36UUA0000000000".to_string(),
        radius_km: "200".to_string(),
        remarks: String::new(),
        field: AuthorField::Kind,
        edit_key: None,
        error: None,
    });

    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    let text = term.backend().to_string();
    assert!(text.contains("AUTHOR INTEL"), "author modal title");
    assert!(text.contains("Kind"), "kind field");
    assert!(text.contains("Zone"), "kind toggle shows Zone");
    assert!(text.contains("Affil"), "affiliation field");
    assert!(text.contains("Radius km"), "zone shows the radius field");
    assert!(text.contains("AO ZULU"), "callsign value shown");
    assert!(text.contains("commit"), "commit/cancel legend");
}

/// Wall-clock seconds — the test builds events relative to now so the live filter
/// and stale sweep keep them.
fn super_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[test]
fn world_map_draws_cities_and_g_hides_them() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    // Zoom in past the globe view (which is dots-only) so the megacity anchors
    // label. The default viewport is centred on the origin, where Lagos is an
    // in-view anchor clear of the operator/peer markers.
    let mut app = map_app();
    app.map.span = 120.0;
    let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    assert!(
        term.backend().to_string().contains("Lagos"),
        "a megacity anchor is labelled once zoomed in"
    );

    // Toggling the layer off (g) removes them.
    app.map_cities = false;
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    assert!(
        !term.backend().to_string().contains("Lagos"),
        "the cities layer is hidden when toggled off"
    );
}

#[test]
fn world_map_empty_without_a_fix() {
    use crate::app::{App, AppState, Tool};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = App::new();
    app.state = AppState::Running;
    app.active = Tool::WorldMap;
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    let text = term.backend().to_string();
    // No operator fix and no peer telemetry → the roster shows the hint.
    assert!(text.contains("no positions yet"));
}

/// Visual aid: dump the rendered World Map to stdout. Ignored by default; run
/// with `cargo test dump_world_map -- --ignored --nocapture` to eyeball it.
#[test]
#[ignore]
fn dump_world_map() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let app = map_app();
    let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
    term.draw(|f| crate::ui::render(f, &app)).unwrap();
    println!("{}", term.backend());
}
