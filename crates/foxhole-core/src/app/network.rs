//! Network tool: the two-column peers/nodes view, propagation-node selection,
//! and rnpath-style path probes.

use std::collections::HashMap;

use super::*;
use crate::domain::{now_secs, short_hash};

/// The Network tool's own state: the propagation-node registry, which column
/// has focus, and the reachability readouts (path probes, interface status).
///
/// The peers column is deliberately *not* here — it is the conversations
/// roster, which the Network tool only borrows a view of.
#[derive(Default)]
pub struct NetworkState {
    /// Discovered propagation nodes.
    pub nodes: Vec<Node>,
    /// Highlighted row in the propagation-node list.
    pub selected: usize,
    /// Which column has focus (Peers reuses the conversations selection, Nodes
    /// uses [`NetworkState::selected`] for the in-column cursor).
    pub col: NetColumn,
    /// Latest rnpath-style path probe per hex destination hash.
    pub path_probes: HashMap<String, PathProbe>,
    /// Live interface status (Interfaces tab); empty until the stack reports.
    pub interfaces: Vec<Interface>,
    /// Active link count reported alongside the interface snapshot.
    pub link_count: u32,
}

impl App {
    /// Network: two columns (peers | nodes). Up/Down move within the focused
    /// column; Tab/Left/Right switch columns; Enter opens a peer's conversation
    /// or sets a node active; `p` path-probes the selection; `s` syncs.
    pub(super) fn handle_network_key(&mut self, _ctrl: bool, key: KeyEvent) {
        match key.code {
            KeyCode::Up => match self.net.col {
                NetColumn::Peers => self.select_prev(),
                NetColumn::Nodes => self.net.selected = self.net.selected.saturating_sub(1),
            },
            KeyCode::Down => match self.net.col {
                NetColumn::Peers => self.select_next(),
                NetColumn::Nodes => {
                    if self.net.selected + 1 < self.net.nodes.len() {
                        self.net.selected += 1;
                    }
                }
            },
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => self.net.col = self.net.col.other(),
            KeyCode::Enter => match self.net.col {
                // Jump straight from the roster into the chat.
                NetColumn::Peers => {
                    if self.selected_conv().is_some() {
                        self.active = Tool::Conversations;
                        self.convs.focus = Pane::Transmit;
                        self.mark_selected_read();
                    }
                }
                NetColumn::Nodes => {
                    if let Some(node) = self.net.nodes.get(self.net.selected) {
                        let hash = node.hash.clone();
                        self.config.propagation_node = Some(hash.clone());
                        self.outbox
                            .commands
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
                    self.outbox
                        .commands
                        .push_back(NetCommand::RequestPath(hash));
                }
            }
            KeyCode::Char('s') => self.outbox.commands.push_back(NetCommand::SyncNow),
            // Show the focused selection's address as a mnemonic phrase.
            KeyCode::Char('m') => {
                if let Some(hash) = self.focused_net_hash() {
                    self.open_mnemonic(&hash);
                }
            }
            // Cycle the selected peer's trust level (peers column only — nodes
            // are relays, not correspondents).
            KeyCode::Char('t') if self.net.col == NetColumn::Peers => self.cycle_selected_trust(),
            _ => {}
        }
    }

    /// The hex destination hash of the focused Network-tab selection, if any.
    fn focused_net_hash(&self) -> Option<String> {
        match self.net.col {
            NetColumn::Peers => self.selected_conv().map(|c| c.peer.clone()),
            NetColumn::Nodes => self
                .net
                .nodes
                .get(self.net.selected)
                .map(|n| n.hash.clone()),
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
        self.net.path_probes.insert(
            hash,
            PathProbe {
                at: now_secs(),
                hops,
                iface,
            },
        );
    }

    /// Replace the interface-status snapshot shown by the Interfaces tab and
    /// record the active link count (a status refresh, not an upsert).
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn set_interfaces(&mut self, interfaces: Vec<Interface>, links: u32) {
        self.net.interfaces = interfaces;
        self.net.link_count = links;
    }
}
