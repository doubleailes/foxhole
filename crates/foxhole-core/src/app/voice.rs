//! Voice tool: the LXST telephony roster, the call HUD, and the keys that drive
//! a call.
//!
//! This is the App-level *binding* for voice, in the same sense `map.rs` is the
//! binding for the World Map: it holds what the operator sees and turns keys
//! into [`VoiceCommand`]s, while the live telephony runtime (rsLXST, Opus, the
//! audio devices) lives in `foxhole-voice` behind the `voice` feature. Nothing
//! here does I/O, so the whole tool — roster, HUD, key routing — is unit-tested
//! and renders identically in an offline build, where it simply never receives a
//! [`VoiceEvent`].
//!
//! **The task owns call state, not this module.** Every transition arrives as
//! `VoiceEvent::Call`; the keys below only *request* things. That one-way rule
//! is what keeps the UI from showing a call the telephony runtime has already
//! torn down (or vice versa) — LXST's own `TelephonyRuntimeCore` is the single
//! authority on whether a line is busy, and second-guessing it from here is how
//! a UI ends up offering "answer" for a call that no longer exists.

use super::*;
use crate::domain::now_secs;

/// Voice tool state: who can be called, the call in progress, and the meters.
pub struct VoiceState {
    /// Peers heard announcing an `lxst.telephony` destination, newest-heard
    /// last. Keyed by hex **identity** hash (not an LXMF destination hash).
    pub peers: Vec<VoicePeer>,
    /// Highlighted row in the roster.
    pub selected: usize,
    /// The call in progress, as last reported by the telephony task.
    pub call: Option<Call>,
    /// Whether the microphone is muted (requested locally, echoed in the HUD).
    pub muted: bool,
    /// Profile requested for the next/current call. The negotiated one lives on
    /// [`Call::profile`]; they differ while a renegotiation is in flight.
    pub profile: VoiceProfile,
    /// Platform audio-backend readiness, as the task reported it.
    pub audio: AudioStatus,
    /// Our own hex identity hash — what a peer dials to reach us.
    pub local_identity: Option<String>,
    /// Transmit level, 0–100, for the VU meter.
    pub tx_level: u8,
    /// Receive level, 0–100.
    pub rx_level: u8,
    /// Call-history scrollback (`[VOX]` lines), newest last.
    pub log: Vec<Entry>,
    /// Display names by hex identity hash, learned from `lxmf.delivery`
    /// announces (LXST's own announce carries none). Kept apart from the roster
    /// because an alias says a peer *has* a name, not that it can take a call.
    pub aliases: HashMap<String, String>,
}

/// Cap on the voice roster. `lxst.telephony` announces are as cheap to mint as
/// any other, so the same bound the peer roster carries applies here; past it
/// the least-recently-heard entry is evicted.
pub(crate) const VOICE_PEERS_MAX: usize = 512;

/// Cap on the voice call-history scrollback (bottom-pinned, so trimming the
/// head is invisible).
pub(crate) const VOICE_LOG_MAX: usize = 500;

/// Cap on the identity→name alias table. It is fed by *every* `lxmf.delivery`
/// announce, not just callable peers, so it grows faster than the roster it
/// labels — and announces are free to mint. Bounded on the same grounds as the
/// peer cache and the roster: an announce flood must not grow memory without
/// limit. Generous relative to [`VOICE_PEERS_MAX`], since an alias is two short
/// strings and holding one for a peer not yet heard on the telephony aspect is
/// exactly what makes the roster read with names the moment it is.
pub(crate) const VOICE_ALIASES_MAX: usize = 4096;

impl VoiceState {
    /// Nothing heard, no call, meters at rest.
    pub(super) fn new() -> Self {
        Self {
            peers: Vec::new(),
            selected: 0,
            call: None,
            muted: false,
            profile: VoiceProfile::default(),
            audio: AudioStatus::Unknown,
            local_identity: None,
            tx_level: 0,
            rx_level: 0,
            log: Vec::new(),
            aliases: HashMap::new(),
        }
    }

    /// The highlighted peer, if the roster is non-empty.
    pub fn selected_peer(&self) -> Option<&VoicePeer> {
        self.peers.get(self.selected)
    }

    /// Whether a call is ringing here and can be answered.
    pub fn is_ringing(&self) -> bool {
        matches!(
            self.call,
            Some(Call {
                phase: CallPhase::Ringing,
                direction: CallDirection::Incoming,
                ..
            })
        )
    }

    /// Whether there is any call to hang up (including one still ringing or
    /// being dialled — abandoning those is the same key).
    pub fn is_busy(&self) -> bool {
        self.call.is_some()
    }
}

impl App {
    /// Voice tool keys. Up/Down move the roster; Enter places a call to the
    /// selection *or* answers a ringing one; `h`/Esc hangs up (or rejects);
    /// `m` mutes; `p` cycles the profile; `a` re-announces.
    ///
    /// Enter is deliberately overloaded: when the phone is ringing, answering is
    /// the only thing the operator wants from that key, and hunting for a
    /// separate binding while it rings is exactly the wrong ergonomics.
    pub(super) fn handle_voice_key(&mut self, _ctrl: bool, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.voice.selected = self.voice.selected.saturating_sub(1),
            KeyCode::Down => {
                if self.voice.selected + 1 < self.voice.peers.len() {
                    self.voice.selected += 1;
                }
            }
            KeyCode::Enter => {
                if self.voice.is_ringing() {
                    self.answer_call();
                } else {
                    self.place_call();
                }
            }
            KeyCode::Esc | KeyCode::Char('h') => self.hangup_call(),
            KeyCode::Char('m') => self.toggle_mute(),
            KeyCode::Char('p') => self.cycle_voice_profile(),
            KeyCode::Char('a') => {
                self.push_voice_log("[VOX] announcing lxst.telephony".to_string());
                self.outbox
                    .commands
                    .push_back(NetCommand::Voice(VoiceCommand::Announce));
            }
            _ => {}
        }
    }

    /// Place a call to the highlighted roster entry. Refused while a call is
    /// already up — LXST telephony is single-line, so the task would reject it
    /// anyway; saying so here keeps the operator from wondering why nothing
    /// happened.
    pub(super) fn place_call(&mut self) {
        if self.voice.is_busy() {
            self.push_voice_log("[VOX] [WRN] line busy — hang up first".to_string());
            return;
        }
        let Some(peer) = self.voice.selected_peer() else {
            self.push_voice_log("[VOX] [WRN] no voice peer selected".to_string());
            return;
        };
        let (identity, label) = (peer.identity.clone(), peer.label());
        self.push_voice_log(format!("[VOX] calling {label}"));
        self.outbox
            .commands
            .push_back(NetCommand::Voice(VoiceCommand::Call(identity)));
    }

    /// Dial the peer behind an LXMF destination hash — what the Conversations
    /// roster's Ctrl+V does. That roster only ever knows a *destination* hash,
    /// and LXST addresses an *identity*; the two hang off the same key but are
    /// not interchangeable, so the resolution is left to the telephony task,
    /// which holds the announce-learned key cache. Jumps to the Voice tool so
    /// the ring/answer HUD is what the operator is looking at while it comes up.
    pub(super) fn call_peer_dest(&mut self, dest: String, label: String) {
        if self.voice.is_busy() {
            self.push_voice_log("[VOX] [WRN] line busy — hang up first".to_string());
            return;
        }
        self.active = Tool::Voice;
        self.push_voice_log(format!("[VOX] calling {label}"));
        self.outbox
            .commands
            .push_back(NetCommand::Voice(VoiceCommand::CallPeer(dest)));
    }

    /// Answer the ringing incoming call (no-op when nothing is ringing).
    pub(super) fn answer_call(&mut self) {
        if !self.voice.is_ringing() {
            return;
        }
        self.push_voice_log("[VOX] answering".to_string());
        self.outbox
            .commands
            .push_back(NetCommand::Voice(VoiceCommand::Answer));
    }

    /// Hang up / reject / abandon, whichever applies (no-op when idle).
    pub(super) fn hangup_call(&mut self) {
        if !self.voice.is_busy() {
            return;
        }
        self.push_voice_log("[VOX] hanging up".to_string());
        self.outbox
            .commands
            .push_back(NetCommand::Voice(VoiceCommand::Hangup));
    }

    /// Toggle the microphone mute. Optimistic: the flag flips here so the HUD
    /// responds to the keystroke at once, and the task is told to match. A mute
    /// that fails to reach the task is far less likely than one the operator
    /// believes took effect because the UI lagged a round-trip.
    pub(super) fn toggle_mute(&mut self) {
        self.voice.muted = !self.voice.muted;
        let muted = self.voice.muted;
        self.push_voice_log(if muted {
            "[VOX] microphone muted".to_string()
        } else {
            "[VOX] microphone live".to_string()
        });
        self.outbox
            .commands
            .push_back(NetCommand::Voice(VoiceCommand::SetMuted(muted)));
    }

    /// Cycle to the next profile this build can actually carry audio on, and
    /// ask the task to renegotiate when a call is up.
    pub(super) fn cycle_voice_profile(&mut self) {
        self.voice.profile = self.voice.profile.next_supported();
        let p = self.voice.profile;
        self.push_voice_log(format!(
            "[VOX] profile {} ({}, {} Hz, {} ms)",
            p.abbreviation(),
            p.name(),
            p.sample_rate_hz(),
            p.frame_ms(),
        ));
        self.outbox
            .commands
            .push_back(NetCommand::Voice(VoiceCommand::SetProfile(p)));
    }

    /// Fold one event from the telephony task into the Voice tool.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn apply_voice_event(&mut self, ev: VoiceEvent) {
        match ev {
            VoiceEvent::Peer {
                identity,
                name,
                hops,
            } => self.upsert_voice_peer(identity, name, hops),
            VoiceEvent::Alias { identity, name } => self.learn_voice_alias(identity, name),
            VoiceEvent::Local(identity) => self.voice.local_identity = Some(identity),
            VoiceEvent::Call(call) => self.set_call(call),
            VoiceEvent::Ended(reason) => {
                self.voice.call = None;
                // Meters would otherwise freeze at their last reading and read
                // as a live call on a glance at the HUD.
                self.voice.tx_level = 0;
                self.voice.rx_level = 0;
                self.push_voice_log(format!("[VOX] call ended: {reason}"));
            }
            VoiceEvent::Audio(status) => {
                match &status {
                    AudioStatus::Ready => {
                        self.push_voice_log("[VOX] audio devices ready".to_string())
                    }
                    // One working direction is a warning, not a failure: the
                    // call is still useful, and saying "no audio" would be wrong.
                    AudioStatus::TransmitOnly(_) | AudioStatus::ReceiveOnly(_) => self
                        .push_voice_log(format!("[VOX] [WRN] audio {}", status.summary())),
                    AudioStatus::Unavailable(why) => self.push_voice_log(format!(
                        "[VOX] [WRN] no audio devices ({why}) — calls will signal but carry no audio"
                    )),
                    AudioStatus::Unknown => {}
                }
                self.voice.audio = status;
            }
            VoiceEvent::Levels { tx, rx } => {
                self.voice.tx_level = tx.min(100);
                self.voice.rx_level = rx.min(100);
            }
            VoiceEvent::Sys(line) => self.push_voice_log(line),
        }
    }

    /// Adopt the task's authoritative call state, logging the transitions the
    /// operator cares about (ring, connect, clear) exactly once each.
    fn set_call(&mut self, call: Option<Call>) {
        let previous = self.voice.call.as_ref().map(|c| (c.peer.clone(), c.phase));
        match (&previous, &call) {
            // A new call appeared.
            (None, Some(c)) => {
                let verb = match c.direction {
                    CallDirection::Incoming => "incoming call from",
                    CallDirection::Outgoing => "calling",
                };
                self.push_voice_log(format!("[VOX] {verb} {}", c.label()));
            }
            // Same call, new phase.
            (Some((_, was)), Some(c)) if *was != c.phase => {
                self.push_voice_log(format!("[VOX] {} {}", c.label(), c.phase.label()));
            }
            _ => {}
        }
        if call.is_none() {
            self.voice.tx_level = 0;
            self.voice.rx_level = 0;
        }
        // A fresh call always starts unmuted; carrying a mute across calls is a
        // classic way to talk into a dead microphone for the first ten seconds.
        // The task keeps its own copy and applies it when capture opens, so the
        // reset has to be *told* to it — clearing only the local flag would
        // show a live microphone while the task muted the real one.
        if previous.is_none() && call.is_some() && self.voice.muted {
            self.voice.muted = false;
            self.outbox
                .commands
                .push_back(NetCommand::Voice(VoiceCommand::SetMuted(false)));
        }
        // Label the call from the alias table when the task had no name for it.
        let mut call = call;
        if let Some(c) = call.as_mut()
            && c.name.is_none()
        {
            c.name = self.voice.aliases.get(&c.peer).cloned();
        }
        // Track the negotiated profile so the next call opens on what actually
        // worked rather than re-proposing one the peer already renegotiated.
        if let Some(p) = call.as_ref().and_then(|c| c.profile) {
            self.voice.profile = p;
        }
        self.voice.call = call;
    }

    /// Record a display name for an identity and apply it to whatever already
    /// refers to that identity — the roster entry and the call in progress —
    /// so a name learned mid-call is not stuck showing a hash until the next
    /// announce.
    fn learn_voice_alias(&mut self, identity: String, name: String) {
        if let Some(peer) = self.voice.peers.iter_mut().find(|p| p.identity == identity) {
            peer.name = Some(name.clone());
        }
        if let Some(call) = self.voice.call.as_mut()
            && call.peer == identity
        {
            call.name = Some(name.clone());
        }
        self.voice.aliases.insert(identity, name);
        self.prune_voice_aliases();
    }

    /// Keep the alias table bounded. Anything still referenced — a roster entry
    /// or the call in progress — is retained regardless of age, since those are
    /// precisely the labels on screen; the rest are dropped wholesale once the
    /// cap is passed. Losing an alias costs nothing permanent: the peer's next
    /// announce re-learns it.
    fn prune_voice_aliases(&mut self) {
        if self.voice.aliases.len() <= VOICE_ALIASES_MAX {
            return;
        }
        let mut keep: std::collections::HashSet<&str> = self
            .voice
            .peers
            .iter()
            .map(|p| p.identity.as_str())
            .collect();
        if let Some(call) = &self.voice.call {
            keep.insert(call.peer.as_str());
        }
        let referenced: Vec<String> = self
            .voice
            .aliases
            .keys()
            .filter(|k| keep.contains(k.as_str()))
            .cloned()
            .collect();
        let mut kept = std::collections::HashMap::with_capacity(referenced.len());
        for id in referenced {
            if let Some(name) = self.voice.aliases.remove(&id) {
                kept.insert(id, name);
            }
        }
        self.voice.aliases = kept;
    }

    /// Record/refresh a voice-capable peer, keyed by hex identity hash.
    fn upsert_voice_peer(&mut self, identity: String, name: Option<String>, hops: Option<u8>) {
        let now = now_secs();
        if let Some(peer) = self.voice.peers.iter_mut().find(|p| p.identity == identity) {
            // A re-announce without a name must not blank a name we already have.
            if name.is_some() {
                peer.name = name;
            }
            if hops.is_some() {
                peer.hops = hops;
            }
            peer.last_seen = now;
            return;
        }
        // Fall back to a name learned from the peer's LXMF announce, so the
        // roster reads "alice" rather than a hash whenever we know one.
        let name = name.or_else(|| self.voice.aliases.get(&identity).cloned());
        self.voice.peers.push(VoicePeer {
            identity,
            name,
            hops,
            last_seen: now,
        });
        self.prune_voice_peers();
    }

    /// Evict the least-recently-heard entries once the roster passes its cap,
    /// keeping the operator's cursor on the same peer across the eviction.
    fn prune_voice_peers(&mut self) {
        if self.voice.peers.len() <= VOICE_PEERS_MAX {
            return;
        }
        let keep = self.voice.selected_peer().map(|p| p.identity.clone());
        let excess = self.voice.peers.len() - VOICE_PEERS_MAX;
        // Oldest-heard first; `last_seen` ties break by current order, which is
        // insertion order, so the result is stable.
        let mut order: Vec<usize> = (0..self.voice.peers.len()).collect();
        order.sort_by_key(|&i| (self.voice.peers[i].last_seen, i));
        let mut drop: Vec<usize> = order.into_iter().take(excess).collect();
        drop.sort_unstable();
        for i in drop.into_iter().rev() {
            self.voice.peers.remove(i);
        }
        self.voice.selected = keep
            .and_then(|id| self.voice.peers.iter().position(|p| p.identity == id))
            .unwrap_or(0);
    }

    /// Append a `[VOX]` line to both the call history and the system log, so a
    /// call is reconstructable from the Log tool alone after the fact.
    pub(crate) fn push_voice_log(&mut self, line: String) {
        self.voice.log.push(Entry::now(line.clone()));
        if self.voice.log.len() > VOICE_LOG_MAX {
            let excess = self.voice.log.len() - VOICE_LOG_MAX;
            self.voice.log.drain(..excess);
        }
        self.push_log(line);
    }
}
