//! World Map tool body: an equirectangular world map drawn on a ratatui
//! [`Canvas`] (braille cells, the same monochrome-friendly technique mapscii
//! uses), with the operator and any located peers plotted as labelled markers,
//! plus a side roster of those positions. Pan/zoom/selection state lives in
//! core's [`MapView`](crate::app::MapView); see
//! [`App::handle_map_key`](crate::app::App) for the bindings.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::widgets::canvas::{Canvas, Map, MapResolution, Points};

use crate::app::{App, MapMarker, MarkerKind};

use super::style::{ACCENT, BG, BORDER_LIVE, BORDER_REST, INK, base_style, tag_style, ts_style};
use super::widgets::{NOSEL, SEL, count_tag, tactical_block};

/// Land/coastline tint — kept dim so the markers read on top of it.
const LAND: Color = BORDER_REST;
/// Peer marker tint — a bright phosphor green.
const PEER: Color = Color::Rgb(120, 220, 160);

/// World Map tool: the canvas on the left (most of the width) and a selectable
/// roster of plotted positions on the right.
pub(super) fn render_map(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(34)])
        .split(rows[0]);

    render_canvas(frame, app, cols[0]);
    render_marker_list(frame, app, cols[1]);

    // Footer: the key legend.
    let legend = Line::styled(
        "[\u{2190}\u{2191}\u{2193}\u{2192}] pan  [+/-] zoom  [ [ / ] ] marker  [Enter/c] center  [r] reset",
        ts_style(),
    );
    frame.render_widget(Paragraph::new(legend).style(base_style()), rows[1]);
}

/// The map canvas itself: the world outline, then peer/operator points, then the
/// marker labels on top (selected one lit).
fn render_canvas(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.map;
    let markers = app.map_markers();
    let selected = app.map_selected;

    // Pre-split coordinates by kind so each layer is one cheap `Points` draw.
    let peer_pts: Vec<(f64, f64)> = markers
        .iter()
        .filter(|m| m.kind == MarkerKind::Peer)
        .map(|m| (m.pos.lon, m.pos.lat))
        .collect();
    let op_pts: Vec<(f64, f64)> = markers
        .iter()
        .filter(|m| m.kind == MarkerKind::Operator)
        .map(|m| (m.pos.lon, m.pos.lat))
        .collect();

    // HUD readout: viewport centre + zoom span, in the top-right corner.
    let hud = Span::styled(
        format!(
            " {:.1},{:.1} z{:.0}\u{00b0} ",
            view.center.lat, view.center.lon, view.span
        ),
        ts_style(),
    );

    let canvas = Canvas::default()
        .block(tactical_block("WORLD MAP", Some(hud), true))
        .background_color(BG)
        .marker(Marker::Braille)
        .x_bounds(view.x_bounds())
        .y_bounds(view.y_bounds())
        .paint(move |ctx| {
            ctx.draw(&Map {
                resolution: MapResolution::High,
                color: LAND,
            });
            ctx.layer();
            ctx.draw(&Points {
                coords: &peer_pts,
                color: PEER,
            });
            ctx.draw(&Points {
                coords: &op_pts,
                color: ACCENT,
            });
            // Labels ride above the dots; the selected one is lit and chevroned.
            ctx.layer();
            for (i, m) in markers.iter().enumerate() {
                ctx.print(m.pos.lon, m.pos.lat, marker_label(m, i == selected));
            }
        });
    frame.render_widget(canvas, area);
}

/// A marker's on-map label: a kind glyph plus the name. The selected marker is
/// drawn lit (bright + bold) with a leading chevron so it stands out among a
/// cluster; the rest take their kind colour.
fn marker_label(m: &MapMarker, selected: bool) -> Line<'static> {
    let glyph = match m.kind {
        MarkerKind::Operator => "\u{25b2}", // ▲ — this node
        MarkerKind::Peer => "\u{25c6}",     // ◆ — a peer
    };
    let base = match m.kind {
        MarkerKind::Operator => Style::default().fg(ACCENT),
        MarkerKind::Peer => Style::default().fg(PEER),
    };
    let style = if selected {
        Style::default()
            .fg(BORDER_LIVE)
            .add_modifier(Modifier::BOLD)
    } else {
        base
    };
    let lead = if selected { "\u{25b6}" } else { "" }; // ▶
    Line::styled(format!("{lead}{glyph} {}", m.label), style)
}

/// Right-hand roster of plotted positions: one selectable row per marker with
/// its glyph, label and decimal-degree coordinates. Mirrors the Network roster's
/// look (chevron + reversed selection) so the two tabs feel of a piece.
fn render_marker_list(frame: &mut Frame, app: &App, area: Rect) {
    let markers = app.map_markers();
    let lines: Vec<Line<'static>> = if markers.is_empty() {
        vec![
            Line::raw(""),
            Line::styled("  (no positions yet)", ts_style()),
            Line::raw(""),
            Line::styled("  Set lat/lon in foxhole.conf", ts_style()),
            Line::styled("  to plot this node, or await", ts_style()),
            Line::styled("  peer telemetry over LXMF.", ts_style()),
        ]
    } else {
        markers
            .iter()
            .enumerate()
            .map(|(i, m)| marker_row(m, i == app.map_selected))
            .collect()
    };
    let para = Paragraph::new(lines).block(tactical_block(
        "POSITIONS",
        Some(count_tag(markers.len())),
        false,
    ));
    frame.render_widget(para, area);
}

/// One roster row: `▶ ◆ label   12.34,-5.67`. The selected row is reversed; the
/// operator's own row carries the brass accent.
fn marker_row(m: &MapMarker, selected: bool) -> Line<'static> {
    let marker = if selected { SEL } else { NOSEL };
    let glyph = match m.kind {
        MarkerKind::Operator => "\u{25b2}",
        MarkerKind::Peer => "\u{25c6}",
    };
    let glyph_style = match m.kind {
        MarkerKind::Operator => Style::default().fg(ACCENT),
        MarkerKind::Peer => tag_style("DLV"),
    };
    let mut row = Style::default().fg(INK);
    let mut gstyle = glyph_style;
    if selected {
        row = row.add_modifier(Modifier::REVERSED);
        gstyle = gstyle.add_modifier(Modifier::REVERSED);
    }
    Line::from(vec![
        Span::styled(format!("{marker}{glyph} "), gstyle),
        Span::styled(
            format!("{:<12.12} {:>6.2},{:>7.2}", m.label, m.pos.lat, m.pos.lon),
            row,
        ),
    ])
}
