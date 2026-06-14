//! Cold-boot bring-up state and the text-pane scroll model.
//!
//! Both are pure: the boot sequence is driven by a tick counter that `main`
//! advances on a timer (so `App` needs no `Instant`/I/O), and the scroll offset
//! is nudged by the key handler and clamped by the renderer. The `impl App`
//! block here carries the splash-advance methods so they sit next to the `Boot`
//! internals they touch.

use std::cell::Cell;

use super::App;

/// Top-level screen: the cold-boot bring-up splash, or the operator console.
/// Initial state is [`AppState::Splash`] only when the `splash` feature is on
/// and `FOXHOLE_NO_SPLASH` is unset; otherwise the console shows immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppState {
    /// The boot bring-up monitor is playing (rendered by `src/splash.rs`).
    Splash,
    /// The operator console (the normal three-tier UI).
    Running,
}

/// One line in the cold-boot sequence. The variant order *is* the reveal order
/// and the single source of truth shared by the marker (`app`) and the renderer
/// (`splash`). Each line appears on a timer, or earlier if its real readiness
/// event arrives first (see [`App::mark_boot`]).
#[cfg(feature = "splash")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootStep {
    Boot,
    SelfTest,
    Identity,
    Store,
    Cache,
    Iface,
    Mesh,
    Console,
}

#[cfg(feature = "splash")]
impl BootStep {
    /// Reveal order, top to bottom.
    pub const ALL: [BootStep; 8] = [
        BootStep::Boot,
        BootStep::SelfTest,
        BootStep::Identity,
        BootStep::Store,
        BootStep::Cache,
        BootStep::Iface,
        BootStep::Mesh,
        BootStep::Console,
    ];

    /// Position within [`BootStep::ALL`] — its timed reveal slot.
    fn index(self) -> usize {
        Self::ALL.iter().position(|&s| s == self).unwrap_or(0)
    }
}

/// Boot-sequence progress. The tick counter *is* the clock — `main` advances it
/// on a timer so `App` stays free of `Instant`/I/O — paired with which steps a
/// real network event has reported in.
#[cfg(feature = "splash")]
pub struct Boot {
    /// Timer ticks elapsed (driven by `main`'s splash interval while in Splash).
    ticks: u32,
    /// Steps confirmed by a real readiness event (vs. mere timed reveal).
    marks: [bool; BootStep::ALL.len()],
    /// Tick at which the operator address went live (`Local`); `None` until then.
    /// Starts the short hand-off to the console.
    ready_at: Option<u32>,
}

/// Ticks between successive lines appearing (timed-reveal pacing).
#[cfg(feature = "splash")]
const TICKS_PER_STEP: u32 = 1;
/// Linger after the last line / after readiness before opening the console.
#[cfg(feature = "splash")]
const HOLD_TICKS: u32 = 4;
/// Hard cap: never hold the operator at the splash beyond this many ticks.
#[cfg(feature = "splash")]
pub(super) const MAX_TICKS: u32 = 33;

#[cfg(feature = "splash")]
impl Boot {
    pub(super) fn new() -> Self {
        Self {
            ticks: 0,
            marks: [false; BootStep::ALL.len()],
            ready_at: None,
        }
    }
}

/// Scroll position for a text pane. The key handler nudges the offset; the
/// renderer clamps it to the content/viewport via [`Scroll::visible`] and writes
/// the corrected value back (the `Cell`s), so over-scroll self-corrects and
/// PageUp/Down step by the true last-rendered page height. Bottom-anchored panes
/// (log, thread) follow the newest line until the operator scrolls up.
pub struct Scroll {
    /// First visible visual row (counted from the top, after wrapping).
    offset: Cell<u16>,
    /// While set, the renderer pins to the bottom (follow newest content).
    stick_bottom: Cell<bool>,
    /// Last rendered inner height — the PageUp/PageDown step.
    viewport: Cell<u16>,
    /// Whether this pane defaults to (and re-engages) the bottom.
    anchored_bottom: bool,
}

impl Scroll {
    /// A top-anchored pane that opens at the top (Browser page, Guide).
    pub(super) fn top() -> Self {
        Self {
            offset: Cell::new(0),
            stick_bottom: Cell::new(false),
            viewport: Cell::new(0),
            anchored_bottom: false,
        }
    }

    /// A bottom-anchored pane that follows the newest line (Log, thread).
    pub(super) fn bottom() -> Self {
        Self {
            offset: Cell::new(0),
            stick_bottom: Cell::new(true),
            viewport: Cell::new(0),
            anchored_bottom: true,
        }
    }

    fn line_up(&self, n: u16) {
        self.stick_bottom.set(false);
        self.offset.set(self.offset.get().saturating_sub(n));
    }

    fn line_down(&self, n: u16) {
        // The renderer clamps; reaching the bottom re-engages stick (live panes).
        self.offset.set(self.offset.get().saturating_add(n));
    }

    /// Scroll up/down by one viewport.
    pub fn page_up(&self) {
        self.line_up(self.viewport.get().max(1));
    }
    pub fn page_down(&self) {
        self.line_down(self.viewport.get().max(1));
    }
    /// Jump to the very top / bottom.
    pub fn to_top(&self) {
        self.stick_bottom.set(false);
        self.offset.set(0);
    }
    pub fn to_bottom(&self) {
        self.stick_bottom.set(true);
    }

    /// Clamp to `content_rows`/`viewport`, cache the viewport for paging, and
    /// return the visual row offset to render at (writing the corrected offset
    /// back). Pure arithmetic — no rendering types.
    pub fn visible(&self, content_rows: u16, viewport: u16) -> u16 {
        self.viewport.set(viewport);
        let max = content_rows.saturating_sub(viewport);
        let off = if self.stick_bottom.get() {
            max
        } else {
            self.offset.get().min(max)
        };
        // A bottom-anchored pane resumes following once scrolled back to the end.
        if self.anchored_bottom && off >= max {
            self.stick_bottom.set(true);
        }
        self.offset.set(off);
        off
    }
}

impl App {
    /// Advance the boot sequence by one timer tick and decide whether to hand
    /// off to the console. The console opens once the bring-up has settled —
    /// after the operator address is live (`net`), or after the timed reveal
    /// finishes (offline) — and always by the `MAX_TICKS` hard cap so a stalled
    /// stack never traps the operator. No-op once running.
    pub fn tick_splash(&mut self) {
        // No-op without the `splash` feature (the state is never `Splash`).
        #[cfg(feature = "splash")]
        {
            if self.state != AppState::Splash {
                return;
            }
            self.boot.ticks += 1;
            let t = self.boot.ticks;
            let all_done = BootStep::ALL.iter().all(|&s| self.boot_done(s));
            // Live builds wait for the real address; offline builds use the timer.
            let last_reveal = (BootStep::ALL.len() as u32 - 1) * TICKS_PER_STEP;
            let timed_ok = !cfg!(feature = "net") && t >= last_reveal + HOLD_TICKS;
            let ready_ok = self.boot.ready_at.is_some_and(|r| t >= r + HOLD_TICKS);
            if t >= MAX_TICKS || (all_done && (timed_ok || ready_ok)) {
                self.state = AppState::Running;
            }
        }
    }

    /// Mark a boot step confirmed by a real readiness event (so its line flips
    /// to its reported status ahead of the timer). The final `Console` step is
    /// driven by the operator address going live and starts the hand-off clock.
    #[cfg(feature = "splash")]
    pub fn mark_boot(&mut self, step: BootStep) {
        self.boot.marks[step.index()] = true;
        if step == BootStep::Console && self.boot.ready_at.is_none() {
            self.boot.ready_at = Some(self.boot.ticks);
        }
    }

    /// Whether a boot line should render as reported: its real event arrived, or
    /// the timed reveal has reached it.
    #[cfg(feature = "splash")]
    pub fn boot_done(&self, step: BootStep) -> bool {
        self.boot.marks[step.index()] || self.boot.ticks >= step.index() as u32 * TICKS_PER_STEP
    }
}
