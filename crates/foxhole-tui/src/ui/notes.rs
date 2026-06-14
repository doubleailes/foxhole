//! Notes tool body: the ten-slot scratch buffer. One editable list — Up/Down
//! pick a slot, typing edits the selected one (a synthetic caret marks it).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::notes::SLOTS;

use super::style::ts_style;
use super::widgets::{NOSEL, SEL, pane_block};

/// Notes tool: ten free-text slots (`0`–`9`). The selected row is reversed and
/// carries a caret; empty slots read `(empty)` in muted text. See
/// [`crate::app::App::handle_notes_key`] for the bindings.
pub(super) fn render_notes(frame: &mut Frame, app: &App, area: Rect) {
    let caret = Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED));
    let mut lines: Vec<Line> = Vec::with_capacity(SLOTS + 1);

    for (i, value) in app.notes.slots().iter().enumerate() {
        let selected = i == app.note_selected;
        let marker = if selected { SEL } else { NOSEL };
        let mut spans = vec![Span::raw(format!("{marker}{i}  "))];
        if value.is_empty() {
            spans.push(Span::styled("(empty)", ts_style()));
        } else {
            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            spans.push(Span::styled(value.clone(), style));
        }
        if selected {
            spans.push(caret.clone());
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "[Up/Dn] slot   [type] edit   [Backspace] delete   [Ctrl+X] clear slot",
        ts_style(),
    ));

    let title = format!("NOTES ({}/{} used)", app.notes.count(), SLOTS);
    let para = Paragraph::new(lines).block(pane_block(&title, true));
    frame.render_widget(para, area);
}
