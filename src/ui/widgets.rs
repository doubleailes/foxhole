//! Shared drawing primitives: the 7-bit ASCII border set, the bordered pane
//! block (with active-pane highlight), and the two scrollback renderers
//! (bottom-pinned vs. operator-scrolled). Every tool body draws through these.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::Scroll;

/// Pure 7-bit ASCII border set. Used by every pane so the layout is stable on
/// terminals with no box-drawing glyphs.
pub(super) const ASCII_BORDER: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// A centered `width`×`height` rectangle within `area` (clamped to fit).
pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

/// A bordered block carrying the shared ASCII border set, with the active pane
/// flagged by reversing its border and title.
pub(super) fn pane_block(title: &str, active: bool) -> Block<'_> {
    let mut title_style = Style::default().add_modifier(Modifier::BOLD);
    let mut border_style = Style::default();
    if active {
        title_style = title_style.add_modifier(Modifier::REVERSED);
        border_style = border_style.add_modifier(Modifier::REVERSED);
    }
    Block::default()
        .borders(Borders::ALL)
        .border_set(ASCII_BORDER)
        .border_style(border_style)
        .title(Span::styled(format!(" {title} "), title_style))
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
