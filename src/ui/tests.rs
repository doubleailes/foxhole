//! Unit tests for the pure rendering helpers (styling, wrapping, layout math).

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
