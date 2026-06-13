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
//! Read-only (Phase 1): links render as their label, fields are skipped, remote
//! partials show a placeholder. Colours use 3-nibble (`f80`), grayscale (`g50`)
//! and truecolor (`Tff8800`) forms. Anything unrecognised is dropped — never
//! fatal, never leaks the control byte.

use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Link label colour — links are inert in Phase 1 but still flagged.
const LINK: Color = Color::Rgb(110, 160, 180);
/// Section-heading colour (faded brass), matching the tactical palette.
const HEADING: Color = Color::Rgb(159, 139, 60);
/// Spaces of indent per heading depth level (mirrors `SECTION_INDENT`).
const SECTION_INDENT: usize = 2;
/// Divider width when a `-` line is expanded.
const DIVIDER_W: usize = 48;

/// Render micron `source` into display lines.
pub fn render(source: &str) -> Vec<Line<'static>> {
    let mut literal = false;
    source
        .split('\n')
        .map(|raw| render_line(raw.strip_suffix('\r').unwrap_or(raw), &mut literal))
        .collect()
}

/// Render one source line, threading the cross-line `literal` block state.
fn render_line(line: &str, literal: &mut bool) -> Line<'static> {
    // `= toggles a literal block; `\`=` inside one is an escaped literal marker.
    if line == "`=" {
        *literal = !*literal;
        return Line::raw("");
    }
    if *literal {
        let text = if line == "\\`=" { "`=" } else { line };
        return Line::raw(text.to_string());
    }
    if line.is_empty() {
        return Line::raw("");
    }

    let chars: Vec<char> = line.chars().collect();

    // A leading backslash escapes the line's first control char.
    if chars[0] == '\\' {
        let (spans, align) = make_output(&chars[1..], Style::default(), true);
        return Line::from(spans).alignment(align);
    }

    match chars[0] {
        '#' => Line::raw(""), // comment
        '`' if chars.get(1) == Some(&'{') => {
            Line::styled("[partial]", Style::default().add_modifier(Modifier::DIM))
        }
        '<' => {
            // Section-depth reset, then render the remainder.
            let (spans, align) = make_output(&chars[1..], Style::default(), false);
            Line::from(spans).alignment(align)
        }
        '>' => render_heading(&chars),
        '-' => render_divider(&chars),
        _ => {
            let (spans, align) = make_output(&chars, Style::default(), false);
            Line::from(spans).alignment(align)
        }
    }
}

/// `>`-prefixed heading: depth = number of `>`, the rest is the (formatted) title.
fn render_heading(chars: &[char]) -> Line<'static> {
    let depth = chars.iter().take_while(|&&c| c == '>').count();
    let rest = &chars[depth..];
    if rest.is_empty() {
        return Line::raw("");
    }
    let base = Style::default().fg(HEADING).add_modifier(Modifier::BOLD);
    let (mut spans, align) = make_output(rest, base, false);
    let indent = depth.saturating_sub(1) * SECTION_INDENT;
    if indent > 0 {
        spans.insert(0, Span::raw(" ".repeat(indent)));
    }
    Line::from(spans).alignment(align)
}

/// A leading `-` is a horizontal divider; `-X` uses `X` as the fill character.
fn render_divider(chars: &[char]) -> Line<'static> {
    let fill = match chars.get(1) {
        Some(&c) if chars.len() == 2 && c.is_ascii_graphic() => c,
        _ => '-',
    };
    Line::styled(
        fill.to_string().repeat(DIVIDER_W),
        Style::default().fg(HEADING),
    )
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
fn make_output(chars: &[char], base: Style, pre_escape: bool) -> (Vec<Span<'static>>, Alignment) {
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
                let label = read_link(chars, &mut i);
                if !label.is_empty() {
                    spans.push(Span::styled(
                        label,
                        base.fg(LINK).add_modifier(Modifier::UNDERLINED),
                    ));
                }
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

/// Read a link `` `[label`url`fields]`` and return its display label (the url
/// when no explicit label is given). Advances past the closing `]`.
fn read_link(chars: &[char], i: &mut usize) -> String {
    let start = *i;
    let mut j = *i;
    while j < chars.len() && chars[j] != ']' {
        j += 1;
    }
    let data: String = chars[start..j].iter().collect();
    *i = if j < chars.len() { j + 1 } else { j };

    let mut parts = data.split('`');
    let first = parts.next().unwrap_or("");
    match parts.next() {
        // `label`url[`fields]` — show the label, or the url if unlabelled.
        Some(url) => {
            if first.is_empty() {
                url.to_string()
            } else {
                first.to_string()
            }
        }
        // `url]` with no separator — the url is its own label.
        None => first.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::render;

    /// Concatenated visible text of a rendered line.
    fn text_of(line: &ratatui::text::Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn heading_strips_markers_and_keeps_text() {
        // Depth 1, no indent (the common form on real pages).
        assert_eq!(text_of(&render(">Messages")[0]), "Messages");
        // Deeper headings are indented by (depth-1)*2 but keep the title.
        assert!(text_of(&render(">>Deep")[0]).contains("Deep"));
    }

    #[test]
    fn comment_lines_become_blank() {
        assert_eq!(text_of(&render("# a comment")[0]), "");
    }

    #[test]
    fn divider_line_is_ascii_dashes() {
        let t = text_of(&render("---")[0]);
        assert!(t.chars().all(|c| c == '-') && !t.is_empty() && t.is_ascii());
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
}
