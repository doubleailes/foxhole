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

mod app;
mod config;
#[cfg(feature = "net")]
mod net;
mod storage;
#[cfg(feature = "net")]
mod store;
mod ui;

use std::io::{self, Stdout};

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

    // The network task feeds events in; the UI loop drains them. Under the
    // `net` feature it is the real LXMF/Reticulum stack (and we keep a sender to
    // hand it outbound messages); otherwise it is an offline stub.
    let (net_tx, net_rx) = mpsc::channel::<NetEvent>(INBOUND_CAPACITY);

    let mut app = App::new();
    app.config = config::Config::load();

    // Under `net` the network task gets a clone of the config plus channels for
    // outbound messages and UI commands; offline it is a quiet stub.
    #[cfg(feature = "net")]
    let (outbound_tx, command_tx) = {
        let (otx, orx) = mpsc::channel::<app::Outbound>(64);
        let (ctx, crx) = mpsc::channel::<NetCommand>(16);
        tokio::spawn(net::run(net_tx, orx, crx, app.config.clone()));
        (Some(otx), Some(ctx))
    };
    #[cfg(not(feature = "net"))]
    let (outbound_tx, command_tx): (
        Option<mpsc::Sender<app::Outbound>>,
        Option<mpsc::Sender<NetCommand>>,
    ) = {
        spawn_stub_task(net_tx);
        (None, None)
    };

    // Live discovery replaces the offline demo peers; start from an empty list.
    #[cfg(feature = "net")]
    app.conversations.clear();

    // `_guard` drops as this returns, restoring the terminal whether `run`
    // finished cleanly or propagated an I/O error.
    run(&mut terminal, &mut app, net_rx, outbound_tx, command_tx).await
}

/// The render + event loop. Draws the current state, then waits on whichever
/// happens first: a keyboard event or an inbound message. Resize/other events
/// simply fall through and trigger a redraw on the next iteration.
async fn run(
    terminal: &mut Tui,
    app: &mut App,
    mut net_rx: mpsc::Receiver<NetEvent>,
    outbound_tx: Option<mpsc::Sender<app::Outbound>>,
    command_tx: Option<mpsc::Sender<NetCommand>>,
) -> io::Result<()> {
    let mut events = EventStream::new();
    // Conversation-store key, once the network task derives it from the identity.
    #[cfg(feature = "net")]
    let mut store_key: Option<[u8; 64]> = None;

    while !app.should_quit {
        terminal.draw(|frame| ui::render(frame, app))?;

        tokio::select! {
            // --- Keyboard input -------------------------------------------------
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) => {
                    app.handle_key(key);
                    // Hand off anything the keystroke queued for transmission.
                    if let Some(tx) = &outbound_tx {
                        while let Some(out) = app.outbound.pop_front() {
                            let _ = tx.try_send(out);
                        }
                    }
                    // Drain UI commands: persist a node change, then forward.
                    while let Some(cmd) = app.commands.pop_front() {
                        if matches!(cmd, NetCommand::SetPropagationNode(_))
                            && let Err(e) = app.config.save()
                        {
                            app.push_log(format!("[SYS] config save failed: {e}"));
                        }
                        if let Some(tx) = &command_tx {
                            let _ = tx.try_send(cmd);
                        }
                    }
                }
                // Resize is handled implicitly by redrawing; other events
                // (we never enable mouse capture) are ignored.
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err),
                // Input stream closed (stdin EOF / detached) — shut down.
                None => app.should_quit = true,
            },

            // --- Events from the network task -----------------------------------
            maybe_event = net_rx.recv() => {
                // `None` => the sender was dropped (task ended); fall through
                // silently and keep the TUI usable for reviewing scrollback.
                if let Some(ev) = maybe_event {
                    // The store key arrives once; stash it and load history before
                    // the live event is applied.
                    #[cfg(feature = "net")]
                    if let NetEvent::StoreKey(key) = &ev {
                        let (loaded, skipped) = store::load_all(key);
                        let n = loaded.len();
                        for conv in loaded {
                            app.load_conversation(conv);
                        }
                        store_key = Some(*key);
                        if n > 0 || skipped > 0 {
                            app.push_log(format!(
                                "[SYS] loaded {n} conversation(s), {skipped} skipped"
                            ));
                        }
                    }
                    apply_net_event(app, ev);
                }
            },
        }

        // Persist any conversation whose history changed this iteration. Skips
        // empty (discovery-only) threads; failures are logged, never fatal.
        #[cfg(feature = "net")]
        if let Some(key) = &store_key {
            for peer in std::mem::take(&mut app.dirty) {
                let result = app
                    .conversations
                    .iter()
                    .find(|c| c.peer == peer)
                    .filter(|c| c.pinned || !c.messages.is_empty())
                    .map(|conv| store::save(key, conv));
                if let Some(Err(e)) = result {
                    app.push_log(format!("[SYS] store save failed: {e}"));
                }
            }
        }
        // Offline build never persists; keep the dirty list from growing.
        #[cfg(not(feature = "net"))]
        app.dirty.clear();
    }

    Ok(())
}

/// Fold a single network event into UI state.
fn apply_net_event(app: &mut App, ev: NetEvent) {
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
        NetEvent::Sync(status) => app.sync_status = status,
        NetEvent::MsgStatus { id, status } => app.set_msg_status(id, status),
        // Handled in `run` (loads history); nothing to fold into UI state here.
        NetEvent::StoreKey(_) => {}
    }
}

/// Offline stand-in for the network task (no `net` feature). Emits a couple of
/// banners so the Log tab confirms the async path is live, then parks — the
/// bounded channel means we hold no resources and never spin. With `--features
/// net`, `net::run` replaces this with the real LXMF/Reticulum stack.
#[cfg(not(feature = "net"))]
fn spawn_stub_task(tx: mpsc::Sender<NetEvent>) {
    tokio::spawn(async move {
        let _ = tx
            .send(NetEvent::Sys("[SYS] FoxHole terminal online.".to_string()))
            .await;
        let _ = tx
            .send(NetEvent::Sys(
                "[SYS] protocol layer offline — rebuild with --features net.".to_string(),
            ))
            .await;
    });
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
