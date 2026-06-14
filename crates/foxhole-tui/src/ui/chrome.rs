//! Program chrome: the top tab strip, the active-tool dispatch, and the bottom
//! status/metadata bar. None of these are focusable.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Pane, Tool};

use super::browser::render_browser;
use super::conversations::render_conversations;
use super::network::render_network;
use super::views::{render_guide, render_interfaces, render_log};
use super::widgets::pane_block;

/// Top menu strip. Each tool's title is shown left to right; the active one is
/// reversed. Separated by a plain ASCII pipe so it reads on a serial console.
pub(super) fn render_tab_strip(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, tool) in Tool::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" | "));
        }
        let label = format!(" {} ", tool.title());
        let style = if *tool == app.active {
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(label, style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Dispatch to the active tool's body renderer. Each tool owns its internal
/// layout, so adding a tool means adding one arm here plus its render fn.
pub(super) fn render_tool(frame: &mut Frame, app: &App, area: Rect) {
    match app.active {
        Tool::Conversations => render_conversations(frame, app, area),
        Tool::Network => render_network(frame, app, area),
        Tool::Browser => render_browser(frame, app, area),
        Tool::Log => render_log(frame, app, area),
        Tool::Interfaces => render_interfaces(frame, app, area),
        Tool::Guide => render_guide(frame, app, area),
    }
}

/// Single-line metadata strip plus the keybinding legend. Never focusable.
pub(super) fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let pane = match app.focus {
        Pane::PeerList => "PEERS",
        Pane::Thread => "THREAD",
        Pane::Transmit => "TRANSMIT",
    };
    // `net` reflects whether the protocol stack was compiled in.
    let net = if cfg!(feature = "net") { "ON" } else { "OFF" };

    // Short form of our own address (full one lives in the Network tab).
    let me = match &app.local_address {
        Some(a) if a.len() >= 8 => format!("  ME:{}\u{2026}", &a[..8]),
        Some(a) => format!("  ME:{a}"),
        None => String::new(),
    };
    let text = format!(
        "TOOL:{tool}  PANE:{pane}  NET:{net}  QUEUE:{queue}  PEERS:{peers}{me}  |  Ctrl+N/P:Tab  Ctrl+O:New  Tab:Pane  Up/Dn:Peer  Ctrl+S:Send  Ctrl+R:Sync  Ctrl+X:Purge  Ctrl+Q:Quit",
        tool = app.active.tag(),
        queue = app.outbound.len(),
        peers = app.conversations.len(),
    );

    let para = Paragraph::new(text).block(pane_block("STATUS", false));
    frame.render_widget(para, area);
}
