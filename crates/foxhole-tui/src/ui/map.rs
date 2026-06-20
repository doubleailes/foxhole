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
use ratatui::widgets::canvas::{Canvas, Circle, Map, MapResolution, Points};

use crate::app::{Affiliation, App};
use foxhole_map::{CITIES, City, CityKind, MapMarker, MarkerKind, Zone};

use super::style::{ACCENT, BG, BORDER_LIVE, BORDER_REST, INK, base_style, tag_style, ts_style};
use super::widgets::{NOSEL, SEL, count_tag, tactical_block};

/// Land/coastline tint — kept dim so the markers read on top of it.
const LAND: Color = BORDER_REST;
/// Peer marker tint — a bright phosphor green.
const PEER: Color = Color::Rgb(120, 220, 160);
/// Hazard-zone tint — dried tactical red, matching the `ERR` palette.
const ZONE: Color = Color::Rgb(214, 96, 88);
/// Capital-city reference tint — a cool steel blue that sits apart from the
/// green land/markers and the red zones without competing with them.
const CITY_CAP: Color = Color::Rgb(140, 168, 186);
/// Major (non-capital) city reference tint — dimmer still, a hint of a place.
const CITY: Color = Color::Rgb(96, 120, 134);

/// Affiliation → tint for the received-intel layer (design note §10.5): friendly
/// green, hostile red, neutral grey, unknown amber. Colour reinforces the glyph
/// (`Affiliation::glyph`) so meaning still reads on a monochrome display.
fn affil_color(a: Affiliation) -> Color {
    match a {
        Affiliation::Friendly => Color::Rgb(120, 220, 160),
        Affiliation::Hostile => Color::Rgb(214, 96, 88),
        Affiliation::Neutral => Color::Rgb(170, 178, 170),
        Affiliation::Unknown => Color::Rgb(214, 176, 96),
    }
}

/// Format a time-to-stale (seconds) compactly for the INTEL panel: `"5h59m"`,
/// `"12m"`, `"45s"`, or `"stale"` once elapsed.
fn fmt_ttl(secs: i64) -> String {
    if secs <= 0 {
        return "stale".to_string();
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// Current Unix time in whole seconds (UTC) — the clock the TTL readouts count
/// down against. `0` if the system clock predates the epoch.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

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

    // Right column stacks the positions roster over the INTEL roster (local
    // hazard zones + received CoT intel).
    let staged_row = usize::from(!app.intel_staged.is_empty());
    let intel_rows = app.zones.len() + app.live_intel().len() + staged_row;
    let intel_h = (intel_rows as u16 + 2).clamp(3, 14);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(intel_h)])
        .split(cols[1]);

    render_canvas(frame, app, cols[0]);
    render_marker_list(frame, app, right[0]);
    render_intel_list(frame, app, right[1]);

    // Footer: the key legend. Keys are named plainly so none of the separators
    // read as bindings (the bracket keys cycle markers). `i` reviews staged intel.
    let legend = Line::styled(
        "[\u{2190}\u{2191}\u{2193}\u{2192}] pan  [+/-] zoom  [Tab] cycle  [c] center  [g] cities  [a] author  [e] edit  [x] remove  [i] intel  [r] reset",
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
    let zones = app.zones.clone();
    // Received-intel circular overlays (live, affiliation-tinted).
    let intel_zones = app.intel_zones();
    let show_cities = app.map_cities;

    // Pre-split coordinates by kind so each layer is one cheap `Points` draw.
    // `project_lon` shifts each point by ±360 when the viewport straddles the
    // antimeridian, so features near the dateline don't vanish; off-screen ones
    // drop out.
    let peer_pts: Vec<(f64, f64)> = markers
        .iter()
        .filter(|m| m.kind == MarkerKind::Peer)
        .filter_map(|m| Some((view.project_lon(m.pos.lon)?, m.pos.lat)))
        .collect();
    let op_pts: Vec<(f64, f64)> = markers
        .iter()
        .filter(|m| m.kind == MarkerKind::Operator)
        .filter_map(|m| Some((view.project_lon(m.pos.lon)?, m.pos.lat)))
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
            // ratatui's `Map` shape plots coastlines at their canonical −180..180
            // longitudes with no offset, so the wrapped sliver of a dateline-
            // straddling viewport shows no coastline; the operator-critical
            // markers and zones below are projected (`project_lon`) so they
            // remain visible there.
            ctx.draw(&Map {
                resolution: MapResolution::High,
                color: LAND,
            });
            // Capitals/cities reference layer, just above the land and beneath
            // everything operator-critical. Dots for every in-view place (split
            // by kind so each is one cheap `Points` draw, capitals on top), then
            // names that reveal as the operator zooms in past each city's
            // `label_span` — so the globe view stays legible. `project_lon`
            // keeps dateline-straddling viewports populated.
            if show_cities {
                ctx.layer();
                let major_pts: Vec<(f64, f64)> = CITIES
                    .iter()
                    .filter(|c| c.kind == CityKind::Major)
                    .filter_map(|c| Some((view.project_lon(c.lon)?, c.lat)))
                    .collect();
                let capital_pts: Vec<(f64, f64)> = CITIES
                    .iter()
                    .filter(|c| c.kind == CityKind::Capital)
                    .filter_map(|c| Some((view.project_lon(c.lon)?, c.lat)))
                    .collect();
                ctx.draw(&Points {
                    coords: &major_pts,
                    color: CITY,
                });
                ctx.draw(&Points {
                    coords: &capital_pts,
                    color: CITY_CAP,
                });
                for city in CITIES {
                    if view.span <= city.label_span
                        && let Some(lon) = view.project_lon(city.lon)
                    {
                        ctx.print(lon, city.lat, city_label(city));
                    }
                }
            }
            // Hazard zones sit just above the land: a red danger ring per area,
            // then its label, beneath the operator/peer markers so those stay
            // legible on top.
            ctx.layer();
            // Project each zone's centre so dateline-straddling viewports keep
            // showing it (see `MapView::project_lon`).
            for z in &zones {
                if let Some(lon) = view.project_lon(z.center.lon) {
                    ctx.draw(&Circle {
                        x: lon,
                        y: z.center.lat,
                        radius: z.radius_deg(),
                        color: ZONE,
                    });
                }
            }
            for z in &zones {
                if let Some(lon) = view.project_lon(z.center.lon) {
                    ctx.print(
                        lon,
                        z.center.lat,
                        Line::styled(
                            format!("\u{26a0} {}", z.label),
                            Style::default().fg(ZONE).add_modifier(Modifier::BOLD),
                        ),
                    );
                }
            }
            // Received-intel zones ride on the same layer, tinted by affiliation
            // so a hostile AO reads differently from a friendly/neutral one.
            for z in &intel_zones {
                if let Some(lon) = view.project_lon(z.center.lon) {
                    ctx.draw(&Circle {
                        x: lon,
                        y: z.center.lat,
                        radius: (z.radius_km / 111.0).max(0.3),
                        color: affil_color(z.affiliation),
                    });
                }
            }
            ctx.layer();
            ctx.draw(&Points {
                coords: &peer_pts,
                color: PEER,
            });
            ctx.draw(&Points {
                coords: &op_pts,
                color: ACCENT,
            });
            // Received-intel point markers, each dot in its affiliation tint.
            for m in &markers {
                if let MarkerKind::Intel(a) = m.kind
                    && let Some(lon) = view.project_lon(m.pos.lon)
                {
                    ctx.draw(&Points {
                        coords: &[(lon, m.pos.lat)],
                        color: affil_color(a),
                    });
                }
            }
            // Labels ride above the dots; the selected one is lit and chevroned.
            ctx.layer();
            for (i, m) in markers.iter().enumerate() {
                if let Some(lon) = view.project_lon(m.pos.lon) {
                    ctx.print(lon, m.pos.lat, marker_label(m, i == selected));
                }
            }
        });
    frame.render_widget(canvas, area);
}

/// A reference city's on-map label: a kind glyph (capital ring vs. city dot)
/// plus its name, in the dim cool-steel city palette so it never competes with
/// the operator/peer markers drawn above it. Capitals read brighter and bold.
fn city_label(c: &City) -> Line<'static> {
    let (glyph, style) = match c.kind {
        CityKind::Capital => (
            "\u{229b} ", // ⊛ — a national capital
            Style::default().fg(CITY_CAP).add_modifier(Modifier::BOLD),
        ),
        CityKind::Major => (
            "\u{00b7} ", // · — a major city
            Style::default().fg(CITY),
        ),
    };
    Line::styled(format!("{glyph}{}", c.name), style)
}

/// A marker's on-map label: a kind glyph plus the name. The selected marker is
/// drawn lit (bright + bold) with a leading chevron so it stands out among a
/// cluster; the rest take their kind colour.
fn marker_label(m: &MapMarker, selected: bool) -> Line<'static> {
    let glyph = match m.kind {
        MarkerKind::Operator => "\u{25b2}".to_string(), // ▲ — this node
        MarkerKind::Peer => "\u{25c6}".to_string(),     // ◆ — a peer
        MarkerKind::Intel(a) => a.glyph().to_string(),  // affiliation glyph
    };
    let base = match m.kind {
        MarkerKind::Operator => Style::default().fg(ACCENT),
        MarkerKind::Peer => Style::default().fg(PEER),
        MarkerKind::Intel(a) => Style::default().fg(affil_color(a)),
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
        MarkerKind::Operator => "\u{25b2}".to_string(),
        MarkerKind::Peer => "\u{25c6}".to_string(),
        MarkerKind::Intel(a) => a.glyph().to_string(),
    };
    let glyph_style = match m.kind {
        MarkerKind::Operator => Style::default().fg(ACCENT),
        MarkerKind::Peer => tag_style("DLV"),
        MarkerKind::Intel(a) => Style::default().fg(affil_color(a)),
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

/// Bottom-right INTEL roster: local hazard zones (operator-authored, brass) over
/// the received CoT intel layer (affiliation-tinted, with source + time-to-stale)
/// — design note §7. A footer flags any staged intel awaiting review (`i`).
fn render_intel_list(frame: &mut Frame, app: &App, area: Rect) {
    let live = app.live_intel();
    let now = now_secs();
    let ttl = app.config.intel_ttl_secs;

    let mut lines: Vec<Line<'static>> = Vec::new();
    for z in &app.zones {
        lines.push(zone_row(z));
    }
    for r in &live {
        lines.push(intel_row(
            &r.label(),
            r.affiliation(),
            r.source.get(..8).unwrap_or(&r.source),
            r.seconds_to_stale(now, ttl),
        ));
    }
    if !app.intel_staged.is_empty() {
        lines.push(Line::styled(
            format!("  \u{2691} {} staged — [i] review", app.intel_staged.len()),
            tag_style("WRN").add_modifier(Modifier::BOLD),
        ));
    }
    if lines.is_empty() {
        lines.push(Line::styled("  (no intel)", ts_style()));
    }

    let count = app.zones.len() + live.len();
    let para = Paragraph::new(lines).block(tactical_block("INTEL", Some(count_tag(count)), false));
    frame.render_widget(para, area);
}

/// One local-zone row: `⚠ AO ALPHA      LOCAL  r450km`, in the danger-red palette
/// with a `LOCAL` provenance tag (authoritative, operator-authored).
fn zone_row(z: &Zone) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("\u{26a0} {:<12.12}", z.label),
            Style::default().fg(ZONE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" LOCAL", ts_style()),
        Span::styled(format!(" r{:.0}km", z.radius_km), ts_style()),
    ])
}

/// One received-intel row: `◆ AO ALPHA   a1b2c3d4  5h59m`, the glyph + label in
/// the affiliation tint, then the source short-hash and time-to-stale.
fn intel_row(label: &str, affil: Affiliation, source: &str, ttl_secs: i64) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} {:<11.11}", affil.glyph(), label),
            Style::default().fg(affil_color(affil)),
        ),
        Span::styled(format!(" {source:<8.8}"), ts_style()),
        Span::styled(format!(" {:>5}", fmt_ttl(ttl_secs)), ts_style()),
    ])
}
