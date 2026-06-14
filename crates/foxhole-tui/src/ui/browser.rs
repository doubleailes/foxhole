//! Browser tool body: discovered Nomad Network nodes (left) and the current
//! micron page (right). Phase 1 is read-only — `Enter` fetches a node's index.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, BrowserPane, PageStatus};

use super::style::{fmt_time, tag_style, ts_style};
use super::widgets::{NOSEL, SEL, pane_block, render_scroll};

/// Browser tool: discovered Nomad Network nodes (left) and the current micron
/// page (right). Phase 1 is read-only — `Enter` fetches a node's index page.
pub(super) fn render_browser(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // path + status header
            Constraint::Min(3),    // node list | page viewport
            Constraint::Length(1), // legend
        ])
        .split(area);

    // Header: which page, and how the fetch is going.
    let header = match &app.page {
        Some(p) => {
            let status = match &p.status {
                PageStatus::Fetching => "fetching...",
                PageStatus::Loaded(_) => "ok",
                PageStatus::Error(_) => "error",
            };
            Line::from(vec![
                Span::styled("PAGE ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{}  [{status}]", p.path)),
            ])
        }
        None => Line::styled("PAGE  (select a node, press Enter)", ts_style()),
    };
    frame.render_widget(Paragraph::new(header), rows[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(rows[1]);
    render_nomad_list(frame, app, cols[0]);
    render_page(frame, app, cols[1]);

    let legend = match app.browser_pane {
        BrowserPane::Nodes => "[Tab] page  [Up/Dn] node  [Enter] open  [r] reload",
        BrowserPane::Page => {
            "[Up/Dn] field/link  type to edit  [Enter] follow/submit  [Bksp] back  [PgUp/PgDn] scroll"
        }
    };
    frame.render_widget(Paragraph::new(Line::styled(legend, ts_style())), rows[2]);
}

/// The discovered Nomad node list, with a last-seen UTC stamp per row.
fn render_nomad_list(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = if app.nomad_nodes.is_empty() {
        vec![Line::raw("  (none discovered)")]
    } else {
        app.nomad_nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let selected = i == app.browser_selected;
                let marker = if selected { SEL } else { NOSEL };
                let ts = match n.last_seen {
                    0 => "--:--:--".to_string(),
                    t => format!("{}Z", fmt_time(t)),
                };
                let mut style = Style::default();
                if selected {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                Line::from(Span::styled(
                    format!("{marker}{:<12.12} {ts}", n.label()),
                    style,
                ))
            })
            .collect()
    };
    let focused = app.browser_pane == BrowserPane::Nodes;
    frame.render_widget(
        Paragraph::new(lines).block(pane_block("NODES", focused)),
        area,
    );
}

/// The page viewport: rendered micron (with the selected link highlighted while
/// the Page pane holds focus), or the fetch state.
fn render_page(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.browser_pane == BrowserPane::Page;
    let lines: Vec<Line> = match &app.page {
        None => vec![Line::styled("  no page loaded", ts_style())],
        Some(p) => match &p.status {
            PageStatus::Fetching => vec![Line::styled("  fetching page...", ts_style())],
            PageStatus::Error(e) => vec![Line::styled(format!("  error: {e}"), tag_style("ERR"))],
            PageStatus::Loaded(src) => {
                let focus = if focused { Some(p.element_sel) } else { None };
                // Match the pane's inner width so heading bars/dividers fill it.
                crate::micron::render(src, area.width.saturating_sub(2), focus, &p.field_values)
            }
        },
    };
    render_scroll(frame, "PAGE", lines, focused, &app.page_scroll, area);
}
