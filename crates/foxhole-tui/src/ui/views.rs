//! The read-only single-pane tool bodies: Log, Interfaces, and Guide.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, fmt_bitrate, fmt_bytes};

use super::style::{plain_lines, styled_entry, tag_style, ts_style};
use super::widgets::{render_scroll, render_scrollback};

/// Log tool: the system/application log scrollback, timestamped (UTC) and tinted.
pub(super) fn render_log(frame: &mut Frame, app: &App, area: Rect) {
    let lines = app.syslog.iter().map(styled_entry).collect();
    render_scroll(
        frame,
        "SYSTEM LOG (UTC)",
        lines,
        false,
        &app.log_scroll,
        area,
    );
}

/// Placeholder shown when the interface snapshot is empty — distinct for the
/// offline build (where the stack never reports) versus the live build (where
/// it just hasn't reported yet).
#[cfg(feature = "net")]
const NO_INTERFACES: &str = "(no interfaces reported yet)";
#[cfg(not(feature = "net"))]
const NO_INTERFACES: &str = "(networking offline — rebuild with --features net)";

/// Interfaces tool: live Reticulum interface status, rnstatus-style — one
/// compact row per interface (name, up/down, bitrate, RX/TX), with a footer
/// summarising the interface and active-link counts. Fed by
/// [`crate::app::App::set_interfaces`] from the transport's interface-stats RPC.
pub(super) fn render_interfaces(frame: &mut Frame, app: &App, area: Rect) {
    if app.interfaces.is_empty() {
        render_scrollback(
            frame,
            "INTERFACES",
            plain_lines([NO_INTERFACES]),
            false,
            area,
        );
        return;
    }

    // Header, then one row per interface, then a blank line + summary footer.
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(app.interfaces.len() + 3);
    lines.push(Line::styled(
        format!(
            "{:<16} {:<4} {:>10} {:>10} {:>10}",
            "IFACE", "ST", "RATE", "RX", "TX"
        ),
        ts_style(),
    ));
    for iface in &app.interfaces {
        lines.push(iface_row(iface));
    }
    lines.push(Line::raw(""));
    let n = app.interfaces.len();
    let links = app.link_count;
    lines.push(Line::styled(
        format!(
            "{n} interface{}  *  {links} link{}",
            if n == 1 { "" } else { "s" },
            if links == 1 { "" } else { "s" },
        ),
        ts_style(),
    ));

    render_scrollback(frame, "INTERFACES", lines, false, area);
}

/// One interface row: `name  ST  RATE  RX  TX`. The status cell is tinted green
/// when up, red when down; the rest stays in the muted scrollback palette.
fn iface_row(iface: &crate::app::Interface) -> Line<'static> {
    let (st, st_style) = if iface.online {
        ("UP", tag_style("DLV").add_modifier(Modifier::BOLD))
    } else {
        ("DOWN", tag_style("ERR"))
    };
    Line::from(vec![
        Span::raw(format!("{:<16} ", trunc(&iface.name, 16))),
        Span::styled(format!("{st:<4} "), st_style),
        Span::styled(
            format!(
                "{:>10} {:>10} {:>10}",
                fmt_bitrate(iface.bitrate),
                fmt_bytes(iface.rx_bytes),
                fmt_bytes(iface.tx_bytes),
            ),
            Style::default(),
        ),
    ])
}

/// Truncate to `max` columns with an ellipsis when it would overflow.
fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        format!("{}\u{2026}", s.chars().take(keep).collect::<String>())
    }
}

/// The operator manual, authored as micron markup and embedded at build time.
/// Rendered through the same `micron::render` path the Browser uses, so headings,
/// dividers, and inline emphasis all come for free and re-flow to the pane width.
const GUIDE_SRC: &str = include_str!("guide.mu");

/// Guide tool: the built-in operator manual. The content lives in `guide.mu`
/// (micron markup); here we just render it to the pane width — `None` focus and
/// empty field values since it is a static, non-interactive page. Scrolled with
/// PgUp/PgDn/Home/End via `app.guide_scroll`.
pub(super) fn render_guide(frame: &mut Frame, app: &App, area: Rect) {
    // Match the pane's inner width (minus borders) so heading bars/dividers fill
    // it — exactly as the Browser renders a fetched page.
    let lines = crate::micron::render(
        GUIDE_SRC,
        area.width.saturating_sub(2),
        None,
        &HashMap::new(),
    );
    render_scroll(frame, "GUIDE", lines, false, &app.guide_scroll, area);
}
