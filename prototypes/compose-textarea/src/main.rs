//! Evaluation prototype — foxhole's TRANSMIT BUFFER pane, re-backed by
//! `tui-textarea` instead of the current `String::push`/`pop` editor.
//!
//! This is a throwaway, standalone binary (outside the workspace). It exists so
//! the compose UX can be *felt* — multi-line wrapping, word/line motion, kill &
//! yank, undo/redo, selection — and compared against foxhole's current
//! single-character input handling in `app/conversations.rs`.
//!
//! Run it:  `cargo run --manifest-path prototypes/compose-textarea/Cargo.toml`
//!
//! Keys:  Tab = switch TITLE/BODY · Ctrl+S = "transmit" · Esc / Ctrl+C = quit
//! Editing (free from tui-textarea):
//!   ←/→/↑/↓ move · Alt+←/→ word jump · Ctrl+A/E line ends · Ctrl+K kill to EOL
//!   Ctrl+W delete word · Ctrl+U kill line · Ctrl+Z undo · Ctrl+Y redo
//!   Shift+arrows select · Ctrl+C/M/X copy/paste cut (tui-textarea defaults)

use std::io::{self, Stdout};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tui_textarea::{CursorMove, Input, Key, TextArea};

// --- foxhole theme constants (mirrored from foxhole-tui/src/ui/style.rs) ------
const BG: Color = Color::Rgb(9, 12, 9);
const INK: Color = Color::Rgb(205, 224, 190);
const BORDER_REST: Color = Color::Rgb(108, 146, 112);
const BORDER_LIVE: Color = Color::Rgb(190, 244, 150);
const ACCENT: Color = Color::Rgb(208, 182, 86);
const DIM: Color = Color::Rgb(122, 136, 120);

/// Which compose field has the cursor (mirrors core's `TransmitField`).
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Title,
    Body,
}

struct Compose {
    title: TextArea<'static>,
    body: TextArea<'static>,
    field: Field,
    /// Echo of the last "transmitted" payload, like the `[TX]` thread line.
    last_sent: Option<String>,
}

impl Compose {
    fn new() -> Self {
        let mut title = TextArea::default();
        title.set_placeholder_text("(optional subject — Ctrl+T in foxhole)");
        let mut body = TextArea::default();
        body.set_placeholder_text("Type a message. It wraps, scrolls, and edits like a real buffer.");
        Self {
            title,
            body,
            field: Field::Body,
            last_sent: None,
        }
    }

    fn active_mut(&mut self) -> &mut TextArea<'static> {
        match self.field {
            Field::Title => &mut self.title,
            Field::Body => &mut self.body,
        }
    }

    /// Pull the draft out the way `App::transmit` reads `draft`/`draft_title`.
    fn transmit(&mut self) {
        let title = self.title.lines().join(" ").trim().to_string();
        let body = self.body.lines().join("\n").trim().to_string();
        if body.is_empty() {
            return;
        }
        self.last_sent = Some(if title.is_empty() {
            format!("[TX] {body}")
        } else {
            format!("[TX] {title}: {body}")
        });
        // Reset the form, like foxhole does after a send.
        self.title = TextArea::default();
        self.title
            .set_placeholder_text("(optional subject — Ctrl+T in foxhole)");
        self.body = TextArea::default();
        self.body
            .set_placeholder_text("Type a message. It wraps, scrolls, and edits like a real buffer.");
        self.field = Field::Body;
    }
}

fn main() -> io::Result<()> {
    let mut terminal = setup()?;
    let res = run(&mut terminal);
    restore(&mut terminal)?;
    res
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    let mut compose = Compose::new();
    loop {
        terminal.draw(|f| ui(f, &compose))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key {
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return Ok(()),
            KeyEvent {
                code: KeyCode::Char('s'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => compose.transmit(),
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                compose.field = match compose.field {
                    Field::Title => Field::Body,
                    Field::Body => Field::Title,
                };
            }
            other => {
                let input: Input = other.into();
                // The TITLE field is single-line: swallow newlines, mapping
                // Enter to a no-op (foxhole's title is one line too).
                if compose.field == Field::Title && input.key == Key::Enter {
                    continue;
                }
                let on_title = compose.field == Field::Title;
                let area = compose.active_mut();
                area.input(input);
                if on_title {
                    // Defensive: keep the title collapsed to a single row.
                    if area.lines().len() > 1 {
                        area.move_cursor(CursorMove::End);
                    }
                }
            }
        }
    }
}

fn ui(frame: &mut Frame, compose: &Compose) {
    // Paint the field-night surface under everything (foxhole's `style::BG`).
    frame.render_widget(Block::default().style(Style::default().bg(BG)), frame.area());

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(40), Constraint::Length(34)])
        .split(frame.area());

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(3), // title field
            Constraint::Min(6),    // body field
            Constraint::Length(3), // last-sent echo
        ])
        .split(cols[0]);

    render_header(frame, rows[0]);
    render_field(frame, compose, Field::Title, rows[1], "TITLE");
    render_field(frame, compose, Field::Body, rows[2], "TRANSMIT BUFFER");
    render_echo(frame, compose, rows[3]);
    render_legend(frame, cols[1]);
}

fn render_header(frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled("◆ ", Style::default().fg(ACCENT)),
        Span::styled(
            "tui-textarea prototype",
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  —  Tab switch · Ctrl+S send · Esc quit", Style::default().fg(DIM)),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(BORDER_REST))
        .style(Style::default().bg(BG));
    frame.render_widget(Paragraph::new(line).block(block), area);
}

/// Render one compose field. The focused field gets foxhole's lit double rule;
/// the resting one the heavy single rule. tui-textarea draws its own cursor.
fn render_field(frame: &mut Frame, compose: &Compose, field: Field, area: Rect, name: &str) {
    let active = compose.field == field;
    let (border_type, border_color) = if active {
        (BorderType::Double, BORDER_LIVE)
    } else {
        (BorderType::Thick, BORDER_REST)
    };
    let nameplate = if active {
        Style::default().fg(BG).bg(BORDER_LIVE).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(INK)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(format!(" {name} "), nameplate))
        .style(Style::default().bg(BG));

    // Clone the textarea so we can attach the block for this frame (cheap; the
    // buffers are tiny). In a real integration the block would be set in place.
    let mut ta = match field {
        Field::Title => compose.title.clone(),
        Field::Body => compose.body.clone(),
    };
    ta.set_block(block);
    ta.set_style(Style::default().fg(INK).bg(BG));
    ta.set_cursor_line_style(Style::default());
    ta.set_cursor_style(if active {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        // Hide the cursor on the unfocused field.
        Style::default().fg(INK).bg(BG)
    });
    frame.render_widget(&ta, area);
}

fn render_echo(frame: &mut Frame, compose: &Compose, area: Rect) {
    let content = match &compose.last_sent {
        Some(s) => Line::from(Span::styled(s.clone(), Style::default().fg(BORDER_LIVE))),
        None => Line::from(Span::styled(
            "last transmit appears here…",
            Style::default().fg(DIM),
        )),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(BORDER_REST))
        .title(Span::styled(" THREAD ECHO ", Style::default().fg(INK)))
        .style(Style::default().bg(BG));
    frame.render_widget(
        Paragraph::new(content).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

fn render_legend(frame: &mut Frame, area: Rect) {
    let kv = |k: &str, v: &str| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), Style::default().fg(ACCENT)),
            Span::styled(v.to_string(), Style::default().fg(INK)),
        ])
    };
    let lines = vec![
        Line::from(Span::styled(
            "free from tui-textarea",
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        kv("←/→ ↑/↓", "move cursor"),
        kv("Alt+←/→", "word jump"),
        kv("Ctrl+A/E", "line start/end"),
        kv("Ctrl+K", "kill to EOL"),
        kv("Ctrl+U", "kill line"),
        kv("Ctrl+W", "delete word"),
        kv("Ctrl+Z/Y", "undo / redo"),
        kv("Shift+arr", "select"),
        kv("Enter", "newline (body)"),
        Line::from(""),
        Line::from(Span::styled(
            "today (String::push)",
            Style::default().fg(DIM).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled("type + Backspace only", Style::default().fg(DIM))),
        Line::from(Span::styled("no caret motion, no wrap", Style::default().fg(DIM))),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(BORDER_REST))
        .title(Span::styled(" CAPABILITIES ", Style::default().fg(INK)))
        .style(Style::default().bg(BG));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

// --- terminal lifecycle (panic-safe, mirrors foxhole's main.rs) ---------------
fn setup() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Restore the terminal even if a panic unwinds through the draw loop.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_hook(info);
    }));
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
