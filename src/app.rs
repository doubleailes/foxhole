//! Application state machine.
//!
//! `App` is the single source of truth for what the operator sees: which tool
//! (top-level tab) is active, which pane within it holds focus, the per-peer
//! conversations, and the various scrollbacks. It is intentionally free of any
//! I/O or rendering — `main` drives it from input/network events, `ui` reads it
//! to draw. This keeps the hot render path trivial and the logic unit-testable.
//!
//! Two focus tiers mirror Nomadnet's layout:
//!   * **Tool** — the active top-level tab (Conversations, Network, Log,
//!     Interfaces, Guide), switched with Ctrl+N / Ctrl+P.
//!   * **Pane** — the focusable region *within* a tool, cycled with Tab. The
//!     Conversations tool has three panes (peer list, thread, transmit); the
//!     other tools are read-only single views.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::Config;
pub use crate::domain::{
    Conversation, Entry, MsgStatus, NetCommand, NetEvent, Node, NomadNode, Outbound, Page,
    PageStatus, PathProbe, PeerKind, path_summary,
};
use crate::domain::{normalize_address, now_secs, short_hash};

/// Which field the New Conversation popup is editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewConvField {
    Address,
    Alias,
}

impl NewConvField {
    /// Toggle between the two fields (Tab).
    fn next(self) -> Self {
        match self {
            NewConvField::Address => NewConvField::Alias,
            NewConvField::Alias => NewConvField::Address,
        }
    }
}

/// Modal state for adding a conversation by LXMF address (Ctrl+O).
pub struct NewConv {
    /// LXMF destination hash being typed (colons/spaces tolerated).
    pub address: String,
    /// Optional local alias for the peer.
    pub alias: String,
    /// Which field has focus.
    pub field: NewConvField,
    /// Set when the last Enter had an invalid address; cleared on edit.
    pub error: bool,
}

impl NewConv {
    /// The buffer for the focused field.
    fn current_mut(&mut self) -> &mut String {
        match self.field {
            NewConvField::Address => &mut self.address,
            NewConvField::Alias => &mut self.alias,
        }
    }
}

/// The exact token the operator must type to confirm a burn.
pub const BURN_TOKEN: &str = "BURN";

/// Modal state for the burn confirmation (Ctrl+K). Destroying all session data
/// is gated behind typing [`BURN_TOKEN`] so it can't fire by accident.
pub struct BurnConfirm {
    /// The confirmation token as typed so far.
    pub input: String,
    /// Set when the last Enter had the wrong token; cleared on edit.
    pub error: bool,
}

/// A top-level tool, rendered as a tab in the menu strip. Each tool owns its
/// own body layout and key handling (see `ui` and [`App::handle_tool_key`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// LXMF messaging: per-peer conversations plus the compose buffer.
    Conversations,
    /// Discovered peers and propagation nodes.
    Network,
    /// Nomad Network page browser (micron pages served by `nomadnetwork.node`).
    Browser,
    /// System/application log (banners, diagnostics).
    Log,
    /// Reticulum interface status.
    Interfaces,
    /// Static help text.
    Guide,
}

impl Tool {
    /// Tab order, left to right. Drives both the menu strip and Ctrl+N/P
    /// cycling, so there is a single source of truth for ordering.
    pub const ALL: [Tool; 6] = [
        Tool::Conversations,
        Tool::Network,
        Tool::Browser,
        Tool::Log,
        Tool::Interfaces,
        Tool::Guide,
    ];

    /// Label shown in the tab strip.
    pub fn title(self) -> &'static str {
        match self {
            Tool::Conversations => "Conversations",
            Tool::Network => "Network",
            Tool::Browser => "Browser",
            Tool::Log => "Log",
            Tool::Interfaces => "Interfaces",
            Tool::Guide => "Guide",
        }
    }

    /// Short tag for the status bar's `TOOL:` field.
    pub fn tag(self) -> &'static str {
        match self {
            Tool::Conversations => "CONV",
            Tool::Network => "NET",
            Tool::Browser => "WEB",
            Tool::Log => "LOG",
            Tool::Interfaces => "IFACE",
            Tool::Guide => "GUIDE",
        }
    }

    /// Index within [`Tool::ALL`]. Panics-free because every variant is listed.
    fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    /// Next tab, wrapping (bound to Ctrl+N).
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// Previous tab, wrapping (bound to Ctrl+P).
    pub fn prev(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// The focusable regions *within the Conversations tool*. The status bar and
/// the read-only tools never take pane focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    /// The list of conversations (one per peer). Up/Down move the selection.
    PeerList,
    /// Scrollback of the selected conversation's received/sent traffic.
    Thread,
    /// The editable buffer the operator composes outbound messages in.
    Transmit,
}

impl Pane {
    /// Next pane in the Tab cycle: PeerList -> Thread -> Transmit -> PeerList.
    pub fn next(self) -> Self {
        match self {
            Pane::PeerList => Pane::Thread,
            Pane::Thread => Pane::Transmit,
            Pane::Transmit => Pane::PeerList,
        }
    }
}

/// The two columns of the Network tab; `net_col` tracks which has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetColumn {
    /// Known `lxmf.delivery` peers (the conversations roster).
    Peers,
    /// `lxmf.propagation` store-and-forward nodes.
    Nodes,
}

impl NetColumn {
    /// Toggle to the other column (Tab / Left / Right).
    fn other(self) -> Self {
        match self {
            NetColumn::Peers => NetColumn::Nodes,
            NetColumn::Nodes => NetColumn::Peers,
        }
    }
}

/// Which Browser-tab pane has focus: the node list or the page viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPane {
    Nodes,
    Page,
}

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
const MAX_TICKS: u32 = 33;

#[cfg(feature = "splash")]
impl Boot {
    fn new() -> Self {
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
    fn top() -> Self {
        Self {
            offset: Cell::new(0),
            stick_bottom: Cell::new(false),
            viewport: Cell::new(0),
            anchored_bottom: false,
        }
    }

    /// A bottom-anchored pane that follows the newest line (Log, thread).
    fn bottom() -> Self {
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

/// Whole-program UI state.
pub struct App {
    /// Active top-level tool (drives the tab strip + key delegation).
    pub active: Tool,
    /// Focused pane within the Conversations tool (drives the reversed
    /// highlight + key routing there). Ignored by the read-only tools.
    pub focus: Pane,
    /// All conversations, in display order.
    pub conversations: Vec<Conversation>,
    /// Index of the selected conversation within `conversations`.
    pub selected: usize,
    /// Discovered propagation nodes (Network tab).
    pub nodes: Vec<Node>,
    /// Highlighted row in the Network tab's propagation-node list.
    pub node_selected: usize,
    /// Which Network-tab column has focus (Peers reuses `selected`, Nodes
    /// reuses `node_selected` for the in-column cursor).
    pub net_col: NetColumn,
    /// Latest rnpath-style path probe per hex destination hash (Network tab).
    pub path_probes: HashMap<String, PathProbe>,
    /// Discovered Nomad Network nodes (Browser tab).
    pub nomad_nodes: Vec<NomadNode>,
    /// Highlighted row in the Browser tab's node list.
    pub browser_selected: usize,
    /// Which Browser-tab pane has focus (node list vs page viewport).
    pub browser_pane: BrowserPane,
    /// The page currently being viewed/fetched in the Browser tab, if any.
    pub page: Option<Page>,
    /// Back stack of visited `(node identity, path)` pages (Backspace pops).
    pub history: Vec<(String, String)>,
    /// Scroll positions for the overflowing text panes (PageUp/PageDown/Home/End).
    pub page_scroll: Scroll,
    pub guide_scroll: Scroll,
    pub log_scroll: Scroll,
    pub thread_scroll: Scroll,
    /// This node's own LXMF address (hex), once the network task reports it.
    pub local_address: Option<String>,
    /// When `Some`, a propagation sync is running and the pop-up shows this text.
    pub sync_status: Option<String>,
    /// When `Some`, the New Conversation popup is open (and captures all input).
    pub new_conv: Option<NewConv>,
    /// When `Some`, the burn-confirmation modal is open (captures all input).
    pub burn_confirm: Option<BurnConfirm>,
    /// Set once the operator confirms a burn; `main` shreds the config dir and
    /// exits. (The wipe itself is I/O — done outside `App`.)
    pub burn: bool,
    /// Persisted operator settings (display name, hub, active propagation node).
    pub config: Config,
    /// Commands queued for the network task; drained by `main` after key input.
    pub commands: VecDeque<NetCommand>,
    /// Monotonic id source for correlating outbound messages with their status.
    pub next_msg_id: u64,
    /// Peer keys whose on-disk copy is stale; `main` drains this and persists
    /// each changed conversation. Keeps `App` itself free of I/O.
    pub dirty: Vec<String>,
    /// Messages accepted for transmission, awaiting handoff to the protocol
    /// task. FIFO so ordering on the wire matches operator intent.
    pub outbound: VecDeque<Outbound>,
    /// System log scrollback shown by the Log tool (`[SYS]` lines, diagnostics),
    /// each timestamped (UTC).
    pub syslog: Vec<Entry>,
    /// Set when the operator requests shutdown (Ctrl+Q); the main loop checks
    /// this each iteration.
    pub should_quit: bool,
    /// Current top-level screen (cold-boot splash vs. console).
    pub state: AppState,
    /// Boot-sequence progress (only meaningful while `state == Splash`).
    #[cfg(feature = "splash")]
    pub boot: Boot,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Fresh session: open on Conversations with the Transmit pane focused so
    /// the operator can type at once. Seeds a few demo peers so the offline UI
    /// is usable; under the `net` feature `main` clears them at startup and live
    /// announce-based discovery fills the list instead.
    pub fn new() -> Self {
        let mut alice = Conversation::new("alice");
        alice
            .messages
            .push(Entry::now("[RX] hey, you on the mesh?".to_string()));
        let conversations = vec![alice, Conversation::new("bob"), Conversation::new("carol")];

        Self {
            active: Tool::Conversations,
            focus: Pane::Transmit,
            conversations,
            selected: 0,
            nodes: Vec::new(),
            node_selected: 0,
            net_col: NetColumn::Peers,
            path_probes: HashMap::new(),
            nomad_nodes: Vec::new(),
            browser_selected: 0,
            browser_pane: BrowserPane::Nodes,
            page: None,
            history: Vec::new(),
            page_scroll: Scroll::top(),
            guide_scroll: Scroll::top(),
            log_scroll: Scroll::bottom(),
            thread_scroll: Scroll::bottom(),
            local_address: None,
            sync_status: None,
            new_conv: None,
            burn_confirm: None,
            burn: false,
            config: Config::default(),
            commands: VecDeque::new(),
            next_msg_id: 1,
            dirty: Vec::new(),
            outbound: VecDeque::new(),
            syslog: Vec::new(),
            should_quit: false,
            // Cold-boot through the splash unless it's compiled out, suppressed,
            // or under unit tests (which exercise the console directly).
            state: if cfg!(feature = "splash")
                && !cfg!(test)
                && std::env::var_os("FOXHOLE_NO_SPLASH").is_none()
            {
                AppState::Splash
            } else {
                AppState::Running
            },
            #[cfg(feature = "splash")]
            boot: Boot::new(),
        }
    }

    /// Record/refresh a discovered peer. Delivery peers become conversations
    /// (so they appear in the peer list and can be messaged); propagation nodes
    /// go to the Network-tab registry. Keyed by hex destination hash.
    pub fn upsert_peer(&mut self, kind: PeerKind, hash: String, name: Option<String>) {
        let now = now_secs();
        match kind {
            PeerKind::Delivery => {
                if let Some(conv) = self.conversations.iter_mut().find(|c| c.peer == hash) {
                    if name.is_some() {
                        conv.display_name = name;
                    }
                    conv.last_seen = now;
                } else {
                    let mut conv = Conversation::new(hash);
                    conv.display_name = name;
                    conv.last_seen = now;
                    self.conversations.push(conv);
                }
            }
            PeerKind::Propagation => {
                if let Some(node) = self.nodes.iter_mut().find(|n| n.hash == hash) {
                    if name.is_some() {
                        node.name = name;
                    }
                    node.last_seen = now;
                } else {
                    self.nodes.push(Node {
                        hash,
                        name,
                        last_seen: now,
                    });
                }
            }
        }
    }

    /// Route a key event in three tiers: program-global bindings first, then
    /// tool switching, then whatever is left is delegated to the active tool.
    pub fn handle_key(&mut self, key: KeyEvent) {
        // On Windows (and with kitty keyboard protocol) both press and release
        // are reported; act on press only so each keystroke fires once.
        if key.kind != KeyEventKind::Press {
            return;
        }

        // While the boot splash is up, any key dismisses it straight to console.
        if self.state == AppState::Splash {
            self.state = AppState::Running;
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // The burn-confirmation modal, when open, captures all input.
        if self.burn_confirm.is_some() {
            self.handle_burn_key(key);
            return;
        }

        // The New Conversation modal, when open, captures all input.
        if self.new_conv.is_some() {
            self.handle_new_conv_key(ctrl, key);
            return;
        }

        // Scrolling works in whichever text pane has focus; these keys are unused
        // by the tools, so handle them globally.
        if matches!(
            key.code,
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
        ) {
            if let Some(s) = self.active_scroll() {
                match key.code {
                    KeyCode::PageUp => s.page_up(),
                    KeyCode::PageDown => s.page_down(),
                    KeyCode::Home => s.to_top(),
                    KeyCode::End => s.to_bottom(),
                    _ => {}
                }
            }
            return;
        }

        match (ctrl, key.code) {
            // --- Program-global --------------------------------------------------
            (true, KeyCode::Char('q')) => self.should_quit = true,
            (true, KeyCode::Char('o')) => self.open_new_conv(),
            (true, KeyCode::Char('k')) => self.open_burn(),

            // --- Tool (tab) switching -------------------------------------------
            (true, KeyCode::Char('n')) => self.active = self.active.next(),
            (true, KeyCode::Char('p')) => self.active = self.active.prev(),

            // --- Delegated to the active tool -----------------------------------
            _ => self.handle_tool_key(ctrl, key),
        }
    }

    /// The scrollable text pane that currently has focus, if any — what
    /// PageUp/PageDown/Home/End act on.
    fn active_scroll(&self) -> Option<&Scroll> {
        match self.active {
            Tool::Browser if self.browser_pane == BrowserPane::Page => Some(&self.page_scroll),
            Tool::Log => Some(&self.log_scroll),
            Tool::Guide => Some(&self.guide_scroll),
            Tool::Conversations if self.focus == Pane::Thread => Some(&self.thread_scroll),
            _ => None,
        }
    }

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

    /// Open the New Conversation popup (Ctrl+O), focused on the address field.
    fn open_new_conv(&mut self) {
        self.new_conv = Some(NewConv {
            address: String::new(),
            alias: String::new(),
            field: NewConvField::Address,
            error: false,
        });
    }

    /// Open the burn-confirmation modal (Ctrl+K).
    fn open_burn(&mut self) {
        self.burn_confirm = Some(BurnConfirm {
            input: String::new(),
            error: false,
        });
    }

    /// Key handling while the burn modal is open: type the token, Enter to
    /// confirm (only when it exactly matches), Esc to cancel.
    fn handle_burn_key(&mut self, key: KeyEvent) {
        let Some(b) = &mut self.burn_confirm else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.burn_confirm = None,
            KeyCode::Enter => {
                if b.input == BURN_TOKEN {
                    // Confirmed — `main` shreds the config dir and exits.
                    self.burn = true;
                    self.should_quit = true;
                    self.burn_confirm = None;
                } else {
                    b.error = true;
                }
            }
            KeyCode::Backspace => {
                b.input.pop();
                b.error = false;
            }
            KeyCode::Char(c) => {
                b.input.push(c);
                b.error = false;
            }
            _ => {}
        }
    }

    /// Key handling while the New Conversation modal is open.
    fn handle_new_conv_key(&mut self, ctrl: bool, key: KeyEvent) {
        match (ctrl, key.code) {
            (_, KeyCode::Esc) => self.new_conv = None,
            (_, KeyCode::Tab) => {
                if let Some(nc) = self.new_conv.as_mut() {
                    nc.field = nc.field.next();
                }
            }
            (_, KeyCode::Enter) => {
                // Read the fields without holding the borrow across the create.
                let Some((addr, alias)) = self
                    .new_conv
                    .as_ref()
                    .map(|nc| (nc.address.clone(), nc.alias.clone()))
                else {
                    return;
                };
                if self.start_conversation(&addr, &alias) {
                    self.new_conv = None;
                } else if let Some(nc) = self.new_conv.as_mut() {
                    nc.error = true;
                }
            }
            (false, KeyCode::Backspace) => {
                if let Some(nc) = self.new_conv.as_mut() {
                    nc.error = false;
                    nc.current_mut().pop();
                }
            }
            (false, KeyCode::Char(c)) => {
                if let Some(nc) = self.new_conv.as_mut() {
                    nc.error = false;
                    nc.current_mut().push(c);
                }
            }
            _ => {}
        }
    }

    /// Create (or focus) a conversation for a manually-entered LXMF address.
    /// Returns false if the address isn't a 32-char hex destination hash.
    pub fn start_conversation(&mut self, address: &str, alias: &str) -> bool {
        let key = normalize_address(address);
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        let alias = alias.trim();
        let idx = match self.conversations.iter().position(|c| c.peer == key) {
            Some(i) => i,
            None => {
                let mut conv = Conversation::new(key.clone());
                conv.pinned = true;
                self.conversations.push(conv);
                self.conversations.len() - 1
            }
        };
        if !alias.is_empty() {
            self.conversations[idx].display_name = Some(alias.to_string());
            self.conversations[idx].pinned = true;
        }
        self.selected = idx;
        self.active = Tool::Conversations;
        self.focus = Pane::Transmit;
        self.mark_dirty(&key);
        true
    }

    /// Per-tool key handling. Conversations composes/sends; Network selects the
    /// active propagation node; the remaining tools are read-only.
    fn handle_tool_key(&mut self, ctrl: bool, key: KeyEvent) {
        match self.active {
            Tool::Conversations => self.handle_conversations_key(ctrl, key),
            Tool::Network => self.handle_network_key(ctrl, key),
            Tool::Browser => self.handle_browser_key(key),
            _ => {}
        }
    }

    /// Conversations: pane cycling, peer navigation, compose + send.
    fn handle_conversations_key(&mut self, ctrl: bool, key: KeyEvent) {
        match (ctrl, key.code) {
            (true, KeyCode::Char('s')) => self.transmit(),
            (true, KeyCode::Char('x')) => self.purge(),
            // On-demand propagation sync (off-grid: no automatic polling).
            (true, KeyCode::Char('r')) => self.commands.push_back(NetCommand::SyncNow),
            (_, KeyCode::Tab) => self.toggle_focus(),

            // Peer-list navigation — only when that pane is focused.
            (false, KeyCode::Up) if self.focus == Pane::PeerList => self.select_prev(),
            (false, KeyCode::Down) if self.focus == Pane::PeerList => self.select_next(),

            // Transmit-pane editing — only when that pane is focused.
            (false, KeyCode::Char(c)) if self.focus == Pane::Transmit => {
                if let Some(conv) = self.selected_conv_mut() {
                    conv.draft.push(c);
                }
            }
            (false, KeyCode::Backspace) if self.focus == Pane::Transmit => {
                if let Some(conv) = self.selected_conv_mut() {
                    conv.draft.pop();
                }
            }

            // Everything else is ignored.
            _ => {}
        }
    }

    /// Network: two columns (peers | nodes). Up/Down move within the focused
    /// column; Tab/Left/Right switch columns; Enter opens a peer's conversation
    /// or sets a node active; `p` path-probes the selection; `s` syncs.
    fn handle_network_key(&mut self, _ctrl: bool, key: KeyEvent) {
        match key.code {
            KeyCode::Up => match self.net_col {
                NetColumn::Peers => self.select_prev(),
                NetColumn::Nodes => self.node_selected = self.node_selected.saturating_sub(1),
            },
            KeyCode::Down => match self.net_col {
                NetColumn::Peers => self.select_next(),
                NetColumn::Nodes => {
                    if self.node_selected + 1 < self.nodes.len() {
                        self.node_selected += 1;
                    }
                }
            },
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => self.net_col = self.net_col.other(),
            KeyCode::Enter => match self.net_col {
                // Jump straight from the roster into the chat.
                NetColumn::Peers => {
                    if self.selected_conv().is_some() {
                        self.active = Tool::Conversations;
                        self.focus = Pane::Transmit;
                        self.mark_selected_read();
                    }
                }
                NetColumn::Nodes => {
                    if let Some(node) = self.nodes.get(self.node_selected) {
                        let hash = node.hash.clone();
                        self.config.propagation_node = Some(hash.clone());
                        self.commands
                            .push_back(NetCommand::SetPropagationNode(Some(hash)));
                    }
                }
            },
            // rnpath-style path probe of the focused selection.
            KeyCode::Char('p') => {
                if let Some(hash) = self.focused_net_hash() {
                    self.syslog.push(Entry::now(format!(
                        "[RT] PATH {}.. requesting",
                        short_hash(&hash)
                    )));
                    self.commands.push_back(NetCommand::RequestPath(hash));
                }
            }
            KeyCode::Char('s') => self.commands.push_back(NetCommand::SyncNow),
            _ => {}
        }
    }

    /// The hex destination hash of the focused Network-tab selection, if any.
    fn focused_net_hash(&self) -> Option<String> {
        match self.net_col {
            NetColumn::Peers => self.selected_conv().map(|c| c.peer.clone()),
            NetColumn::Nodes => self.nodes.get(self.node_selected).map(|n| n.hash.clone()),
        }
    }

    /// Record an rnpath probe result: store it for the Network tab and log a
    /// tagged `[RT]` line so the Log tab keeps the history.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn record_path(&mut self, hash: String, hops: Option<u8>, iface: Option<String>) {
        let summary = path_summary(hops, iface.as_deref());
        self.syslog.push(Entry::now(format!(
            "[RT] PATH {}..: {summary}",
            short_hash(&hash)
        )));
        self.path_probes.insert(
            hash,
            PathProbe {
                at: now_secs(),
                hops,
                iface,
            },
        );
    }

    /// Browser: two panes (node list / page viewport), switched with Tab.
    /// Nodes — Up/Down select, Enter/`g` open the node's index. Page — Up/Down
    /// move the element cursor, type into a focused field, Enter follow a link,
    /// Backspace back. `r` reloads (unless a field is being edited).
    fn handle_browser_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab => {
                self.browser_pane = match self.browser_pane {
                    BrowserPane::Nodes => BrowserPane::Page,
                    BrowserPane::Page => BrowserPane::Nodes,
                };
            }
            KeyCode::Char('r') if !self.editing_field() => self.reload(),
            _ => match self.browser_pane {
                BrowserPane::Nodes => self.handle_browser_nodes_key(key),
                BrowserPane::Page => self.handle_browser_page_key(key),
            },
        }
    }

    /// Node-list pane keys.
    fn handle_browser_nodes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.browser_selected = self.browser_selected.saturating_sub(1),
            KeyCode::Down => {
                if self.browser_selected + 1 < self.nomad_nodes.len() {
                    self.browser_selected += 1;
                }
            }
            KeyCode::Enter | KeyCode::Char('g') => {
                if let Some(node) = self.nomad_nodes.get(self.browser_selected) {
                    let id = node.identity.clone();
                    self.fetch_page(id, "/page/index.mu".to_string(), Vec::new(), true);
                    self.browser_pane = BrowserPane::Page; // focus the page to read/follow
                }
            }
            _ => {}
        }
    }

    /// Page-viewport pane keys: element navigation, field editing, link follow.
    fn handle_browser_page_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.move_element(-1),
            KeyCode::Down => self.move_element(1),
            // A focused text field captures typing; otherwise these act on links.
            KeyCode::Char(c) if self.focused_field_name().is_some() => self.field_push(c),
            KeyCode::Backspace if self.focused_field_name().is_some() => self.field_pop(),
            KeyCode::Enter => self.follow_link(),
            KeyCode::Backspace => self.go_back(),
            _ => {}
        }
    }

    /// Move the page-element cursor by `delta`, clamped.
    fn move_element(&mut self, delta: isize) {
        if let Some(p) = &mut self.page {
            let n = p.elements.len();
            if n == 0 {
                return;
            }
            let cur = p.element_sel as isize;
            p.element_sel = cur.saturating_add(delta).clamp(0, n as isize - 1) as usize;
        }
    }

    /// Name of the focused element if it is a text field.
    fn focused_field_name(&self) -> Option<String> {
        let p = self.page.as_ref()?;
        match p.elements.get(p.element_sel)? {
            crate::micron::Element::Field { name, .. } => Some(name.clone()),
            _ => None,
        }
    }

    /// Whether the operator is editing a page text field (so `r` types instead
    /// of reloading).
    fn editing_field(&self) -> bool {
        self.active == Tool::Browser
            && self.browser_pane == BrowserPane::Page
            && self.focused_field_name().is_some()
    }

    /// Append a char to the focused field's value (end-insert editing).
    fn field_push(&mut self, c: char) {
        if let Some(name) = self.focused_field_name()
            && let Some(p) = &mut self.page
        {
            p.field_values.entry(name).or_default().push(c);
        }
    }

    /// Delete the last char of the focused field's value.
    fn field_pop(&mut self) {
        if let Some(name) = self.focused_field_name()
            && let Some(p) = &mut self.page
        {
            p.field_values.entry(name).or_default().pop();
        }
    }

    /// Reload the current page (no history push).
    fn reload(&mut self) {
        if let Some(p) = &self.page {
            let (node, path) = (p.node.clone(), p.path.clone());
            self.fetch_page(node, path, Vec::new(), false);
        }
    }

    /// Follow the focused link, submitting its form fields if it has any.
    fn follow_link(&mut self) {
        let Some(crate::micron::Element::Link { target, fields }) = self
            .page
            .as_ref()
            .and_then(|p| p.elements.get(p.element_sel))
        else {
            return;
        };
        let (target, fields) = (target.clone(), fields.clone());
        match self.resolve_link(&target) {
            Some((identity, path)) => {
                let form = self.collect_form(&fields);
                self.fetch_page(identity, path, form, true);
            }
            None => {
                // Unsupported scheme or an undiscovered node — surface it inline.
                if let Some(p) = &mut self.page {
                    p.status = PageStatus::Error(format!("cannot follow link: {target}"));
                }
            }
        }
    }

    /// Collect a link's form submission per NomadNet: `*` → every field;
    /// `name` → `field_<name>`; `k=v` → `var_<k>`. (Checkbox/radio deferred.)
    fn collect_form(&self, link_fields: &[String]) -> Vec<(String, String)> {
        let Some(p) = &self.page else {
            return Vec::new();
        };
        let all = link_fields.iter().any(|f| f == "*");
        let mut out = Vec::new();
        // Literal `k=v` variables embedded in the link.
        for f in link_fields {
            if let Some((k, v)) = f.split_once('=') {
                out.push((format!("var_{k}"), v.to_string()));
            }
        }
        // Field values (all, or the named ones).
        for el in &p.elements {
            if let crate::micron::Element::Field { name, .. } = el
                && (all || link_fields.iter().any(|f| f == name))
            {
                let value = p.field_values.get(name).cloned().unwrap_or_default();
                out.push((format!("field_{name}"), value));
            }
        }
        out
    }

    /// Resolve a micron link `url` to `(node identity, page path)`.
    /// `:/path` → the current page's node; `<dest>:/path` or `<dest>` → the
    /// discovered node with that destination hash (`/page/index.mu` default).
    /// Returns `None` for unsupported schemes or unknown destinations.
    fn resolve_link(&self, url: &str) -> Option<(String, String)> {
        // Out of scope (Phase 2): LXMF and partial schemes.
        if url.contains('@') || url.starts_with("p:") {
            return None;
        }
        let (host, path) = match url.split_once(':') {
            Some((h, p)) => (h, p.to_string()),
            // No ':' — only a bare 32-hex destination is valid (default page).
            None => (url, "/page/index.mu".to_string()),
        };
        let path = if path.is_empty() {
            "/page/index.mu".to_string()
        } else {
            path
        };
        if host.is_empty() {
            // Relative link — stay on the current page's node.
            return self.page.as_ref().map(|p| (p.node.clone(), path));
        }
        // Absolute link — `host` is a destination hash; map it to a known node.
        let host = host.to_lowercase();
        self.nomad_nodes
            .iter()
            .find(|n| n.dest == host)
            .map(|n| (n.identity.clone(), path))
    }

    /// Go back to the previous page in history (no-op when empty).
    fn go_back(&mut self) {
        if let Some((node, path)) = self.history.pop() {
            self.fetch_page(node, path, Vec::new(), false);
        }
    }

    /// Queue a page fetch and show the fetching state. `fields` is the form
    /// submission (empty for a plain GET). When `push_history`, the current loaded
    /// page is pushed onto the back stack first. Skips a duplicate fetch already
    /// in flight for the same page.
    fn fetch_page(
        &mut self,
        identity: String,
        path: String,
        fields: Vec<(String, String)>,
        push_history: bool,
    ) {
        let already = matches!(
            &self.page,
            Some(p) if p.node == identity && p.path == path && matches!(p.status, PageStatus::Fetching)
        );
        if already {
            return;
        }
        if push_history && let Some(p) = &self.page {
            self.history.push((p.node.clone(), p.path.clone()));
        }
        self.page_scroll.to_top(); // each navigation opens at the top
        self.commands.push_back(NetCommand::FetchPage {
            identity: identity.clone(),
            path: path.clone(),
            fields,
        });
        self.page = Some(Page {
            node: identity,
            path,
            status: PageStatus::Fetching,
            elements: Vec::new(),
            element_sel: 0,
            field_values: HashMap::new(),
        });
    }

    /// Record/refresh a discovered Nomad Network node (dedupe by identity hash;
    /// `last_seen` is the announce timestamp). Mirrors [`App::upsert_peer`].
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn upsert_nomad(
        &mut self,
        identity: String,
        dest: String,
        name: Option<String>,
        last_seen: u64,
    ) {
        if let Some(node) = self.nomad_nodes.iter_mut().find(|n| n.identity == identity) {
            if name.is_some() {
                node.name = name;
            }
            node.dest = dest;
            node.last_seen = node.last_seen.max(last_seen);
        } else {
            self.nomad_nodes.push(NomadNode {
                identity,
                dest,
                name,
                last_seen,
            });
        }
    }

    /// Fold a page-fetch result into the Browser view, if it matches the page
    /// the operator is currently looking at. On success, extract its focusable
    /// elements and seed each text field's value from its default.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn set_page(&mut self, identity: String, path: String, body: Result<String, String>) {
        // Ignore stale results for a page the operator has navigated away from.
        if !matches!(&self.page, Some(p) if p.node == identity && p.path == path) {
            return;
        }
        let (status, elements, field_values) = match body {
            Ok(src) => {
                let elements = crate::micron::elements(&src);
                let mut field_values = HashMap::new();
                for el in &elements {
                    if let crate::micron::Element::Field { name, default, .. } = el {
                        field_values
                            .entry(name.clone())
                            .or_insert_with(|| default.clone());
                    }
                }
                (PageStatus::Loaded(src), elements, field_values)
            }
            Err(e) => (PageStatus::Error(e), Vec::new(), HashMap::new()),
        };
        self.page = Some(Page {
            node: identity,
            path,
            status,
            elements,
            element_sel: 0,
            field_values,
        });
    }

    /// Cycle focus between Conversations panes (bound to Tab).
    pub fn toggle_focus(&mut self) {
        self.focus = self.focus.next();
    }

    /// The selected conversation, if any (the list is empty only in degenerate
    /// states — seeding gives at least one).
    pub fn selected_conv(&self) -> Option<&Conversation> {
        self.conversations.get(self.selected)
    }

    /// Mutable access to the selected conversation.
    pub fn selected_conv_mut(&mut self) -> Option<&mut Conversation> {
        self.conversations.get_mut(self.selected)
    }

    /// Move the selection up one peer (clamped at the top). Marks the newly
    /// selected conversation as read.
    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.mark_selected_read();
        }
    }

    /// Move the selection down one peer (clamped at the bottom). Marks the
    /// newly selected conversation as read.
    pub fn select_next(&mut self) {
        if self.selected + 1 < self.conversations.len() {
            self.selected += 1;
            self.mark_selected_read();
        }
    }

    /// Clear the unread counter on the selected conversation (it is now on
    /// screen).
    fn mark_selected_read(&mut self) {
        // Switching conversations re-anchors the thread to its newest message.
        self.thread_scroll.to_bottom();
        if let Some(conv) = self.selected_conv_mut() {
            conv.unread = 0;
            let peer = conv.peer.clone();
            self.mark_dirty(&peer);
        }
    }

    /// Flag a conversation as needing a re-save (deduplicated). `main` drains
    /// `dirty` and persists; `App` itself never touches the disk.
    fn mark_dirty(&mut self, peer: &str) {
        if !self.dirty.iter().any(|p| p == peer) {
            self.dirty.push(peer.to_string());
        }
    }

    /// Merge a conversation loaded from the encrypted store into live state.
    /// History is prepended ahead of anything already received, and a missing
    /// display name is filled. Loaded conversations are not re-marked dirty.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn load_conversation(&mut self, mut loaded: Conversation) {
        if let Some(existing) = self
            .conversations
            .iter_mut()
            .find(|c| c.peer == loaded.peer)
        {
            loaded.messages.append(&mut existing.messages);
            existing.messages = loaded.messages;
            if existing.display_name.is_none() {
                existing.display_name = loaded.display_name;
            }
        } else {
            self.conversations.push(loaded);
        }
    }

    /// Accept the selected conversation's draft for transmission: enqueue it
    /// (addressed to that peer), echo it into the thread, and clear the draft.
    /// No-op on an empty/whitespace draft so a stray Ctrl+S doesn't emit a
    /// blank frame.
    pub fn transmit(&mut self) {
        let body = match self.conversations.get(self.selected) {
            Some(conv) => conv.draft.trim().to_string(),
            None => return,
        };
        if body.is_empty() {
            return;
        }
        let id = self.next_id();
        let conv = &mut self.conversations[self.selected];
        let mut entry = Entry::now(format!("[TX] {body}"));
        entry.id = id;
        entry.status = MsgStatus::Sending;
        conv.messages.push(entry);
        conv.draft.clear();
        let peer = conv.peer.clone();
        // `conv`'s borrow ends above; safe to touch `self.outbound`/`dirty` now.
        self.outbound.push_back(Outbound {
            id,
            peer: peer.clone(),
            body,
        });
        self.mark_dirty(&peer);
    }

    /// Next correlation id for an outbound message.
    fn next_id(&mut self) -> u64 {
        let id = self.next_msg_id;
        self.next_msg_id += 1;
        id
    }

    /// Update the delivery status of the outbound message with this id (wherever
    /// it lives), and mark its conversation dirty so the status is persisted.
    pub fn set_msg_status(&mut self, id: u64, status: MsgStatus) {
        let mut hit = None;
        for conv in &mut self.conversations {
            if let Some(entry) = conv.messages.iter_mut().find(|e| e.id == id) {
                entry.status = status;
                hit = Some(conv.peer.clone());
                break;
            }
        }
        if let Some(peer) = hit {
            self.mark_dirty(&peer);
        }
    }

    /// Discard the selected conversation's draft without transmitting (Ctrl+X).
    pub fn purge(&mut self) {
        if let Some(conv) = self.selected_conv_mut() {
            conv.draft.clear();
        }
    }

    /// Deliver an inbound message from `peer` into its conversation, creating
    /// the conversation if this is the first contact. Bumps the unread counter
    /// unless the conversation is currently selected. This is the hook the
    /// `net` feature calls with the real LXMF source identity.
    pub fn deliver(&mut self, peer: &str, body: &str) {
        let idx = match self.conversations.iter().position(|c| c.peer == peer) {
            Some(i) => i,
            None => {
                self.conversations.push(Conversation::new(peer));
                self.conversations.len() - 1
            }
        };
        self.conversations[idx]
            .messages
            .push(Entry::now(format!("[RX] {body}")));
        if idx != self.selected {
            self.conversations[idx].unread += 1;
        }
        self.mark_dirty(peer);
    }

    /// Append an inbound or system line. `[SYS]`-tagged lines belong to the Log
    /// tool; everything else is conversation traffic routed to a peer. This
    /// keeps `main`'s single inbound channel routing to the right view without
    /// the network task needing to know about tools or peers yet.
    pub fn push_log(&mut self, line: String) {
        if line.starts_with("[SYS]") {
            self.syslog.push(Entry::now(line));
        } else {
            self.deliver("(direct)", &line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    /// Build a press event with no modifiers.
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Build a Ctrl+<char> press event.
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Type a string into the focused Transmit buffer.
    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
    }

    /// Just the text of each entry (timestamps are non-deterministic).
    fn texts(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.text.as_str()).collect()
    }

    /// A fresh app forced into the boot splash (tests otherwise start running).
    #[cfg(feature = "splash")]
    fn booting() -> App {
        let mut app = App::new();
        app.state = AppState::Splash;
        app.boot = Boot::new();
        app
    }

    #[cfg(feature = "splash")]
    #[test]
    fn any_key_dismisses_splash() {
        let mut app = booting();
        app.handle_key(press(KeyCode::Char('x')));
        assert_eq!(app.state, AppState::Running);
    }

    #[cfg(feature = "splash")]
    #[test]
    fn boot_reveals_in_order_then_hands_off() {
        let mut app = booting();
        // At tick 0 only the first line shows; the last is still pending.
        assert!(app.boot_done(BootStep::Boot));
        assert!(!app.boot_done(BootStep::Console));
        // The clock must reach the console within the hard cap (via the timed
        // path offline, or the cap under `net` where no real address arrives).
        for _ in 0..=MAX_TICKS {
            if app.state == AppState::Running {
                break;
            }
            app.tick_splash();
        }
        assert_eq!(app.state, AppState::Running);
        assert!(BootStep::ALL.iter().all(|&s| app.boot_done(s)));
    }

    #[cfg(feature = "splash")]
    #[test]
    fn local_address_marks_console_and_opens_handoff() {
        let mut app = booting();
        app.mark_boot(BootStep::Console); // what NetEvent::Local triggers
        assert!(app.boot_done(BootStep::Console));
        for _ in 0..=MAX_TICKS {
            if app.state == AppState::Running {
                break;
            }
            app.tick_splash();
        }
        assert_eq!(app.state, AppState::Running);
    }

    #[test]
    fn ctrl_n_p_cycle_tools() {
        let mut app = App::new();
        assert_eq!(app.active, Tool::Conversations);
        app.handle_key(ctrl('n'));
        assert_eq!(app.active, Tool::Network);
        app.handle_key(ctrl('p'));
        assert_eq!(app.active, Tool::Conversations);
        // Wrap backwards from the first tab to the last.
        app.handle_key(ctrl('p'));
        assert_eq!(app.active, Tool::Guide);
    }

    #[test]
    fn tab_cycles_peerlist_thread_transmit() {
        let mut app = App::new();
        assert_eq!(app.focus, Pane::Transmit);
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.focus, Pane::PeerList);
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Thread);
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Transmit);
    }

    #[test]
    fn up_down_changes_selection_only_in_peerlist() {
        let mut app = App::new();
        assert_eq!(app.selected, 0);

        // Transmit focused: Up/Down do not move the selection.
        app.handle_key(press(KeyCode::Down));
        assert_eq!(
            app.selected, 0,
            "selection only moves with PeerList focused"
        );

        // Focus the peer list, then navigate (clamped at the ends).
        app.focus = Pane::PeerList;
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.selected, 1);
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.selected, 2);
        app.handle_key(press(KeyCode::Down)); // clamp at bottom (3 demo peers)
        assert_eq!(app.selected, 2);
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn selecting_marks_conversation_read() {
        let mut app = App::new();
        app.deliver("bob", "ping"); // bob is index 1, not selected -> unread
        assert_eq!(app.conversations[1].unread, 1);

        app.focus = Pane::PeerList;
        app.handle_key(press(KeyCode::Down)); // select bob
        assert_eq!(app.selected, 1);
        assert_eq!(app.conversations[1].unread, 0, "viewing clears unread");
    }

    #[test]
    fn drafts_are_per_conversation() {
        let mut app = App::new(); // Transmit focused, alice (0) selected
        type_str(&mut app, "to-alice");
        assert_eq!(app.conversations[0].draft, "to-alice");

        // Switch to bob via the peer list; bob's draft is independent/empty.
        app.focus = Pane::PeerList;
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.conversations[1].draft, "");

        app.focus = Pane::Transmit;
        type_str(&mut app, "to-bob");
        assert_eq!(app.conversations[1].draft, "to-bob");
        assert_eq!(
            app.conversations[0].draft, "to-alice",
            "alice's draft preserved"
        );
    }

    #[test]
    fn typing_only_edits_when_transmit_focused() {
        let mut app = App::new();
        type_str(&mut app, "hi");
        assert_eq!(app.selected_conv().unwrap().draft, "hi");

        app.focus = Pane::Thread;
        app.handle_key(press(KeyCode::Char('x')));
        assert_eq!(
            app.selected_conv().unwrap().draft,
            "hi",
            "thread pane must not capture text"
        );
    }

    #[test]
    fn typing_is_ignored_outside_conversations() {
        let mut app = App::new();
        app.active = Tool::Network;
        type_str(&mut app, "h");
        app.handle_key(press(KeyCode::Tab));
        assert!(
            app.conversations[0].draft.is_empty(),
            "non-Conversations tools take no compose input"
        );
        assert_eq!(
            app.focus,
            Pane::Transmit,
            "Tab must not move focus off Conversations"
        );
    }

    #[test]
    fn transmit_targets_selected_peer() {
        let mut app = App::new();
        // Select bob (index 1) and give alice a stray draft to prove isolation.
        app.conversations[0].draft = "stray".to_string();
        app.selected = 1;
        app.conversations[1].draft = "  hello bob  ".to_string();

        app.handle_key(ctrl('s'));

        assert_eq!(app.outbound.len(), 1);
        let out = app.outbound.front().unwrap();
        assert_eq!(out.peer, "bob");
        assert_eq!(out.body, "hello bob");
        assert_eq!(
            app.conversations[1].messages.last().unwrap().text,
            "[TX] hello bob"
        );
        assert!(app.conversations[1].draft.is_empty(), "sent draft cleared");
        assert_eq!(
            app.conversations[0].draft, "stray",
            "other drafts untouched"
        );
    }

    #[test]
    fn transmit_ignores_blank_draft() {
        let mut app = App::new();
        app.conversations[0].draft = "   ".to_string();
        app.handle_key(ctrl('s'));
        assert!(app.outbound.is_empty());
    }

    #[test]
    fn purge_clears_selected_draft_only() {
        let mut app = App::new();
        app.conversations[0].draft = "secret".to_string();
        app.conversations[1].draft = "keep".to_string();
        app.handle_key(ctrl('x'));
        assert!(app.conversations[0].draft.is_empty());
        assert_eq!(app.conversations[1].draft, "keep");
        assert!(app.outbound.is_empty());
    }

    #[test]
    fn deliver_routes_to_peer_and_increments_unread() {
        let mut app = App::new();
        // Unknown peer -> conversation is created.
        app.deliver("dave", "first contact");
        let dave = app.conversations.iter().find(|c| c.peer == "dave").unwrap();
        assert_eq!(texts(&dave.messages), vec!["[RX] first contact"]);
        assert_eq!(dave.unread, 1, "unread bumps when not selected");

        // A message to the currently selected peer does not bump unread.
        app.deliver("alice", "yo");
        assert_eq!(app.conversations[0].peer, "alice");
        assert_eq!(app.conversations[0].unread, 0, "selected peer stays read");
    }

    #[test]
    fn push_log_routes_sys_to_syslog() {
        let mut app = App::new();
        app.push_log("[SYS] online".to_string());
        app.push_log("hello there".to_string());
        assert_eq!(texts(&app.syslog), vec!["[SYS] online"]);
        // Non-SYS line lands in the "(direct)" conversation as inbound.
        let direct = app
            .conversations
            .iter()
            .find(|c| c.peer == "(direct)")
            .unwrap();
        assert_eq!(texts(&direct.messages), vec!["[RX] hello there"]);
    }

    #[test]
    fn ctrl_q_requests_quit() {
        let mut app = App::new();
        assert!(!app.should_quit);
        app.handle_key(ctrl('q'));
        assert!(app.should_quit);
    }

    /// Type a string into the open burn modal.
    fn type_burn(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
    }

    #[test]
    fn ctrl_k_opens_burn_and_token_confirms() {
        let mut app = App::new();
        app.handle_key(ctrl('k'));
        assert!(app.burn_confirm.is_some(), "burn modal opened");
        assert!(!app.burn && !app.should_quit);

        type_burn(&mut app, BURN_TOKEN);
        app.handle_key(press(KeyCode::Enter));
        assert!(app.burn, "burn confirmed");
        assert!(app.should_quit, "and quitting");
        assert!(app.burn_confirm.is_none(), "modal closed");
    }

    #[test]
    fn wrong_burn_token_does_not_burn() {
        let mut app = App::new();
        app.handle_key(ctrl('k'));
        type_burn(&mut app, "burn"); // lowercase — not the token
        app.handle_key(press(KeyCode::Enter));
        assert!(!app.burn, "no burn for the wrong token");
        assert!(!app.should_quit);
        assert!(app.burn_confirm.as_ref().unwrap().error, "error flagged");
        // Editing clears the error; the modal stays open until Esc or the token.
        app.handle_key(press(KeyCode::Backspace));
        assert!(!app.burn_confirm.as_ref().unwrap().error);
    }

    #[test]
    fn esc_cancels_burn() {
        let mut app = App::new();
        app.handle_key(ctrl('k'));
        type_burn(&mut app, BURN_TOKEN);
        app.handle_key(press(KeyCode::Esc));
        assert!(app.burn_confirm.is_none(), "cancelled");
        assert!(!app.burn && !app.should_quit, "nothing burned");
    }

    /// A propagation node with a given hash/name (no last-seen).
    fn node(hash: &str, name: Option<&str>) -> Node {
        Node {
            hash: hash.to_string(),
            name: name.map(str::to_string),
            last_seen: 0,
        }
    }

    #[test]
    fn network_tab_selects_and_sets_propagation_node() {
        let mut app = App::new();
        app.active = Tool::Network;
        app.net_col = NetColumn::Nodes; // focus the right column
        let n0 = "aa".repeat(16); // 32 hex chars = 16 bytes
        let n1 = "bb".repeat(16);
        app.nodes = vec![node(&n0, Some("n0")), node(&n1, None)];

        // Down moves the selection and clamps at the last row.
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.node_selected, 1);
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.node_selected, 1, "clamped at bottom");
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.node_selected, 0);

        // Enter activates the highlighted node (config + queued command).
        app.handle_key(press(KeyCode::Enter));
        assert_eq!(app.config.propagation_node.as_deref(), Some(n0.as_str()));
        assert_eq!(
            app.commands.pop_front(),
            Some(NetCommand::SetPropagationNode(Some(n0)))
        );

        // `s` queues a sync.
        app.handle_key(press(KeyCode::Char('s')));
        assert_eq!(app.commands.pop_front(), Some(NetCommand::SyncNow));
    }

    #[test]
    fn network_node_column_inert_with_no_nodes() {
        let mut app = App::new();
        app.active = Tool::Network;
        app.net_col = NetColumn::Nodes;
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        assert_eq!(app.node_selected, 0);
        assert!(app.config.propagation_node.is_none());
        assert!(
            app.commands.is_empty(),
            "Enter on an empty node list does nothing"
        );
    }

    #[test]
    fn network_columns_toggle_and_navigate_independently() {
        let mut app = App::new();
        app.active = Tool::Network;
        app.nodes = vec![node(&"aa".repeat(16), None), node(&"bb".repeat(16), None)];
        // Defaults to the Peers column.
        assert_eq!(app.net_col, NetColumn::Peers);

        // Up/Down move the peer cursor (seeded with 3 conversations).
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.selected, 1);
        assert_eq!(app.node_selected, 0, "node cursor untouched while on Peers");

        // Tab / Left / Right switch the focused column.
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.net_col, NetColumn::Nodes);
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.node_selected, 1);
        assert_eq!(app.selected, 1, "peer cursor untouched while on Nodes");
        app.handle_key(press(KeyCode::Left));
        assert_eq!(app.net_col, NetColumn::Peers);
        app.handle_key(press(KeyCode::Right));
        assert_eq!(app.net_col, NetColumn::Nodes);
    }

    #[test]
    fn enter_on_peer_opens_its_conversation() {
        let mut app = App::new();
        app.active = Tool::Network;
        app.net_col = NetColumn::Peers;
        app.selected = 1; // "bob"
        app.handle_key(press(KeyCode::Enter));
        assert_eq!(app.active, Tool::Conversations);
        assert_eq!(app.focus, Pane::Transmit);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn p_queues_path_probe_for_focused_selection() {
        let mut app = App::new();
        app.active = Tool::Network;
        let n0 = "cc".repeat(16);
        app.nodes = vec![node(&n0, None)];

        // On the Peers column: probes the selected peer's key.
        app.net_col = NetColumn::Peers;
        app.selected = 0;
        let peer = app.conversations[0].peer.clone();
        app.handle_key(press(KeyCode::Char('p')));
        assert_eq!(
            app.commands.pop_front(),
            Some(NetCommand::RequestPath(peer))
        );

        // On the Nodes column: probes the selected node's hash.
        app.net_col = NetColumn::Nodes;
        app.handle_key(press(KeyCode::Char('p')));
        assert_eq!(app.commands.pop_front(), Some(NetCommand::RequestPath(n0)));
    }

    #[test]
    fn upsert_peer_stamps_last_seen() {
        let mut app = App::new();
        let peer = "dd".repeat(16);
        app.upsert_peer(PeerKind::Delivery, peer.clone(), None);
        let conv = app.conversations.iter().find(|c| c.peer == peer).unwrap();
        assert!(conv.last_seen > 0, "delivery peer stamped");

        let nodehash = "ee".repeat(16);
        app.upsert_peer(PeerKind::Propagation, nodehash.clone(), None);
        let n = app.nodes.iter().find(|n| n.hash == nodehash).unwrap();
        assert!(n.last_seen > 0, "propagation node stamped");
    }

    #[test]
    fn record_path_stores_probe_and_logs() {
        let mut app = App::new();
        let hash = "ff".repeat(16);
        app.record_path(hash.clone(), Some(3), Some("AutoInterface".to_string()));
        let p = app.path_probes.get(&hash).expect("probe stored");
        assert_eq!(p.hops, Some(3));
        assert!(
            app.syslog.iter().any(
                |e| e.text.contains("[RT] PATH") && e.text.contains("3 hops via AutoInterface")
            ),
            "an [RT] log line was emitted"
        );
    }

    #[test]
    fn upsert_nomad_dedupes_and_keeps_newest_last_seen() {
        let mut app = App::new();
        let id = "11".repeat(16);
        let dest = "aa".repeat(16);
        app.upsert_nomad(id.clone(), dest.clone(), Some("hub".to_string()), 100);
        app.upsert_nomad(id.clone(), dest.clone(), None, 250); // newer, no name update
        assert_eq!(app.nomad_nodes.len(), 1);
        let n = &app.nomad_nodes[0];
        assert_eq!(n.name.as_deref(), Some("hub"));
        assert_eq!(n.dest, dest);
        assert_eq!(n.last_seen, 250);
    }

    #[test]
    fn browser_enter_queues_index_fetch() {
        let mut app = App::new();
        app.active = Tool::Browser;
        let id = "22".repeat(16);
        app.upsert_nomad(id.clone(), "bb".repeat(16), Some("node".to_string()), 1);
        app.browser_selected = 0;
        app.handle_key(press(KeyCode::Enter));
        assert_eq!(
            app.commands.pop_front(),
            Some(NetCommand::FetchPage {
                identity: id.clone(),
                path: "/page/index.mu".to_string(),
                fields: Vec::new(),
            })
        );
        let page = app.page.as_ref().expect("page set to fetching");
        assert_eq!(page.node, id);
        assert!(matches!(page.status, PageStatus::Fetching));
        // Opening a node focuses the page pane.
        assert_eq!(app.browser_pane, BrowserPane::Page);
    }

    #[test]
    fn set_page_folds_ok_and_err_for_current_page() {
        let mut app = App::new();
        let id = "33".repeat(16);
        let path = "/page/index.mu".to_string();
        let viewing = || Page {
            node: "33".repeat(16),
            path: "/page/index.mu".to_string(),
            status: PageStatus::Fetching,
            elements: Vec::new(),
            element_sel: 0,
            field_values: HashMap::new(),
        };

        app.page = Some(viewing());
        app.set_page(id.clone(), path.clone(), Ok(">Hello".to_string()));
        assert!(matches!(
            app.page.as_ref().unwrap().status,
            PageStatus::Loaded(_)
        ));

        app.page = Some(viewing());
        app.set_page(id, path, Err("timeout".to_string()));
        assert!(matches!(
            app.page.as_ref().unwrap().status,
            PageStatus::Error(_)
        ));

        // A result for a page we're no longer viewing is ignored.
        app.page = Some(viewing());
        app.set_page(
            "99".repeat(16),
            "/other.mu".to_string(),
            Ok("x".to_string()),
        );
        assert!(matches!(
            app.page.as_ref().unwrap().status,
            PageStatus::Fetching
        ));
    }

    #[test]
    fn set_page_extracts_elements_and_seeds_fields() {
        let mut app = App::new();
        let id = "44".repeat(16);
        let path = "/page/index.mu".to_string();
        app.page = Some(Page {
            node: id.clone(),
            path: path.clone(),
            status: PageStatus::Fetching,
            elements: Vec::new(),
            element_sel: 0,
            field_values: HashMap::new(),
        });
        app.set_page(
            id,
            path,
            Ok("`[Home`:/page/a.mu] `<user`alice>".to_string()),
        );
        let p = app.page.as_ref().unwrap();
        // A link element followed by a text field element.
        assert!(matches!(
            p.elements.as_slice(),
            [
                crate::micron::Element::Link { .. },
                crate::micron::Element::Field { .. }
            ]
        ));
        // The field value is seeded from its default.
        assert_eq!(
            p.field_values.get("user").map(String::as_str),
            Some("alice")
        );
    }

    /// A Browser viewing a loaded page on `node`, with the given link targets as
    /// its (link) elements.
    fn browsing(node: &str, path: &str, links: Vec<String>) -> App {
        let elements = links
            .into_iter()
            .map(|target| crate::micron::Element::Link {
                target,
                fields: Vec::new(),
            })
            .collect();
        let mut app = App::new();
        app.active = Tool::Browser;
        app.browser_pane = BrowserPane::Page;
        app.page = Some(Page {
            node: node.to_string(),
            path: path.to_string(),
            status: PageStatus::Loaded(String::new()),
            elements,
            element_sel: 0,
            field_values: HashMap::new(),
        });
        app
    }

    #[test]
    fn tab_toggles_browser_pane() {
        let mut app = App::new();
        app.active = Tool::Browser;
        assert_eq!(app.browser_pane, BrowserPane::Nodes);
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.browser_pane, BrowserPane::Page);
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.browser_pane, BrowserPane::Nodes);
    }

    #[test]
    fn page_pane_element_cursor_clamps() {
        let node = "55".repeat(16);
        let mut app = browsing(
            &node,
            "/page/index.mu",
            vec![":/a.mu".to_string(), ":/b.mu".to_string()],
        );
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.page.as_ref().unwrap().element_sel, 1);
        app.handle_key(press(KeyCode::Down)); // clamp at last
        assert_eq!(app.page.as_ref().unwrap().element_sel, 1);
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.page.as_ref().unwrap().element_sel, 0);
    }

    #[test]
    fn relative_link_follows_on_current_node() {
        let node = "66".repeat(16);
        let mut app = browsing(&node, "/page/index.mu", vec![":/page/about.mu".to_string()]);
        app.handle_key(press(KeyCode::Enter)); // follow link 0
        assert_eq!(
            app.commands.pop_front(),
            Some(NetCommand::FetchPage {
                identity: node.clone(),
                path: "/page/about.mu".to_string(),
                fields: Vec::new(),
            })
        );
        // The previous page was pushed to history.
        assert_eq!(app.history.last().unwrap().1, "/page/index.mu");
    }

    #[test]
    fn absolute_link_resolves_known_dest_to_identity() {
        let here = "77".repeat(16);
        let other_id = "88".repeat(16);
        let other_dest = "99".repeat(16);
        let url = format!("{other_dest}:/page/index.mu");
        let mut app = browsing(&here, "/page/index.mu", vec![url]);
        app.upsert_nomad(other_id.clone(), other_dest.clone(), None, 1);
        app.handle_key(press(KeyCode::Enter));
        assert_eq!(
            app.commands.pop_front(),
            Some(NetCommand::FetchPage {
                identity: other_id,
                path: "/page/index.mu".to_string(),
                fields: Vec::new(),
            })
        );
    }

    #[test]
    fn link_to_unknown_dest_errors_without_fetching() {
        let here = "aa".repeat(16);
        let url = format!("{}:/page/x.mu", "bc".repeat(16)); // never discovered
        let mut app = browsing(&here, "/page/index.mu", vec![url]);
        app.handle_key(press(KeyCode::Enter));
        assert!(app.commands.is_empty(), "no fetch for an unknown node");
        assert!(matches!(
            app.page.as_ref().unwrap().status,
            PageStatus::Error(_)
        ));
    }

    #[test]
    fn backspace_pops_history_and_refetches() {
        let node = "cc".repeat(16);
        let mut app = browsing(&node, "/page/two.mu", vec![]);
        app.history.push((node.clone(), "/page/one.mu".to_string()));
        app.handle_key(press(KeyCode::Backspace));
        assert_eq!(
            app.commands.pop_front(),
            Some(NetCommand::FetchPage {
                identity: node,
                path: "/page/one.mu".to_string(),
                fields: Vec::new(),
            })
        );
        assert!(app.history.is_empty(), "history was popped (not re-pushed)");
    }

    /// A Browser page with the given pre-built elements, focused on the page pane.
    fn browsing_elements(node: &str, elements: Vec<crate::micron::Element>) -> App {
        let mut app = App::new();
        app.active = Tool::Browser;
        app.browser_pane = BrowserPane::Page;
        app.page = Some(Page {
            node: node.to_string(),
            path: "/page/index.mu".to_string(),
            status: PageStatus::Loaded(String::new()),
            elements,
            element_sel: 0,
            field_values: HashMap::new(),
        });
        app
    }

    #[test]
    fn typing_into_focused_field_edits_its_value() {
        let mut app = browsing_elements(
            &"dd".repeat(16),
            vec![crate::micron::Element::Field {
                name: "q".to_string(),
                default: String::new(),
            }],
        );
        app.handle_key(press(KeyCode::Char('h')));
        app.handle_key(press(KeyCode::Char('i')));
        assert_eq!(
            app.page
                .as_ref()
                .unwrap()
                .field_values
                .get("q")
                .map(String::as_str),
            Some("hi")
        );
        app.handle_key(press(KeyCode::Backspace));
        assert_eq!(
            app.page
                .as_ref()
                .unwrap()
                .field_values
                .get("q")
                .map(String::as_str),
            Some("h")
        );
    }

    #[test]
    fn submit_link_collects_field_and_var_values() {
        let node = "ee".repeat(16);
        let mut app = browsing_elements(
            &node,
            vec![
                crate::micron::Element::Field {
                    name: "q".to_string(),
                    default: String::new(),
                },
                crate::micron::Element::Link {
                    target: ":/page/search.mu".to_string(),
                    fields: vec!["v=1".to_string(), "*".to_string()],
                },
            ],
        );
        app.handle_key(press(KeyCode::Char('x'))); // type into the field
        app.handle_key(press(KeyCode::Down)); // move to the submit link
        app.handle_key(press(KeyCode::Enter));
        let cmd = app.commands.pop_front().expect("a fetch was queued");
        match cmd {
            NetCommand::FetchPage {
                identity,
                path,
                fields,
            } => {
                assert_eq!(identity, node);
                assert_eq!(path, "/page/search.mu");
                assert_eq!(
                    fields,
                    vec![
                        ("var_v".to_string(), "1".to_string()),
                        ("field_q".to_string(), "x".to_string()),
                    ]
                );
            }
            other => panic!("expected FetchPage, got {other:?}"),
        }
    }

    #[test]
    fn scroll_top_paging_clamps_to_content() {
        let s = Scroll::top();
        assert_eq!(s.visible(30, 10), 0, "opens at the top");
        s.page_down(); // step == cached viewport (10)
        assert_eq!(s.visible(30, 10), 10);
        s.page_down();
        s.page_down(); // past the end → clamps at max (30-10)
        assert_eq!(s.visible(30, 10), 20);
        s.to_top();
        assert_eq!(s.visible(30, 10), 0);
    }

    #[test]
    fn scroll_bottom_follows_then_releases_then_resticks() {
        let s = Scroll::bottom();
        assert_eq!(s.visible(30, 10), 20, "starts at the bottom");
        assert_eq!(s.visible(50, 10), 40, "follows new content while stuck");
        s.page_up(); // releases follow, up one viewport from 40
        assert_eq!(s.visible(50, 10), 30);
        assert_eq!(s.visible(60, 10), 30, "no longer yanked to the bottom");
        s.to_bottom();
        assert_eq!(s.visible(60, 10), 50, "End re-engages follow");
    }

    #[test]
    fn pagekeys_scroll_focused_pane() {
        let mut app = App::new();
        app.active = Tool::Browser;
        app.browser_pane = BrowserPane::Page;
        app.page_scroll.visible(100, 10); // prime the viewport step
        app.handle_key(press(KeyCode::PageDown));
        assert_eq!(app.page_scroll.visible(100, 10), 10);
        app.handle_key(press(KeyCode::End));
        assert_eq!(app.page_scroll.visible(100, 10), 90);
        app.handle_key(press(KeyCode::Home));
        assert_eq!(app.page_scroll.visible(100, 10), 0);
    }

    #[test]
    fn active_scroll_follows_focus() {
        let mut app = App::new();
        app.active = Tool::Log;
        assert!(app.active_scroll().is_some());
        app.active = Tool::Network;
        assert!(
            app.active_scroll().is_none(),
            "node columns aren't a text pane"
        );
        app.active = Tool::Browser;
        app.browser_pane = BrowserPane::Nodes;
        assert!(app.active_scroll().is_none(), "node list isn't scrollable");
        app.browser_pane = BrowserPane::Page;
        assert!(app.active_scroll().is_some());
    }

    #[test]
    fn fetch_page_resets_scroll_to_top() {
        let node = "dd".repeat(16);
        let mut app = browsing(&node, "/page/index.mu", vec![]);
        app.page_scroll.visible(100, 10);
        app.page_scroll.page_down(); // scrolled down
        app.browser_pane = BrowserPane::Nodes;
        app.upsert_nomad(node.clone(), "ee".repeat(16), None, 1);
        app.browser_selected = 0;
        app.handle_key(press(KeyCode::Enter)); // open index → resets to top
        assert_eq!(app.page_scroll.visible(100, 10), 0);
    }

    #[test]
    fn start_conversation_validates_and_normalizes() {
        let mut app = App::new();
        let before = app.conversations.len();
        // Colons / spaces / case are tolerated → 32 hex chars.
        assert!(
            app.start_conversation("A1:b2:c3:d4 e5 f6:00:11:22:33:44:55:66:77:88:99", "Bravo-6")
        );
        assert_eq!(app.conversations.len(), before + 1);
        let conv = app.conversations.last().unwrap();
        assert_eq!(conv.peer, "a1b2c3d4e5f600112233445566778899");
        assert_eq!(conv.display_name.as_deref(), Some("Bravo-6"));
        assert!(conv.pinned);
        assert_eq!(app.selected, app.conversations.len() - 1);
        assert_eq!(app.active, Tool::Conversations);
        assert_eq!(app.focus, Pane::Transmit);
        assert!(app.dirty.iter().any(|p| p == &conv.peer));
    }

    #[test]
    fn start_conversation_rejects_bad_address() {
        let mut app = App::new();
        let before = app.conversations.len();
        assert!(!app.start_conversation("", ""));
        assert!(!app.start_conversation("abcd", ""), "too short");
        assert!(!app.start_conversation(&"z".repeat(32), ""), "not hex");
        assert_eq!(app.conversations.len(), before);
    }

    #[test]
    fn start_conversation_reuses_existing_and_updates_alias() {
        let mut app = App::new();
        let addr = "ff".repeat(16);
        assert!(app.start_conversation(&addr, ""));
        let n = app.conversations.len();
        assert!(app.start_conversation(&addr, "Renamed"));
        assert_eq!(app.conversations.len(), n, "no duplicate thread");
        let conv = app.conversations.iter().find(|c| c.peer == addr).unwrap();
        assert_eq!(conv.display_name.as_deref(), Some("Renamed"));
    }

    #[test]
    fn new_conv_modal_open_type_confirm() {
        let mut app = App::new();
        app.handle_key(ctrl('o'));
        assert!(app.new_conv.is_some(), "Ctrl+O opens the popup");

        // Modal captures input: address, Tab to alias, then a name.
        type_str(&mut app, &"aa".repeat(16));
        app.handle_key(press(KeyCode::Tab));
        type_str(&mut app, "Alpha");
        app.handle_key(press(KeyCode::Enter));

        assert!(app.new_conv.is_none(), "Enter closes on success");
        let conv = app
            .conversations
            .iter()
            .find(|c| c.peer == "aa".repeat(16))
            .unwrap();
        assert_eq!(conv.display_name.as_deref(), Some("Alpha"));
    }

    #[test]
    fn new_conv_esc_cancels_and_invalid_shows_error() {
        let mut app = App::new();
        app.handle_key(ctrl('o'));
        app.handle_key(press(KeyCode::Char('a')));
        app.handle_key(press(KeyCode::Esc));
        assert!(app.new_conv.is_none(), "Esc cancels");

        app.handle_key(ctrl('o'));
        app.handle_key(press(KeyCode::Char('z'))); // non-hex → normalizes to empty
        app.handle_key(press(KeyCode::Enter));
        assert!(
            app.new_conv.as_ref().is_some_and(|nc| nc.error),
            "stays open with error"
        );
    }

    #[test]
    fn transmit_stamps_id_and_sending_status() {
        let mut app = App::new();
        app.conversations[0].draft = "hi".to_string();
        app.handle_key(ctrl('s'));
        let entry = app.conversations[0].messages.last().unwrap();
        assert!(entry.id > 0);
        assert_eq!(entry.status, MsgStatus::Sending);
        assert_eq!(
            app.outbound.back().unwrap().id,
            entry.id,
            "Outbound shares the id"
        );
    }

    #[test]
    fn set_msg_status_updates_matching_entry_and_marks_dirty() {
        let mut app = App::new();
        app.conversations[0].draft = "yo".to_string();
        app.handle_key(ctrl('s'));
        let id = app.conversations[0].messages.last().unwrap().id;
        app.dirty.clear();

        app.set_msg_status(id, MsgStatus::Delivered);
        assert_eq!(
            app.conversations[0].messages.last().unwrap().status,
            MsgStatus::Delivered
        );
        let peer = app.conversations[0].peer.clone();
        assert!(app.dirty.iter().any(|p| p == &peer));

        app.set_msg_status(999_999, MsgStatus::Failed); // unknown id: no-op
    }
}
