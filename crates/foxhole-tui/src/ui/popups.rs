//! Centered modal overlays: the burn notice (Ctrl+K), the New Conversation
//! prompt (Ctrl+O), and the propagation-sync progress pop-up. Each blanks the
//! cells beneath it and draws over the tool body.

use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::{
    App, AuthorField, AuthorForm, AuthorKind, BURN_TOKEN, BurnConfirm, DeviceColumn, DevicePicker,
    GotoMgrs, IntelReview, MnemonicView, NewConv, NewConvField, ShareZone,
};

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
    if app.intel.staged.is_empty() {
        lines.push(Line::styled("  (nothing staged)", base_style()));
    } else {
        for (i, r) in app.intel.staged.iter().enumerate() {
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

/// The share-zone picker (Ctrl+G in Conversations): choose a local hazard zone
/// to send to the active peer as CoT intel. Reads the zone list off `App`,
/// highlighting the selected row; the header names the recipient.
pub(super) fn render_share_zone_popup(frame: &mut Frame, app: &App, share: &ShareZone) {
    let area = centered_rect(64, 14, frame.area());
    frame.render_widget(Clear, area);
    let cfg = tag_style("CFG");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(FRAME_BORDER)
        .style(base_style())
        .border_style(cfg)
        .title(Span::styled(
            " SHARE INTEL ",
            cfg.add_modifier(Modifier::BOLD),
        ));

    let mut lines = vec![
        Line::styled(
            format!("  Send a hazard zone to {} as CoT:", share.peer_label),
            base_style(),
        ),
        Line::raw(""),
    ];
    if app.map.zones.is_empty() {
        lines.push(Line::styled(
            "  (no local zones — add to zones.conf)",
            base_style(),
        ));
    } else {
        for (i, z) in app.map.zones.iter().enumerate() {
            let sel = i == share.selected;
            let lead = if sel { "\u{25b6} " } else { "  " }; // ▶
            let mut style = Style::default();
            if sel {
                style = style.add_modifier(Modifier::REVERSED);
            }
            lines.push(Line::styled(
                format!(
                    "{lead}\u{26a0} {:<18.18} {:>7.2},{:>7.2}  r{:.0}km",
                    z.label, z.center.lat, z.center.lon, z.radius_km
                ),
                style,
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  [\u{2191}\u{2193}] select   [Enter]/[s] share   [r] revoke   [Esc] cancel",
        base_style(),
    ));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// The audio device picker (`d` in the Voice tool): which microphone and which
/// speaker a call uses.
///
/// Worth a modal rather than a config-file-only setting because of how the
/// problem presents. A machine with more than one input — a webcam, an HDMI
/// sink, a headset — can default to the one with no microphone behind it, and
/// the symptom is a call that connects, sounds fine inbound, and transmits
/// silence. Nothing errors, so there is nothing to read; the only way through
/// is to see the list and pick another. Row 0 hands the choice back to the
/// host, so "undo" is a selection like any other.
pub(super) fn render_device_popup(frame: &mut Frame, app: &App, picker: &DevicePicker) {
    let area = centered_rect(66, 16, frame.area());
    frame.render_widget(Clear, area);
    let cfg = tag_style("CFG");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(FRAME_BORDER)
        .style(base_style())
        .border_style(cfg)
        .title(Span::styled(
            " AUDIO DEVICES ",
            cfg.add_modifier(Modifier::BOLD),
        ));

    let devices = &app.voice.devices;
    let default = match picker.column {
        DeviceColumn::Input => devices.default_input.as_deref(),
        DeviceColumn::Output => devices.default_output.as_deref(),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("  ", base_style()),
            Span::styled(picker.column.label(), cfg.add_modifier(Modifier::BOLD)),
            Span::styled("   [Tab] switch", base_style()),
        ]),
        Line::raw(""),
    ];

    // Row 0 is the "follow the host" choice, named so the operator can see
    // which device that actually is before deciding it is the wrong one.
    let default_label = match default {
        Some(name) => format!("system default ({name})"),
        None => "system default (none reported)".to_string(),
    };
    lines.push(device_row(
        &default_label,
        picker.is_selected(devices, None),
        picker.index == 0,
    ));
    let names = picker.names(devices);
    if names.is_empty() {
        lines.push(Line::styled("    (no devices enumerated)", base_style()));
    }
    for (i, name) in names.iter().enumerate() {
        lines.push(device_row(
            name,
            picker.is_selected(devices, Some(name.as_str())),
            picker.index == i + 1,
        ));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  [\u{2191}\u{2193}] select   [Enter] use   [Tab] mic/speaker   [Esc] cancel",
        base_style(),
    ));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// One device row: `\u{25b6}` marks the cursor, `\u{25cf}` the device in use.
/// The two are distinct on purpose — the highlighted row is where Enter would
/// take you, the dot is where you already are.
fn device_row(label: &str, selected: bool, active: bool) -> Line<'static> {
    let lead = if active { "\u{25b6} " } else { "  " }; // ▶
    let mark = if selected { "\u{25cf} " } else { "  " }; // ●
    let mut style = base_style();
    if active {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Line::styled(format!("{lead}{mark}{label:.56}"), style)
}

/// The intel authoring form (`a`/`e` on the World Map): place or edit a marker
/// or zone of any affiliation, committed to the live intel layer. The focused
/// field is chevroned; text fields carry a synthetic caret.
pub(super) fn render_author_popup(frame: &mut Frame, form: &AuthorForm) {
    let area = centered_rect(60, 15, frame.area());
    frame.render_widget(Clear, area);
    let cfg = tag_style("CFG");
    let title = if form.edit_key.is_some() {
        " EDIT INTEL "
    } else {
        " AUTHOR INTEL "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(FRAME_BORDER)
        .style(base_style())
        .border_style(cfg)
        .title(Span::styled(title, cfg.add_modifier(Modifier::BOLD)));

    let caret = Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED));
    // One row: `▶ Label   value`, with the chevron on the focused field and a
    // caret after a focused text value.
    let row = |label: &str, value: String, focused: bool, is_text: bool| -> Line<'static> {
        let lead = if focused { "\u{25b6} " } else { "  " };
        let lstyle = if focused {
            cfg.add_modifier(Modifier::BOLD)
        } else {
            base_style()
        };
        let mut spans = vec![
            Span::styled(format!("{lead}{label:<11}"), lstyle),
            Span::raw(value),
        ];
        if focused && is_text {
            spans.push(caret.clone());
        }
        Line::from(spans)
    };

    let kind_str = match form.kind {
        AuthorKind::Marker => "Marker",
        AuthorKind::Zone => "Zone",
    };
    let mut lines = vec![
        row(
            "Kind",
            format!("< {kind_str} >"),
            form.field == AuthorField::Kind,
            false,
        ),
        row(
            "Affil",
            format!("< {} >", form.affiliation.label()),
            form.field == AuthorField::Affiliation,
            false,
        ),
        row(
            "Callsign",
            form.callsign.clone(),
            form.field == AuthorField::Callsign,
            true,
        ),
        row(
            "Lat",
            form.lat.clone(),
            form.field == AuthorField::Lat,
            true,
        ),
        row(
            "Lon",
            form.lon.clone(),
            form.field == AuthorField::Lon,
            true,
        ),
        // MGRS mirrors Lat/Lon — edit either and the other follows.
        row(
            "MGRS",
            form.mgrs.clone(),
            form.field == AuthorField::Mgrs,
            true,
        ),
    ];
    // Radius only matters for a zone; show it dimmed for a marker.
    if form.kind == AuthorKind::Zone {
        lines.push(row(
            "Radius km",
            form.radius_km.clone(),
            form.field == AuthorField::Radius,
            true,
        ));
    } else {
        lines.push(Line::styled(
            "  Radius km  (zone only)",
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    lines.push(row(
        "Remarks",
        form.remarks.clone(),
        form.field == AuthorField::Remarks,
        true,
    ));
    lines.push(Line::raw(""));
    if let Some(err) = form.error {
        lines.push(Line::styled(format!("  {err}"), tag_style("ERR")));
    } else {
        lines.push(Line::styled(
            "  [\u{2191}\u{2193}] field  [\u{2190}\u{2192}] toggle  [Enter] commit  [Esc] cancel",
            base_style(),
        ));
    }

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// The "go to MGRS" modal (`/` on the World Map): the operator types a grid
/// reference and the map reframes onto it. Shows the live decode of what's typed
/// so a designation can be eyeballed before committing, and flags a bad entry.
pub(super) fn render_goto_mgrs_popup(frame: &mut Frame, goto: &GotoMgrs) {
    let area = centered_rect(56, 9, frame.area());
    frame.render_widget(Clear, area);
    let cfg = tag_style("CFG");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(FRAME_BORDER)
        .style(base_style())
        .border_style(cfg)
        .title(Span::styled(
            " GO TO MGRS ",
            cfg.add_modifier(Modifier::BOLD),
        ));

    let caret = Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED));
    let mut lines = vec![
        Line::styled("  Reframe the map onto a grid reference:", base_style()),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  MGRS:  "),
            Span::raw(goto.input.clone()),
            caret,
        ]),
        Line::raw(""),
    ];
    // Live feedback: the decoded lat/lon, or why it won't parse.
    if goto.error {
        lines.push(Line::styled(
            "  unrecognised grid reference (e.g. 31U DQ 48251 11932)",
            tag_style("ERR"),
        ));
    } else if let Some(p) = foxhole_map::mgrs::parse(&goto.input) {
        lines.push(Line::styled(
            format!("  \u{2192} {:.5}, {:.5}", p.lat, p.lon),
            base_style(),
        ));
    } else {
        lines.push(Line::styled(
            "  zone + band + square + digits, e.g. 31U DQ 48251 11932",
            base_style(),
        ));
    }
    lines.push(Line::styled("  [Enter] go   [Esc] cancel", base_style()));

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
