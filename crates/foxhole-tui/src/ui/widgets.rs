//! Shared drawing primitives: the tactical border sets, the bordered pane block
//! (with active-pane highlight), and the two scrollback renderers (bottom-pinned
//! vs. operator-scrolled). Every tool body draws through these.
//!
//! We deliberately trade the old strict 7-bit ASCII guarantee for a heavier,
//! command-console look: panels are drawn with Unicode box-drawing (`FRAME_BORDER`)
//! and the focused pane gets a *double-ruled* frame (`FOCUS_BORDER`) so the live
//! pane reads structurally — not just by colour — on a monochrome display. This
//! assumes a UTF-8 terminal; pure line-printer gear is no longer a target.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

use super::style::{BG, BORDER_LIVE, BORDER_REST, INK, PANEL, base_style, ts_style};
use crate::app::Scroll;

/// Heavy box-drawing frame — the default tactical panel border (`┏━┓┃┗┛`). Reads
/// as a reinforced command-console panel on any UTF-8 terminal.
pub(super) const FRAME_BORDER: border::Set = border::Set {
    top_left: "┏",
    top_right: "┓",
    bottom_left: "┗",
    bottom_right: "┛",
    vertical_left: "┃",
    vertical_right: "┃",
    horizontal_top: "━",
    horizontal_bottom: "━",
};

/// Focused-pane frame — double-ruled (`╔═╗║╚╝`) so the live pane is unmistakable
/// even with colour stripped, distinct from the heavy `FRAME_BORDER` resting panes.
pub(super) const FOCUS_BORDER: border::Set = border::Set {
    top_left: "╔",
    top_right: "╗",
    bottom_left: "╚",
    bottom_right: "╝",
    vertical_left: "║",
    vertical_right: "║",
    horizontal_top: "═",
    horizontal_bottom: "═",
};

/// Tactical row-selection chevron, and the blank that keeps unselected rows in
/// the same column. Used by every roster/list body.
pub(super) const SEL: &str = "▶ ";
pub(super) const NOSEL: &str = "  ";

/// A centered `width`×`height` rectangle within `area` (clamped to fit).
pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

/// A bordered tactical panel with an optional right-aligned HUD readout in the
/// top border (scroll position, item count, …). Resting panes wear the heavy
/// [`FRAME_BORDER`] with a dim border and a panel nameplate; the active pane is
/// flagged by the double-ruled [`FOCUS_BORDER`], a lit phosphor border, an
/// ink-on-green nameplate, a leading status pip (`◆` live / `·` resting) and a
/// `LIVE` corner stamp — so focus is legible whether or not the terminal honours
/// colour (border weight + bold carry it in monochrome).
pub(super) fn tactical_block<'a>(
    title: &'a str,
    right: Option<Span<'a>>,
    active: bool,
) -> Block<'a> {
    // The active pane: double rule, lit phosphor border, a lit nameplate (ink-on-
    // green) and a `◆` pip. Resting: heavy rule, dim border, a panel nameplate.
    let (set, pip, border_style, title_style) = if active {
        (
            FOCUS_BORDER,
            "◆",
            Style::default()
                .fg(BORDER_LIVE)
                .add_modifier(Modifier::BOLD),
            Style::default()
                .fg(BG)
                .bg(BORDER_LIVE)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            FRAME_BORDER,
            "·",
            Style::default().fg(BORDER_REST),
            Style::default()
                .fg(INK)
                .bg(PANEL)
                .add_modifier(Modifier::BOLD),
        )
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_set(set)
        .border_style(border_style)
        .style(base_style())
        .title_top(Line::from(Span::styled(
            format!(" {pip} {title} "),
            title_style,
        )));
    // Right-corner readout: the caller's tag, or a `LIVE` stamp on the focused pane.
    let corner = right.or_else(|| {
        active.then(|| {
            Span::styled(
                " ◆LIVE◆ ",
                Style::default()
                    .fg(BORDER_LIVE)
                    .add_modifier(Modifier::BOLD),
            )
        })
    });
    if let Some(tag) = corner {
        block = block.title_top(Line::from(tag).right_aligned());
    }
    block
}

/// A bordered tactical panel with no right-corner readout (the common case).
pub(super) fn pane_block(title: &str, active: bool) -> Block<'_> {
    tactical_block(title, None, active)
}

/// A muted right-corner position readout for a scroll pane: the visible row span
/// over the total (`12–34/80`), or just the line count when it all fits.
fn pos_tag(off: u16, viewport: u16, content: u16) -> Span<'static> {
    let label = if content == 0 || content <= viewport {
        format!(" {content} ln ")
    } else {
        let first = off + 1;
        let last = (off + viewport).min(content);
        format!(" {first}\u{2013}{last}/{content} ")
    };
    Span::styled(label, ts_style())
}

/// A muted right-corner item count for a roster pane (`▶ PEERS … [ 3 ]`).
pub(super) fn count_tag(n: usize) -> Span<'static> {
    Span::styled(format!(" {n} "), ts_style())
}

/// Overlay a tactical vertical scrollbar on a pane's right border — caps (`▲`/`▼`),
/// a dotted track and a solid thumb — but only when the content overflows the
/// viewport (otherwise the heavy border reads cleaner).
fn render_scrollbar(frame: &mut Frame, area: Rect, content: u16, viewport: u16, off: u16) {
    if content <= viewport || area.height < 3 {
        return;
    }
    let mut state = ScrollbarState::new(content as usize)
        .viewport_content_length(viewport as usize)
        .position(off as usize);
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("▲"))
        .end_symbol(Some("▼"))
        .track_symbol(Some("┊"))
        .thumb_symbol("█")
        .track_style(Style::default().fg(BORDER_REST).bg(BG))
        .thumb_style(Style::default().fg(BORDER_LIVE).bg(BG))
        .begin_style(Style::default().fg(BORDER_LIVE).bg(BG))
        .end_style(Style::default().fg(BORDER_LIVE).bg(BG));
    frame.render_stateful_widget(
        bar,
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

/// Render a read-only scrollback pane: a bordered block whose view is pinned to
/// the newest lines so fresh content is always visible without manual
/// scrolling. Shared by every list/log-style tool.
pub(super) fn render_scrollback(
    frame: &mut Frame,
    title: &str,
    lines: Vec<Line<'static>>,
    active: bool,
    area: Rect,
) {
    // Inner height excludes the top/bottom border rows. Offset so the last
    // `inner_h` lines are shown (approximate for wrapped lines — fine here).
    let inner_h = area.height.saturating_sub(2) as usize;
    let scroll = lines.len().saturating_sub(inner_h) as u16;
    let content = lines.len().min(u16::MAX as usize) as u16;

    let tag = pos_tag(scroll, inner_h as u16, content);
    let para = Paragraph::new(lines)
        .block(tactical_block(title, Some(tag), active))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(para, area);
    render_scrollbar(frame, area, content, inner_h as u16, scroll);
}

/// Total visual rows `lines` occupy once wrapped at `width` — so PageDown/End can
/// reach the true bottom of wrapped content (line count alone under-counts).
pub(super) fn wrapped_height(lines: &[Line], width: u16) -> usize {
    if width == 0 {
        return lines.len();
    }
    let w = width as usize;
    lines
        .iter()
        .map(|l| match l.width() {
            0 => 1,
            lw => lw.div_ceil(w),
        })
        .sum()
}

/// Render a scrollable text pane: like [`render_scrollback`] but driven by a
/// [`Scroll`] (operator PageUp/PageDown/Home/End) rather than always pinning to
/// the bottom. The `scroll` clamps itself to the content/viewport at render time.
pub(super) fn render_scroll(
    frame: &mut Frame,
    title: &str,
    lines: Vec<Line<'static>>,
    active: bool,
    scroll: &Scroll,
    area: Rect,
) {
    let inner_h = area.height.saturating_sub(2);
    let inner_w = area.width.saturating_sub(2);
    let content = wrapped_height(&lines, inner_w).min(u16::MAX as usize) as u16;
    let off = scroll.visible(content, inner_h);
    let tag = pos_tag(off, inner_h, content);
    let para = Paragraph::new(lines)
        .block(tactical_block(title, Some(tag), active))
        .wrap(Wrap { trim: false })
        .scroll((off, 0));
    frame.render_widget(para, area);
    render_scrollbar(frame, area, content, inner_h, off);
}
