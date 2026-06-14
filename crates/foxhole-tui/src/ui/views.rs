//! The read-only single-pane tool bodies: Log, Interfaces, and Guide.

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

/// Guide tool: static help text.
pub(super) fn render_guide(frame: &mut Frame, app: &App, area: Rect) {
    let lines = [
        "FoxHole — off-grid LXMF comms terminal".to_string(),
        String::new(),
        "Tabs (Ctrl+N / Ctrl+P to switch):".to_string(),
        "  Conversations  Send and receive LXMF messages.".to_string(),
        "  Network        Discovered peers and propagation nodes.".to_string(),
        "  Browser        Read Nomad Network pages (micron).".to_string(),
        "  Log            System and diagnostic messages.".to_string(),
        "  Interfaces     Reticulum interface status.".to_string(),
        "  Guide          This help.".to_string(),
        String::new(),
        "Keys:".to_string(),
        "  Ctrl+N / Ctrl+P  Next / previous tab".to_string(),
        "  Ctrl+O           New conversation by LXMF address".to_string(),
        "  Tab              Cycle panes (Peers / Thread / Transmit; Browser cols)".to_string(),
        "  Up / Down        Select peer / node / page element".to_string(),
        "  PgUp / PgDn      Scroll the focused text pane".to_string(),
        "  Home / End       Jump to top / bottom".to_string(),
        "  Enter            Send / open node / follow link / submit form".to_string(),
        "  (type)           Browser: edit a focused page input field".to_string(),
        "  Backspace        Browser: back, or delete in a field".to_string(),
        "  Ctrl+S           Send to selected peer".to_string(),
        "  Ctrl+R           Sync now from propagation node (on demand)".to_string(),
        "  Ctrl+X           Purge compose buffer".to_string(),
        "  Ctrl+K           BURN — destroy all session data (confirm: BURN)".to_string(),
        "  Ctrl+Q           Quit".to_string(),
    ];
    render_scroll(
        frame,
        "GUIDE",
        plain_lines(lines),
        false,
        &app.guide_scroll,
        area,
    );
}
