//! Voice-call domain model — the vocabulary the UI and the LXST telephony task
//! agree on.
//!
//! Deliberately free of any LXST type. `foxhole-core` must stay buildable
//! without the `voice` feature (and without the rsLXST/audio stack at all), so
//! the profile table below *mirrors* `lxst_core::Profile` rather than
//! re-exporting it, and `foxhole-voice` converts at the boundary. The mirror is
//! kept honest by wire values: [`VoiceProfile::wire_value`] is the same byte LXST
//! puts on the wire, so a drift shows up as a failing round-trip test on both
//! sides rather than as a silently mis-negotiated call.
//!
//! Addressing differs from the rest of FoxHole and that difference is load
//! bearing: LXMF conversations are keyed by an `lxmf.delivery` **destination**
//! hash, but a call is placed to the peer's **identity** hash — LXST derives the
//! `lxst.telephony` destination from it. Both hang off the same identity, so a
//! peer heard on either aspect can be correlated with the other (see
//! `foxhole-voice`'s roster), but the two hashes are not interchangeable and the
//! field names here say which one is meant.

/// One negotiated call quality/latency point. Mirrors `lxst_core::Profile`
/// (same discriminants, same order), so the two convert by wire value.
///
/// The three `Bandwidth*` profiles are Codec2 in LXST; rsLXST's first release
/// only ships Opus, so [`VoiceProfile::is_supported`] marks them unavailable and
/// the profile cycle skips them — the operator never selects a profile that
/// cannot carry audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VoiceProfile {
    /// Codec2 700C — lowest bandwidth (not yet supported by rsLXST).
    BandwidthUltraLow,
    /// Codec2 1600 (not yet supported by rsLXST).
    BandwidthVeryLow,
    /// Codec2 3200 (not yet supported by rsLXST).
    BandwidthLow,
    /// Opus voice @ 24 kHz, 60 ms frames — the default.
    #[default]
    QualityMedium,
    /// Opus voice @ 48 kHz, 60 ms frames.
    QualityHigh,
    /// Opus stereo @ 48 kHz, 60 ms frames.
    QualityMax,
    /// Opus voice @ 24 kHz, 20 ms frames.
    LatencyLow,
    /// Opus voice @ 24 kHz, 10 ms frames.
    LatencyUltraLow,
}

impl VoiceProfile {
    /// LXST's own profile order (what `Profile::ORDER` lists), so cycling here
    /// and cycling in a Python LXST client land on the same sequence.
    pub const ALL: [VoiceProfile; 8] = [
        VoiceProfile::BandwidthUltraLow,
        VoiceProfile::BandwidthVeryLow,
        VoiceProfile::BandwidthLow,
        VoiceProfile::QualityMedium,
        VoiceProfile::QualityHigh,
        VoiceProfile::QualityMax,
        VoiceProfile::LatencyLow,
        VoiceProfile::LatencyUltraLow,
    ];

    /// The byte LXST signals this profile as (`lxst_core::Profile::wire_value`).
    pub fn wire_value(self) -> u32 {
        match self {
            VoiceProfile::BandwidthUltraLow => 0x10,
            VoiceProfile::BandwidthVeryLow => 0x20,
            VoiceProfile::BandwidthLow => 0x30,
            VoiceProfile::QualityMedium => 0x40,
            VoiceProfile::QualityHigh => 0x50,
            VoiceProfile::QualityMax => 0x60,
            VoiceProfile::LatencyUltraLow => 0x70,
            VoiceProfile::LatencyLow => 0x80,
        }
    }

    /// Inverse of [`wire_value`](Self::wire_value); `None` for an unknown byte
    /// (a peer speaking a profile this build doesn't model).
    pub fn from_wire(value: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.wire_value() == value)
    }

    /// Full label, matching LXST's own naming.
    pub fn name(self) -> &'static str {
        match self {
            VoiceProfile::BandwidthUltraLow => "Ultra Low Bandwidth",
            VoiceProfile::BandwidthVeryLow => "Very Low Bandwidth",
            VoiceProfile::BandwidthLow => "Low Bandwidth",
            VoiceProfile::QualityMedium => "Medium Quality",
            VoiceProfile::QualityHigh => "High Quality",
            VoiceProfile::QualityMax => "Super High Quality",
            VoiceProfile::LatencyLow => "Low Latency",
            VoiceProfile::LatencyUltraLow => "Ultra Low Latency",
        }
    }

    /// Short tag for the status chip (LXST's own abbreviation).
    pub fn abbreviation(self) -> &'static str {
        match self {
            VoiceProfile::BandwidthUltraLow => "ULBW",
            VoiceProfile::BandwidthVeryLow => "VLBW",
            VoiceProfile::BandwidthLow => "LBW",
            VoiceProfile::QualityMedium => "MQ",
            VoiceProfile::QualityHigh => "HQ",
            VoiceProfile::QualityMax => "SHQ",
            VoiceProfile::LatencyLow => "LL",
            VoiceProfile::LatencyUltraLow => "ULL",
        }
    }

    /// Whether this build can actually carry audio on the profile. rsLXST's
    /// first release is Opus-only, so the Codec2 profiles are signalling-only.
    pub fn is_supported(self) -> bool {
        !matches!(
            self,
            VoiceProfile::BandwidthUltraLow
                | VoiceProfile::BandwidthVeryLow
                | VoiceProfile::BandwidthLow
        )
    }

    /// Packet cadence in milliseconds — what the operator trades latency for.
    pub fn frame_ms(self) -> u16 {
        match self {
            VoiceProfile::BandwidthUltraLow => 400,
            VoiceProfile::BandwidthVeryLow => 320,
            VoiceProfile::BandwidthLow => 200,
            VoiceProfile::QualityMedium | VoiceProfile::QualityHigh | VoiceProfile::QualityMax => {
                60
            }
            VoiceProfile::LatencyLow => 20,
            VoiceProfile::LatencyUltraLow => 10,
        }
    }

    /// Codec sample rate in Hz. The audio backend resamples the device to this.
    pub fn sample_rate_hz(self) -> u32 {
        match self {
            VoiceProfile::BandwidthUltraLow
            | VoiceProfile::BandwidthVeryLow
            | VoiceProfile::BandwidthLow => 8_000,
            VoiceProfile::QualityMedium
            | VoiceProfile::LatencyLow
            | VoiceProfile::LatencyUltraLow => 24_000,
            VoiceProfile::QualityHigh | VoiceProfile::QualityMax => 48_000,
        }
    }

    /// Channel count the codec expects (only `QualityMax` is stereo).
    pub fn channels(self) -> u8 {
        match self {
            VoiceProfile::QualityMax => 2,
            _ => 1,
        }
    }

    /// Nominal codec ceiling in bits per second — the headline number for
    /// "will this fit down the link", shown next to the profile.
    pub fn bitrate_ceiling(self) -> u32 {
        match self {
            VoiceProfile::BandwidthUltraLow => 700,
            VoiceProfile::BandwidthVeryLow => 1_600,
            VoiceProfile::BandwidthLow => 3_200,
            VoiceProfile::QualityMedium
            | VoiceProfile::LatencyLow
            | VoiceProfile::LatencyUltraLow => 8_000,
            VoiceProfile::QualityHigh => 16_000,
            VoiceProfile::QualityMax => 32_000,
        }
    }

    /// Next profile the operator can actually use: LXST's cycle order with the
    /// unsupported (Codec2) entries skipped. Falls back to `self` in the
    /// impossible case that nothing is supported, so the key is never a no-op
    /// that leaves the caller looping.
    pub fn next_supported(self) -> Self {
        let start = Self::ALL.iter().position(|&p| p == self).unwrap_or(0);
        for step in 1..=Self::ALL.len() {
            let candidate = Self::ALL[(start + step) % Self::ALL.len()];
            if candidate.is_supported() {
                return candidate;
            }
        }
        self
    }
}

/// Who placed the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallDirection {
    /// A peer is calling us.
    Incoming,
    /// We are calling a peer.
    Outgoing,
}

impl CallDirection {
    /// One-char roster/HUD marker: `<` inbound, `>` outbound.
    pub fn glyph(self) -> char {
        match self {
            CallDirection::Incoming => '<',
            CallDirection::Outgoing => '>',
        }
    }
}

/// Where a call is in its lifecycle. A superset of LXST's signalling statuses,
/// with the pre-signalling `Discovering` step (path/announce resolution, which
/// runs before there is any link to signal on) made visible — on a mesh it can
/// take seconds and the operator should see *why* nothing is ringing yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPhase {
    /// Resolving a path to the peer's `lxst.telephony` destination.
    Discovering,
    /// Link up, ringing at the far end (outgoing).
    Calling,
    /// Ringing here (incoming), awaiting the operator's answer.
    Ringing,
    /// Answered; media streams being brought up.
    Connecting,
    /// Media flowing.
    Established,
    /// Teardown in flight.
    Ending,
}

impl CallPhase {
    /// Status-chip label.
    pub fn label(self) -> &'static str {
        match self {
            CallPhase::Discovering => "DISCOVERING",
            CallPhase::Calling => "CALLING",
            CallPhase::Ringing => "RINGING",
            CallPhase::Connecting => "CONNECTING",
            CallPhase::Established => "ESTABLISHED",
            CallPhase::Ending => "ENDING",
        }
    }

    /// Whether media can flow in this phase — what gates the mute key, the VU
    /// meters, and the call timer.
    pub fn is_live(self) -> bool {
        matches!(self, CallPhase::Established)
    }
}

/// The call the terminal currently has (at most one — LXST telephony is a
/// single-line runtime, and `TelephonyRuntimeCore` enforces that).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// Peer's hex **identity** hash (not an LXMF destination hash).
    pub peer: String,
    /// Peer's display name when the roster knows one.
    pub name: Option<String>,
    pub direction: CallDirection,
    pub phase: CallPhase,
    /// Negotiated profile, once signalling has settled on one.
    pub profile: Option<VoiceProfile>,
    /// When the call was placed/received (Unix epoch **seconds, UTC**).
    pub started_at: u64,
    /// When media came up (Unix epoch **seconds, UTC**); `None` until then. The
    /// call timer counts from here, so a long ring doesn't inflate talk time.
    pub connected_at: Option<u64>,
}

impl Call {
    /// A fresh call in its opening phase.
    pub fn new(peer: String, name: Option<String>, direction: CallDirection, at: u64) -> Self {
        let phase = match direction {
            CallDirection::Incoming => CallPhase::Ringing,
            CallDirection::Outgoing => CallPhase::Discovering,
        };
        Self {
            peer,
            name,
            direction,
            phase,
            profile: None,
            started_at: at,
            connected_at: None,
        }
    }

    /// What to show for the peer: its name when known, else a shortened hash.
    pub fn label(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => n.clone(),
            _ => format!("{}\u{2026}", super::short_hash(&self.peer)),
        }
    }

    /// Seconds of connected talk time as of `now` (0 before media came up).
    pub fn talk_secs(&self, now: u64) -> u64 {
        self.connected_at
            .map(|start| now.saturating_sub(start))
            .unwrap_or(0)
    }
}

/// A peer heard announcing an `lxst.telephony` destination — i.e. someone who
/// can actually take a call. Keyed by hex identity hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoicePeer {
    /// Hex **identity** hash — what [`VoiceCommand::Call`] addresses.
    pub identity: String,
    /// Announced/correlated display name, if any.
    pub name: Option<String>,
    /// Announced hop count, when the transport reported one.
    pub hops: Option<u8>,
    /// Last announce (Unix epoch **seconds, UTC**).
    pub last_seen: u64,
}

impl VoicePeer {
    /// What to show in the roster: the name, else a shortened identity hash.
    pub fn label(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => n.clone(),
            _ => format!("{}\u{2026}", super::short_hash(&self.identity)),
        }
    }
}

/// Whether the platform audio backend came up, and why not when it didn't.
/// A call still signals and connects without audio devices (useful on a headless
/// relay), so this is reported rather than fatal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AudioStatus {
    /// No voice stack in this build (no `voice` feature), or it hasn't reported.
    #[default]
    Unknown,
    /// Capture and playback both opened.
    Ready,
    /// Devices unavailable — the reason as the backend reported it. Calls still
    /// signal; there is simply no audio in or out.
    Unavailable(String),
}

/// A command from the UI down to the LXST telephony task. Carried inside
/// [`NetCommand::Voice`](super::NetCommand::Voice) so voice reuses the one
/// UI→network channel rather than opening a second one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceCommand {
    /// Place a call to this hex **identity** hash (a Voice-tool roster entry,
    /// which is keyed by identity).
    Call(String),
    /// Place a call to the peer behind this hex LXMF **destination** hash — how
    /// the Conversations roster dials, since that is the only hash it knows.
    /// The telephony task resolves it to an identity against the announce-learned
    /// key cache, and reports back if it cannot.
    CallPeer(String),
    /// Answer the ringing incoming call.
    Answer,
    /// Hang up (or reject a ringing call / abandon an outgoing one).
    Hangup,
    /// Stop sending microphone audio (`true`) or resume (`false`). The stream
    /// keeps running and sends silence, so packet cadence and the negotiated
    /// profile survive a mute.
    SetMuted(bool),
    /// Renegotiate the active call onto another profile.
    SetProfile(VoiceProfile),
    /// Re-announce our `lxst.telephony` destination now, so peers learn a path.
    Announce,
}

/// An event from the LXST telephony task up to the UI. Carried inside
/// [`NetEvent::Voice`](super::NetEvent::Voice).
#[derive(Clone, Debug, PartialEq)]
pub enum VoiceEvent {
    /// A peer announced an `lxst.telephony` destination (upsert by identity).
    Peer {
        identity: String,
        name: Option<String>,
        hops: Option<u8>,
    },
    /// Our own hex identity hash — what a peer dials to reach us.
    Local(String),
    /// The call state changed wholesale. `None` means there is no call: the
    /// task is the single authority on that, so the UI never infers idleness.
    Call(Option<Call>),
    /// The call ended; the string is a human-readable reason for the log.
    Ended(String),
    /// Audio-backend readiness, reported once at bring-up.
    Audio(AudioStatus),
    /// Live signal levels, 0–100, for the transmit and receive VU meters.
    Levels { tx: u8, rx: u8 },
    /// A voice-layer log line (already `[VOX]`-tagged) for the Log tool.
    Sys(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_wire_values_round_trip() {
        // The mirror of `lxst_core::Profile` is only safe if every variant maps
        // to a distinct byte and back — a drift here mis-negotiates a call.
        for p in VoiceProfile::ALL {
            assert_eq!(VoiceProfile::from_wire(p.wire_value()), Some(p));
        }
        assert_eq!(VoiceProfile::from_wire(0x00), None);
        assert_eq!(VoiceProfile::from_wire(0x41), None);
    }

    #[test]
    fn profile_wire_values_match_lxst() {
        // Spot-check against LXST's own table, including the deliberate
        // non-monotonicity: LatencyUltraLow is 0x70 and LatencyLow is 0x80,
        // even though the cycle order puts LatencyLow first.
        assert_eq!(VoiceProfile::QualityMedium.wire_value(), 0x40);
        assert_eq!(VoiceProfile::LatencyUltraLow.wire_value(), 0x70);
        assert_eq!(VoiceProfile::LatencyLow.wire_value(), 0x80);
    }

    #[test]
    fn profile_cycle_skips_unsupported_codec2() {
        // Cycling must never land the operator on a profile that cannot carry
        // audio in this build.
        let mut p = VoiceProfile::default();
        for _ in 0..VoiceProfile::ALL.len() * 2 {
            p = p.next_supported();
            assert!(p.is_supported(), "cycled onto unsupported {p:?}");
        }
        // And it must reach every supported profile rather than sticking.
        let mut seen = vec![VoiceProfile::default()];
        let mut p = VoiceProfile::default();
        for _ in 0..VoiceProfile::ALL.len() {
            p = p.next_supported();
            if !seen.contains(&p) {
                seen.push(p);
            }
        }
        assert_eq!(
            seen.len(),
            VoiceProfile::ALL
                .iter()
                .filter(|p| p.is_supported())
                .count()
        );
    }

    #[test]
    fn default_profile_is_supported_opus() {
        let p = VoiceProfile::default();
        assert!(p.is_supported());
        assert_eq!(p, VoiceProfile::QualityMedium);
        assert_eq!(p.sample_rate_hz(), 24_000);
        assert_eq!(p.channels(), 1);
    }

    #[test]
    fn talk_time_counts_from_connect_not_from_ring() {
        let mut call = Call::new("ab".repeat(16), None, CallDirection::Outgoing, 100);
        // Still ringing: no talk time however long it has been.
        assert_eq!(call.talk_secs(160), 0);
        call.connected_at = Some(150);
        assert_eq!(call.talk_secs(160), 10);
        // A clock that went backwards must not underflow.
        assert_eq!(call.talk_secs(140), 0);
    }

    #[test]
    fn call_label_falls_back_to_short_hash() {
        let hash = "ab".repeat(16);
        let call = Call::new(hash.clone(), None, CallDirection::Incoming, 0);
        assert!(call.label().starts_with("abab"));
        assert_eq!(call.phase, CallPhase::Ringing);

        let named = Call::new(hash, Some("bravo".into()), CallDirection::Outgoing, 0);
        assert_eq!(named.label(), "bravo");
        assert_eq!(named.phase, CallPhase::Discovering);
    }
}
