//! Opt-in capture of the mesh stack's own `tracing` diagnostics.
//!
//! The `rns-*` and `lxmf-core` crates report their internals through `tracing`
//! — including the three events that decide whether a peer's message is ever
//! *acknowledged*:
//!
//! ```text
//! delivery proof queued for link data packet (unencrypted)
//! could not sign delivery proof for link data packet
//! delivery proof could not be retained
//! ```
//!
//! With no subscriber installed those events are dropped on the floor, which
//! makes a whole class of fault invisible from inside FoxHole: a peer whose
//! send is never proved sits waiting for a delivery confirmation that will
//! never come, and all we see is a message that arrived once and a peer that
//! went quiet. (Sideband is exactly this case — it latches
//! `telemetry.<hash>.update_sending` until the message reaches DELIVERED or
//! FAILED, so an unproved telemetry send silently blocks every later one until
//! the app restarts.)
//!
//! Hand-rolled on `tracing-core` alone rather than pulling in
//! `tracing-subscriber`: this crate already has `tracing-core` transitively, the
//! whole sink is a few dozen lines, and the project pays for its dependencies
//! deliberately. Nothing is installed unless `FOXHOLE_TRACE` is set, so the
//! default build keeps the no-op global dispatcher and its ~zero cost.
//!
//! Output goes to `{config_dir}/trace.log` — inside the tree `burn::execute`
//! zero-overwrites, because a trace carries peer destination hashes.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::{Event, Level, LevelFilter, Metadata, Subscriber};

/// Trace file name inside the config dir.
const TRACE_FILE: &str = "trace.log";

/// Environment variable enabling capture. Unset → nothing is installed.
///
/// `FOXHOLE_TRACE=1` (or `all`) captures every target; any other value is a
/// comma-separated list of substrings matched against the event's target, so
/// `FOXHOLE_TRACE=link_manager,router` narrows to the delivery path.
const ENV_ENABLE: &str = "FOXHOLE_TRACE";

/// Verbosity cap: `error`/`warn`/`info`/`debug`/`trace`. Defaults to `debug`,
/// which is where the delivery-proof events live.
const ENV_LEVEL: &str = "FOXHOLE_TRACE_LEVEL";

/// Install the trace sink if [`ENV_ENABLE`] is set, returning the path being
/// written to so the caller can tell the operator where to look.
///
/// Returns `None` when capture is off, the file cannot be opened, or a
/// subscriber is already installed — diagnostics must never be the reason the
/// terminal fails to start.
pub fn install(dir: &Path) -> Option<PathBuf> {
    let filter = std::env::var(ENV_ENABLE).ok()?;
    let level = level_from_env();
    let path = dir.join(TRACE_FILE);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let sink = TraceSink::new(file, Targets::parse(&filter), level);
    tracing_core::dispatcher::set_global_default(tracing_core::Dispatch::new(sink)).ok()?;
    Some(path)
}

/// Parse [`ENV_LEVEL`], defaulting to `DEBUG` (the level the delivery-proof
/// events are emitted at). An unrecognised value falls back to the default
/// rather than disabling capture the operator just asked for.
fn level_from_env() -> Level {
    match std::env::var(ENV_LEVEL)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "error" => Level::ERROR,
        "warn" => Level::WARN,
        "info" => Level::INFO,
        "trace" => Level::TRACE,
        _ => Level::DEBUG,
    }
}

/// Which targets to capture: everything, or any target containing one of the
/// listed substrings.
#[derive(Debug, PartialEq, Eq)]
enum Targets {
    All,
    Any(Vec<String>),
}

impl Targets {
    /// `1`/`all`/`*`/empty → [`Targets::All`]; otherwise the comma-separated
    /// substrings (blank entries dropped, so a trailing comma is harmless).
    fn parse(spec: &str) -> Self {
        let spec = spec.trim();
        if spec.is_empty() || spec == "1" || spec.eq_ignore_ascii_case("all") || spec == "*" {
            return Self::All;
        }
        let parts: Vec<String> = spec
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if parts.is_empty() {
            Self::All
        } else {
            Self::Any(parts)
        }
    }

    fn matches(&self, target: &str) -> bool {
        match self {
            Self::All => true,
            Self::Any(parts) => {
                let target = target.to_ascii_lowercase();
                parts.iter().any(|p| target.contains(p))
            }
        }
    }
}

/// A minimal [`Subscriber`]: it renders events to a line-oriented log and treats
/// spans as no-ops.
///
/// Spans are deliberately not stored. The stack's diagnostics carry their
/// payload in event *fields* (`link_id`, `proof_len`, …), so span context buys
/// nothing here and storing it would mean a registry, a lock on every
/// enter/exit, and unbounded growth in a long-running session.
struct TraceSink {
    out: Mutex<BufWriter<File>>,
    targets: Targets,
    level: Level,
}

impl TraceSink {
    fn new(file: File, targets: Targets, level: Level) -> Self {
        Self {
            out: Mutex::new(BufWriter::new(file)),
            targets,
            level,
        }
    }
}

impl Subscriber for TraceSink {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= self.level && self.targets.matches(metadata.target())
    }

    /// Report the verbosity cap so the stack can skip building events we would
    /// only discard.
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::from_level(self.level))
    }

    /// Spans are not tracked, but the id must still be non-zero (a `Id::from_u64`
    /// of 0 panics), and every span has to get one.
    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let mut line = Line::default();
        event.record(&mut line);
        // A poisoned lock means a previous write panicked mid-line; recover the
        // guard rather than taking the whole terminal down over a log line.
        let mut out = match self.out.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = writeln!(
            out,
            "{:>5} {}: {}{}",
            meta.level(),
            meta.target(),
            line.message,
            line.fields
        );
        // Flushed per event on purpose: a trace is read *after* the fault, and a
        // buffered tail is exactly the part that goes missing.
        let _ = out.flush();
    }
}

/// One rendered event: the `message` field verbatim, every other field appended
/// as ` key=value`.
#[derive(Default)]
struct Line {
    message: String,
    fields: String,
}

impl Line {
    fn put(&mut self, field: &Field, value: &dyn fmt::Display) {
        use fmt::Write as _;
        if field.name() == "message" {
            let _ = write!(self.message, "{value}");
        } else {
            let _ = write!(self.fields, " {}={}", field.name(), value);
        }
    }
}

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, &format_args!("{value:?}"));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, &value);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, &value);
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, &value);
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, &value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_spec_parses_to_all_or_substrings() {
        assert_eq!(Targets::parse("1"), Targets::All);
        assert_eq!(Targets::parse("  all "), Targets::All);
        assert_eq!(Targets::parse("*"), Targets::All);
        // An enabled-but-empty value must not silently capture nothing.
        assert_eq!(Targets::parse(""), Targets::All);
        assert_eq!(Targets::parse(" , "), Targets::All);

        let t = Targets::parse("link_manager, Router,");
        assert_eq!(
            t,
            Targets::Any(vec!["link_manager".to_string(), "router".to_string()])
        );
        assert!(t.matches("rns_runtime::link_manager"));
        assert!(t.matches("LXMF_CORE::Router"), "match is case-insensitive");
        assert!(!t.matches("rns_transport::interface"));
        assert!(Targets::All.matches("anything at all"));
    }

    #[test]
    fn rendered_line_separates_message_from_fields() {
        let mut line = Line::default();
        // Mirrors the shape of the delivery-proof event we are hunting:
        // `tracing::info!(link_id = …, proof_len = …, "delivery proof queued …")`.
        line.put(&field("message"), &"delivery proof queued");
        line.put(&field("proof_len"), &96u64);
        assert_eq!(line.message, "delivery proof queued");
        assert_eq!(line.fields, " proof_len=96");
    }

    /// Fabricate a `Field` by name — `Field` is only constructible from a
    /// `FieldSet`, which needs `Metadata`, so build the callsite by hand.
    fn field(name: &'static str) -> Field {
        use tracing_core::callsite::Callsite;
        use tracing_core::field::FieldSet;
        use tracing_core::metadata::Kind;
        use tracing_core::{Interest, Metadata, identify_callsite};

        struct Cs;
        impl Callsite for Cs {
            fn set_interest(&self, _: Interest) {}
            fn metadata(&self) -> &Metadata<'_> {
                &META
            }
        }
        static CS: Cs = Cs;
        static META: Metadata<'static> = Metadata::new(
            "test",
            "test",
            Level::INFO,
            None,
            None,
            None,
            FieldSet::new(&["message", "proof_len"], identify_callsite!(&CS)),
            Kind::EVENT,
        );
        META.fields().field(name).expect("declared field")
    }
}
