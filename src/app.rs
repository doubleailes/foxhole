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

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::Config;

/// A command from the UI down to the network task (mirrors [`NetEvent`] in the
/// other direction). Produced by Network-tab key handling, drained by `main`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetCommand {
    /// Make this hex destination hash the active propagation node (or clear it).
    SetPropagationNode(Option<String>),
    /// Pull queued messages from the configured propagation node now.
    SyncNow,
}

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

/// A top-level tool, rendered as a tab in the menu strip. Each tool owns its
/// own body layout and key handling (see `ui` and [`App::handle_tool_key`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// LXMF messaging: per-peer conversations plus the compose buffer.
    Conversations,
    /// Discovered peers, propagation nodes, and Nomadnet page servers.
    Network,
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
    pub const ALL: [Tool; 5] = [
        Tool::Conversations,
        Tool::Network,
        Tool::Log,
        Tool::Interfaces,
        Tool::Guide,
    ];

    /// Label shown in the tab strip.
    pub fn title(self) -> &'static str {
        match self {
            Tool::Conversations => "Conversations",
            Tool::Network => "Network",
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

/// A message accepted for transmission, carrying its destination so the
/// protocol task knows which peer to address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outbound {
    /// Destination peer (display name offline; hex destination hash under `net`).
    pub peer: String,
    /// The message text.
    pub body: String,
}

/// Whether a discovered peer is a messaging endpoint or a propagation relay.
// Only constructed by the `net`-gated module; offline these variants are unused.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerKind {
    /// An `lxmf.delivery` destination — a peer we can hold a conversation with.
    Delivery,
    /// An `lxmf.propagation` node — a store-and-forward relay (Network tab only).
    Propagation,
}

/// Events flowing from the network task up to the UI over an mpsc channel.
/// Defined here (not in the `net`-gated module) because the offline stub and
/// the event loop both speak it regardless of the feature. The `Peer`/`Message`
/// variants are only produced under `net`.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub enum NetEvent {
    /// A status/diagnostic line destined for the Log tab.
    Sys(String),
    /// This node's own `lxmf.delivery` address (hex) — what peers send to.
    Local(String),
    /// A peer discovered via an announce. `hash` is the hex destination hash.
    Peer {
        kind: PeerKind,
        hash: String,
        name: Option<String>,
    },
    /// An inbound LXMF message. `source` is the hex destination hash.
    Message {
        source: String,
        title: String,
        content: String,
    },
    /// Propagation-sync progress: `Some(status)` shows the pop-up, `None` hides it.
    Sync(Option<String>),
    /// The derived 64-byte conversation-store key, handed up once at startup so
    /// `main` can load history and persist new messages.
    StoreKey([u8; 64]),
}

/// A discovered propagation node, shown in the Network tab.
pub struct Node {
    /// Hex destination hash (the registry key).
    pub hash: String,
    /// Announced display name, if any.
    pub name: Option<String>,
}

/// One scrollback line: when it occurred (Unix epoch **seconds, UTC**) and its
/// text. `at == 0` marks an unknown time (rendered `--:--:--`). The timestamp is
/// captured at creation so the UI can show it without re-reading the clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub at: u64,
    pub text: String,
}

impl Entry {
    /// An entry stamped with the current UTC time.
    pub fn now(text: String) -> Self {
        Self {
            at: now_secs(),
            text,
        }
    }
}

/// Current Unix time in whole seconds (UTC); 0 if the clock predates the epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Reduce a typed LXMF address to canonical form: lowercase hex digits only, so
/// `a1:b2 c3`, `A1B2C3`, and `<a1b2c3>` all normalize the same. The caller
/// validates the length (a destination hash is 16 bytes → 32 hex chars).
fn normalize_address(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A single peer conversation: its message history and an unsent draft.
pub struct Conversation {
    /// Routing key: a display name offline, or the hex destination hash under
    /// `net`. Outbound messages address this verbatim.
    pub peer: String,
    /// Human-friendly name when known (from an announce); the peer list prefers
    /// it over the raw key.
    pub display_name: Option<String>,
    /// Scrollback (oldest first): local `[TX]` echoes and inbound `[RX]` lines,
    /// each timestamped at receipt/send.
    pub messages: Vec<Entry>,
    /// Per-conversation compose buffer, preserved across peer switches.
    pub draft: String,
    /// Inbound messages received since this conversation was last viewed.
    pub unread: usize,
    /// Manually added (Ctrl+O) — persist it even with no messages yet so a peer
    /// you entered by hand survives a restart. Transient (not serialized): its
    /// only job is to force the first save, after which the file persists.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub pinned: bool,
}

impl Conversation {
    /// A fresh, empty conversation with the given peer key.
    pub fn new(peer: impl Into<String>) -> Self {
        Self {
            peer: peer.into(),
            display_name: None,
            messages: Vec::new(),
            draft: String::new(),
            unread: 0,
            pinned: false,
        }
    }

    /// What to show in the peer list: the display name, else a shortened key.
    pub fn label(&self) -> String {
        match &self.display_name {
            Some(n) if !n.is_empty() => n.clone(),
            _ if self.peer.len() > 10 => format!("{}\u{2026}", &self.peer[..10]),
            _ => self.peer.clone(),
        }
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
    /// This node's own LXMF address (hex), once the network task reports it.
    pub local_address: Option<String>,
    /// When `Some`, a propagation sync is running and the pop-up shows this text.
    pub sync_status: Option<String>,
    /// When `Some`, the New Conversation popup is open (and captures all input).
    pub new_conv: Option<NewConv>,
    /// Persisted operator settings (display name, hub, active propagation node).
    pub config: Config,
    /// Commands queued for the network task; drained by `main` after key input.
    pub commands: VecDeque<NetCommand>,
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
            local_address: None,
            sync_status: None,
            new_conv: None,
            config: Config::default(),
            commands: VecDeque::new(),
            dirty: Vec::new(),
            outbound: VecDeque::new(),
            syslog: Vec::new(),
            should_quit: false,
        }
    }

    /// Record/refresh a discovered peer. Delivery peers become conversations
    /// (so they appear in the peer list and can be messaged); propagation nodes
    /// go to the Network-tab registry. Keyed by hex destination hash.
    pub fn upsert_peer(&mut self, kind: PeerKind, hash: String, name: Option<String>) {
        match kind {
            PeerKind::Delivery => {
                if let Some(conv) = self.conversations.iter_mut().find(|c| c.peer == hash) {
                    if name.is_some() {
                        conv.display_name = name;
                    }
                } else {
                    let mut conv = Conversation::new(hash);
                    conv.display_name = name;
                    self.conversations.push(conv);
                }
            }
            PeerKind::Propagation => {
                if let Some(node) = self.nodes.iter_mut().find(|n| n.hash == hash) {
                    if name.is_some() {
                        node.name = name;
                    }
                } else {
                    self.nodes.push(Node { hash, name });
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

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // The New Conversation modal, when open, captures all input.
        if self.new_conv.is_some() {
            self.handle_new_conv_key(ctrl, key);
            return;
        }

        match (ctrl, key.code) {
            // --- Program-global --------------------------------------------------
            (true, KeyCode::Char('q')) => self.should_quit = true,
            (true, KeyCode::Char('o')) => self.open_new_conv(),

            // --- Tool (tab) switching -------------------------------------------
            (true, KeyCode::Char('n')) => self.active = self.active.next(),
            (true, KeyCode::Char('p')) => self.active = self.active.prev(),

            // --- Delegated to the active tool -----------------------------------
            _ => self.handle_tool_key(ctrl, key),
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

    /// Network: navigate the propagation-node list, set the active node, sync.
    fn handle_network_key(&mut self, _ctrl: bool, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.node_selected = self.node_selected.saturating_sub(1),
            KeyCode::Down => {
                if self.node_selected + 1 < self.nodes.len() {
                    self.node_selected += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(node) = self.nodes.get(self.node_selected) {
                    let hash = node.hash.clone();
                    self.config.propagation_node = Some(hash.clone());
                    self.commands
                        .push_back(NetCommand::SetPropagationNode(Some(hash)));
                }
            }
            KeyCode::Char('s') => self.commands.push_back(NetCommand::SyncNow),
            _ => {}
        }
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
        if let Some(existing) = self.conversations.iter_mut().find(|c| c.peer == loaded.peer) {
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
        let Some(conv) = self.conversations.get_mut(self.selected) else {
            return;
        };
        let body = conv.draft.trim().to_string();
        if body.is_empty() {
            return;
        }
        conv.messages.push(Entry::now(format!("[TX] {body}")));
        conv.draft.clear();
        let peer = conv.peer.clone();
        // `conv`'s borrow ends above; safe to touch `self.outbound`/`dirty` now.
        self.outbound.push_back(Outbound {
            peer: peer.clone(),
            body,
        });
        self.mark_dirty(&peer);
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

    #[test]
    fn network_tab_selects_and_sets_propagation_node() {
        let mut app = App::new();
        app.active = Tool::Network;
        let n0 = "aa".repeat(16); // 32 hex chars = 16 bytes
        let n1 = "bb".repeat(16);
        app.nodes = vec![
            Node {
                hash: n0.clone(),
                name: Some("n0".to_string()),
            },
            Node {
                hash: n1,
                name: None,
            },
        ];

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
    fn network_keys_are_inert_with_no_nodes() {
        let mut app = App::new();
        app.active = Tool::Network;
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        assert_eq!(app.node_selected, 0);
        assert!(app.config.propagation_node.is_none());
        assert!(app.commands.is_empty(), "Enter on an empty list does nothing");
    }

    #[test]
    fn start_conversation_validates_and_normalizes() {
        let mut app = App::new();
        let before = app.conversations.len();
        // Colons / spaces / case are tolerated → 32 hex chars.
        assert!(app.start_conversation(
            "A1:b2:c3:d4 e5 f6:00:11:22:33:44:55:66:77:88:99",
            "Bravo-6"
        ));
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
        assert!(app.new_conv.as_ref().is_some_and(|nc| nc.error), "stays open with error");
    }
}
