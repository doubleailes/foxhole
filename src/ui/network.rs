//! Network tool body: known delivery peers and propagation nodes in two
//! keyboard-navigable columns, each row carrying a last-seen UTC stamp, with a
//! legend + last-probe footer.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, NetColumn, path_summary};

use super::style::{fmt_time, tag_style, ts_style};
use super::widgets::pane_block;

/// Network tool: known delivery peers and propagation nodes in two keyboard-
/// navigable columns, each row carrying a last-seen UTC stamp. The focused
/// column (`net_col`) gets the reversed border + an active row highlight.
/// Offline (no `net`) the lists stay seeded/empty until live discovery feeds
/// them. See [`crate::app::App::handle_network_key`] for the bindings.
pub(super) fn render_network(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // this-node address
            Constraint::Min(3),    // the two columns
            Constraint::Length(2), // legend + last probe result
        ])
        .split(area);

    // This node's address — what the operator hands to peers.
    let addr = app
        .local_address
        .as_deref()
        .unwrap_or("(starting — address pending)");
    let header = Line::from(vec![
        Span::styled(
            "THIS NODE (lxmf.delivery): ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(addr.to_string()),
    ]);
    frame.render_widget(Paragraph::new(header), rows[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    render_peer_column(frame, app, cols[0]);
    render_node_column(frame, app, cols[1]);

    // Footer: legend, then the most recent path probe result (if any).
    let legend = Line::styled(
        "[Tab/<>] column  [Up/Dn] select  [Enter] open/set  [p] path  [s] sync",
        ts_style(),
    );
    frame.render_widget(Paragraph::new(vec![legend, last_probe_line(app)]), rows[2]);
}

/// Left column: known `lxmf.delivery` peers (the conversations roster).
fn render_peer_column(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.net_col == NetColumn::Peers;
    let lines: Vec<Line> = if app.conversations.is_empty() {
        vec![Line::raw("  (none discovered)")]
    } else {
        app.conversations
            .iter()
            .enumerate()
            .map(|(i, c)| {
                net_row(
                    &c.label(),
                    &c.peer,
                    c.last_seen,
                    hop_hint(app, &c.peer),
                    i == app.selected,
                    focused,
                )
            })
            .collect()
    };
    let para = Paragraph::new(lines).block(pane_block("PEERS (lxmf.delivery)", focused));
    frame.render_widget(para, area);
}

/// Right column: `lxmf.propagation` store-and-forward nodes.
fn render_node_column(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.net_col == NetColumn::Nodes;
    let active = app.config.propagation_node.as_deref();
    let lines: Vec<Line> = if app.nodes.is_empty() {
        vec![Line::raw("  (none discovered)")]
    } else {
        app.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let mut tail = String::new();
                if active == Some(n.hash.as_str()) {
                    tail.push_str(" [active]");
                }
                tail.push_str(&hop_hint(app, &n.hash));
                let name = n.name.as_deref().unwrap_or("?");
                net_row(
                    name,
                    &n.hash,
                    n.last_seen,
                    tail,
                    i == app.node_selected,
                    focused,
                )
            })
            .collect()
    };
    let para = Paragraph::new(lines).block(pane_block("PROPAGATION NODES", focused));
    frame.render_widget(para, area);
}

/// One roster row: `> name   hash8.. HH:MM:SSZ <tail>`. The selected row is
/// reversed only while its column holds focus, so the active column is obvious.
fn net_row(
    name: &str,
    hash: &str,
    last_seen: u64,
    tail: String,
    selected: bool,
    focused: bool,
) -> Line<'static> {
    let marker = if selected { "> " } else { "  " };
    let h8 = hash.get(..8).unwrap_or(hash);
    let ts = match last_seen {
        0 => "--:--:--".to_string(),
        t => format!("{}Z", fmt_time(t)),
    };
    let text = format!("{marker}{name:<10.10} {h8}.. {ts}{tail}");
    let mut style = Style::default();
    if selected && focused {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Line::from(Span::styled(text, style))
}

/// Compact per-row path indicator from the last probe: ` 3h`, ` x` (no path),
/// or empty when never probed.
fn hop_hint(app: &App, hash: &str) -> String {
    match app.path_probes.get(hash) {
        Some(p) => match p.hops {
            Some(n) => format!(" {n}h"),
            None => " x".to_string(),
        },
        None => String::new(),
    }
}

/// The most recent path probe, formatted for the Network footer (or blank).
fn last_probe_line(app: &App) -> Line<'static> {
    match app.path_probes.iter().max_by_key(|(_, p)| p.at) {
        Some((hash, p)) => {
            let h8 = hash.get(..8).unwrap_or(hash);
            let summary = path_summary(p.hops, p.iface.as_deref());
            Line::from(vec![
                Span::styled("[RT] ", tag_style("RT")),
                Span::raw(format!("PATH {h8}..: {summary}  ")),
                Span::styled(format!("{}Z", fmt_time(p.at)), ts_style()),
            ])
        }
        None => Line::raw(""),
    }
}
