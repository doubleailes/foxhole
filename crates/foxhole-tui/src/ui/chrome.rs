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
use super::style::{ACCENT, BG, BORDER_LIVE, INK, PANEL, base_style, tag_style, ts_style};
use super::views::{render_guide, render_interfaces, render_log};
use super::widgets::tactical_block;

/// Top menu strip, styled as a HUD mode-selector: a brass `FOXHOLE` callsign
/// nameplate, then each tool's title left to right. The active tool is a lit
/// phosphor key (ink-on-green) flanked by inward `▶ ◀` chevrons; the rest are
/// dimmed, divided by thin tactical rules. Unicode box-drawing throughout.
pub(super) fn render_tab_strip(frame: &mut Frame, app: &App, area: Rect) {
    let bold = Modifier::BOLD;
    let mut spans: Vec<Span> = vec![
        // Brass callsign nameplate.
        Span::styled(
            "▌ FOXHOLE ▐",
            Style::default().fg(BG).bg(ACCENT).add_modifier(bold),
        ),
        Span::raw(" "),
    ];
    for (i, tool) in Tool::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", ts_style()));
        }
        if *tool == app.active {
            // Active tool: a lit phosphor key flanked by inward chevrons.
            spans.push(Span::styled(
                "▶",
                Style::default().fg(BORDER_LIVE).add_modifier(bold),
            ));
            spans.push(Span::styled(
                format!(" {} ", tool.title()),
                Style::default().fg(BG).bg(BORDER_LIVE).add_modifier(bold),
            ));
            spans.push(Span::styled(
                "◀",
                Style::default().fg(BORDER_LIVE).add_modifier(bold),
            ));
        } else {
            spans.push(Span::styled(
                tool.title().to_string(),
                Style::default().fg(INK).add_modifier(Modifier::DIM),
            ));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(base_style()), area);
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

/// Bottom status readout, styled as an instrument cluster: each datum is a
/// raised-panel label "chip" (`PANE`, `NET`, `Q`, `PEERS`, `ME`) followed by its
/// value, led by a brass tool chip and a lit/hollow NET pip. The keybinding
/// legend rides the block's right-corner so the gauges read first. Never focusable.
pub(super) fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let pane = match app.focus {
        Pane::PeerList => "PEERS",
        Pane::Thread => "THREAD",
        Pane::Transmit => "TRANSMIT",
    };

    // A raised-panel "chip" gauge label.
    let chip = |label: &str| {
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(INK)
                .bg(PANEL)
                .add_modifier(Modifier::BOLD),
        )
    };
    let gap = || Span::raw("  ");

    let mut spans: Vec<Span> = vec![
        // The active tool as a bright brass leading chip.
        Span::styled(
            format!(" {} ", app.active.tag()),
            Style::default()
                .fg(BG)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        gap(),
        chip("PANE"),
        Span::raw(format!(" {pane}")),
        gap(),
        chip("NET"),
        Span::raw(" "),
        // `net` reflects whether the protocol stack was compiled in: a lit pip
        // when armed, a hollow dim one when the build is offline.
        if cfg!(feature = "net") {
            Span::styled("●", tag_style("DLV").add_modifier(Modifier::BOLD))
        } else {
            Span::styled("○", ts_style())
        },
        gap(),
        chip("Q"),
        Span::raw(format!(" {}", app.outbound.len())),
        gap(),
        chip("PEERS"),
        Span::raw(format!(" {}", app.conversations.len())),
    ];

    // Short form of our own address (full one lives in the Network tab).
    match &app.local_address {
        Some(a) if a.len() >= 8 => {
            spans.push(gap());
            spans.push(chip("ME"));
            spans.push(Span::styled(
                format!(" {}\u{2026}", &a[..8]),
                tag_style("ID"),
            ));
        }
        Some(a) => {
            spans.push(gap());
            spans.push(chip("ME"));
            spans.push(Span::styled(format!(" {a}"), tag_style("ID")));
        }
        None => {}
    }

    // The keybinding legend rides the right corner of the status frame, muted.
    let legend = Span::styled(
        " Ctrl+N/P Tab · Ctrl+O New · Tab Pane · Ctrl+T Title · Ctrl+S Send · Ctrl+R Sync · Ctrl+Q Quit ",
        ts_style(),
    );
    let para =
        Paragraph::new(Line::from(spans)).block(tactical_block("STATUS", Some(legend), false));
    frame.render_widget(para, area);
}
