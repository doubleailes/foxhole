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
    /// The platform audio devices and the current selection, as the task last
    /// enumerated them. Empty until it reports (and always empty offline).
    pub devices: AudioDevices,
    /// Open device picker (`d`), if any.
    pub picker: Option<DevicePicker>,
}

/// Which direction the device picker is choosing for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceColumn {
    Input,
    Output,
}

impl DeviceColumn {
    /// Heading for the picker, and the word used in its log lines.
    pub fn label(self) -> &'static str {
        match self {
            DeviceColumn::Input => "MICROPHONE",
            DeviceColumn::Output => "SPEAKER",
        }
    }

    fn other(self) -> Self {
        match self {
            DeviceColumn::Input => DeviceColumn::Output,
            DeviceColumn::Output => DeviceColumn::Input,
        }
    }
}

/// Device picker overlay: one direction at a time, Tab to swap.
///
/// Row 0 is always "system default" — the `None` preference — so clearing a
/// choice is a selection like any other rather than a separate key to discover.
pub struct DevicePicker {
    /// Direction being chosen.
    pub column: DeviceColumn,
    /// Highlighted row: 0 = system default, `n` = the `n-1`th device.
    pub index: usize,
}

impl DevicePicker {
    /// The device names offered for the current column.
    pub fn names<'a>(&self, devices: &'a AudioDevices) -> &'a [String] {
        match self.column {
            DeviceColumn::Input => &devices.inputs,
            DeviceColumn::Output => &devices.outputs,
        }
    }

    /// The highlighted choice: `None` on row 0 (follow the system default).
    pub fn choice(&self, devices: &AudioDevices) -> Option<String> {
        self.index
            .checked_sub(1)
            .and_then(|i| self.names(devices).get(i))
            .cloned()
    }

    /// Rows in the current column, including the "system default" row.
    pub fn len(&self, devices: &AudioDevices) -> usize {
        self.names(devices).len() + 1
    }

    /// Whether `name` (or the default row) is the one currently in use.
    pub fn is_selected(&self, devices: &AudioDevices, name: Option<&str>) -> bool {
        let current = match self.column {
            DeviceColumn::Input => devices.selected.input.as_deref(),
            DeviceColumn::Output => devices.selected.output.as_deref(),
        };
        current == name
    }
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
            devices: AudioDevices::default(),
            picker: None,
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
            KeyCode::Char('d') => self.open_device_picker(),
            _ => {}
        }
    }

    /// Open the audio device picker, refreshing the list first: a headset
    /// plugged in after bring-up must show up without restarting the terminal,
    /// and the enumeration is cheap enough to redo on every open.
    pub(super) fn open_device_picker(&mut self) {
        self.outbox
            .commands
            .push_back(NetCommand::Voice(VoiceCommand::ListDevices));
        let column = DeviceColumn::Input;
        let picker = DevicePicker { column, index: 0 };
        // Start on the row already in use, so Enter is a no-op rather than a
        // silent change of device.
        let index = current_row(&self.voice.devices, column);
        self.voice.picker = Some(DevicePicker { index, ..picker });
    }

    /// Device-picker keys: Up/Down move, Tab swaps microphone/speaker, Enter
    /// selects (row 0 = follow the system default), Esc closes.
    pub(super) fn handle_device_picker_key(&mut self, key: KeyEvent) {
        let devices = self.voice.devices.clone();
        let Some(picker) = self.voice.picker.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up => picker.index = picker.index.saturating_sub(1),
            KeyCode::Down => {
                if picker.index + 1 < picker.len(&devices) {
                    picker.index += 1;
                }
            }
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                picker.column = picker.column.other();
                picker.index = current_row(&devices, picker.column);
            }
            KeyCode::Enter => {
                let column = picker.column;
                let choice = picker.choice(&devices);
                self.select_audio_device(column, choice);
                self.close_device_picker();
            }
            KeyCode::Esc => self.close_device_picker(),
            _ => {}
        }
    }

    /// Close the picker without changing anything.
    pub(super) fn close_device_picker(&mut self) {
        self.voice.picker = None;
    }

    /// Adopt a device choice: persist it, mirror it into the displayed
    /// selection, and tell the task — which reopens the device, so a wrong
    /// microphone can be corrected during the very call that exposed it.
    ///
    /// The config write itself happens in the runtime, which drains the command
    /// queue and saves on a command that says it persists; doing it here would
    /// put disk I/O in the state machine.
    pub(super) fn select_audio_device(&mut self, column: DeviceColumn, name: Option<String>) {
        let label = name.clone().unwrap_or_else(|| "system default".to_string());
        let cmd = match column {
            DeviceColumn::Input => {
                self.config.voice_input_device = name.clone();
                self.voice.devices.selected.input = name.clone();
                VoiceCommand::SetInputDevice(name)
            }
            DeviceColumn::Output => {
                self.config.voice_output_device = name.clone();
                self.voice.devices.selected.output = name.clone();
                VoiceCommand::SetOutputDevice(name)
            }
        };
        self.push_voice_log(format!("[VOX] {} = {label}", column.label().to_lowercase()));
        self.outbox.commands.push_back(NetCommand::Voice(cmd));
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
            VoiceEvent::Devices(devices) => self.set_audio_devices(devices),
            VoiceEvent::Sys(line) => self.push_voice_log(line),
        }
    }

    /// Adopt the enumerated device list. The task is the authority on what
    /// exists *and* on what it is actually using, so its selection replaces the
    /// local mirror — a configured name the host no longer has comes back as
    /// whatever the task fell back to, rather than lingering in the HUD.
    fn set_audio_devices(&mut self, devices: AudioDevices) {
        self.voice.devices = devices;
        // Keep the open picker pointing at a row that still exists: the list
        // can shrink between opening it and this arriving.
        if let Some(picker) = self.voice.picker.as_mut() {
            let last = picker.len(&self.voice.devices).saturating_sub(1);
            picker.index = picker.index.min(last);
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

/// The picker row matching what is in use for `column`: 0 for the system
/// default, else the device's position in the list (+1 for that row).
fn current_row(devices: &AudioDevices, column: DeviceColumn) -> usize {
    let (selected, names) = match column {
        DeviceColumn::Input => (devices.selected.input.as_deref(), &devices.inputs),
        DeviceColumn::Output => (devices.selected.output.as_deref(), &devices.outputs),
    };
    selected
        .and_then(|want| names.iter().position(|n| n == want))
        .map_or(0, |i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices() -> AudioDevices {
        AudioDevices {
            inputs: vec!["HDMI".to_string(), "USB PnP Sound Device".to_string()],
            outputs: vec!["Headphones".to_string()],
            default_input: Some("HDMI".to_string()),
            default_output: Some("Headphones".to_string()),
            selected: DevicePrefs::default(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// `d` opens the picker and asks for a fresh enumeration — a headset
    /// plugged in after bring-up has to appear without a restart.
    #[test]
    fn d_opens_the_picker_and_refreshes_the_list() {
        let mut app = App::new();
        app.active = Tool::Voice;
        app.handle_key(key(KeyCode::Char('d')));

        assert!(app.voice.picker.is_some(), "picker open");
        assert!(
            app.outbox
                .commands
                .iter()
                .any(|c| matches!(c, NetCommand::Voice(VoiceCommand::ListDevices))),
            "enumeration requested"
        );
    }

    /// Choosing a device persists it to the config *and* tells the task, and
    /// the command says it needs a config save — the two must not drift, or the
    /// choice survives the call but not the restart.
    #[test]
    fn choosing_an_input_sets_the_config_and_commands_the_task() {
        let mut app = App::new();
        app.voice.devices = devices();
        app.active = Tool::Voice;
        app.handle_key(key(KeyCode::Char('d')));
        app.outbox.commands.clear();

        // Row 0 is "system default"; row 2 is the second enumerated input.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(
            app.config.voice_input_device.as_deref(),
            Some("USB PnP Sound Device")
        );
        let cmd = app
            .outbox
            .commands
            .iter()
            .find(|c| matches!(c, NetCommand::Voice(VoiceCommand::SetInputDevice(_))))
            .expect("task told");
        assert!(cmd.persists_config(), "a device choice must be saved");
        assert!(app.voice.picker.is_none(), "picker closed after choosing");
    }

    /// Row 0 hands the choice back to the host, so clearing a stored device is
    /// a selection like any other rather than a separate key to find.
    #[test]
    fn row_zero_clears_the_stored_device() {
        let mut app = App::new();
        app.voice.devices = devices();
        app.config.voice_input_device = Some("USB PnP Sound Device".to_string());
        app.voice.devices.selected.input = Some("USB PnP Sound Device".to_string());
        app.active = Tool::Voice;
        app.handle_key(key(KeyCode::Char('d')));

        // Opens on the row in use (the second input = row 2), so Enter alone
        // would change nothing; walk back up to the default row.
        assert_eq!(app.voice.picker.as_ref().unwrap().index, 2);
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.config.voice_input_device, None);
    }

    /// Tab swaps direction and re-homes the cursor on that direction's own
    /// selection; Esc leaves everything alone.
    #[test]
    fn tab_switches_direction_and_esc_cancels() {
        let mut app = App::new();
        app.voice.devices = devices();
        app.voice.devices.selected.output = Some("Headphones".to_string());
        app.active = Tool::Voice;
        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Tab));

        let picker = app.voice.picker.as_ref().expect("still open");
        assert_eq!(picker.column, DeviceColumn::Output);
        assert_eq!(picker.index, 1, "homed on the selected output");

        app.handle_key(key(KeyCode::Esc));
        assert!(app.voice.picker.is_none());
        assert_eq!(
            app.config.voice_output_device, None,
            "cancel changes nothing"
        );
    }

    /// The task is the authority on what exists: a shrinking list must not
    /// leave the cursor pointing past the end.
    #[test]
    fn a_shrinking_device_list_clamps_the_cursor() {
        let mut app = App::new();
        app.voice.devices = devices();
        app.active = Tool::Voice;
        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));

        app.apply_voice_event(VoiceEvent::Devices(AudioDevices::default()));

        assert_eq!(app.voice.picker.as_ref().unwrap().index, 0);
    }

    /// The HUD names the device in use, falling back to the host default so a
    /// blank readout never implies "no microphone".
    #[test]
    fn labels_name_the_device_in_use() {
        let mut devices = devices();
        assert_eq!(devices.input_label(), "HDMI (default)");
        devices.selected.input = Some("USB PnP Sound Device".to_string());
        assert_eq!(devices.input_label(), "USB PnP Sound Device");
        assert_eq!(AudioDevices::default().input_label(), "none");
    }
}
