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
use super::notes::render_notes;
use super::style::{tag_style, ts_style};
use super::views::{render_guide, render_interfaces, render_log};
use super::widgets::pane_block;

/// Top menu strip, styled as a HUD mode-selector: a leading accent pip, then each
/// tool's title left to right with the active one boxed (reversed + bold) and the
/// rest dimmed, divided by thin tactical rules. Unicode box-drawing throughout.
pub(super) fn render_tab_strip(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = vec![Span::styled(
        "▌ ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    for (i, tool) in Tool::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", ts_style()));
        }
        if *tool == app.active {
            spans.push(Span::styled(
                format!(" {} ", tool.title()),
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::REVERSED),
            ));
        } else {
            spans.push(Span::styled(
                tool.title().to_string(),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
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
        Tool::Notes => render_notes(frame, app, area),
        Tool::Guide => render_guide(frame, app, area),
    }
}

/// Bottom status readout: a segmented tactical strip (tool / pane / net LED /
/// queue / peers / own address) divided by thin rules, followed by the muted
/// keybinding legend. Never focusable.
pub(super) fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let pane = match app.focus {
        Pane::PeerList => "PEERS",
        Pane::Thread => "THREAD",
        Pane::Transmit => "TRANSMIT",
    };

    // A thin divider between readout segments.
    let div = || Span::styled(" │ ", ts_style());

    let mut spans: Vec<Span> = vec![
        Span::styled(
            format!("{} ", app.active.tag()),
            tag_style("CFG").add_modifier(Modifier::BOLD),
        ),
        div(),
        Span::raw(format!("PANE {pane}")),
        div(),
        Span::raw("NET "),
        // `net` reflects whether the protocol stack was compiled in: a lit pip
        // when armed, a hollow dim one when the build is offline.
        if cfg!(feature = "net") {
            Span::styled("●", tag_style("DLV").add_modifier(Modifier::BOLD))
        } else {
            Span::styled("○", ts_style())
        },
        div(),
        Span::raw(format!("Q:{}", app.outbound.len())),
        div(),
        Span::raw(format!("PEERS:{}", app.conversations.len())),
    ];

    // Short form of our own address (full one lives in the Network tab).
    match &app.local_address {
        Some(a) if a.len() >= 8 => {
            spans.push(div());
            spans.push(Span::styled(
                format!("ME:{}\u{2026}", &a[..8]),
                tag_style("ID"),
            ));
        }
        Some(a) => {
            spans.push(div());
            spans.push(Span::styled(format!("ME:{a}"), tag_style("ID")));
        }
        None => {}
    }

    // The keybinding legend, set off and muted so the readout reads first.
    spans.push(Span::styled("   ▐ ", ts_style()));
    spans.push(Span::styled(
        "Ctrl+N/P:Tab  Ctrl+O:New  Tab:Pane  Up/Dn:Peer  Ctrl+S:Send  Ctrl+R:Sync  Ctrl+X:Purge  Ctrl+Q:Quit",
        ts_style(),
    ));

    let para = Paragraph::new(Line::from(spans)).block(pane_block("STATUS", false));
    frame.render_widget(para, area);
}
