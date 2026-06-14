//! Centered modal overlays: the burn notice (Ctrl+K), the New Conversation
//! prompt (Ctrl+O), and the propagation-sync progress pop-up. Each blanks the
//! cells beneath it and draws over the tool body.

use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::{BURN_TOKEN, BurnConfirm, NewConv, NewConvField};

use super::style::tag_style;
use super::widgets::{ASCII_BORDER, centered_rect};

/// The burn-confirmation modal (Ctrl+K): a red notice listing what gets
/// destroyed, gated behind typing the confirmation token.
pub(super) fn render_burn_popup(frame: &mut Frame, b: &BurnConfirm) {
    let area = centered_rect(60, 11, frame.area());
    frame.render_widget(Clear, area);
    let err = tag_style("ERR");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ASCII_BORDER)
        .border_style(err)
        .title(Span::styled(
            " BURN NOTICE ",
            err.add_modifier(Modifier::BOLD),
        ));

    let caret = Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED));
    let mut lines = vec![
        Line::styled(
            "  DESTROY ALL SESSION DATA. This cannot be undone.",
            err.add_modifier(Modifier::BOLD),
        ),
        Line::raw("    - identity (your cryptographic identity)"),
        Line::raw("    - known peers and propagation nodes"),
        Line::raw("    - all conversation history"),
        Line::raw("    - settings and Reticulum state"),
        Line::raw(""),
        Line::from(vec![
            Span::raw(format!("  Type {BURN_TOKEN} to confirm:  ")),
            Span::raw(b.input.clone()),
            caret,
        ]),
    ];
    if b.error {
        lines.push(Line::styled(
            format!("  not {BURN_TOKEN} — nothing burned"),
            err,
        ));
    } else {
        lines.push(Line::raw("  [Enter] burn    [Esc] cancel"));
    }

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// Modal for adding a conversation by LXMF address (Ctrl+O). The focused field
/// carries a synthetic reversed caret (the real cursor stays hidden).
pub(super) fn render_new_conv_popup(frame: &mut Frame, nc: &NewConv) {
    let area = centered_rect(60, 8, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ASCII_BORDER)
        .border_style(Style::default().add_modifier(Modifier::BOLD))
        .title(Span::styled(
            " NEW CONVERSATION ",
            Style::default().add_modifier(Modifier::BOLD),
        ));

    let caret = Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED));
    let field = |label: &str, value: &str, focused: bool| -> Line<'static> {
        let mut spans = vec![
            Span::raw(format!("  {label} ")),
            Span::raw(value.to_string()),
        ];
        if focused {
            spans.push(caret.clone());
        }
        Line::from(spans)
    };

    let mut lines = vec![
        field(
            "LXMF address:   ",
            &nc.address,
            nc.field == NewConvField::Address,
        ),
        field(
            "Alias (option): ",
            &nc.alias,
            nc.field == NewConvField::Alias,
        ),
        Line::raw(""),
    ];
    if nc.error {
        lines.push(Line::styled(
            "  invalid address — need 32 hex characters",
            tag_style("ERR"),
        ));
    } else {
        lines.push(Line::raw(
            "  destination hash, e.g. a1b2…  (colons/spaces ok)",
        ));
    }
    lines.push(Line::raw("  [Tab] field   [Enter] open   [Esc] cancel"));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// Modal pop-up shown while a propagation sync is in progress.
pub(super) fn render_sync_popup(frame: &mut Frame, status: &str) {
    let area = centered_rect(48, 4, frame.area());
    frame.render_widget(Clear, area); // blank the cells underneath
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ASCII_BORDER)
        .border_style(Style::default().add_modifier(Modifier::BOLD))
        .title(Span::styled(
            " PROPAGATION SYNC ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let lines = vec![
        Line::raw(format!("  {status}")),
        Line::raw("  pulling messages — please wait"),
    ];
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}
