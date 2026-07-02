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
//! truecolor (`Tff8800`) forms. Links and editable text fields are the focusable
//! [`Element`]s (numbered in document order, [`elements`]); the focused one is
//! highlighted and text fields render their live value (masked → `*`). Checkbox/
//! radio inputs render read-only and remote partials show a placeholder.
//! Anything unrecognised is dropped — never fatal, never leaks the control byte.
//! Every source line is scrubbed of C0/C1 control characters before spans are
//! built, so a hostile page can't smuggle terminal escape sequences past ratatui.

use std::borrow::Cow;
use std::collections::HashMap;

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
/// Background of an input field box.
const FIELD_BG: Color = Color::Rgb(0x33, 0x33, 0x33);

/// A focusable page element, in document order — the Browser navigates these and
/// the focus index lines up with [`render`]'s `focus` argument.
pub enum Element {
    /// A hyperlink: `target` is the resolvable url, `fields` its submit spec
    /// (`*`, field names, or `k=v` vars).
    Link { target: String, fields: Vec<String> },
    /// An editable text input: `name` keys its value, `default` is the initial
    /// value. (Width/masked are applied at render time from the source.)
    Field { name: String, default: String },
}

/// Body text style — the default grey, the base every line builds on.
fn body_style() -> Style {
    Style::default().fg(DEFAULT_FG)
}

/// Strip C0/C1 control characters (and DEL) from one source line before any
/// span is built. Pages come from untrusted remote nodes — a raw ESC/CSI in
/// the markup must never reach the terminal, not even inside a literal block.
/// Tabs become a single space so words stay apart; there is no other
/// legitimate use of a control char in micron source. Clean lines (the normal
/// case) borrow through untouched.
fn sanitize(line: &str) -> Cow<'_, str> {
    if !line.chars().any(char::is_control) {
        return Cow::Borrowed(line);
    }
    Cow::Owned(
        line.chars()
            .filter_map(|c| match c {
                '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect(),
    )
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

/// Cross-line bookkeeping for one walk over the source: focusable elements
/// (links + text fields) are numbered in document order, collected, and — when
/// rendering — highlighted (`focus`) and filled from the live field `values`.
struct Walk<'a> {
    /// Index assigned to the next focusable element.
    next: usize,
    /// The element index to highlight, if any (render only).
    focus: Option<usize>,
    /// Current field values by name (render only; empty when just collecting).
    values: &'a HashMap<String, String>,
    /// The focusable elements, in order.
    elements: Vec<Element>,
}

/// Render micron `source` into display lines at viewport `width`, highlighting
/// the `focus`ed element and drawing fields from their current `values`.
pub fn render(
    source: &str,
    width: u16,
    focus: Option<usize>,
    values: &HashMap<String, String>,
) -> Vec<Line<'static>> {
    let mut literal = false;
    let mut depth = 0usize;
    let mut walk = Walk {
        next: 0,
        focus,
        values,
        elements: Vec::new(),
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

/// The focusable elements of `source` (links + text fields) in document order —
/// what the Browser navigates and submits. No ratatui types.
pub fn elements(source: &str) -> Vec<Element> {
    let empty = HashMap::new();
    let mut literal = false;
    let mut depth = 0usize;
    let mut walk = Walk {
        next: 0,
        focus: None,
        values: &empty,
        elements: Vec::new(),
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
    walk.elements
}

/// Render one source line, threading the cross-line `literal` block + section
/// `depth` state and the link walk.
fn render_line(
    line: &str,
    literal: &mut bool,
    depth: &mut usize,
    walk: &mut Walk<'_>,
    width: u16,
) -> Line<'static> {
    // Remote markup: drop terminal control chars before anything is emitted.
    let line = sanitize(line);
    let line = line.as_ref();
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
fn body_line(chars: &[char], pre_escape: bool, depth: usize, walk: &mut Walk<'_>) -> Line<'static> {
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
    walk: &mut Walk<'_>,
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
    walk: &mut Walk<'_>,
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
            '<' => emit_field(chars, &mut i, base, fmt, walk, &mut spans),
            '[' => {
                let (label, url, fields) = read_link(chars, &mut i);
                // Number every link so the Browser can focus + follow it.
                let idx = walk.next;
                walk.next += 1;
                walk.elements.push(Element::Link {
                    target: url.clone(),
                    fields,
                });
                let shown = if label.is_empty() { url } else { label };
                // Links inherit the page's own styling (NomadNet does the same),
                // underlined to flag them; the focused element is reversed.
                let mut style = fmt.style(base).add_modifier(Modifier::UNDERLINED);
                if walk.focus == Some(idx) {
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

/// The micron field kinds. Checkboxes/radios render read-only this phase.
enum FieldKind {
    Text,
    Checkbox,
    Radio,
}

/// A parsed `` `<flags|name`data>`` input field.
struct FieldSpec {
    kind: FieldKind,
    name: String,
    width: u16,
    masked: bool,
    /// Initial text (Text) or label (checkbox/radio).
    data: String,
    prechecked: bool,
}

/// Parse a field block `` `<content`data>`` (cursor `*i` is just past `<`),
/// advancing past the closing `>`. Mirrors `MicronParser.parse_line` field logic.
fn parse_field(chars: &[char], i: &mut usize) -> Option<FieldSpec> {
    // content up to the next backtick.
    let cstart = *i;
    let mut j = *i;
    while j < chars.len() && chars[j] != '`' {
        j += 1;
    }
    if j >= chars.len() {
        *i = chars.len();
        return None; // no `, invalid field
    }
    let content: String = chars[cstart..j].iter().collect();
    // data from after the backtick up to '>'.
    let dstart = j + 1;
    let mut k = dstart;
    while k < chars.len() && chars[k] != '>' {
        k += 1;
    }
    if k >= chars.len() {
        *i = chars.len();
        return None; // no '>', invalid field
    }
    let data: String = chars[dstart..k].iter().collect();
    *i = k + 1; // past '>'

    let mut kind = FieldKind::Text;
    let mut masked = false;
    let mut width = 24u16;
    let mut prechecked = false;
    let name;
    if let Some((flags, rest)) = content.split_once('|') {
        let comps: Vec<&str> = rest.split('|').collect();
        let mut flagstr = flags.to_string();
        if flagstr.contains('^') {
            kind = FieldKind::Radio;
            flagstr = flagstr.replace('^', "");
        } else if flagstr.contains('?') {
            kind = FieldKind::Checkbox;
            flagstr = flagstr.replace('?', "");
        } else if flagstr.contains('!') {
            masked = true;
            flagstr = flagstr.replace('!', "");
        }
        if !flagstr.is_empty()
            && let Ok(w) = flagstr.parse::<u16>()
        {
            width = w.min(256);
        }
        name = comps.first().copied().unwrap_or("").to_string();
        prechecked = comps.get(2) == Some(&"*");
    } else {
        name = content;
    }
    Some(FieldSpec {
        kind,
        name,
        width,
        masked,
        data,
        prechecked,
    })
}

/// Render an input field, pushing a focusable [`Element::Field`] for text inputs
/// (checkboxes/radios render read-only this phase).
fn emit_field(
    chars: &[char],
    i: &mut usize,
    base: Style,
    fmt: Fmt,
    walk: &mut Walk<'_>,
    spans: &mut Vec<Span<'static>>,
) {
    let Some(spec) = parse_field(chars, i) else {
        return;
    };
    match spec.kind {
        FieldKind::Text => {
            let idx = walk.next;
            walk.next += 1;
            let value = walk
                .values
                .get(&spec.name)
                .cloned()
                .unwrap_or_else(|| spec.data.clone());
            spans.push(field_span(
                &value,
                spec.width,
                spec.masked,
                walk.focus == Some(idx),
            ));
            walk.elements.push(Element::Field {
                name: spec.name,
                default: spec.data,
            });
        }
        FieldKind::Checkbox => {
            let mark = if spec.prechecked { "[x] " } else { "[ ] " };
            spans.push(Span::styled(
                format!("{mark}{}", spec.data),
                fmt.style(base),
            ));
        }
        FieldKind::Radio => {
            let mark = if spec.prechecked { "(o) " } else { "( ) " };
            spans.push(Span::styled(
                format!("{mark}{}", spec.data),
                fmt.style(base),
            ));
        }
    }
}

/// A text-input box: the value (masked as `*`) bounded to `width`, on a field
/// background; reversed (a cursor cue) when focused.
fn field_span(value: &str, width: u16, masked: bool, focused: bool) -> Span<'static> {
    let w = (width as usize).max(1);
    let shown: String = if masked {
        "*".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    let mut s: String = shown.chars().take(w).collect();
    let len = s.chars().count();
    if len < w {
        s.extend(std::iter::repeat_n(' ', w - len));
    }
    let mut style = Style::default().bg(FIELD_BG).fg(DEFAULT_FG);
    if focused {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Span::styled(s, style)
}

/// Read a link `` `[label`url`fields]`` into `(label, url, fields)`. `label`
/// empty when none is given (the caller then shows the url); `fields` is the
/// submit spec (`*`, names, `k=v`). Advances past the closing `]`.
fn read_link(chars: &[char], i: &mut usize) -> (String, String, Vec<String>) {
    let start = *i;
    let mut j = *i;
    while j < chars.len() && chars[j] != ']' {
        j += 1;
    }
    let data: String = chars[start..j].iter().collect();
    *i = if j < chars.len() { j + 1 } else { j };

    let parts: Vec<&str> = data.split('`').collect();
    let split_fields = |f: &str| -> Vec<String> {
        f.split('|')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    match parts.as_slice() {
        // `url]` with no separator — the url is its own label.
        [only] => (String::new(), only.to_string(), Vec::new()),
        [label, url] => (label.to_string(), url.to_string(), Vec::new()),
        [label, url, fields, ..] => (label.to_string(), url.to_string(), split_fields(fields)),
        [] => (String::new(), String::new(), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Element, elements, render as render_sel};
    use ratatui::style::Modifier;
    use std::collections::HashMap;

    /// Render (nothing focused, default values, fixed width) for text assertions.
    fn render(source: &str) -> Vec<ratatui::text::Line<'static>> {
        render_sel(source, 80, None, &HashMap::new())
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
        let line = &render_sel(">Hi", 20, None, &HashMap::new())[0];
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
        let lines = render_sel(">>Sec\nbody text", 40, None, &HashMap::new());
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
    fn control_chars_are_stripped_from_body_text() {
        // A raw ESC (or C1 CSI) in remote markup must never reach a span.
        assert_eq!(
            text_of(&render("a\u{1b}[31mb\u{9b}2Jc\u{7}")[0]),
            "a[31mb2Jc"
        );
        // Tabs collapse to a space; DEL and stray CR are dropped.
        assert_eq!(text_of(&render("a\tb\u{7f}c\rd")[0]), "a bcd");
    }

    #[test]
    fn control_chars_are_stripped_inside_literal_blocks() {
        // `= passes markup through verbatim, but not terminal control bytes.
        let out = render("`=\n\u{1b}]0;owned\u{7}text\n`=");
        assert_eq!(text_of(&out[1]), "]0;ownedtext");
    }

    #[test]
    fn control_chars_are_stripped_from_link_labels_and_field_defaults() {
        assert_eq!(text_of(&render("`[Cli\u{1b}ck`:/page/x.mu]")[0]), "Click");
        let t = text_of(&render("`<12|user`al\u{1b}[31mice>")[0]);
        assert!(t.contains("al[31mice") && !t.contains('\u{1b}'));
    }

    #[test]
    fn link_targets_collected_in_document_order() {
        let src = "`[Home`:/page/index.mu]\nmid\n`[Files`:/page/files.mu`a|b]\n`[plainurl]";
        let targets: Vec<String> = elements(src)
            .into_iter()
            .filter_map(|e| match e {
                Element::Link { target, .. } => Some(target),
                _ => None,
            })
            .collect();
        assert_eq!(
            targets,
            vec![":/page/index.mu", ":/page/files.mu", "plainurl"]
        );
    }

    #[test]
    fn text_field_parsed_and_rendered() {
        let src = "Name: `<12|user`alice>";
        match elements(src).as_slice() {
            [Element::Field { name, default }] => {
                assert_eq!(name, "user");
                assert_eq!(default, "alice");
            }
            other => panic!("expected one text field, got {} elements", other.len()),
        }
        assert!(text_of(&render(src)[0]).contains("alice"));
    }

    #[test]
    fn masked_field_hides_value() {
        let t = text_of(&render("`<!|pw`secret>")[0]);
        assert!(t.contains("******") && !t.contains("secret"));
    }

    #[test]
    fn link_captures_field_spec() {
        match &elements("`[Go`/page/x.mu`q|name]")[0] {
            Element::Link { target, fields } => {
                assert_eq!(target, "/page/x.mu");
                assert_eq!(fields, &vec!["q".to_string(), "name".to_string()]);
            }
            _ => panic!("expected a link"),
        }
    }

    #[test]
    fn selected_link_renders_reversed() {
        let src = "`[One`:/a] `[Two`:/b]";
        // Highlight the second link (index 1).
        let line = &render_sel(src, 80, Some(1), &HashMap::new())[0];
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
