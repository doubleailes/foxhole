//! Voice tool body: the LXST telephony roster, the in-call HUD, and the call log.
//!
//! Layout mirrors the Network tool — a roster on the left, detail on the right —
//! but the right half is split between a call HUD and the `[VOX]` scrollback,
//! because during a call the phase/level readout is the only thing the operator
//! is looking at and it must not scroll away under log traffic.
//!
//! The HUD leans on the same "colour reinforces, never carries" rule as the rest
//! of the chrome: the phase reads as a word, not just a tint, and the VU meters
//! are block glyphs whose *length* is the signal, so both survive a monochrome
//! terminal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, AudioStatus, Call, CallPhase, VoicePeer};

use super::network::signal_meter;
use super::style::{BORDER_LIVE, INK, base_style, styled_entry, tag_style, ts_style};
use super::widgets::{count_tag, render_scrollback, tactical_block};

/// Placeholder for the roster when the voice stack isn't compiled in — distinct
/// from "nobody has announced yet", which is what a `voice` build shows.
#[cfg(feature = "voice")]
const NO_VOICE_PEERS: &str = "  (no lxst.telephony peers heard yet)";
#[cfg(not(feature = "voice"))]
const NO_VOICE_PEERS: &str = "  (voice offline — rebuild with --features voice)";

/// Voice tool: roster on the left, call HUD + call log on the right, with a
/// this-node header and a key legend. See [`crate::app::App::handle_voice_key`].
pub(super) fn render_voice(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // this-node identity
            Constraint::Min(3),    // roster | (HUD + log)
            Constraint::Length(1), // legend
        ])
        .split(area);

    // This node's *identity* hash — what a peer dials to call us. Deliberately
    // labelled as such: it is not the lxmf.delivery address the Network tab
    // shows, and handing a peer the wrong one is a call that never connects.
    let ident = app
        .voice
        .local_identity
        .as_deref()
        .unwrap_or("(starting — identity pending)");
    let header = Line::from(vec![
        Span::styled(
            "THIS NODE (lxst.telephony identity): ",
            Style::default()
                .fg(BORDER_LIVE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(ident.to_string(), Style::default().fg(INK)),
    ]);
    frame.render_widget(Paragraph::new(header).style(base_style()), rows[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(rows[1]);
    render_roster(frame, app, cols[0]);

    // The HUD is a fixed 8 rows (its content is fixed-height); the log takes the
    // slack, so a long call history never squeezes the phase readout off-screen.
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(3)])
        .split(cols[1]);
    render_hud(frame, app, right[0]);
    render_call_log(frame, app, right[1]);

    frame.render_widget(
        Paragraph::new(Line::styled(
            "[Up/Dn] sel  [Enter] call/answer  [h/Esc] hang up  [m] mute  [p] profile  [a] announce  \u{25cf} on call",
            ts_style(),
        ))
        .style(base_style()),
        rows[2],
    );
}

/// Left column: peers heard announcing an `lxst.telephony` destination.
fn render_roster(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = if app.voice.peers.is_empty() {
        vec![Line::raw(NO_VOICE_PEERS)]
    } else {
        app.voice
            .peers
            .iter()
            .enumerate()
            .map(|(i, p)| peer_row(p, i == app.voice.selected, app))
            .collect()
    };
    let para = Paragraph::new(lines).block(tactical_block(
        "VOICE PEERS (lxst.telephony)",
        Some(count_tag(app.voice.peers.len())),
        true,
    ));
    frame.render_widget(para, area);
}

/// One roster row: `▶●name       ident8..  ▰▰▱▱ 2h`. The pip after the selection
/// chevron marks the peer currently on a call.
///
/// Unlike the Network tab's peer row this carries no last-seen stamp, and the
/// omission is deliberate rather than an oversight: the roster pane is ~38
/// columns, which does not fit a name, a hash, a clock *and* the hop meter, and
/// LXST re-announces only every three hours — so a last-seen clock is a poor
/// liveness signal here, while the hop meter is exactly the "will this call
/// connect" reading the operator is after.
fn peer_row(peer: &VoicePeer, selected: bool, app: &App) -> Line<'static> {
    let id8 = peer.identity.get(..8).unwrap_or(&peer.identity);
    let on_call = app
        .voice
        .call
        .as_ref()
        .is_some_and(|c| c.peer == peer.identity);
    let row_style = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };

    // Two fixed columns before the name — chevron then pip — so rows stay
    // aligned whether or not either is lit.
    let chevron = if selected { '\u{25b6}' } else { ' ' };
    let pip = if on_call { '\u{25cf}' } else { ' ' };
    let mut spans = vec![Span::styled(
        format!("{chevron}{pip}{:<10.10} {id8}..", peer.label()),
        row_style,
    )];
    // Hop count comes straight off the announce, so unlike the Network tab's
    // meter it needs no separate probe — show it whenever the transport gave us one.
    if peer.hops.is_some() {
        let mut ms = match peer.hops {
            Some(0..=2) => tag_style("DLV"),
            Some(3..=4) => tag_style("WRN"),
            _ => tag_style("ERR"),
        };
        if selected {
            ms = ms.add_modifier(Modifier::REVERSED);
        }
        let label = peer.hops.map(|n| format!(" {n}h")).unwrap_or_default();
        spans.push(Span::styled(
            format!("  {}{label}", signal_meter(peer.hops)),
            ms,
        ));
    }
    Line::from(spans)
}

/// The call HUD: phase, peer, profile, talk timer, and the two VU meters. Shows
/// the idle readout (audio-backend state + the profile the next call will open
/// on) when there is no call, so the panel is never dead space.
fn render_hud(frame: &mut Frame, app: &App, area: Rect) {
    let lines = match &app.voice.call {
        Some(call) => active_call_lines(app, call),
        None => idle_lines(app),
    };
    let live = app.voice.call.as_ref().is_some_and(|c| c.phase.is_live());
    let corner = app.voice.call.as_ref().map(|c| {
        Span::styled(
            format!(" {} {} ", c.direction.glyph(), c.phase.label()),
            phase_style(c.phase),
        )
    });
    frame.render_widget(
        Paragraph::new(lines).block(tactical_block("CALL", corner, live)),
        area,
    );
}

/// HUD body while a call exists.
fn active_call_lines(app: &App, call: &Call) -> Vec<Line<'static>> {
    let profile = call.profile.unwrap_or(app.voice.profile);
    let secs = call.talk_secs(crate::app::now_secs());
    let timer = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", call.phase.label()),
                phase_style(call.phase).add_modifier(Modifier::BOLD),
            ),
            Span::styled(call.label(), Style::default().fg(INK)),
            Span::styled(
                format!("  {}\u{2026}", call.peer.get(..8).unwrap_or(&call.peer)),
                ts_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled("profile ", ts_style()),
            Span::raw(format!(
                "{} ({}, {} Hz, {} ms, \u{2264}{} kbps)",
                profile.abbreviation(),
                profile.name(),
                profile.sample_rate_hz(),
                profile.frame_ms(),
                profile.bitrate_ceiling() / 1000,
            )),
        ]),
        Line::from(vec![
            Span::styled("talk    ", ts_style()),
            Span::raw(timer),
            Span::styled("   mic ", ts_style()),
            if app.voice.muted {
                Span::styled("MUTED", tag_style("WRN").add_modifier(Modifier::BOLD))
            } else {
                Span::styled("LIVE", tag_style("DLV").add_modifier(Modifier::BOLD))
            },
        ]),
        Line::raw(""),
    ];
    // Meters only mean anything once media is flowing; before that they'd read
    // as a dead line rather than as "not connected yet".
    if call.phase.is_live() {
        lines.push(vu_line("TX", app.voice.tx_level, app.voice.muted));
        lines.push(vu_line("RX", app.voice.rx_level, false));
    } else {
        lines.push(Line::styled("  (no media yet)", ts_style()));
    }
    lines
}

/// HUD body with no call up.
fn idle_lines(app: &App) -> Vec<Line<'static>> {
    let p = app.voice.profile;
    // One readout for every state, and it leads with what works: an operator
    // with speakers but no microphone needs to see "receive only", not a flat
    // "unavailable" that reads as a dead call.
    let audio = match &app.voice.audio {
        AudioStatus::Ready => Span::styled(app.voice.audio.summary(), tag_style("DLV")),
        AudioStatus::TransmitOnly(_) | AudioStatus::ReceiveOnly(_) => {
            Span::styled(app.voice.audio.summary(), tag_style("WRN"))
        }
        AudioStatus::Unavailable(_) => Span::styled(app.voice.audio.summary(), tag_style("WRN")),
        AudioStatus::Unknown if cfg!(feature = "voice") => {
            Span::styled(app.voice.audio.summary(), ts_style())
        }
        AudioStatus::Unknown => Span::styled(
            "offline — rebuild with --features voice".to_string(),
            ts_style(),
        ),
    };
    vec![
        Line::styled("IDLE", ts_style().add_modifier(Modifier::BOLD)),
        Line::from(vec![Span::styled("audio   ", ts_style()), audio]),
        Line::from(vec![
            Span::styled("profile ", ts_style()),
            Span::raw(format!(
                "{} ({}, {} Hz, {} ms, \u{2264}{} kbps)",
                p.abbreviation(),
                p.name(),
                p.sample_rate_hz(),
                p.frame_ms(),
                p.bitrate_ceiling() / 1000,
            )),
        ]),
        Line::raw(""),
        Line::styled("  select a peer and press Enter to call", ts_style()),
    ]
}

/// One VU meter row: a 20-cell bar whose *length* carries the level, so it reads
/// with colour stripped. A muted transmit meter is drawn empty and tagged rather
/// than showing whatever the microphone would have picked up.
fn vu_line(label: &str, level: u8, muted: bool) -> Line<'static> {
    const CELLS: usize = 20;
    let level = if muted { 0 } else { level.min(100) };
    let lit = (level as usize * CELLS).div_ceil(100).min(CELLS);
    let style = match level {
        0..=59 => tag_style("DLV"),
        60..=89 => tag_style("WRN"),
        // Sustained clipping is a real problem on a voice link, so the top of
        // the scale is the alarm colour rather than just "more green".
        _ => tag_style("ERR"),
    };
    Line::from(vec![
        Span::styled(format!("{label:<3} ",), ts_style()),
        Span::styled("\u{2588}".repeat(lit), style),
        Span::styled("\u{2591}".repeat(CELLS - lit), ts_style()),
        Span::styled(
            if muted {
                "  muted".to_string()
            } else {
                format!("  {level:>3}")
            },
            ts_style(),
        ),
    ])
}

/// Colour grade for a call phase: settling states muted, ringing brass (it wants
/// an answer), established green, ending red.
fn phase_style(phase: CallPhase) -> Style {
    match phase {
        CallPhase::Discovering | CallPhase::Connecting => tag_style("RT"),
        CallPhase::Calling => tag_style("LNK"),
        CallPhase::Ringing => tag_style("WRN"),
        CallPhase::Established => tag_style("DLV"),
        CallPhase::Ending => tag_style("ERR"),
    }
}

/// Bottom-right: the `[VOX]` call history, bottom-pinned like every other
/// scrollback in the program.
fn render_call_log(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = if app.voice.log.is_empty() {
        vec![Line::styled("  (no calls this session)", ts_style())]
    } else {
        app.voice.log.iter().map(styled_entry).collect()
    };
    render_scrollback(frame, "CALL LOG (UTC)", lines, false, area);
}
