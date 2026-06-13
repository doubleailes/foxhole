//! Micron renderer (Nomad Network page markup) → ratatui `Line`s.
//!
//! A faithful subset of Nomad Network's `MicronParser.py`: the backtick `` ` ``
//! introduces a one-character formatting command (`!` bold, `*` italic, `_`
//! underline, `F`/`B` foreground/background colour, `f`/`b` reset colour,
//! `` `` `` reset all, `c`/`l`/`r`/`a` alignment), `` `[label`url]`` links,
//! and `` `<…`…>`` input fields. Line-level: `>` headings (repeat for depth),
//! `#` comments, a leading `-` divider (`-X` sets the fill char), `\` escape,
//! and `` `= `` literal blocks.
//!
//! Visuals follow the NomadNet dark theme: light-grey (`ddd`) body text,
//! sections rendered as **full-width depth-shaded heading bars** with their
//! bodies indented by depth, and `─`-filled dividers — hence `render` takes the
//! viewport `width`. Colours use 3-nibble (`f80`), grayscale (`g50`) and
//! truecolor (`Tff8800`) forms. Links render in the page's own style (underlined,
//! reversed when selected), are numbered in document order so the Browser can
//! follow one ([`link_targets`]). Input fields are skipped and remote partials
//! show a placeholder (forms are a later phase). Anything unrecognised is
//! dropped — never fatal, never leaks the control byte.

use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// NomadNet dark-theme default foreground (`ddd`) — light-grey body text.
const DEFAULT_FG: Color = Color::Rgb(0xdd, 0xdd, 0xdd);
/// Section-content indent per depth level (NomadNet `SECTION_INDENT`).
const SECTION_INDENT: usize = 2;
/// Default horizontal-divider glyph (NomadNet uses U+2500); a page's `-X`
/// overrides it.
const DIVIDER_CH: char = '\u{2500}';

/// Body text style — the default grey, the base every line builds on.
fn body_style() -> Style {
    Style::default().fg(DEFAULT_FG)
}

/// `(bg, fg)` for a section heading at `depth`, shading darker with depth —
/// the NomadNet dark theme (`STYLES_DARK` heading1/2/3).
fn heading_palette(depth: usize) -> (Color, Color) {
    match depth {
        0 | 1 => (Color::Rgb(0xbb, 0xbb, 0xbb), Color::Rgb(0x22, 0x22, 0x22)),
        2 => (Color::Rgb(0x99, 0x99, 0x99), Color::Rgb(0x11, 0x11, 0x11)),
        _ => (Color::Rgb(0x77, 0x77, 0x77), Color::Rgb(0, 0, 0)),
    }
}

/// Cross-line link bookkeeping for a render pass: links are numbered in document
/// order so the Browser can select one, and their targets are collected for
/// navigation.
struct LinkWalk {
    /// Index assigned to the next link encountered.
    next: usize,
    /// The link index to highlight (`REVERSED`), if any.
    selected: Option<usize>,
    /// Each link's resolvable target (the `url` of `label`url`fields`), in order.
    targets: Vec<String>,
}

/// Render micron `source` into display lines, optionally highlighting the
/// `selected` link (by document-order index). `width` is the viewport width
/// (full-width heading bars and section dividers are filled to it).
pub fn render(source: &str, selected: Option<usize>, width: u16) -> Vec<Line<'static>> {
    let mut literal = false;
    let mut depth = 0usize;
    let mut walk = LinkWalk {
        next: 0,
        selected,
        targets: Vec::new(),
    };
    source
        .split('\n')
        .map(|raw| {
            render_line(
                raw.strip_suffix('\r').unwrap_or(raw),
                &mut literal,
                &mut depth,
                &mut walk,
                width,
            )
        })
        .collect()
}

/// The resolvable target of each link in `source`, in document order — what the
/// Browser follows. No ratatui types, so callers outside the render path stay
/// styling-free.
pub fn link_targets(source: &str) -> Vec<String> {
    let mut literal = false;
    let mut depth = 0usize;
    let mut walk = LinkWalk {
        next: 0,
        selected: None,
        targets: Vec::new(),
    };
    for raw in source.split('\n') {
        let _ = render_line(
            raw.strip_suffix('\r').unwrap_or(raw),
            &mut literal,
            &mut depth,
            &mut walk,
            0,
        );
    }
    walk.targets
}

/// Render one source line, threading the cross-line `literal` block + section
/// `depth` state and the link walk.
fn render_line(
    line: &str,
    literal: &mut bool,
    depth: &mut usize,
    walk: &mut LinkWalk,
    width: u16,
) -> Line<'static> {
    // `= toggles a literal block; `\`=` inside one is an escaped literal marker.
    if line == "`=" {
        *literal = !*literal;
        return Line::raw("");
    }
    if *literal {
        let text = if line == "\\`=" { "`=" } else { line };
        return Line::styled(text.to_string(), body_style());
    }
    if line.is_empty() {
        return Line::raw("");
    }

    let chars: Vec<char> = line.chars().collect();

    // A leading backslash escapes the line's first control char.
    if chars[0] == '\\' {
        return body_line(&chars[1..], true, *depth, walk);
    }

    match chars[0] {
        '#' => Line::raw(""), // comment
        '`' if chars.get(1) == Some(&'{') => {
            Line::styled("[partial]", body_style().add_modifier(Modifier::DIM))
        }
        '<' => {
            // Section-depth reset, then render the remainder as body.
            *depth = 0;
            body_line(&chars[1..], false, 0, walk)
        }
        '>' => {
            let d = chars.iter().take_while(|&&c| c == '>').count();
            *depth = d;
            render_heading(&chars[d..], d, width, walk)
        }
        '-' => render_divider(&chars, *depth, width),
        _ => body_line(&chars, false, *depth, walk),
    }
}

/// A normal text line, indented by its section depth.
fn body_line(chars: &[char], pre_escape: bool, depth: usize, walk: &mut LinkWalk) -> Line<'static> {
    let (mut spans, align) = make_output(chars, body_style(), pre_escape, walk);
    let indent = depth.saturating_sub(1) * SECTION_INDENT;
    if indent > 0 {
        spans.insert(0, Span::styled(" ".repeat(indent), body_style()));
    }
    Line::from(spans).alignment(align)
}

/// A `>`-section heading: a full-width depth-shaded bar, its (indented, aligned)
/// title painted over the heading background.
fn render_heading(
    content: &[char],
    depth: usize,
    width: u16,
    walk: &mut LinkWalk,
) -> Line<'static> {
    if content.is_empty() {
        return Line::raw("");
    }
    let (bg, fg) = heading_palette(depth);
    let base = Style::default().fg(fg).bg(bg);
    let (mut spans, align) = make_output(content, base, false, walk);

    let bar = Style::default().bg(bg);
    let indent = depth.saturating_sub(1) * SECTION_INDENT;
    if indent > 0 {
        spans.insert(0, Span::styled(" ".repeat(indent), bar));
    }
    // Fill the rest of the row with the heading background, placing the title per
    // its alignment (indent + title is the content block).
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let total = (width as usize).max(used);
    let slack = total - used;
    let (left, right) = match align {
        Alignment::Center => (slack / 2, slack - slack / 2),
        Alignment::Right => (slack, 0),
        Alignment::Left => (0, slack),
    };
    if left > 0 {
        spans.insert(0, Span::styled(" ".repeat(left), bar));
    }
    if right > 0 {
        spans.push(Span::styled(" ".repeat(right), bar));
    }
    Line::from(spans)
}

/// A horizontal divider, filled to the (section-indented) width. A page's `-X`
/// sets the fill char; a bare `-` uses the default rule glyph.
fn render_divider(chars: &[char], depth: usize, width: u16) -> Line<'static> {
    let fill = match chars.get(1) {
        Some(&c) if chars.len() == 2 && !c.is_control() => c,
        _ => DIVIDER_CH,
    };
    let indent = depth.saturating_sub(1) * SECTION_INDENT;
    let len = (width as usize).saturating_sub(indent * 2).max(1);
    let mut s = " ".repeat(indent);
    s.extend(std::iter::repeat_n(fill, len));
    Line::styled(s, body_style())
}

/// Mutable inline-formatting state, turned into a `Style` for each text run.
#[derive(Clone, Copy, Default)]
struct Fmt {
    bold: bool,
    italic: bool,
    underline: bool,
    fg: Option<Color>,
    bg: Option<Color>,
}

impl Fmt {
    fn style(self, base: Style) -> Style {
        let mut s = base;
        if self.bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.underline {
            s = s.add_modifier(Modifier::UNDERLINED);
        }
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        s
    }
}

/// Parse one line's inline micron into styled spans plus its alignment. Mirrors
/// `MicronParser.make_output`: each backtick consumes exactly one command (plus
/// its arguments), so nothing desyncs and no control byte leaks.
fn make_output(
    chars: &[char],
    base: Style,
    pre_escape: bool,
    walk: &mut LinkWalk,
) -> (Vec<Span<'static>>, Alignment) {
    let mut spans: Vec<Span> = Vec::new();
    let mut part = String::new();
    let mut fmt = Fmt::default();
    let mut align = Alignment::Left;
    let mut escape = pre_escape;
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !part.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut part), fmt.style(base)));
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            if escape {
                part.push('\\');
                escape = false;
            } else {
                escape = true;
            }
            i += 1;
            continue;
        }
        if c == '`' && escape {
            part.push('`');
            escape = false;
            i += 1;
            continue;
        }
        if c != '`' {
            part.push(c);
            escape = false;
            i += 1;
            continue;
        }

        // Backtick — a formatting command. Emit pending text first.
        flush!();
        i += 1;
        let Some(&cmd) = chars.get(i) else { break };
        i += 1;
        match cmd {
            '!' => fmt.bold = !fmt.bold,
            '*' => fmt.italic = !fmt.italic,
            '_' => fmt.underline = !fmt.underline,
            'f' => fmt.fg = None,
            'b' => fmt.bg = None,
            'F' => fmt.fg = read_color(chars, &mut i),
            'B' => fmt.bg = read_color(chars, &mut i),
            '`' => {
                fmt = Fmt::default();
                align = Alignment::Left;
            }
            'c' => align = Alignment::Center,
            'l' => align = Alignment::Left,
            'r' => align = Alignment::Right,
            'a' => align = Alignment::Left, // default alignment
            '<' => skip_field(chars, &mut i),
            '[' => {
                let (label, url) = read_link(chars, &mut i);
                // Number every link with a target so the Browser can address it,
                // even if its label is empty (then show the url).
                let idx = walk.next;
                walk.next += 1;
                walk.targets.push(url.clone());
                let shown = if label.is_empty() { url } else { label };
                // Links inherit the page's own styling (NomadNet does the same),
                // with an underline to flag them; the selected link is reversed.
                let mut style = fmt.style(base).add_modifier(Modifier::UNDERLINED);
                if walk.selected == Some(idx) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                spans.push(Span::styled(shown, style));
            }
            _ => {} // unknown single-char tag — consumed, ignored
        }
    }
    flush!();
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    (spans, align)
}

/// Read a micron colour spec after `` `F``/`` `B``, advancing past exactly its
/// bytes: `T` + 6 hex (truecolor), `g` + 2 decimal (grayscale), else 3 nibbles.
fn read_color(chars: &[char], i: &mut usize) -> Option<Color> {
    if chars.get(*i) == Some(&'T') {
        *i += 1;
        let spec = take(chars, i, 6);
        return hex6(&spec);
    }
    let spec = take(chars, i, 3);
    if spec.first() == Some(&'g') {
        // Grayscale: two decimal digits scaled 0..=99 → 0..=255.
        let n: String = spec[1..].iter().collect();
        let v = n.parse::<u16>().ok()?.min(99);
        let g = (v * 255 / 99) as u8;
        return Some(Color::Rgb(g, g, g));
    }
    // Three hex nibbles, each scaled to a byte (n * 17).
    let r = spec.first()?.to_digit(16)? as u8 * 17;
    let g = spec.get(1)?.to_digit(16)? as u8 * 17;
    let b = spec.get(2)?.to_digit(16)? as u8 * 17;
    Some(Color::Rgb(r, g, b))
}

/// `#rrggbb` truecolor from six hex chars.
fn hex6(spec: &[char]) -> Option<Color> {
    let h =
        |a: char, b: char| -> Option<u8> { Some((a.to_digit(16)? * 16 + b.to_digit(16)?) as u8) };
    Some(Color::Rgb(
        h(*spec.first()?, *spec.get(1)?)?,
        h(*spec.get(2)?, *spec.get(3)?)?,
        h(*spec.get(4)?, *spec.get(5)?)?,
    ))
}

/// Take up to `n` chars from `chars` at `*i`, advancing `*i` past them.
fn take(chars: &[char], i: &mut usize, n: usize) -> Vec<char> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        match chars.get(*i) {
            Some(&c) => {
                out.push(c);
                *i += 1;
            }
            None => break,
        }
    }
    out
}

/// Skip an input field `` `<…`…>`` up to and including its closing `>`.
fn skip_field(chars: &[char], i: &mut usize) {
    while let Some(&c) = chars.get(*i) {
        *i += 1;
        if c == '>' {
            break;
        }
    }
}

/// Read a link `` `[label`url`fields]`` into `(label, url)` — `label` empty when
/// none is given (the caller then shows the url). Advances past the closing `]`.
fn read_link(chars: &[char], i: &mut usize) -> (String, String) {
    let start = *i;
    let mut j = *i;
    while j < chars.len() && chars[j] != ']' {
        j += 1;
    }
    let data: String = chars[start..j].iter().collect();
    *i = if j < chars.len() { j + 1 } else { j };

    let mut parts = data.split('`');
    let first = parts.next().unwrap_or("").to_string();
    match parts.next() {
        // `label`url[`fields]` — explicit label and target.
        Some(url) => (first, url.to_string()),
        // `url]` with no separator — the url is its own label.
        None => (String::new(), first),
    }
}

#[cfg(test)]
mod tests {
    use super::{link_targets, render as render_sel};
    use ratatui::style::Modifier;

    /// 1-arg render (no link highlighted, fixed width) for text assertions.
    fn render(source: &str) -> Vec<ratatui::text::Line<'static>> {
        render_sel(source, None, 80)
    }

    /// Concatenated visible text of a rendered line.
    fn text_of(line: &ratatui::text::Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn heading_strips_markers_and_keeps_text() {
        assert!(text_of(&render(">Messages")[0]).contains("Messages"));
        assert!(text_of(&render(">>Deep")[0]).contains("Deep"));
    }

    #[test]
    fn heading_is_a_full_width_bar() {
        let line = &render_sel(">Hi", None, 20)[0];
        assert_eq!(line.width(), 20, "filled to the viewport width");
        assert!(
            line.spans.iter().any(|s| s.style.bg.is_some()),
            "carries a heading background"
        );
        assert!(text_of(line).contains("Hi"));
    }

    #[test]
    fn section_body_is_indented_by_depth() {
        // A depth-2 heading sets the section; the following body line indents 2.
        let lines = render_sel(">>Sec\nbody text", None, 40);
        assert!(text_of(&lines[1]).starts_with("  body text"));
    }

    #[test]
    fn comment_lines_become_blank() {
        assert_eq!(text_of(&render("# a comment")[0]), "");
    }

    #[test]
    fn divider_uses_rule_glyph_filled_to_width() {
        let t = text_of(&render("---")[0]);
        assert!(t.chars().all(|c| c == '\u{2500}') && !t.is_empty());
    }

    #[test]
    fn divider_dash_x_uses_fill_char() {
        let t = text_of(&render("-=")[0]);
        assert!(t.chars().all(|c| c == '=') && !t.is_empty());
    }

    #[test]
    fn inline_formatting_is_stripped_from_text() {
        assert_eq!(text_of(&render("a`!b`!c")[0]), "abc");
    }

    #[test]
    fn three_char_color_consumes_exactly_three() {
        // From a real page: bold, fg 222, bg ddd, center, then text.
        assert_eq!(text_of(&render("`!`F222`Bddd`cNomadNet")[0]), "NomadNet");
    }

    #[test]
    fn truecolor_spec_is_consumed() {
        assert_eq!(text_of(&render("`FTff8800hello")[0]), "hello");
    }

    #[test]
    fn reset_tag_strips_cleanly() {
        assert_eq!(text_of(&render("`F222red`` end")[0]), "red end");
    }

    #[test]
    fn link_renders_label_only() {
        assert_eq!(
            text_of(&render("see `[Home`:/page/index.mu] now")[0]),
            "see Home now"
        );
    }

    #[test]
    fn unlabelled_link_shows_url() {
        assert_eq!(text_of(&render("`[lxmf@abc123]")[0]), "lxmf@abc123");
    }

    #[test]
    fn input_field_is_skipped() {
        assert_eq!(text_of(&render("name `<8|user`alice>done")[0]), "name done");
    }

    #[test]
    fn escaped_backtick_is_literal() {
        assert_eq!(text_of(&render("a\\`b")[0]), "a`b");
    }

    #[test]
    fn unknown_tag_is_dropped() {
        assert_eq!(text_of(&render("x`zY")[0]), "xY");
    }

    #[test]
    fn trailing_backtick_does_not_panic() {
        assert_eq!(text_of(&render("dangling`")[0]), "dangling");
    }

    #[test]
    fn literal_block_passes_through_verbatim() {
        // Between `= markers, control chars are shown as-is.
        let out = render("`=\n`!not bold`!\n`=");
        assert_eq!(text_of(&out[1]), "`!not bold`!");
    }

    #[test]
    fn link_targets_collected_in_document_order() {
        let src = "`[Home`:/page/index.mu]\nmid\n`[Files`:/page/files.mu`a|b]\n`[plainurl]";
        assert_eq!(
            link_targets(src),
            vec![":/page/index.mu", ":/page/files.mu", "plainurl"],
        );
    }

    #[test]
    fn selected_link_renders_reversed() {
        let src = "`[One`:/a] `[Two`:/b]";
        // Highlight the second link (index 1).
        let line = &render_sel(src, Some(1), 80)[0];
        let two = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "Two")
            .expect("second link present");
        let one = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "One")
            .expect("first link present");
        assert!(two.style.add_modifier.contains(Modifier::REVERSED));
        assert!(!one.style.add_modifier.contains(Modifier::REVERSED));
    }
}
