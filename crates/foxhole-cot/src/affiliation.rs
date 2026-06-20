//! CoT `type` interpretation: affiliation and the foxhole rendering kind.
//!
//! A CoT `type` is a hierarchical MIL-STD-2525 code (e.g. `a-h-G-U-C`). Foxhole
//! reads only the two facets it renders — the **affiliation** (2nd token) for a
//! tint/glyph, and a coarse **kind** for which map layer the object belongs to.
//! Everything else in the code is preserved verbatim on the [`CotEvent`] but not
//! interpreted, so an unknown `type` still renders as a generic marker rather
//! than erroring (the doc's "never fatal" rule).
//!
//! [`CotEvent`]: crate::CotEvent

/// Force affiliation of a CoT object, from the 2nd token of an atom (`a-…`)
/// type. The foxhole subset collapses the wider 2525 set (pending, assumed,
/// suspect, joker, faker, …) onto the four the TUI tints; anything outside the
/// recognised quartet — or a non-atom type — reads as [`Affiliation::Unknown`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Affiliation {
    /// `f` — friendly.
    Friendly,
    /// `h` — hostile.
    Hostile,
    /// `n` — neutral.
    Neutral,
    /// `u` — unknown (also the default for anything unrecognised).
    #[default]
    Unknown,
}

impl Affiliation {
    /// Read the affiliation from a CoT `type`. Only atom types (`a-…`) carry an
    /// affiliation token; for non-atoms (drawing shapes `u-…`, bits `b-…`, …)
    /// this is [`Affiliation::Unknown`].
    pub fn from_type(cot_type: &str) -> Self {
        let mut tokens = cot_type.split('-');
        if tokens.next() != Some("a") {
            return Affiliation::Unknown;
        }
        match tokens.next() {
            Some("f") => Affiliation::Friendly,
            Some("h") => Affiliation::Hostile,
            Some("n") => Affiliation::Neutral,
            _ => Affiliation::Unknown,
        }
    }

    /// The CoT affiliation token (`f`/`h`/`n`/`u`) for generating a `type`.
    pub fn token(self) -> char {
        match self {
            Affiliation::Friendly => 'f',
            Affiliation::Hostile => 'h',
            Affiliation::Neutral => 'n',
            Affiliation::Unknown => 'u',
        }
    }

    /// Uppercase label for logs / the INTEL panel.
    pub fn label(self) -> &'static str {
        match self {
            Affiliation::Friendly => "FRIENDLY",
            Affiliation::Hostile => "HOSTILE",
            Affiliation::Neutral => "NEUTRAL",
            Affiliation::Unknown => "UNKNOWN",
        }
    }

    /// A monochrome-safe glyph (meaning survives without colour), matching the
    /// mapping proposed in the design note §10.5: friendly `▲`, hostile `◆`,
    /// neutral `■`, unknown `●`.
    pub fn glyph(self) -> char {
        match self {
            Affiliation::Friendly => '\u{25B2}', // ▲
            Affiliation::Hostile => '\u{25C6}',  // ◆
            Affiliation::Neutral => '\u{25A0}',  // ■
            Affiliation::Unknown => '\u{25CF}',  // ●
        }
    }
}

/// Which map layer a CoT object renders on. A coarse classification of the
/// `type` (and whether it carries a zone shape) — foxhole's pragmatic subset,
/// not the full 2525 taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An affiliated atom or point marker (`a-…`, `b-m-p-…`) — a single point.
    Marker,
    /// A drawn area / hazard zone (`u-d-c-c`, or any type bearing a radius).
    Zone,
    /// A route / line (`b-m-r`).
    Route,
    /// Anything else in the recognised-but-unrendered tail — shown as a marker.
    Other,
}

impl Kind {
    /// Classify a CoT object from its `type` and whether it carries a zone
    /// radius. A radius always wins (a point with a circle *is* a zone), then the
    /// `type` prefix decides.
    pub fn classify(cot_type: &str, has_radius: bool) -> Self {
        if has_radius || cot_type.starts_with("u-d-c") {
            Kind::Zone
        } else if cot_type.starts_with("b-m-r") {
            Kind::Route
        } else if cot_type.starts_with("a-") || cot_type.starts_with("b-m-p") {
            Kind::Marker
        } else {
            Kind::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_affiliation_token() {
        assert_eq!(Affiliation::from_type("a-f-G-U-C"), Affiliation::Friendly);
        assert_eq!(Affiliation::from_type("a-h-G-U-C"), Affiliation::Hostile);
        assert_eq!(Affiliation::from_type("a-n-G"), Affiliation::Neutral);
        assert_eq!(Affiliation::from_type("a-u-G"), Affiliation::Unknown);
        // Non-atom and unrecognised tokens fall back to Unknown.
        assert_eq!(Affiliation::from_type("u-d-c-c"), Affiliation::Unknown);
        assert_eq!(Affiliation::from_type("a-p-G"), Affiliation::Unknown);
        assert_eq!(Affiliation::from_type(""), Affiliation::Unknown);
    }

    #[test]
    fn token_round_trips_the_quartet() {
        for a in [
            Affiliation::Friendly,
            Affiliation::Hostile,
            Affiliation::Neutral,
            Affiliation::Unknown,
        ] {
            let t = format!("a-{}-G", a.token());
            assert_eq!(Affiliation::from_type(&t), a);
        }
    }

    #[test]
    fn classifies_kind() {
        assert_eq!(Kind::classify("a-h-G-U-C", false), Kind::Marker);
        assert_eq!(Kind::classify("b-m-p-s-m", false), Kind::Marker);
        assert_eq!(Kind::classify("u-d-c-c", false), Kind::Zone);
        assert_eq!(Kind::classify("b-m-r", false), Kind::Route);
        assert_eq!(Kind::classify("x-y-z", false), Kind::Other);
        // A radius promotes any type to a Zone.
        assert_eq!(Kind::classify("a-h-G-U-C", true), Kind::Zone);
    }
}
