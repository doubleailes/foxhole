//! A tiny, hardened XML subset tokenizer — the mandated safe reader for the
//! `cot/xml` path (design note §9).
//!
//! Received CoT is attacker-controllable external input, so this reader is built
//! to *refuse* the dangerous parts of XML rather than implement them:
//!
//! - **No XXE.** `<!DOCTYPE …>` and `<!ENTITY …>` declarations are rejected
//!   outright ([`XmlError::Dtd`]); only the five predefined entities and bounded
//!   numeric character references are expanded. There is no external-resource
//!   resolution of any kind.
//! - **Bounded.** Total input size ([`MAX_INPUT`]), nesting depth
//!   ([`MAX_DEPTH`]) and token count ([`MAX_TOKENS`]) are all capped, so a
//!   malicious payload cannot exhaust memory or stack (DoS defence).
//! - **Minimal surface.** It understands exactly start/empty/end tags, text and
//!   CDATA, and skips the XML declaration and comments. Anything stranger is an
//!   error, never undefined behaviour.
//!
//! This is intentionally *not* a general XML parser — it reads only the flat,
//! attribute-light shape CoT uses. The [`crate::event`] layer walks the tokens.

/// Largest CoT document we will look at (bytes). A single `<event>` is ~1 KB;
/// 64 KiB is generous headroom while still bounding a flood.
pub const MAX_INPUT: usize = 64 * 1024;
/// Deepest element nesting accepted (`event > detail > shape > ellipse` is 4).
pub const MAX_DEPTH: usize = 16;
/// Most tokens we will emit from one document.
pub const MAX_TOKENS: usize = 4_096;

/// Why a document was rejected. All variants are non-fatal to the program: the
/// caller turns them into a skipped event, never a crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmlError {
    /// Input exceeds [`MAX_INPUT`].
    TooLarge,
    /// A `<!DOCTYPE`/`<!ENTITY` declaration — rejected to foreclose XXE.
    Dtd,
    /// Nesting deeper than [`MAX_DEPTH`].
    TooDeep,
    /// More tokens than [`MAX_TOKENS`].
    TooMany,
    /// Structurally broken markup (unterminated tag, stray `<`, bad attribute…).
    Malformed,
}

/// One lexical token of the subset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// `<name …>` — a start tag with its (unescaped) attributes.
    Open {
        name: String,
        attrs: Vec<(String, String)>,
    },
    /// `<name … />` — a self-closing tag with its attributes.
    Empty {
        name: String,
        attrs: Vec<(String, String)>,
    },
    /// `</name>` — an end tag.
    Close { name: String },
    /// Character data (entity-unescaped; CDATA passed through verbatim).
    Text(String),
}

/// Tokenize a CoT XML document into the subset's tokens, enforcing every bound
/// in this module. Whitespace-only text between tags is dropped so callers see
/// only meaningful character data.
pub fn tokenize(input: &str) -> Result<Vec<Token>, XmlError> {
    if input.len() > MAX_INPUT {
        return Err(XmlError::TooLarge);
    }
    let b = input.as_bytes();
    let mut pos = 0;
    let mut out = Vec::new();
    let mut depth: usize = 0;

    while pos < b.len() {
        if b[pos] == b'<' {
            pos = lex_markup(input, b, pos, &mut out, &mut depth)?;
        } else {
            // Run of character data up to the next '<'.
            let start = pos;
            while pos < b.len() && b[pos] != b'<' {
                pos += 1;
            }
            let raw = &input[start..pos];
            if !raw.trim().is_empty() {
                push(&mut out, Token::Text(unescape(raw)?))?;
            }
        }
    }
    Ok(out)
}

/// Lex one `<…>` construct starting at `pos` (which points at `<`); returns the
/// position just past it. Updates `depth` and appends any emitted token.
fn lex_markup(
    input: &str,
    b: &[u8],
    pos: usize,
    out: &mut Vec<Token>,
    depth: &mut usize,
) -> Result<usize, XmlError> {
    // `<?xml …?>` declaration / processing instruction — skip.
    if b[pos..].starts_with(b"<?") {
        let end = find(b, pos + 2, b"?>").ok_or(XmlError::Malformed)?;
        return Ok(end + 2);
    }
    // `<!-- … -->` comment, `<![CDATA[ … ]]>`, or a rejected `<!DOCTYPE/ENTITY>`.
    if b[pos..].starts_with(b"<!") {
        if b[pos..].starts_with(b"<!--") {
            let end = find(b, pos + 4, b"-->").ok_or(XmlError::Malformed)?;
            return Ok(end + 3);
        }
        if b[pos..].starts_with(b"<![CDATA[") {
            let end = find(b, pos + 9, b"]]>").ok_or(XmlError::Malformed)?;
            push(out, Token::Text(input[pos + 9..end].to_string()))?;
            return Ok(end + 3);
        }
        // DOCTYPE / ENTITY / anything else `<!…` — the XXE surface. Refuse it.
        return Err(XmlError::Dtd);
    }
    // `</name>` end tag.
    if b[pos..].starts_with(b"</") {
        let end = find(b, pos + 2, b">").ok_or(XmlError::Malformed)?;
        let name = input[pos + 2..end].trim();
        if name.is_empty() {
            return Err(XmlError::Malformed);
        }
        *depth = depth.checked_sub(1).ok_or(XmlError::Malformed)?;
        push(
            out,
            Token::Close {
                name: name.to_string(),
            },
        )?;
        return Ok(end + 1);
    }
    // `<name …>` or `<name … />`.
    let end = find(b, pos + 1, b">").ok_or(XmlError::Malformed)?;
    let mut inner = input[pos + 1..end].trim();
    let self_closing = inner.ends_with('/');
    if self_closing {
        inner = inner[..inner.len() - 1].trim_end();
    }
    let (name, attr_src) = split_name(inner)?;
    let attrs = parse_attrs(attr_src)?;
    if self_closing {
        push(
            out,
            Token::Empty {
                name: name.to_string(),
                attrs,
            },
        )?;
    } else {
        *depth += 1;
        if *depth > MAX_DEPTH {
            return Err(XmlError::TooDeep);
        }
        push(
            out,
            Token::Open {
                name: name.to_string(),
                attrs,
            },
        )?;
    }
    Ok(end + 1)
}

/// Append a token, enforcing [`MAX_TOKENS`].
fn push(out: &mut Vec<Token>, t: Token) -> Result<(), XmlError> {
    if out.len() >= MAX_TOKENS {
        return Err(XmlError::TooMany);
    }
    out.push(t);
    Ok(())
}

/// Split a tag body into `(name, remaining-attribute-source)`.
fn split_name(inner: &str) -> Result<(&str, &str), XmlError> {
    let inner = inner.trim_start();
    let end = inner
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner.len());
    let name = &inner[..end];
    if name.is_empty() {
        return Err(XmlError::Malformed);
    }
    Ok((name, inner[end..].trim_start()))
}

/// Parse a tag's attributes: `key="value"` / `key='value'` pairs separated by
/// whitespace. Values are entity-unescaped; bare or unquoted attributes are
/// rejected as malformed.
fn parse_attrs(mut src: &str) -> Result<Vec<(String, String)>, XmlError> {
    let mut attrs = Vec::new();
    src = src.trim();
    while !src.is_empty() {
        let eq = src.find('=').ok_or(XmlError::Malformed)?;
        let key = src[..eq].trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(XmlError::Malformed);
        }
        let after = src[eq + 1..].trim_start();
        let quote = after.as_bytes().first().copied();
        let q = match quote {
            Some(b'"') => '"',
            Some(b'\'') => '\'',
            _ => return Err(XmlError::Malformed),
        };
        let rest = &after[1..];
        let close = rest.find(q).ok_or(XmlError::Malformed)?;
        let value = unescape(&rest[..close])?;
        attrs.push((key.to_string(), value));
        src = rest[close + 1..].trim_start();
    }
    Ok(attrs)
}

/// Find `needle` in `b` at or after `from`, returning its start index.
fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from > b.len() {
        return None;
    }
    (from..=b.len().saturating_sub(needle.len())).find(|&i| &b[i..i + needle.len()] == needle)
}

/// Expand the five predefined XML entities and bounded numeric character
/// references; leave all other text verbatim. Crucially, **no custom/external
/// entity is ever resolved** — an `&foo;` that is not one of the five recognised
/// names is passed through literally rather than looked up, so a DTD-defined
/// entity (the XXE vector) has nothing to expand even if one slipped through.
fn unescape(s: &str) -> Result<String, XmlError> {
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let semi = match tail.find(';') {
            // A reference is short; a long run with no `;` is just literal text.
            Some(i) if i <= 12 => i,
            _ => {
                out.push('&');
                rest = &tail[1..];
                continue;
            }
        };
        let ent = &tail[1..semi];
        match ent {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if ent.starts_with('#') => out.push(numeric_ref(ent).ok_or(XmlError::Malformed)?),
            // Unknown entity name → not expanded; preserved literally.
            _ => {
                out.push('&');
                out.push_str(ent);
                out.push(';');
            }
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Resolve a numeric character reference body (`#NN` decimal or `#xHH` hex) to a
/// `char`, or `None` if it isn't a valid scalar value.
fn numeric_ref(ent: &str) -> Option<char> {
    let digits = &ent[1..];
    let code = if let Some(hex) = digits.strip_prefix(['x', 'X']) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        digits.parse::<u32>().ok()?
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(name: &str, attrs: &[(&str, &str)]) -> Token {
        Token::Open {
            name: name.into(),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn tokenizes_a_small_document() {
        let toks = tokenize(
            r#"<?xml version="1.0"?><event uid="J-1" type="a-h-G"><point lat="1.5" lon="2.5"/><detail><remarks>hi</remarks></detail></event>"#,
        )
        .unwrap();
        assert_eq!(toks[0], open("event", &[("uid", "J-1"), ("type", "a-h-G")]));
        assert_eq!(
            toks[1],
            Token::Empty {
                name: "point".into(),
                attrs: vec![("lat".into(), "1.5".into()), ("lon".into(), "2.5".into())],
            }
        );
        assert_eq!(toks[2], open("detail", &[]));
        assert_eq!(toks[3], open("remarks", &[]));
        assert_eq!(toks[4], Token::Text("hi".into()));
        assert_eq!(
            toks[5],
            Token::Close {
                name: "remarks".into()
            }
        );
    }

    #[test]
    fn rejects_doctype_and_entity_declarations() {
        // The canonical "billion laughs" / XXE shapes must be refused.
        assert_eq!(
            tokenize(r#"<!DOCTYPE foo [<!ENTITY x "boom">]><event/>"#),
            Err(XmlError::Dtd)
        );
        assert_eq!(
            tokenize(r#"<!ENTITY xxe SYSTEM "file:///etc/passwd"><event/>"#),
            Err(XmlError::Dtd)
        );
    }

    #[test]
    fn does_not_expand_custom_entities() {
        // An `&xxe;` reference survives as literal text — never resolved.
        let toks = tokenize("<remarks>&xxe; &amp; done</remarks>").unwrap();
        assert_eq!(toks[1], Token::Text("&xxe; & done".into()));
    }

    #[test]
    fn unescapes_standard_and_numeric_entities() {
        let toks = tokenize(r#"<x a="&lt;&gt;&amp;&quot;&apos;&#65;&#x42;"/>"#).unwrap();
        match &toks[0] {
            Token::Empty { attrs, .. } => assert_eq!(attrs[0].1, "<>&\"'AB"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn enforces_size_and_depth_bounds() {
        assert_eq!(
            tokenize(&"x".repeat(MAX_INPUT + 1)),
            Err(XmlError::TooLarge)
        );
        let deep = "<a>".repeat(MAX_DEPTH + 1);
        assert_eq!(tokenize(&deep), Err(XmlError::TooDeep));
    }

    #[test]
    fn flags_malformed_markup() {
        assert_eq!(tokenize("<event"), Err(XmlError::Malformed)); // unterminated
        assert_eq!(tokenize("<x a=b/>"), Err(XmlError::Malformed)); // unquoted attr
        assert_eq!(tokenize("</>"), Err(XmlError::Malformed)); // empty close
    }
}
