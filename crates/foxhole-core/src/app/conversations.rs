//! Conversations tool: peer discovery, roster navigation, compose + send, and
//! inbound delivery. All operate on `App`'s `conversations`/`selected` state.

use super::*;
use crate::domain::{normalize_address, now_secs, short_hash};

impl App {
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

    /// Create (or focus) a conversation for a manually-entered LXMF address.
    /// Accepts either a 32-char hex destination hash or a 12-word mnemonic
    /// phrase (decoded + checksum-verified). Returns false if it's neither.
    pub fn start_conversation(&mut self, address: &str, alias: &str) -> bool {
        let key = match resolve_address(address) {
            Some(k) => k,
            None => return false,
        };
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

    /// Conversations: pane cycling, peer navigation, compose + send.
    pub(super) fn handle_conversations_key(&mut self, ctrl: bool, key: KeyEvent) {
        match (ctrl, key.code) {
            (true, KeyCode::Char('s')) => self.transmit(),
            (true, KeyCode::Char('x')) => self.purge(),
            // On-demand propagation sync (off-grid: no automatic polling).
            (true, KeyCode::Char('r')) => self.commands.push_back(NetCommand::SyncNow),
            // Set/edit the outbound message title (Nomadnet's Ctrl+T): focus the
            // Transmit pane and toggle between the title and the body field.
            (true, KeyCode::Char('t')) => {
                self.focus = Pane::Transmit;
                self.transmit_field = self.transmit_field.toggle();
            }
            (_, KeyCode::Tab) => self.toggle_focus(),

            // Peer-list navigation — only when that pane is focused.
            (false, KeyCode::Up) if self.focus == Pane::PeerList => self.select_prev(),
            (false, KeyCode::Down) if self.focus == Pane::PeerList => self.select_next(),
            // Cycle the selected peer's trust level.
            (false, KeyCode::Char('t')) if self.focus == Pane::PeerList => {
                self.cycle_selected_trust()
            }

            // Transmit-pane editing — only when that pane is focused. Keystrokes
            // land in whichever field (title or body) Ctrl+T last selected.
            (false, KeyCode::Char(c)) if self.focus == Pane::Transmit => {
                let field = self.transmit_field;
                if let Some(conv) = self.selected_conv_mut() {
                    match field {
                        TransmitField::Title => conv.draft_title.push(c),
                        TransmitField::Body => conv.draft.push(c),
                    }
                }
            }
            (false, KeyCode::Backspace) if self.focus == Pane::Transmit => {
                let field = self.transmit_field;
                if let Some(conv) = self.selected_conv_mut() {
                    match field {
                        TransmitField::Title => conv.draft_title.pop(),
                        TransmitField::Body => conv.draft.pop(),
                    };
                }
            }

            // Everything else is ignored.
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
    pub(super) fn mark_selected_read(&mut self) {
        // Switching conversations re-anchors the thread to its newest message.
        self.thread_scroll.to_bottom();
        if let Some(conv) = self.selected_conv_mut() {
            conv.unread = 0;
            let peer = conv.peer.clone();
            self.mark_dirty(&peer);
        }
    }

    /// Cycle the selected peer's trust level (the `t` key in the peer rosters),
    /// persist it, and log a `[SEC]` line. No-op without a selected conversation.
    pub(super) fn cycle_selected_trust(&mut self) {
        let Some(conv) = self.conversations.get_mut(self.selected) else {
            return;
        };
        conv.trust = conv.trust.next();
        let peer = conv.peer.clone();
        let level = conv.trust.label();
        self.syslog.push(Entry::now(format!(
            "[SEC] TRUST {}.. -> {level}",
            short_hash(&peer)
        )));
        self.mark_dirty(&peer);
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
            // Adopt the persisted trust unless this live session already set one
            // (an announce-created conversation starts at the default `Unknown`).
            if existing.trust == Trust::Unknown {
                existing.trust = loaded.trust;
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
        let (body, title) = match self.conversations.get(self.selected) {
            Some(conv) => (
                conv.draft.trim().to_string(),
                conv.draft_title.trim().to_string(),
            ),
            None => return,
        };
        if body.is_empty() {
            return;
        }
        let id = self.next_id();
        let conv = &mut self.conversations[self.selected];
        // Echo the title (when set) ahead of the body so the thread shows what
        // was sent, mirroring how the recipient sees a titled message.
        let echo = if title.is_empty() {
            format!("[TX] {body}")
        } else {
            format!("[TX] {title}: {body}")
        };
        let mut entry = Entry::now(echo);
        entry.id = id;
        entry.status = MsgStatus::Sending;
        conv.messages.push(entry);
        conv.draft.clear();
        conv.draft_title.clear();
        let peer = conv.peer.clone();
        // `conv`'s borrow ends above; safe to touch `self.outbound`/`dirty` now.
        self.outbound.push_back(Outbound {
            id,
            peer: peer.clone(),
            title,
            body,
        });
        // Sending resets the compose form back to the body field.
        self.transmit_field = TransmitField::Body;
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

    /// Discard the selected conversation's draft (title and body) without
    /// transmitting (Ctrl+X).
    pub fn purge(&mut self) {
        if let Some(conv) = self.selected_conv_mut() {
            conv.draft.clear();
            conv.draft_title.clear();
        }
        self.transmit_field = TransmitField::Body;
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

    /// Record a peer's shared position from LXMF telemetry (Sideband-style),
    /// creating the conversation on first contact so a fix from a peer we have
    /// not messaged still plots on the World Map. The location is transient (not
    /// written to the store) — it is refreshed from live telemetry. Logs the
    /// update to the Log tool.
    pub fn set_location(&mut self, peer: &str, pos: GeoPos) {
        let idx = match self.conversations.iter().position(|c| c.peer == peer) {
            Some(i) => i,
            None => {
                self.conversations.push(Conversation::new(peer));
                self.conversations.len() - 1
            }
        };
        self.conversations[idx].location = Some(pos);
        let label = self.conversations[idx].label();
        self.push_log(format!(
            "[SYS] telemetry: {label} @ {:.4}, {:.4}",
            pos.lat, pos.lon
        ));
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

/// Resolve a typed address to a canonical 32-char hex destination hash, accepting
/// either hex (colons/spaces tolerated) or a 12-word mnemonic phrase (decoded and
/// checksum-verified). `None` if it is neither.
fn resolve_address(input: &str) -> Option<String> {
    // A hex address (even with colon/space/angle-bracket separators) contains no
    // letters outside the hex alphabet; a mnemonic phrase always does (its words
    // carry g–z). Use that to disambiguate without a fragile token count, so a
    // spaced-out hex string like `a1 b2 …` still takes the hex path.
    let is_mnemonic = input
        .chars()
        .any(|c| c.is_ascii_alphabetic() && !c.is_ascii_hexdigit());
    if is_mnemonic {
        let hash = crate::mnemonic::decode(input).ok()?;
        return Some(hash.iter().map(|b| format!("{b:02x}")).collect());
    }
    let key = normalize_address(input);
    (key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit())).then_some(key)
}
