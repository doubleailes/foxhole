//! Rendering layer.
//!
//! Pure functions of `&App` → frame. Field constraints:
//!   * **7-bit ASCII only** — ratatui's default borders are Unicode line-draw
//!     glyphs that corrupt on legacy serial terminals, so every pane uses the
//!     `ASCII_BORDER` set below (`+ - |`).
//!   * **Tactical palette** — scrollback content is tinted by category (see
//!     [`tag_style`]): RX/TX traffic, delivery, link/routing, config, warnings,
//!     errors, with muted timestamps. Structure (borders, active-pane
//!     `REVERSED`, titles) stays glyph-only so it still reads on a mono display.
//!
//! Layout has two tiers, mirroring Nomadnet: a tab strip selects the active
//! [`Tool`], whose body fills the middle; a shared status bar pins the bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::{App, BrowserPane, NetColumn, PageStatus, Pane, Scroll, Tool, path_summary};

/// Pure 7-bit ASCII border set. Used by every pane so the layout is stable on
/// terminals with no box-drawing glyphs.
const ASCII_BORDER: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Draw the whole interface: the tab strip, the active tool's body (fills all
/// slack), and a fixed status bar.
pub fn render(frame: &mut Frame, app: &App) {
    // The cold-boot splash owns the whole frame until it hands off to console.
    #[cfg(feature = "splash")]
    if app.state == crate::app::AppState::Splash {
        crate::splash::render(frame, app);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Tab strip
            Constraint::Min(3),    // Active tool body — takes all slack
            Constraint::Length(3), // Status / Metadata bar (1 text row + borders)
        ])
        .split(frame.area());

    render_tab_strip(frame, app, chunks[0]);
    render_tool(frame, app, chunks[1]);
    render_status(frame, app, chunks[2]);

    // A propagation sync, when running, overlays a small centered pop-up.
    if let Some(ref status) = app.sync_status {
        render_sync_popup(frame, status);
    }
    // The New Conversation modal is on top of everything when open.
    if let Some(ref nc) = app.new_conv {
        render_new_conv_popup(frame, nc);
    }
}

/// Modal for adding a conversation by LXMF address (Ctrl+O). The focused field
/// carries a synthetic reversed caret (the real cursor stays hidden).
fn render_new_conv_popup(frame: &mut Frame, nc: &crate::app::NewConv) {
    use crate::app::NewConvField;

    let area = centered_rect(60, 8, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ASCII_BORDER)
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
            "  invalid address — need 32 hex characters",
            tag_style("ERR"),
        ));
    } else {
        lines.push(Line::raw(
            "  destination hash, e.g. a1b2…  (colons/spaces ok)",
        ));
    }
    lines.push(Line::raw("  [Tab] field   [Enter] open   [Esc] cancel"));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// A centered `width`×`height` rectangle within `area` (clamped to fit).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

/// Modal pop-up shown while a propagation sync is in progress.
fn render_sync_popup(frame: &mut Frame, status: &str) {
    let area = centered_rect(48, 4, frame.area());
    frame.render_widget(Clear, area); // blank the cells underneath
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ASCII_BORDER)
        .border_style(Style::default().add_modifier(Modifier::BOLD))
        .title(Span::styled(
            " PROPAGATION SYNC ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let lines = vec![
        Line::raw(format!("  {status}")),
        Line::raw("  pulling messages — please wait"),
    ];
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// Top menu strip. Each tool's title is shown left to right; the active one is
/// reversed. Separated by a plain ASCII pipe so it reads on a serial console.
fn render_tab_strip(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, tool) in Tool::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" | "));
        }
        let label = format!(" {} ", tool.title());
        let style = if *tool == app.active {
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(label, style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Dispatch to the active tool's body renderer. Each tool owns its internal
/// layout, so adding a tool means adding one arm here plus its render fn.
fn render_tool(frame: &mut Frame, app: &App, area: Rect) {
    match app.active {
        Tool::Conversations => render_conversations(frame, app, area),
        Tool::Network => render_network(frame, app, area),
        Tool::Browser => render_browser(frame, app, area),
        Tool::Log => render_log(frame, app, area),
        Tool::Interfaces => render_interfaces(frame, app, area),
        Tool::Guide => render_guide(frame, app, area),
    }
}

/// A bordered block carrying the shared ASCII border set, with the active pane
/// flagged by reversing its border and title.
fn pane_block(title: &str, active: bool) -> Block<'_> {
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
fn render_scrollback(
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
fn wrapped_height(lines: &[Line], width: u16) -> usize {
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
fn render_scroll(
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

/// Plain (untinted) scrollback lines for static panes (Network/Interfaces/Guide).
fn plain_lines<I, S>(lines: I) -> Vec<Line<'static>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    lines.into_iter().map(|s| Line::raw(s.into())).collect()
}

/// One timestamped, tactically-tinted scrollback entry: muted `HH:MM:SS` UTC,
/// the body coloured by its category, and a delivery-status token for outbound
/// messages.
fn styled_entry(e: &crate::app::Entry) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("{}  ", fmt_time(e.at)), ts_style()),
        Span::styled(e.text.clone(), line_style(&e.text)),
    ];
    if let Some((label, cat)) = status_token(e.status) {
        spans.push(Span::styled(format!("  {label}"), tag_style(cat)));
    }
    Line::from(spans)
}

/// Inline label + palette category for an outbound message's status (`None` for
/// inbound/system lines, which carry no marker).
fn status_token(status: crate::app::MsgStatus) -> Option<(&'static str, &'static str)> {
    use crate::app::MsgStatus;
    match status {
        MsgStatus::None => None,
        MsgStatus::Sending => Some(("[sending]", "OPS")),
        MsgStatus::Sent => Some(("[sent]", "OPS")),
        MsgStatus::Delivered => Some(("[delivered]", "DLV")),
        MsgStatus::Propagated => Some(("[propagated]", "CFG")),
        MsgStatus::Failed => Some(("[failed]", "ERR")),
    }
}

/// Muted grey-green for timestamps.
fn ts_style() -> Style {
    Style::default().fg(Color::Rgb(102, 112, 102))
}

/// Tactical colour for a category tag (the `TACTICAL_STYLES` field theme).
fn tag_style(tag: &str) -> Style {
    let base = Style::default();
    match tag {
        "RX" => base
            .fg(Color::Rgb(110, 143, 114))
            .add_modifier(Modifier::BOLD), // Field Green
        "TX" | "RT" | "LNK" => base
            .fg(Color::Rgb(79, 107, 88))
            .add_modifier(Modifier::BOLD), // Ranger Green
        "DLV" => base
            .fg(Color::Rgb(143, 166, 122))
            .add_modifier(Modifier::BOLD), // Success Olive
        "CFG" | "SYS" => base
            .fg(Color::Rgb(90, 111, 99))
            .add_modifier(Modifier::BOLD), // Slate-Olive
        "ID" => base
            .fg(Color::Rgb(140, 153, 114))
            .add_modifier(Modifier::BOLD), // Olive Drab
        "SEC" => base
            .fg(Color::Rgb(122, 90, 58))
            .add_modifier(Modifier::BOLD), // Weathered Brown
        "WRN" => base
            .fg(Color::Rgb(159, 139, 60))
            .add_modifier(Modifier::BOLD), // Faded Brass
        "ERR" => base
            .fg(Color::Rgb(122, 62, 62))
            .add_modifier(Modifier::BOLD), // Dark Dried Red
        "OPS" => base
            .fg(Color::Rgb(139, 143, 135))
            .add_modifier(Modifier::DIM), // Desaturated Grey
        _ => base.fg(Color::Rgb(90, 111, 99)),
    }
}

/// The leading `[TAG]` of a line, if any (`[RX] hi` -> `RX`).
fn leading_tag(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(&rest[..end])
}

/// Style for a whole scrollback line. `[RX]`/`[TX]` colour by their tag; system
/// lines (all `[SYS]`) are sub-categorised by keyword so the Log reads tactically.
fn line_style(text: &str) -> Style {
    match leading_tag(text) {
        Some("RX") => tag_style("RX"),
        Some("TX") => tag_style("TX"),
        // Explicit category tags (future) colour directly; everything else
        // (the `[SYS]` lines) is classified by content.
        Some("SYS") | None => tag_style(sys_category(text)),
        Some(other) => tag_style(other),
    }
}

/// Classify a system log line into a tactical category by keyword.
fn sys_category(text: &str) -> &'static str {
    let t = text.to_ascii_lowercase();
    if t.contains("delivered") {
        "DLV"
    } else if t.contains("fail") || t.contains("error") || t.contains("net:") {
        "ERR"
    } else if t.contains("not decodable") || t.contains("too large") {
        "WRN"
    } else if t.contains("path") || t.contains("no key") {
        "RT"
    } else if t.contains("opening")
        || t.contains("depositing")
        || t.contains("sent")
        || t.contains("announce")
        || t.contains("sync")
        || t.contains("online")
        || t.contains("registered")
        || t.contains("transport")
        || t.contains("bringing")
    {
        "OPS"
    } else if t.contains("identified") || t.contains("identit") {
        "ID"
    } else if t.contains("link") {
        "LNK"
    } else if t.contains("config") || t.contains("node ") || t.contains("interfaces") {
        "CFG"
    } else {
        "SYS"
    }
}

/// Conversations tool: a peer list and the selected peer's thread side by side,
/// with the compose buffer spanning the bottom — the Nomadnet layout.
fn render_conversations(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // Peer list + thread
            Constraint::Length(5), // Transmit buffer (compose rows + borders)
        ])
        .split(area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30), // Peer list
            Constraint::Min(0),         // Selected thread
        ])
        .split(rows[0]);

    render_peer_list(frame, app, top[0]);

    // Selected thread: title carries the peer; body is its timestamped scrollback.
    let (title, lines, thread_active) = match app.selected_conv() {
        Some(conv) => (
            format!("THREAD: {} (UTC)", conv.peer),
            conv.messages.iter().map(styled_entry).collect(),
            app.focus == Pane::Thread,
        ),
        None => ("THREAD".to_string(), Vec::new(), false),
    };
    render_scroll(
        frame,
        &title,
        lines,
        thread_active,
        &app.thread_scroll,
        top[1],
    );

    render_transmit(frame, app, rows[1]);
}

/// Format a Unix-seconds timestamp as `HH:MM:SS` UTC, with no external deps.
/// `0` (unknown) renders as `--:--:--`.
fn fmt_time(at: u64) -> String {
    if at == 0 {
        return "--:--:--".to_string();
    }
    let day = at % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}

/// Peer list: one row per conversation. The selected row is prefixed with `>`
/// and reversed (so the active thread is identifiable even when the pane is not
/// focused); unread inbound counts show as a trailing `(N)`.
fn render_peer_list(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .conversations
        .iter()
        .enumerate()
        .map(|(i, conv)| {
            let selected = i == app.selected;
            let marker = if selected { "> " } else { "  " };
            let unread = if conv.unread > 0 {
                format!(" ({})", conv.unread)
            } else {
                String::new()
            };
            let mut style = Style::default();
            if selected {
                style = style.add_modifier(Modifier::REVERSED);
            } else if conv.unread > 0 {
                style = style.add_modifier(Modifier::BOLD);
            }
            Line::from(Span::styled(
                format!("{marker}{}{unread}", conv.label()),
                style,
            ))
        })
        .collect();

    let para = Paragraph::new(lines).block(pane_block("PEERS", app.focus == Pane::PeerList));
    frame.render_widget(para, area);
}

/// Network tool: known delivery peers and propagation nodes in two keyboard-
/// navigable columns, each row carrying a last-seen UTC stamp. The focused
/// column (`net_col`) gets the reversed border + an active row highlight.
/// Offline (no `net`) the lists stay seeded/empty until live discovery feeds
/// them. See [`App::handle_network_key`] for the bindings.
fn render_network(frame: &mut Frame, app: &App, area: Rect) {
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

/// Browser tool: discovered Nomad Network nodes (left) and the current micron
/// page (right). Phase 1 is read-only — `Enter` fetches a node's index page.
fn render_browser(frame: &mut Frame, app: &App, area: Rect) {
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
            "[Tab] nodes  [Up/Dn] link  [Enter] follow  [Bksp] back  [PgUp/PgDn] scroll"
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
                let marker = if selected { "> " } else { "  " };
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
                let selected = if focused { Some(p.link_sel) } else { None };
                // Match the pane's inner width so heading bars/dividers fill it.
                crate::micron::render(src, selected, area.width.saturating_sub(2))
            }
        },
    };
    render_scroll(frame, "PAGE", lines, focused, &app.page_scroll, area);
}

/// Log tool: the system/application log scrollback, timestamped (UTC) and tinted.
fn render_log(frame: &mut Frame, app: &App, area: Rect) {
    let lines = app.syslog.iter().map(styled_entry).collect();
    render_scroll(
        frame,
        "SYSTEM LOG (UTC)",
        lines,
        false,
        &app.log_scroll,
        area,
    );
}

/// Interfaces tool: Reticulum interface status. Stub until the stack reports
/// live interface state.
fn render_interfaces(frame: &mut Frame, _app: &App, area: Rect) {
    render_scrollback(
        frame,
        "INTERFACES",
        plain_lines(["(no interfaces configured)"]),
        false,
        area,
    );
}

/// Guide tool: static help text.
fn render_guide(frame: &mut Frame, app: &App, area: Rect) {
    let lines = [
        "FoxHole — off-grid LXMF comms terminal".to_string(),
        String::new(),
        "Tabs (Ctrl+N / Ctrl+P to switch):".to_string(),
        "  Conversations  Send and receive LXMF messages.".to_string(),
        "  Network        Discovered peers and propagation nodes.".to_string(),
        "  Browser        Read Nomad Network pages (micron).".to_string(),
        "  Log            System and diagnostic messages.".to_string(),
        "  Interfaces     Reticulum interface status.".to_string(),
        "  Guide          This help.".to_string(),
        String::new(),
        "Keys:".to_string(),
        "  Ctrl+N / Ctrl+P  Next / previous tab".to_string(),
        "  Ctrl+O           New conversation by LXMF address".to_string(),
        "  Tab              Cycle panes (Peers / Thread / Transmit; Browser cols)".to_string(),
        "  Up / Down        Select peer / node / link".to_string(),
        "  PgUp / PgDn      Scroll the focused text pane".to_string(),
        "  Home / End       Jump to top / bottom".to_string(),
        "  Enter            Send / open node / follow link".to_string(),
        "  Backspace        Browser: back to previous page".to_string(),
        "  Ctrl+S           Send to selected peer".to_string(),
        "  Ctrl+R           Sync now from propagation node (on demand)".to_string(),
        "  Ctrl+X           Purge compose buffer".to_string(),
        "  Ctrl+Q           Quit".to_string(),
    ];
    render_scroll(
        frame,
        "GUIDE",
        plain_lines(lines),
        false,
        &app.guide_scroll,
        area,
    );
}

/// Single-line metadata strip plus the keybinding legend. Never focusable.
fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let pane = match app.focus {
        Pane::PeerList => "PEERS",
        Pane::Thread => "THREAD",
        Pane::Transmit => "TRANSMIT",
    };
    // `net` reflects whether the protocol stack was compiled in.
    let net = if cfg!(feature = "net") { "ON" } else { "OFF" };

    // Short form of our own address (full one lives in the Network tab).
    let me = match &app.local_address {
        Some(a) if a.len() >= 8 => format!("  ME:{}\u{2026}", &a[..8]),
        Some(a) => format!("  ME:{a}"),
        None => String::new(),
    };
    let text = format!(
        "TOOL:{tool}  PANE:{pane}  NET:{net}  QUEUE:{queue}  PEERS:{peers}{me}  |  Ctrl+N/P:Tab  Ctrl+O:New  Tab:Pane  Up/Dn:Peer  Ctrl+S:Send  Ctrl+R:Sync  Ctrl+X:Purge  Ctrl+Q:Quit",
        tool = app.active.tag(),
        queue = app.outbound.len(),
        peers = app.conversations.len(),
    );

    let para = Paragraph::new(text).block(pane_block("STATUS", false));
    frame.render_widget(para, area);
}

/// Compose pane. Shows the *selected conversation's* draft. The real terminal
/// cursor stays hidden (field constraint), so when focused we paint a synthetic
/// reversed block as the caret.
fn render_transmit(frame: &mut Frame, app: &App, area: Rect) {
    let active = app.focus == Pane::Transmit;
    let draft = app.selected_conv().map(|c| c.draft.as_str()).unwrap_or("");

    let mut spans = vec![Span::raw("> "), Span::raw(draft)];
    if active {
        spans.push(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        ));
    }

    let para = Paragraph::new(Line::from(spans))
        .block(pane_block("TRANSMIT BUFFER", active))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    use super::{
        centered_rect, fmt_time, leading_tag, line_style, status_token, sys_category, tag_style,
        wrapped_height,
    };
    use crate::app::MsgStatus;
    use ratatui::layout::Rect;
    use ratatui::text::Line;

    #[test]
    fn wrapped_height_counts_wrapped_rows() {
        let lines = [Line::raw("ab"), Line::raw("abcdef"), Line::raw("")];
        // width 3: "ab"→1, "abcdef"→2, ""→1  = 4 visual rows.
        assert_eq!(wrapped_height(&lines, 3), 4);
        // width 0 falls back to the raw line count.
        assert_eq!(wrapped_height(&lines, 0), 3);
    }

    #[test]
    fn status_tokens_map_to_palette() {
        assert!(status_token(MsgStatus::None).is_none());
        assert_eq!(status_token(MsgStatus::Sending), Some(("[sending]", "OPS")));
        assert_eq!(
            status_token(MsgStatus::Delivered),
            Some(("[delivered]", "DLV"))
        );
        assert_eq!(
            status_token(MsgStatus::Propagated),
            Some(("[propagated]", "CFG"))
        );
        assert_eq!(status_token(MsgStatus::Failed), Some(("[failed]", "ERR")));
    }

    #[test]
    fn leading_tag_extracts_bracket() {
        assert_eq!(leading_tag("[RX] hi"), Some("RX"));
        assert_eq!(leading_tag("  [SYS] x"), Some("SYS"));
        assert_eq!(leading_tag("no tag"), None);
    }

    #[test]
    fn rx_tx_colour_by_tag() {
        assert_eq!(line_style("[RX] hello"), tag_style("RX"));
        assert_eq!(line_style("[TX] hello"), tag_style("TX"));
    }

    #[test]
    fn system_lines_classified_by_keyword() {
        assert_eq!(sys_category("[SYS] delivered (direct)"), "DLV");
        assert_eq!(sys_category("[SYS] delivery to X failed (timeout)"), "ERR");
        assert_eq!(
            sys_category("[SYS] direct data not decodable as LXMF"),
            "WRN"
        );
        assert_eq!(
            sys_category("[SYS] no key for X yet — requesting path"),
            "RT"
        );
        assert_eq!(sys_category("[SYS] sent to X"), "OPS");
        assert_eq!(
            sys_category("[SYS] peer X identified on inbound link"),
            "ID"
        );
        assert_eq!(sys_category("[SYS] inbound link established"), "LNK");
        assert_eq!(sys_category("[SYS] using existing RNS config"), "CFG");
        assert_eq!(sys_category("[SYS] something else entirely"), "SYS");
        // `[SYS]` lines route through sys_category.
        assert_eq!(line_style("[SYS] delivered (direct)"), tag_style("DLV"));
    }

    #[test]
    fn formats_utc_hms() {
        assert_eq!(fmt_time(0), "--:--:--", "unknown time");
        assert_eq!(fmt_time(3661), "01:01:01");
        assert_eq!(
            fmt_time(86_400 + 3661),
            "01:01:01",
            "time-of-day wraps daily"
        );
        assert_eq!(fmt_time(1_700_000_000), "22:13:20", "known UTC instant");
    }

    #[test]
    fn centered_rect_centers_and_clamps() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let r = centered_rect(48, 4, area);
        assert_eq!((r.x, r.y, r.width, r.height), (16, 10, 48, 4));

        // Clamp to a tiny area without overflow.
        let tiny = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 2,
        };
        let r2 = centered_rect(48, 4, tiny);
        assert_eq!((r2.width, r2.height), (10, 2));
    }
}
