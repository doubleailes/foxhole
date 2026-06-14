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
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

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

/// A bordered tactical panel. Resting panes wear the heavy [`FRAME_BORDER`];
/// the active pane is flagged by the double-ruled [`FOCUS_BORDER`], a brightened
/// border, a reversed title, and a leading status pip (`◆` live / `·` resting) so
/// focus is legible whether or not the terminal honours colour.
pub(super) fn pane_block(title: &str, active: bool) -> Block<'_> {
    let mut title_style = Style::default().add_modifier(Modifier::BOLD);
    let mut border_style = Style::default();
    let (set, pip) = if active {
        title_style = title_style.add_modifier(Modifier::REVERSED);
        border_style = border_style.add_modifier(Modifier::BOLD);
        (FOCUS_BORDER, "◆")
    } else {
        (FRAME_BORDER, "·")
    };
    Block::default()
        .borders(Borders::ALL)
        .border_set(set)
        .border_style(border_style)
        .title(Span::styled(format!(" {pip} {title} "), title_style))
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

    let para = Paragraph::new(lines)
        .block(pane_block(title, active))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(para, area);
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
    let para = Paragraph::new(lines)
        .block(pane_block(title, active))
        .wrap(Wrap { trim: false })
        .scroll((off, 0));
    frame.render_widget(para, area);
}
