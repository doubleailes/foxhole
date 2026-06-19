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
//!
//! The struct lives here together with program-global key routing and the modal
//! handlers; the per-tool behaviour is split into sibling modules
//! ([`conversations`], [`network`], [`browser`]) as further `impl App` blocks,
//! and the cold-boot/scroll machinery into [`boot`].

mod boot;
mod browser;
mod conversations;
mod intel;
mod map;
mod network;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::Config;
pub use crate::domain::{
    Conversation, Entry, GeoPos, Interface, MsgStatus, NetCommand, NetEvent, Node, NomadNode,
    Outbound, Page, PageStatus, PathProbe, PeerKind, Trust, Zone, fmt_bitrate, fmt_bytes,
    path_summary,
};
pub use crate::notes::Notes;

pub use boot::{AppState, Scroll};
#[cfg(feature = "splash")]
pub use boot::{Boot, BootStep};
pub use intel::{IntelRecord, IntelReview, IntelZone, ShareZone};
pub use map::{MapMarker, MapView, MarkerKind};

// Re-exported so the renderer (and the binary) reach the CoT model through
// `crate::app::…` without each crate depending on `foxhole-cot` directly.
pub use foxhole_cot::{Affiliation, CotEvent, Kind as CotKind};

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

/// Cap on retained system-log entries ([`App::syslog`]). The Log tool only ever
/// shows the tail (it is bottom-pinned), so once the buffer grows past this the
/// oldest lines are dropped — keeping memory bounded against a chatty source such
/// as frequent location telemetry.
pub(crate) const SYSLOG_MAX: usize = 4000;

/// Modal state for the burn confirmation (Ctrl+K). Destroying all session data
/// is gated behind typing [`BURN_TOKEN`] so it can't fire by accident.
pub struct BurnConfirm {
    /// The confirmation token as typed so far.
    pub input: String,
    /// Set when the last Enter had the wrong token; cleared on edit.
    pub error: bool,
}

/// Read-only modal showing a peer's address as a 12-word mnemonic phrase (the
/// `m` key in the Network tab). Dismissed by any key — it only needs to be read
/// aloud, so it captures no input.
pub struct MnemonicView {
    /// The hex destination hash this phrase encodes (shown for reference).
    pub hash: String,
    /// The 12-word mnemonic phrase.
    pub phrase: String,
}

/// A top-level tool, rendered as a tab in the menu strip. Each tool owns its
/// own body layout and key handling (see `ui` and [`App::handle_tool_key`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// LXMF messaging: per-peer conversations plus the compose buffer.
    Conversations,
    /// Discovered peers and propagation nodes.
    Network,
    /// World map: the operator and located peers plotted on a globe.
    WorldMap,
    /// Nomad Network page browser (micron pages served by `nomadnetwork.node`).
    Browser,
    /// System/application log (banners, diagnostics).
    Log,
    /// Reticulum interface status.
    Interfaces,
    /// Ten-slot scratch note buffer.
    Notes,
    /// Static help text.
    Guide,
}

impl Tool {
    /// Tab order, left to right. Drives both the menu strip and Ctrl+N/P
    /// cycling, so there is a single source of truth for ordering.
    pub const ALL: [Tool; 8] = [
        Tool::Conversations,
        Tool::Network,
        Tool::WorldMap,
        Tool::Browser,
        Tool::Log,
        Tool::Interfaces,
        Tool::Notes,
        Tool::Guide,
    ];

    /// Label shown in the tab strip.
    pub fn title(self) -> &'static str {
        match self {
            Tool::Conversations => "Conversations",
            Tool::Network => "Network",
            Tool::WorldMap => "Map",
            Tool::Browser => "Browser",
            Tool::Log => "Log",
            Tool::Interfaces => "Interfaces",
            Tool::Notes => "Notes",
            Tool::Guide => "Guide",
        }
    }

    /// Short tag for the status bar's `TOOL:` field.
    pub fn tag(self) -> &'static str {
        match self {
            Tool::Conversations => "CONV",
            Tool::Network => "NET",
            Tool::WorldMap => "MAP",
            Tool::Browser => "WEB",
            Tool::Log => "LOG",
            Tool::Interfaces => "IFACE",
            Tool::Notes => "NOTE",
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

/// Which field the Transmit pane is editing. Mirrors Nomadnet's compose form,
/// where Ctrl+T toggles between an optional message title and the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransmitField {
    /// The optional LXMF message title (the `Title` field).
    Title,
    /// The message body.
    Body,
}

impl TransmitField {
    /// Toggle between the title and the body (Ctrl+T).
    fn toggle(self) -> Self {
        match self {
            TransmitField::Title => TransmitField::Body,
            TransmitField::Body => TransmitField::Title,
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

/// Whole-program UI state.
pub struct App {
    /// Active top-level tool (drives the tab strip + key delegation).
    pub active: Tool,
    /// Focused pane within the Conversations tool (drives the reversed
    /// highlight + key routing there). Ignored by the read-only tools.
    pub focus: Pane,
    /// Which Transmit-pane field keystrokes edit (title vs body), toggled with
    /// Ctrl+T. Only meaningful while the Transmit pane is focused.
    pub transmit_field: TransmitField,
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
    /// World Map viewport (pan/zoom state).
    pub map: MapView,
    /// Selected marker index within [`App::map_markers`] (World Map tab).
    pub map_selected: usize,
    /// Hazard areas overlaid on the World Map (war zones / areas of operations).
    /// Seeded with a demo set; overridden from `zones.conf` when present. This is
    /// *local* (operator-authored) intel — distinct from `intel` below.
    pub zones: Vec<Zone>,
    /// Live received CoT intel applied to the map (from Trusted peers, or all
    /// peers when `intel_auto_apply` is set, or operator-accepted). Keyed by
    /// `(source, uid)`; expired entries are swept. See [`intel`].
    pub intel: Vec<IntelRecord>,
    /// Received CoT intel from Unknown/Untrusted peers, staged for operator
    /// review (accept → `intel`, or discard). See the incoming-intel modal.
    pub intel_staged: Vec<IntelRecord>,
    /// When `Some`, the incoming-intel review modal is open (captures input).
    pub intel_review: Option<IntelReview>,
    /// When `Some`, the share-zone picker is open (captures input).
    pub share_zone: Option<ShareZone>,
    /// Latest rnpath-style path probe per hex destination hash (Network tab).
    pub path_probes: HashMap<String, PathProbe>,
    /// Live interface status (Interfaces tab); empty until the stack reports.
    pub interfaces: Vec<Interface>,
    /// Active link count reported alongside the interface snapshot.
    pub link_count: u32,
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
    /// When `Some`, the read-only mnemonic-phrase modal is open (any key closes).
    pub mnemonic_view: Option<MnemonicView>,
    /// Set once the operator confirms a burn; `main` shreds the config dir and
    /// exits. (The wipe itself is I/O — done outside `App`.)
    pub burn: bool,
    /// Persisted operator settings (display name, hub, active propagation node).
    pub config: Config,
    /// Ten-slot scratch note buffer (Notes tool).
    pub notes: Notes,
    /// Highlighted slot in the Notes tool.
    pub note_selected: usize,
    /// Set when a note slot changed; `main` drains it and persists the buffer.
    pub notes_dirty: bool,
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
    /// each timestamped (UTC). Bounded to the most recent [`SYSLOG_MAX`] entries
    /// (see [`App::push_log`]) so a chatty source — e.g. frequent location
    /// telemetry — can't grow it without bound.
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
            transmit_field: TransmitField::Body,
            conversations,
            selected: 0,
            nodes: Vec::new(),
            node_selected: 0,
            net_col: NetColumn::Peers,
            map: MapView::default(),
            map_selected: 0,
            zones: crate::zones::demo(),
            intel: Vec::new(),
            intel_staged: Vec::new(),
            intel_review: None,
            share_zone: None,
            path_probes: HashMap::new(),
            interfaces: Vec::new(),
            link_count: 0,
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
            mnemonic_view: None,
            burn: false,
            config: Config::default(),
            notes: Notes::default(),
            note_selected: 0,
            notes_dirty: false,
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

        // The read-only mnemonic modal is dismissed by any key.
        if self.mnemonic_view.is_some() {
            self.mnemonic_view = None;
            return;
        }

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

        // The incoming-intel review modal, when open, captures all input.
        if self.intel_review.is_some() {
            self.handle_intel_review_key(key);
            return;
        }

        // The share-zone picker, when open, captures all input.
        if self.share_zone.is_some() {
            self.handle_share_zone_key(key);
            return;
        }

        // A running propagation sync shows a (non-capturing) progress pop-up.
        // Esc abandons it so the operator can dismiss a slow/stuck sync at once
        // instead of waiting out the node's timeout; the network task stops
        // re-asserting the pop-up on the matching `CancelSync`.
        if self.sync_status.is_some() && key.code == KeyCode::Esc {
            self.sync_status = None;
            self.commands.push_back(NetCommand::CancelSync);
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

    /// Open the New Conversation popup (Ctrl+O), focused on the address field.
    fn open_new_conv(&mut self) {
        self.new_conv = Some(NewConv {
            address: String::new(),
            alias: String::new(),
            field: NewConvField::Address,
            error: false,
        });
    }

    /// Open the read-only mnemonic-phrase modal for a hex destination hash,
    /// encoding it to a 12-word phrase. No-op if the hash isn't 16 bytes.
    pub(super) fn open_mnemonic(&mut self, hash: &str) {
        let bytes = crate::domain::normalize_address(hash);
        let mut buf = [0u8; 16];
        if bytes.len() != 32 {
            return;
        }
        for (i, b) in buf.iter_mut().enumerate() {
            match u8::from_str_radix(&bytes[i * 2..i * 2 + 2], 16) {
                Ok(v) => *b = v,
                Err(_) => return,
            }
        }
        let phrase = crate::mnemonic::encode(&buf);
        self.syslog.push(Entry::now(format!(
            "[ID] MNEMONIC {}.. -> {phrase}",
            crate::domain::short_hash(&bytes)
        )));
        self.mnemonic_view = Some(MnemonicView {
            hash: bytes,
            phrase,
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

    /// Per-tool key handling. Conversations composes/sends; Network selects the
    /// active propagation node; the remaining tools are read-only.
    fn handle_tool_key(&mut self, ctrl: bool, key: KeyEvent) {
        match self.active {
            Tool::Conversations => self.handle_conversations_key(ctrl, key),
            Tool::Network => self.handle_network_key(ctrl, key),
            Tool::WorldMap => self.handle_map_key(ctrl, key),
            Tool::Browser => self.handle_browser_key(key),
            Tool::Notes => self.handle_notes_key(ctrl, key),
            _ => {}
        }
    }

    /// Notes tool: Up/Down pick a slot, typing edits the selected slot,
    /// Backspace deletes a char, Ctrl+X clears the slot. Any change flags the
    /// buffer dirty so `main` persists it.
    fn handle_notes_key(&mut self, ctrl: bool, key: KeyEvent) {
        match (ctrl, key.code) {
            (false, KeyCode::Up) => self.note_selected = self.note_selected.saturating_sub(1),
            (false, KeyCode::Down) => {
                if self.note_selected + 1 < crate::notes::SLOTS {
                    self.note_selected += 1;
                }
            }
            (true, KeyCode::Char('x')) => {
                self.notes.clear(self.note_selected);
                self.notes_dirty = true;
            }
            (false, KeyCode::Backspace) => {
                self.notes.pop_char(self.note_selected);
                self.notes_dirty = true;
            }
            (false, KeyCode::Char(c)) => {
                self.notes.push_char(self.note_selected, c);
                self.notes_dirty = true;
            }
            _ => {}
        }
    }
}
