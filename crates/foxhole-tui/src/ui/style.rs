//! Tactical palette and scrollback-line styling — the colour theme that tints
//! content (timestamps, RX/TX traffic, system-log categories) while structure
//! stays glyph-only. Pure helpers, shared by every tool body.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{Entry, MsgStatus};

/// Plain (untinted) scrollback lines for static panes (Network/Interfaces/Guide).
pub(super) fn plain_lines<I, S>(lines: I) -> Vec<Line<'static>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    lines.into_iter().map(|s| Line::raw(s.into())).collect()
}

/// One timestamped, tactically-tinted scrollback entry: muted `HH:MM:SS` UTC,
/// the body coloured by its category, and a delivery-status token for outbound
/// messages.
pub(super) fn styled_entry(e: &Entry) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("{}  ", fmt_time(e.at)), ts_style()),
        Span::styled(e.text.clone(), line_style(&e.text)),
    ];
    if let Some((label, cat)) = status_token(e.status) {
        spans.push(Span::styled(format!("  {label}"), tag_style(cat)));
    }
    Line::from(spans)
}

/// Inline label + palette category for an outbound message's status (`None` for
/// inbound/system lines, which carry no marker).
pub(super) fn status_token(status: MsgStatus) -> Option<(&'static str, &'static str)> {
    match status {
        MsgStatus::None => None,
        MsgStatus::Sending => Some(("[sending]", "OPS")),
        MsgStatus::Sent => Some(("[sent]", "OPS")),
        MsgStatus::Delivered => Some(("[delivered]", "DLV")),
        MsgStatus::Propagated => Some(("[propagated]", "CFG")),
        MsgStatus::Failed => Some(("[failed]", "ERR")),
    }
}

/// Muted grey-green for timestamps.
pub(super) fn ts_style() -> Style {
    Style::default().fg(Color::Rgb(102, 112, 102))
}

/// Tactical colour for a category tag (the `TACTICAL_STYLES` field theme).
pub(super) fn tag_style(tag: &str) -> Style {
    let base = Style::default();
    match tag {
        "RX" => base
            .fg(Color::Rgb(110, 143, 114))
            .add_modifier(Modifier::BOLD), // Field Green
        "TX" | "RT" | "LNK" => base
            .fg(Color::Rgb(79, 107, 88))
            .add_modifier(Modifier::BOLD), // Ranger Green
        "DLV" => base
            .fg(Color::Rgb(143, 166, 122))
            .add_modifier(Modifier::BOLD), // Success Olive
        "CFG" | "SYS" => base
            .fg(Color::Rgb(90, 111, 99))
            .add_modifier(Modifier::BOLD), // Slate-Olive
        "ID" => base
            .fg(Color::Rgb(140, 153, 114))
            .add_modifier(Modifier::BOLD), // Olive Drab
        "SEC" => base
            .fg(Color::Rgb(122, 90, 58))
            .add_modifier(Modifier::BOLD), // Weathered Brown
        "WRN" => base
            .fg(Color::Rgb(159, 139, 60))
            .add_modifier(Modifier::BOLD), // Faded Brass
        "ERR" => base
            .fg(Color::Rgb(122, 62, 62))
            .add_modifier(Modifier::BOLD), // Dark Dried Red
        "OPS" => base
            .fg(Color::Rgb(139, 143, 135))
            .add_modifier(Modifier::DIM), // Desaturated Grey
        _ => base.fg(Color::Rgb(90, 111, 99)),
    }
}

/// The leading `[TAG]` of a line, if any (`[RX] hi` -> `RX`).
pub(super) fn leading_tag(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(&rest[..end])
}

/// Style for a whole scrollback line. `[RX]`/`[TX]` colour by their tag; system
/// lines (all `[SYS]`) are sub-categorised by keyword so the Log reads tactically.
pub(super) fn line_style(text: &str) -> Style {
    match leading_tag(text) {
        Some("RX") => tag_style("RX"),
        Some("TX") => tag_style("TX"),
        // Explicit category tags (future) colour directly; everything else
        // (the `[SYS]` lines) is classified by content.
        Some("SYS") | None => tag_style(sys_category(text)),
        Some(other) => tag_style(other),
    }
}

/// Classify a system log line into a tactical category by keyword.
pub(super) fn sys_category(text: &str) -> &'static str {
    let t = text.to_ascii_lowercase();
    if t.contains("delivered") {
        "DLV"
    } else if t.contains("fail") || t.contains("error") || t.contains("net:") {
        "ERR"
    } else if t.contains("not decodable") || t.contains("too large") {
        "WRN"
    } else if t.contains("path") || t.contains("no key") {
        "RT"
    } else if t.contains("opening")
        || t.contains("depositing")
        || t.contains("sent")
        || t.contains("announce")
        || t.contains("sync")
        || t.contains("online")
        || t.contains("registered")
        || t.contains("transport")
        || t.contains("bringing")
    {
        "OPS"
    } else if t.contains("identified") || t.contains("identit") {
        "ID"
    } else if t.contains("link") {
        "LNK"
    } else if t.contains("config") || t.contains("node ") || t.contains("interfaces") {
        "CFG"
    } else {
        "SYS"
    }
}

/// Format a Unix-seconds timestamp as `HH:MM:SS` UTC, with no external deps.
/// `0` (unknown) renders as `--:--:--`.
pub(super) fn fmt_time(at: u64) -> String {
    if at == 0 {
        return "--:--:--".to_string();
    }
    let day = at % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}
