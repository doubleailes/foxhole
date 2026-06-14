//! Network tool: the two-column peers/nodes view, propagation-node selection,
//! and rnpath-style path probes.

use super::*;
use crate::domain::{now_secs, short_hash};

impl App {
    /// Network: two columns (peers | nodes). Up/Down move within the focused
    /// column; Tab/Left/Right switch columns; Enter opens a peer's conversation
    /// or sets a node active; `p` path-probes the selection; `s` syncs.
    pub(super) fn handle_network_key(&mut self, _ctrl: bool, key: KeyEvent) {
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

    /// Replace the interface-status snapshot shown by the Interfaces tab and
    /// record the active link count (a status refresh, not an upsert).
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn set_interfaces(&mut self, interfaces: Vec<Interface>, links: u32) {
        self.interfaces = interfaces;
        self.link_count = links;
    }
}
