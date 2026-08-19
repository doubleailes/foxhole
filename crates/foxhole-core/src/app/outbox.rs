//! The handoff queues between `App` and `main`.
//!
//! Everything `App` produces for the outside world lands here: messages accepted
//! for transmission, commands for the network task, and the peer keys whose
//! on-disk copy went stale. `main` drains all three each iteration, which is why
//! `App` itself can stay free of I/O.
//!
//! Unlike the per-tool groups, this one is touched from *every* tool — that is
//! what a send queue is, and grouping it names the four fields that move
//! together rather than promising any reduction in coupling.

use std::collections::VecDeque;

use crate::domain::{NetCommand, Outbound};

/// The outbound side of `App`: what is queued for the network task and the
/// store, plus the id source that correlates a sent message with its status.
#[derive(Default)]
pub struct Outbox {
    /// Commands queued for the network task; drained by `main` after key input.
    pub commands: VecDeque<NetCommand>,
    /// Messages accepted for transmission, awaiting handoff to the protocol
    /// task. FIFO so ordering on the wire matches operator intent.
    pub outbound: VecDeque<Outbound>,
    /// Peer keys whose on-disk copy is stale; `main` drains this and persists
    /// each changed conversation.
    pub dirty: Vec<String>,
    /// Monotonic id source for correlating outbound messages with their status.
    next_msg_id: u64,
}

impl Outbox {
    /// Next correlation id for an outbound message.
    pub(super) fn next_id(&mut self) -> u64 {
        self.next_msg_id += 1;
        self.next_msg_id
    }

    /// Flag a conversation as needing a re-save (deduplicated).
    pub(super) fn mark_dirty(&mut self, peer: &str) {
        if !self.dirty.iter().any(|p| p == peer) {
            self.dirty.push(peer.to_string());
        }
    }
}
