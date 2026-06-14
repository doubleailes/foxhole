//! Boot splash (behind the default-on `splash` feature).
//!
//! A brief, skippable "cold boot" sequence: FoxHole reveals its field-terminal
//! bring-up one service at a time — encrypted store, identity keys, network
//! interface, mesh stack — then hands off to the console. It doubles as a live
//! readiness monitor: under the `net` feature each line flips to its reported
//! status as the real event arrives (see [`crate::app::App::mark_boot`]), and
//! the console opens the moment the operator address is live. Pure 7-bit ASCII,
//! tactically tinted; no image, no extra dependencies.
//!
//! This module is *pure render* — all timing and state live in [`crate::app`].
//! `main` advances the sequence on a timer and `ui` calls [`render`] while the
//! app is in [`crate::app::AppState::Splash`].

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, BootStep};

/// Inner width of the status bracket — sized to the longest status word.
const STATUS_W: usize = 7;
/// Width of the centered boot-log column: `[STATUS ]` + space + longest label.
const BLOCK_W: u16 = (STATUS_W as u16 + 2) + 1 + 30;

/// Muted tactical green — a service reporting in clean (matches `ui::tag_style`
/// "DLV" success-olive).
const GREEN: Color = Color::Rgb(143, 166, 122);
/// Faded brass / amber — armed but holding (matches `ui::tag_style` "WRN").
const AMBER: Color = Color::Rgb(159, 139, 60);
/// Field-night background — matches the console's `ui::style::BG` so cold boot
/// and the running console share one continuous dark tactical surface.
const BG: Color = Color::Rgb(13, 17, 13);

/// Label and reported status for a boot line.
fn step_text(step: BootStep) -> (&'static str, &'static str) {
    match step {
        BootStep::Boot => ("Booting FoxHole field terminal", "ONLINE"),
        BootStep::SelfTest => ("Power-on self test", "NOMINAL"),
        BootStep::Identity => ("Deriving identity keys", "OK"),
        BootStep::Store => ("Mounting encrypted store", "SEALED"),
        BootStep::Cache => ("Decrypting cached traffic", "OK"),
        BootStep::Iface => ("Arming network interface", "ARMED"),
        BootStep::Mesh => ("Reticulum mesh stack", "STANDBY"),
        BootStep::Console => ("Operator console", "READY"),
    }
}

/// The tactical boot header, 7-bit ASCII.
fn header_lines() -> [&'static str; 2] {
    [
        "FOXHOLE FIELD TERMINAL  //  COLD BOOT",
        "GRID: CONTESTED  //  STAND BY ...",
    ]
}

/// Render the boot sequence: header, then each line that has reported in,
/// left-aligned in a column centered on a clean black field.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    // Lay the field-night surface so the boot screen matches the console.
    frame.render_widget(
        ratatui::widgets::Block::default().style(Style::default().bg(BG)),
        area,
    );

    let mut lines: Vec<Line> = header_lines()
        .iter()
        .map(|h| Line::styled(*h, heading_style()))
        .collect();
    lines.push(Line::raw(""));

    let mut all_done = true;
    for &step in &BootStep::ALL {
        if app.boot_done(step) {
            lines.push(boot_line(step));
        } else {
            all_done = false;
        }
    }
    if all_done {
        lines.push(Line::raw(""));
        lines.push(Line::styled("STAND BY  //  PRESS ANY KEY", footer_style()));
    }

    let h = (lines.len() as u16).min(area.height);
    let w = BLOCK_W.min(area.width);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), rect);
}

/// `[ STATUS ] label`, the status word tinted by disposition.
fn boot_line(step: BootStep) -> Line<'static> {
    let (label, status) = step_text(step);
    let tag = format!("[{status:^STATUS_W$}]");
    Line::from(vec![
        Span::styled(tag, status_style(status)),
        Span::styled(format!(" {label}"), label_style()),
    ])
}

fn heading_style() -> Style {
    Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
}

fn footer_style() -> Style {
    Style::default().fg(AMBER).add_modifier(Modifier::DIM)
}

fn label_style() -> Style {
    Style::default().fg(Color::Rgb(180, 180, 170))
}

/// Green for a clean report, amber for "armed but standing by".
fn status_style(status: &str) -> Style {
    let color = match status {
        "SEALED" | "ARMED" | "STANDBY" => AMBER,
        _ => GREEN,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::{BLOCK_W, STATUS_W, header_lines, step_text};
    use crate::app::BootStep;

    #[test]
    fn header_is_ascii() {
        for line in header_lines() {
            assert!(line.is_ascii(), "header must stay 7-bit ASCII: {line:?}");
            assert!(line.contains("FOXHOLE") || line.contains("GRID"));
        }
    }

    #[test]
    fn steps_are_ascii_and_fit_the_column() {
        for &step in &BootStep::ALL {
            let (label, status) = step_text(step);
            assert!(label.is_ascii(), "label not ASCII: {label:?}");
            assert!(status.is_ascii(), "status not ASCII: {status:?}");
            // Status must fit its bracket field; the whole line within the column.
            assert!(status.len() <= STATUS_W, "status too wide: {status:?}");
            let line_w = (STATUS_W + 2) + 1 + label.len();
            assert!(
                line_w <= BLOCK_W as usize,
                "label overflows column: {label:?}"
            );
        }
    }
}
