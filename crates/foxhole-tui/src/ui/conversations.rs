//! Conversations tool body: the peer list and selected thread side by side,
//! with the compose buffer spanning the bottom — the Nomadnet layout.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{App, Pane, TransmitField};

use super::style::{styled_entry, trust_style};
use super::widgets::{NOSEL, SEL, count_tag, pane_block, render_scroll, tactical_block};

/// Conversations tool: a peer list and the selected peer's thread side by side,
/// with the compose buffer spanning the bottom — the Nomadnet layout.
pub(super) fn render_conversations(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // Peer list + thread
            Constraint::Length(5), // Transmit buffer (compose rows + borders)
        ])
        .split(area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30), // Peer list
            Constraint::Min(0),         // Selected thread
        ])
        .split(rows[0]);

    render_peer_list(frame, app, top[0]);

    // Selected thread: title carries the peer; body is its timestamped scrollback.
    let (title, lines, thread_active) = match app.selected_conv() {
        Some(conv) => (
            format!("THREAD: {} (UTC)", conv.peer),
            conv.messages.iter().map(styled_entry).collect(),
            app.focus == Pane::Thread,
        ),
        None => ("THREAD".to_string(), Vec::new(), false),
    };
    render_scroll(
        frame,
        &title,
        lines,
        thread_active,
        &app.thread_scroll,
        top[1],
    );

    render_transmit(frame, app, rows[1]);
}

/// Peer list: one row per conversation. The selected row is prefixed with `>`
/// and reversed (so the active thread is identifiable even when the pane is not
/// focused); a colour-coded trust glyph leads each row and unread inbound counts
/// show as a trailing `(N)`.
fn render_peer_list(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .conversations
        .iter()
        .enumerate()
        .map(|(i, conv)| {
            let selected = i == app.selected;
            let marker = if selected { SEL } else { NOSEL };
            let unread = if conv.unread > 0 {
                format!(" ({})", conv.unread)
            } else {
                String::new()
            };
            let mut style = Style::default();
            if selected {
                style = style.add_modifier(Modifier::REVERSED);
            } else if conv.unread > 0 {
                style = style.add_modifier(Modifier::BOLD);
            }
            let mut gstyle = trust_style(conv.trust);
            if selected {
                gstyle = gstyle.add_modifier(Modifier::REVERSED);
            }
            Line::from(vec![
                Span::styled(format!("{marker}{} ", conv.trust.glyph()), gstyle),
                Span::styled(format!("{}{unread}", conv.label()), style),
            ])
        })
        .collect();

    let para = Paragraph::new(lines).block(tactical_block(
        "PEERS",
        Some(count_tag(app.conversations.len())),
        app.focus == Pane::PeerList,
    ));
    frame.render_widget(para, area);
}

/// Compose pane. Shows the *selected conversation's* draft title (Ctrl+T) and
/// body. The real terminal cursor stays hidden (field constraint), so when
/// focused we paint a synthetic reversed block as the caret on the active field.
fn render_transmit(frame: &mut Frame, app: &App, area: Rect) {
    let active = app.focus == Pane::Transmit;
    let conv = app.selected_conv();
    let title = conv.map(|c| c.draft_title.as_str()).unwrap_or("");
    let body = conv.map(|c| c.draft.as_str()).unwrap_or("");

    // Caret sits on whichever field Ctrl+T selected, but only while focused.
    let caret = |on: bool| -> Option<Span> {
        (active && on).then(|| Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)))
    };
    let label_style = Style::default().add_modifier(Modifier::DIM);

    // Title row: dimmed when empty so it reads as an optional prompt.
    let editing_title = app.transmit_field == TransmitField::Title;
    let mut title_spans = vec![Span::styled("TITLE ", label_style), Span::raw(title)];
    if let Some(c) = caret(editing_title) {
        title_spans.push(c);
    }

    let mut body_spans = vec![Span::raw("❯ "), Span::raw(body)];
    if let Some(c) = caret(!editing_title) {
        body_spans.push(c);
    }

    let para = Paragraph::new(vec![Line::from(title_spans), Line::from(body_spans)])
        .block(pane_block("TRANSMIT BUFFER", active))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}
