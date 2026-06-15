//! Centered modal overlays: the burn notice (Ctrl+K), the New Conversation
//! prompt (Ctrl+O), and the propagation-sync progress pop-up. Each blanks the
//! cells beneath it and draws over the tool body.

use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::{App, BURN_TOKEN, BurnConfirm, IntelReview, MnemonicView, NewConv, NewConvField};

use super::style::{base_style, tag_style};
use super::widgets::{FRAME_BORDER, centered_rect};

/// The burn-confirmation modal (Ctrl+K): a red notice listing what gets
/// destroyed, gated behind typing the confirmation token.
pub(super) fn render_burn_popup(frame: &mut Frame, b: &BurnConfirm) {
    let area = centered_rect(60, 11, frame.area());
    frame.render_widget(Clear, area);
    let err = tag_style("ERR");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(FRAME_BORDER)
        .style(base_style())
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
        .border_set(FRAME_BORDER)
        .style(base_style())
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
            "  invalid — need 32 hex chars or a valid 12-word phrase",
            tag_style("ERR"),
        ));
    } else {
        lines.push(Line::raw("  32 hex chars or a 12-word mnemonic phrase"));
    }
    lines.push(Line::raw("  [Tab] field   [Enter] open   [Esc] cancel"));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// Read-only modal showing a peer's address as a 12-word mnemonic phrase (the
/// `m` key in the Network tab) — for reading/verifying an address over voice.
pub(super) fn render_mnemonic_popup(frame: &mut Frame, m: &MnemonicView) {
    let area = centered_rect(60, 9, frame.area());
    frame.render_widget(Clear, area);
    let id = tag_style("ID");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(FRAME_BORDER)
        .style(base_style())
        .border_style(id)
        .title(Span::styled(" MNEMONIC ", id.add_modifier(Modifier::BOLD)));

    let lines = vec![
        Line::raw(format!("  addr: {}", m.hash)),
        Line::raw(""),
        Line::styled(format!("  {}", m.phrase), id.add_modifier(Modifier::BOLD)),
        Line::raw(""),
        Line::raw("  read aloud to share/verify    [any key] close"),
    ];
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// The incoming-intel review modal (`i` on the World Map): the CoT events staged
/// from Unknown/Untrusted peers, which the operator accepts onto the map or
/// discards. Reads the staged list straight off `App`, highlighting the selected
/// row (design note §6 — trust gating / staging).
pub(super) fn render_intel_review_popup(frame: &mut Frame, app: &App, review: &IntelReview) {
    let area = centered_rect(70, 16, frame.area());
    frame.render_widget(Clear, area);
    let wrn = tag_style("WRN");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(FRAME_BORDER)
        .style(base_style())
        .border_style(wrn)
        .title(Span::styled(
            " INCOMING INTEL ",
            wrn.add_modifier(Modifier::BOLD),
        ));

    let mut lines = vec![
        Line::styled(
            "  Unvetted CoT from unknown/untrusted peers — review before applying.",
            base_style(),
        ),
        Line::raw(""),
    ];
    if app.intel_staged.is_empty() {
        lines.push(Line::styled("  (nothing staged)", base_style()));
    } else {
        for (i, r) in app.intel_staged.iter().enumerate() {
            let sel = i == review.selected;
            let lead = if sel { "\u{25b6} " } else { "  " }; // ▶
            let source = r.source.get(..8).unwrap_or(&r.source);
            let mut style = Style::default();
            if sel {
                style = style.add_modifier(Modifier::REVERSED);
            }
            lines.push(Line::styled(
                format!(
                    "{lead}{} {:<14.14} {:<10.10} {} {}",
                    r.affiliation().glyph(),
                    r.label(),
                    r.affiliation().label(),
                    source,
                    r.event.cot_type,
                ),
                style,
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  [\u{2191}\u{2193}] select   [a]/[Enter] accept   [x]/[d] discard   [Esc] close",
        base_style(),
    ));

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
        .border_set(FRAME_BORDER)
        .style(base_style())
        .border_style(Style::default().add_modifier(Modifier::BOLD))
        .title(Span::styled(
            " PROPAGATION SYNC ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let lines = vec![
        Line::raw(format!("  {status}")),
        Line::raw("  pulling messages    [Esc] cancel"),
    ];
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}
