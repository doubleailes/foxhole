//! The read-only single-pane tool bodies: Log, Interfaces, and Guide.

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::App;

use super::style::{plain_lines, styled_entry};
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

/// Interfaces tool: Reticulum interface status. Stub until the stack reports
/// live interface state.
pub(super) fn render_interfaces(frame: &mut Frame, _app: &App, area: Rect) {
    render_scrollback(
        frame,
        "INTERFACES",
        plain_lines(["(no interfaces configured)"]),
        false,
        area,
    );
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
