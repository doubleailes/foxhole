//! FoxHole — off-grid, keyboard-only, monochrome LXMF comms terminal.
//!
//! This module owns the terminal *lifecycle* and the single async event loop.
//! It deliberately holds no UI logic (see [`ui`]) and no state rules (see
//! [`app`]); its job is to:
//!   1. Put the terminal into a known raw / alternate-screen / cursor-hidden
//!      state, and guarantee it is restored on *every* exit path — clean,
//!      error, or panic.
//!   2. Spawn the network workhorse on a separate Tokio task so protocol packet
//!      processing never blocks frame rendering.
//!   3. Multiplex keyboard input and inbound messages in one `select!` loop.

// The logic and rendering layers now live in workspace crates; re-export them
// under the same module paths so `main` keeps referring to `crate::app`,
// `crate::config`, etc. unchanged.
pub use foxhole_core::{app, burn, config, notes, zones};
use foxhole_tui::ui;

// The live networking layer (LXMF/Reticulum stack + encrypted stores) lives in
// the `foxhole-net` crate; import its modules so `main`'s call sites
// (`net::run`, `store::{load_all,save}`, `intel_store::{load,save}`) read
// unchanged.
#[cfg(feature = "net")]
use foxhole_net::{intel_store, net, store};

use std::io::{self, Stdout, Write};

use crossterm::cursor;
use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::app::{App, NetCommand, NetEvent};

/// Depth of the inbound-message channel. Bounded so a stalled UI applies
/// backpressure to the network task rather than growing memory without bound.
const INBOUND_CAPACITY: usize = 128;

type Tui = Terminal<CrosstermBackend<Stdout>>;

#[tokio::main]
async fn main() -> io::Result<()> {
    // Restore the terminal on panic *before* the default hook prints the
    // message, so a crash never strands a field terminal in raw mode.
    install_panic_hook();

    let mut terminal = setup_terminal()?;
    // RAII: restores the terminal on any return path below (incl. `?`).
    let _guard = TerminalGuard;

    // The network task feeds events in; the UI loop drains them.
    let (net_tx, net_rx) = mpsc::channel::<NetEvent>(INBOUND_CAPACITY);

    let mut app = App::new();
    app.config = config::Config::load();
    app.notes = notes::Notes::load();
    // Operator-defined hazard zones override the seeded demo set whenever a
    // `zones.conf` exists — even an empty/all-junk one, so the operator can blank
    // the overlay by creating the file. Only an absent (or unreadable) file keeps
    // the demo set. The filesystem read lives here; core stays I/O-free and only
    // parses.
    match std::fs::read_to_string(config::config_dir().join("zones.conf")) {
        Ok(text) => app.map.zones = zones::parse(&text),
        Err(_) => { /* no file (or unreadable): keep the seeded demo zones */ }
    }

    // Capture the mesh stack's own `tracing` diagnostics if the operator asked
    // for them (`FOXHOLE_TRACE`). Installed before the network task starts so
    // bring-up is covered, and reported into the Log so the file is findable.
    #[cfg(feature = "net")]
    if let Some(path) = foxhole_net::trace::install(&config::config_dir()) {
        app.push_log(format!("[SYS] tracing mesh stack to {}", path.display()));
    }

    let link = spawn_network(net_tx, &app.config);

    // Live discovery replaces the offline demo peers; start from an empty list.
    #[cfg(feature = "net")]
    app.convs.items.clear();

    // `_guard` drops as this returns, restoring the terminal whether `run`
    // finished cleanly or propagated an I/O error.
    let result = run(&mut terminal, &mut app, net_rx, link).await;

    // Burn notice: the operator confirmed destruction. Restore the terminal,
    // shred the config dir, report, and exit hard — `process::exit` skips the
    // `TerminalGuard` drop (hence the explicit restore) and kills the net task
    // before it can recreate anything.
    if app.burn {
        let _ = restore_terminal();
        let report = burn::execute(&config::config_dir());
        print!("{}", report.render());
        let _ = io::stdout().flush();
        std::process::exit(0);
    }

    result
}

/// The render + event loop. Draws the current state, then waits on whichever
/// happens first: a keyboard event or an inbound message. Resize/other events
/// simply fall through and trigger a redraw on the next iteration.
async fn run(
    terminal: &mut Tui,
    app: &mut App,
    mut net_rx: mpsc::Receiver<NetEvent>,
    link: NetLink,
) -> io::Result<()> {
    let mut events = EventStream::new();
    let mut store = Persistence::default();

    // Cold-boot bring-up clock: ticked *only* while the splash is showing (the
    // select branch's `if` precondition gates on `state == Splash`), so the
    // steady-state loop stays purely event-driven with no idle wakeups. Without
    // the `splash` feature the state is never `Splash`, so the branch is inert.
    let mut splash_tick = tokio::time::interval(std::time::Duration::from_millis(120));

    while !app.should_quit {
        // Expire received intel past its validity window before drawing. The
        // render also hides expired entries, but this reclaims them so the map and
        // INTEL panel can't accrete stale markers (design note §6 periodic sweep).
        app.sweep_intel(now_secs());
        terminal.draw(|frame| ui::render(frame, app))?;

        tokio::select! {
            // `biased` makes branch order deterministic instead of pseudo-random:
            // keyboard input is checked first every iteration, so operator
            // keystrokes (emergency exit, flash messages) always win tactical
            // precedence over a net_rx channel flooded with telemetry/CoT traffic.
            biased;

            // --- Keyboard input -------------------------------------------------
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) => {
                    app.handle_key(key);
                    link.drain(app);
                    save_notes(app);
                }
                // Resize is handled implicitly by redrawing; other events
                // (we never enable mouse capture) are ignored.
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err),
                // Input stream closed (stdin EOF / detached) — shut down.
                None => app.should_quit = true,
            },

            // --- Cold-boot splash clock (only while the splash is up) -----------
            // Ranked above net_rx so sustained inbound traffic (telemetry/CoT
            // floods) can't perpetually starve it under `biased` and strand the
            // UI on the splash screen; the `if` guard keeps it inert once running.
            _ = splash_tick.tick(), if app.state == app::AppState::Splash => {
                app.tick_splash();
            },

            // --- Events from the network task -----------------------------------
            maybe_event = net_rx.recv() => {
                // `None` => the sender was dropped (task ended); fall through
                // silently and keep the TUI usable for reviewing scrollback.
                if let Some(ev) = maybe_event {
                    // The store key arrives once; adopting it loads history and
                    // the intel layer before the live event is applied.
                    store.adopt(app, &ev);
                    apply_net_event(app, ev);
                }
            },
        }

        // Persist whatever this iteration marked dirty.
        store.flush(app);
    }

    Ok(())
}

// --- Network link ---------------------------------------------------------------

/// The channels down to the network task. Both are `None` in an offline build,
/// so the UI loop drains its queues the same way either way — nothing on the hot
/// path has to know which stack it is talking to.
struct NetLink {
    outbound: Option<mpsc::Sender<app::Outbound>>,
    commands: Option<mpsc::Sender<NetCommand>>,
}

impl NetLink {
    /// Hand off everything the last keystroke queued.
    fn drain(&self, app: &mut App) {
        self.send_outbound(app);
        self.send_commands(app);
    }

    /// Push accepted messages to the protocol task. A bounded channel means
    /// `try_send` can fail if that task is jammed; never swallow that — a
    /// silently dropped sitrep is worse than none. `App` owns what happens to the
    /// message and what the operator is told; here we only route the transport
    /// outcome: requeue-and-stop on a full pipe, mark-failed on a dead task.
    fn send_outbound(&self, app: &mut App) {
        let Some(tx) = &self.outbound else { return };
        while let Some(out) = app.outbox.outbound.pop_front() {
            match tx.try_send(out) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(out)) => {
                    app.requeue_choked(out);
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(out)) => app.fail_dropped(out),
            }
        }
    }

    /// Forward UI commands, persisting a propagation-node change on the way.
    /// Drained even with no network task so the setting still sticks offline.
    fn send_commands(&self, app: &mut App) {
        while let Some(cmd) = app.outbox.commands.pop_front() {
            if matches!(cmd, NetCommand::SetPropagationNode(_))
                && let Err(e) = app.config.save()
            {
                app.push_log(format!("[SYS] config save failed: {e}"));
            }
            if let Some(tx) = &self.commands {
                let _ = tx.try_send(cmd);
            }
        }
    }
}

/// Start the network task and return the link to it: the real LXMF/Reticulum
/// stack under the `net` feature, a quiet offline stub without it.
#[cfg(feature = "net")]
fn spawn_network(net_tx: mpsc::Sender<NetEvent>, config: &config::Config) -> NetLink {
    let (outbound_tx, outbound_rx) = mpsc::channel::<app::Outbound>(64);
    let (command_tx, command_rx) = mpsc::channel::<NetCommand>(16);
    tokio::spawn(net::run(net_tx, outbound_rx, command_rx, config.clone()));
    NetLink {
        outbound: Some(outbound_tx),
        commands: Some(command_tx),
    }
}

/// Offline stand-in for the network task (no `net` feature). Emits a couple of
/// banners so the Log tab confirms the async path is live, then parks — the
/// bounded channel means we hold no resources and never spin.
#[cfg(not(feature = "net"))]
fn spawn_network(net_tx: mpsc::Sender<NetEvent>, _config: &config::Config) -> NetLink {
    tokio::spawn(async move {
        let _ = net_tx
            .send(NetEvent::Sys("[SYS] FoxHole terminal online.".to_string()))
            .await;
        let _ = net_tx
            .send(NetEvent::Sys(
                "[SYS] protocol layer offline — rebuild with --features net.".to_string(),
            ))
            .await;
    });
    NetLink {
        outbound: None,
        commands: None,
    }
}

// --- Persistence ----------------------------------------------------------------

/// Persist the note buffer if a slot changed this keystroke.
fn save_notes(app: &mut App) {
    if std::mem::take(&mut app.notes_dirty)
        && let Err(e) = app.notes.save()
    {
        app.push_log(format!("[SYS] notes save failed: {e}"));
    }
}

/// The encrypted on-disk stores, driven from the UI loop.
///
/// Their key is derived from the Reticulum identity, so it only exists once the
/// network task reports it — hence the whole thing is a no-op until [`adopt`]
/// sees a [`NetEvent::StoreKey`], and a no-op *always* in an offline build,
/// which has no identity to derive from.
///
/// [`adopt`]: Persistence::adopt
#[derive(Default)]
struct Persistence {
    /// The identity-derived store key, once the network task reports it.
    #[cfg(feature = "net")]
    key: Option<[u8; 64]>,
}

impl Persistence {
    /// Take the store key from a [`NetEvent::StoreKey`] and load what is on
    /// disk: conversation history first, then the intel layer.
    #[cfg(feature = "net")]
    fn adopt(&mut self, app: &mut App, ev: &NetEvent) {
        let NetEvent::StoreKey(key) = ev else { return };

        let (loaded, skipped) = store::load_all(key);
        let n = loaded.len();
        for conv in loaded {
            app.load_conversation(conv);
        }
        self.key = Some(*key);
        if n > 0 || skipped > 0 {
            app.push_log(format!(
                "[SYS] loaded {n} conversation(s), {skipped} skipped"
            ));
        }

        // Restore the persisted intel layer (live + staged).
        let (live, staged) = intel_store::load(key);
        let (nl, ns) = (live.len(), staged.len());
        app.intel.live = live;
        app.intel.staged = staged;
        // Drop anything that expired while we were down, and don't treat the
        // freshly-loaded state as needing a re-save.
        app.sweep_intel(now_secs());
        app.intel.dirty = false;
        if nl > 0 || ns > 0 {
            app.push_log(format!("[SYS] loaded {nl} intel, {ns} staged"));
        }
    }

    /// Offline builds have no identity, so no key ever arrives.
    #[cfg(not(feature = "net"))]
    fn adopt(&mut self, _app: &mut App, _ev: &NetEvent) {}

    /// Write out whatever the last iteration marked dirty: every conversation
    /// whose history changed (skipping empty discovery-only threads) and the
    /// intel layer. Failures are logged, never fatal.
    #[cfg(feature = "net")]
    fn flush(&mut self, app: &mut App) {
        // No key yet: leave the dirty flags set so the first flush after it
        // arrives still writes everything that changed while we waited.
        let Some(key) = &self.key else { return };
        for peer in std::mem::take(&mut app.outbox.dirty) {
            let result = app
                .convs
                .items
                .iter()
                .find(|c| c.peer == peer)
                .filter(|c| c.should_persist())
                .map(|conv| store::save(key, conv));
            if let Some(Err(e)) = result {
                app.push_log(format!("[SYS] store save failed: {e}"));
            }
        }
        if std::mem::take(&mut app.intel.dirty)
            && let Err(e) = intel_store::save(key, &app.intel.live, &app.intel.staged)
        {
            app.push_log(format!("[SYS] intel store save failed: {e}"));
        }
    }

    /// Offline builds never persist; just keep the dirty flags from growing.
    #[cfg(not(feature = "net"))]
    fn flush(&mut self, app: &mut App) {
        app.outbox.dirty.clear();
        app.intel.dirty = false;
    }
}

/// Current Unix time in whole seconds (UTC); `0` if the clock predates the
/// epoch. The clock the intel stale-sweep counts against.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fold a single network event into UI state.
fn apply_net_event(app: &mut App, ev: NetEvent) {
    // While the cold-boot splash is up, let the real readiness events flip its
    // bring-up lines to their reported status (live monitor).
    #[cfg(feature = "splash")]
    if app.state == app::AppState::Splash {
        mark_boot_from_event(app, &ev);
    }

    match ev {
        NetEvent::Sys(line) => app.push_log(line),
        NetEvent::Local(addr) => app.local_address = Some(addr),
        NetEvent::Peer { kind, hash, name } => app.upsert_peer(kind, hash, name),
        NetEvent::Message {
            source,
            title,
            content,
        } => {
            let body = if title.is_empty() {
                content
            } else {
                format!("{title}: {content}")
            };
            app.deliver(&source, &body);
        }
        NetEvent::Telemetry { source, lat, lon } => {
            app.set_location(&source, app::GeoPos::new(lat, lon));
        }
        NetEvent::Cot { source, event } => app.apply_cot(source, event),
        NetEvent::Sync(status) => app.sync_status = status,
        NetEvent::MsgStatus { id, status } => app.set_msg_status(id, status),
        NetEvent::Path { hash, hops, iface } => app.record_path(hash, hops, iface),
        NetEvent::NomadNode {
            identity,
            dest,
            name,
            last_seen,
        } => app.upsert_nomad(identity, dest, name, last_seen),
        NetEvent::Page {
            identity,
            path,
            body,
        } => app.set_page(identity, path, body),
        NetEvent::Interfaces { interfaces, links } => app.set_interfaces(interfaces, links),
        // Handled in `run` (loads history); nothing to fold into UI state here.
        NetEvent::StoreKey(_) => {}
    }
}

/// Flip cold-boot lines to their reported status as the real bring-up events
/// arrive: encrypted store + cache on the store key, mesh + console on the local
/// address (which also opens the hand-off), and best-effort accents off the
/// transport/identity banners. Steps not reached this way still appear on the
/// timer, so a changed banner string only loses an early accent, never a line.
#[cfg(feature = "splash")]
fn mark_boot_from_event(app: &mut App, ev: &NetEvent) {
    use crate::app::BootStep;
    match ev {
        NetEvent::StoreKey(_) => {
            app.mark_boot(BootStep::Store);
            app.mark_boot(BootStep::Cache);
        }
        NetEvent::Local(_) => {
            app.mark_boot(BootStep::Mesh);
            app.mark_boot(BootStep::Console);
        }
        NetEvent::Sys(line) if line.contains("transport online") => {
            app.mark_boot(BootStep::Iface);
        }
        NetEvent::Sys(line) if line.contains("identity ") => {
            app.mark_boot(BootStep::Identity);
        }
        _ => {}
    }
}

/// Enter raw mode, switch to the alternate screen, and hide the cursor.
fn setup_terminal() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // No mouse capture is enabled — FoxHole is strictly keyboard-driven.
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Undo [`setup_terminal`]. Idempotent enough to be safe if called twice (e.g.
/// panic hook then Drop): leaving the alt screen / showing the cursor again is
/// harmless. Operates on a fresh stdout handle so it needs no borrow of the
/// terminal.
fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen, cursor::Show)?;
    disable_raw_mode()
}

/// RAII guard that restores the terminal when dropped, covering normal returns
/// and `?`-propagated errors.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore_terminal();
    }
}

/// Chain a terminal restore in front of the default panic hook so the operator
/// can actually read the panic message on a cleaned-up screen.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        default_hook(info);
    }));
}
