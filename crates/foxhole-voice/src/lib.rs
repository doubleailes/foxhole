//! Live LXST voice calls over Reticulum (compiled only under the `voice`
//! feature).
//!
//! This crate is to voice what `foxhole-net`'s `net` module is to messaging: it
//! owns one async task that brings up the telephony service, then runs a single
//! `select!` loop translating between FoxHole's [`VoiceCommand`]/[`VoiceEvent`]
//! vocabulary and rsLXST's `TelephonyControl`/`TelephonyServiceEvent`.
//!
//! It deliberately sits on rsLXST's *recommended* seam — `TelephonyService`,
//! driven by typed control and event channels — rather than the lower-level
//! endpoint/runtime types. rsLXST marks those as implementation-level SPI: call
//! state, caller policy, destination registration, announce discovery, link
//! ownership, timeouts and teardown all live behind the service, and
//! reimplementing any of that here would be re-deriving call state from raw
//! Reticulum traffic, which is exactly what the upstream API notes warn against.
//!
//! **It shares `foxhole-net`'s transport rather than standing up its own.** The
//! peer we message and the peer we call are the same node on the same
//! interfaces; a second Reticulum instance would announce a second set of paths
//! for it and double the mesh traffic. So `foxhole-net` hands us its
//! `transport_tx` and a second handle on the same on-disk identity, and we
//! register the `lxst.telephony` aspect of it alongside its `lxmf.delivery` one.
//!
//! Two things are ours, not rsLXST's, because it draws its boundary above them:
//! the platform audio devices ([`audio`]) and the roster of who can be called.

mod audio;

use std::collections::HashMap;
use std::time::Duration;

use lxst_core::{Profile, RawAudioFrame, SignallingStatus};
use lxst_telephony::{
    IdentityHash, LinkId, TELEPHONY_ANNOUNCE_INTERVAL, TelephonyControl, TelephonyService,
    TelephonyServiceEvent, request_answer, telephony_destination_hash,
};
use rns_identity::identity::Identity;
use rns_transport::messages::{AnnounceHandlerEvent, TransportMessage};
use tokio::sync::mpsc;

use foxhole_core::app::{
    AudioStatus, Call, CallDirection, CallPhase, NetEvent, VoiceCommand, VoiceEvent, VoiceProfile,
};

use audio::{Capture, Playback};

/// The destination aspect LXST telephony announces and listens on.
const TELEPHONY_ASPECT: &str = "lxst.telephony";

/// How often the VU meters are refreshed. 100 ms is fast enough to read as
/// live and slow enough that a full call costs a few hundred UI events, not
/// one per audio packet.
const LEVEL_INTERVAL: Duration = Duration::from_millis(100);

/// How long to let outgoing discovery (path/announce resolution) run before
/// giving up. Generous by LAN standards because a mesh path can legitimately
/// take seconds to resolve over several hops.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Depth of the announce-handler channel for `lxst.telephony`.
const ANNOUNCE_CAPACITY: usize = 64;

/// Entry point spawned by `foxhole-net` once the transport is up. Runs until the
/// command channel closes or bring-up fails; either way it reports through
/// `events` so the operator sees what happened in the Voice tool and the Log.
pub async fn run(
    transport: mpsc::Sender<TransportMessage>,
    identity: Identity,
    commands: mpsc::Receiver<VoiceCommand>,
    events: mpsc::Sender<NetEvent>,
) {
    if let Err(e) = run_inner(transport, identity, commands, &events).await {
        vox(&events, format!("[VOX] [ERR] voice: {e}")).await;
    }
}

async fn run_inner(
    transport: mpsc::Sender<TransportMessage>,
    identity: Identity,
    mut commands: mpsc::Receiver<VoiceCommand>,
    events: &mpsc::Sender<NetEvent>,
) -> Result<(), String> {
    let local = hex::encode(identity.hash);
    let _ = events
        .send(NetEvent::Voice(VoiceEvent::Local(local.clone())))
        .await;

    // Discover who else can take a call. LXST peers are found on their own
    // aspect, not on `lxmf.delivery`: a node that runs FoxHole without `voice`
    // (or Sideband, or lxmd) announces the latter and cannot be called, so
    // listing it as callable would be a roster full of numbers that never ring.
    let mut announces = register_announces(&transport).await?;

    let parts = TelephonyService::registered(transport, &identity)
        .map_err(|e| format!("register lxst.telephony: {e}"))?;
    let control = parts.control_tx.clone();
    let mut service_events = parts.event_rx;
    // Kept, not detached. The service decodes inbound audio on this task, and
    // a malformed or wider-than-negotiated packet can panic inside the Opus
    // decoder (docs/lxst-voice.md §9) — a fault a peer can induce. Holding the
    // handle turns that from a silently dead voice stack, whose next command
    // would simply time out, into something the operator is told about while
    // the messaging terminal carries on.
    let mut service = tokio::spawn(parts.service.run());

    vox(
        events,
        format!(
            "[VOX] {TELEPHONY_ASPECT} {} registered",
            hex::encode(telephony_destination_hash(&identity.hash))
        ),
    )
    .await;

    let mut session = Session::new(local);
    session.report_audio(events).await;

    let mut level_tick = tokio::time::interval(LEVEL_INTERVAL);
    level_tick.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            cmd = commands.recv() => match cmd {
                Some(cmd) => session.command(cmd, &control, events).await,
                // The UI (or the whole net task) went away — tear the line down
                // rather than leaving a peer talking to nobody.
                None => {
                    let _ = control.send(TelephonyControl::Shutdown).await;
                    return Ok(());
                }
            },
            Some(ev) = service_events.recv() => {
                if session.service_event(ev, &control, events).await.is_break() {
                    return Ok(());
                }
            }
            Some(announce) = announces.recv() => session.announce(&announce, events).await,
            joined = &mut service => {
                session.service_died(joined.err(), events).await;
                return Ok(());
            }
            _ = level_tick.tick() => {
                session.report_faults(events).await;
                session.report_levels(events).await;
            }
        }
    }
}

/// Everything the loop carries between events: the call as rsLXST last reported
/// it, the audio devices bound to it, and the roster correlation table.
struct Session {
    /// Our own hex identity hash, so a self-call can be refused with an
    /// explanation instead of dialling a destination we ourselves registered.
    local: String,
    /// Mirror of the call, in FoxHole's vocabulary. rsLXST remains the
    /// authority; this is what we last published to the UI.
    call: Option<Call>,
    /// Link id of the call awaiting an answer — `request_answer` needs the exact
    /// link, and rsLXST validates it to make sure we answer the call we were
    /// shown rather than whatever arrived in the meantime.
    pending_link: Option<LinkId>,
    /// Profile the operator has selected for the next call.
    profile: VoiceProfile,
    muted: bool,
    /// Devices, held only for the duration of a call so the microphone is not
    /// open while idle.
    capture: Option<Capture>,
    playback: Option<Playback>,
    /// The profile the current media path was opened for, or `None` when no
    /// media is up. Compared against each snapshot so a mid-call renegotiation
    /// reopens the devices: rsLXST stops the old streams on a profile switch,
    /// and a capture still producing at the previous rate would feed a stream
    /// that no longer exists — a call that looks connected and is silent.
    streaming: Option<VoiceProfile>,
    /// Audio-backend readiness, probed once at startup.
    audio: AudioStatus,
    /// Announce-learned names by hex identity hash.
    names: HashMap<String, String>,
    /// Last levels published, so an idle call doesn't emit a redundant event
    /// ten times a second.
    last_levels: (u8, u8),
}

/// Whether the loop should keep running after an event.
enum Flow {
    Continue,
    Break,
}

impl Flow {
    fn is_break(&self) -> bool {
        matches!(self, Flow::Break)
    }
}

impl Session {
    fn new(local: String) -> Self {
        // Probe at startup rather than at call time: an operator needs to find
        // out their microphone is missing before someone calls, not while the
        // phone is ringing. The probe only queries the devices and their default
        // configurations — it deliberately does not *open* the microphone, so
        // idle FoxHole never holds a capture stream open.
        //
        // The two directions are reported separately because they genuinely are
        // separate: `start_media` opens capture and playback independently and
        // runs happily with one of them, so collapsing "no microphone" into
        // "no audio" would tell an operator with working speakers that a call
        // carries nothing when it would in fact be receive-only.
        let audio = audio::probe().into_status();
        Self {
            local,
            call: None,
            pending_link: None,
            profile: VoiceProfile::default(),
            muted: false,
            capture: None,
            playback: None,
            streaming: None,
            audio,
            names: HashMap::new(),
            last_levels: (0, 0),
        }
    }

    async fn report_audio(&self, events: &mpsc::Sender<NetEvent>) {
        let _ = events
            .send(NetEvent::Voice(VoiceEvent::Audio(self.audio.clone())))
            .await;
    }

    /// Act on one UI command.
    async fn command(
        &mut self,
        cmd: VoiceCommand,
        control: &mpsc::Sender<TelephonyControl>,
        events: &mpsc::Sender<NetEvent>,
    ) {
        match cmd {
            // Resolved to an identity by `foxhole-net` before it reaches us;
            // if one slips through unresolved, treat it the same as `Call`.
            VoiceCommand::Call(identity) | VoiceCommand::CallPeer(identity) => {
                self.place(identity, control, events).await
            }
            VoiceCommand::Answer => self.answer(control, events).await,
            VoiceCommand::Hangup => {
                let _ = control
                    .send(TelephonyControl::Hangup {
                        ring_timeout: false,
                    })
                    .await;
            }
            VoiceCommand::SetMuted(muted) => {
                self.muted = muted;
                if let Some(capture) = &self.capture {
                    capture.set_muted(muted);
                }
            }
            VoiceCommand::SetProfile(profile) => {
                self.profile = profile;
                // Only an established call can renegotiate; otherwise the choice
                // simply applies to the next one placed.
                if self.streaming.is_some() {
                    let _ = control
                        .send(TelephonyControl::SwitchProfile {
                            profile: to_lxst(profile),
                        })
                        .await;
                }
            }
            VoiceCommand::Announce => {
                let _ = control.send(TelephonyControl::Announce).await;
                vox(
                    events,
                    format!(
                        "[VOX] announced {TELEPHONY_ASPECT} (peers re-announce every {} min)",
                        TELEPHONY_ANNOUNCE_INTERVAL.as_secs() / 60
                    ),
                )
                .await;
            }
        }
    }

    /// Place an outgoing call.
    async fn place(
        &mut self,
        identity: String,
        control: &mpsc::Sender<TelephonyControl>,
        events: &mpsc::Sender<NetEvent>,
    ) {
        if identity == self.local {
            vox(
                events,
                "[VOX] [WRN] that is this node's own identity".to_string(),
            )
            .await;
            return;
        }
        let Some(remote) = parse_identity(&identity) else {
            vox(
                events,
                format!("[VOX] [WRN] not a valid identity hash: {identity}"),
            )
            .await;
            return;
        };
        // Publish the dialling state immediately. Discovery can take seconds on
        // a multi-hop mesh, and rsLXST runs it asynchronously, so without this
        // the UI would sit blank until the first snapshot arrives.
        self.publish(
            Some(Call::new(
                identity.clone(),
                self.names.get(&identity).cloned(),
                CallDirection::Outgoing,
                now_secs(),
            )),
            events,
        )
        .await;
        let _ = control
            .send(TelephonyControl::Call {
                remote_identity: remote,
                profile: Some(to_lxst(self.profile)),
                discovery_timeout: DISCOVERY_TIMEOUT,
            })
            .await;
    }

    /// Answer the ringing call, on the exact link we were told about.
    async fn answer(
        &mut self,
        control: &mpsc::Sender<TelephonyControl>,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let Some(link) = self.pending_link else {
            vox(events, "[VOX] [WRN] nothing ringing".to_string()).await;
            return;
        };
        // `request_answer` round-trips: it resolves once rsLXST has validated
        // the link, admitted its signalling and published the authoritative
        // CONNECTING snapshot — so a failure here is a real answer failure,
        // not a race with the far end.
        match request_answer(control, link).await {
            Ok(_) => {}
            Err(e) => {
                vox(events, format!("[VOX] [WRN] answer failed: {e}")).await;
                self.pending_link = None;
            }
        }
    }

    /// Fold one rsLXST service event into our state.
    async fn service_event(
        &mut self,
        ev: TelephonyServiceEvent,
        control: &mpsc::Sender<TelephonyControl>,
        events: &mpsc::Sender<NetEvent>,
    ) -> Flow {
        match ev {
            // The snapshot is rsLXST's authoritative view of the line, so it —
            // not the discrete events — is what drives our phase.
            TelephonyServiceEvent::Snapshot(snapshot) => {
                self.apply_snapshot(snapshot.active_call, control, events)
                    .await;
            }
            TelephonyServiceEvent::IncomingCall {
                link_id,
                remote_identity,
            } => {
                self.pending_link = Some(link_id);
                let identity = hex::encode(remote_identity);
                self.publish(
                    Some(Call::new(
                        identity.clone(),
                        self.names.get(&identity).cloned(),
                        CallDirection::Incoming,
                        now_secs(),
                    )),
                    events,
                )
                .await;
            }
            TelephonyServiceEvent::OutgoingCallStarted { link_id, .. } => {
                self.pending_link = Some(link_id);
            }
            TelephonyServiceEvent::OutgoingCallFailed {
                remote_identity,
                message,
            } => {
                let who = self.label(&hex::encode(remote_identity));
                self.end(format!("{who}: {message}"), events).await;
            }
            TelephonyServiceEvent::CallTerminated { reason, .. } => {
                self.end(terminated_reason(reason), events).await;
            }
            TelephonyServiceEvent::OpusFramesReceived { frames, .. } => {
                self.play(&frames);
            }
            TelephonyServiceEvent::OpusReceiveStreamStopped { reason, .. } => {
                // The sink closing mid-call means playback died under us; say so
                // rather than leaving a silent call that looks healthy.
                vox(events, format!("[VOX] receive stream stopped ({reason:?})")).await;
            }
            TelephonyServiceEvent::OpusTransmitStreamStopped { reason, .. } => {
                vox(
                    events,
                    format!("[VOX] transmit stream stopped ({reason:?})"),
                )
                .await;
            }
            TelephonyServiceEvent::Error { message } => {
                vox(events, format!("[VOX] [ERR] {message}")).await;
            }
            TelephonyServiceEvent::Stopped => {
                // Tear the line down before leaving. This task is detached, so
                // nothing else will publish terminal events on its behalf, and
                // a UI left holding a call it can never end would refuse the
                // next one as "line busy" for the rest of the session.
                if self.call.is_some() {
                    self.end("telephony service stopped".to_string(), events)
                        .await;
                }
                self.stop_media();
                vox(events, "[VOX] telephony service stopped".to_string()).await;
                return Flow::Break;
            }
            // Media counters and stream-start notices: the VU meters and the
            // phase readout already carry this, so they'd only be log noise.
            _ => {}
        }
        Flow::Continue
    }

    /// Adopt rsLXST's call snapshot, bringing media up or down as it changes.
    async fn apply_snapshot(
        &mut self,
        active: Option<lxst_telephony::ActiveCallSnapshot>,
        control: &mpsc::Sender<TelephonyControl>,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let Some(active) = active else {
            // No active call. `CallTerminated` normally gets here first; this
            // catches the paths that clear the line without one.
            if self.call.is_some() {
                self.end("cleared".to_string(), events).await;
            }
            return;
        };

        let identity = hex::encode(active.remote_identity);
        let phase = phase_of(active.status, active.answered);
        let profile = active.profile.and_then(from_lxst);

        let mut call = self.call.clone().unwrap_or_else(|| {
            Call::new(
                identity.clone(),
                self.names.get(&identity).cloned(),
                direction_of(active.role),
                now_secs(),
            )
        });
        call.peer = identity;
        call.direction = direction_of(active.role);
        call.phase = phase;
        call.profile = profile;
        if call.name.is_none() {
            call.name = self.names.get(&call.peer).cloned();
        }
        if phase.is_live() && call.connected_at.is_none() {
            call.connected_at = Some(now_secs());
        }

        // Adopt what was actually negotiated, so the *next* outgoing call
        // proposes it too. Without this the UI shows the negotiated profile
        // while the task keeps proposing whatever `SetProfile` last set —
        // after an incoming call, those are routinely different.
        if let Some(p) = profile {
            self.profile = p;
        }

        let open_for = self.streaming;
        self.publish(Some(call), events).await;

        // Bring media up on the transition into Established — the profile is
        // only final at that point — and again if a renegotiation lands on a
        // different one, since rsLXST stops the old streams when it switches.
        if phase.is_live() {
            let negotiated = profile.unwrap_or(self.profile);
            if open_for != Some(negotiated) {
                if open_for.is_some() {
                    vox(
                        events,
                        format!("[VOX] profile changed to {}", negotiated.abbreviation()),
                    )
                    .await;
                    self.stop_media();
                }
                self.start_media(negotiated, control, events).await;
            }
        }
    }

    /// Open the devices and hand rsLXST both ends of the audio path.
    async fn start_media(
        &mut self,
        profile: VoiceProfile,
        control: &mpsc::Sender<TelephonyControl>,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let lxst_profile = to_lxst(profile);

        match audio::open_playback(lxst_profile) {
            Ok(playback) => self.playback = Some(playback),
            Err(e) => vox(events, format!("[VOX] [WRN] no speaker ({e})")).await,
        }

        // Note there is no `StartOpusReceiveStream` here. rsLXST emits
        // `OpusFramesReceived` with the decoded audio *unconditionally*, and a
        // registered receive stream is an additional delivery path fed from the
        // same decode — so registering one would clone every frame into a
        // channel this task would then have to drain and discard. We own
        // `Playback` in the select loop already, so we take the frames from the
        // event and skip the second copy.

        match audio::open_capture(lxst_profile) {
            Ok((capture, frames)) => {
                capture.set_muted(self.muted);
                self.capture = Some(capture);
                let _ = control
                    .send(TelephonyControl::StartOpusStream {
                        profile: lxst_profile,
                        frames,
                    })
                    .await;
            }
            Err(e) => {
                vox(
                    events,
                    format!("[VOX] [WRN] no microphone ({e}) — receive only"),
                )
                .await;
            }
        }

        self.streaming = Some(profile);
        vox(
            events,
            format!(
                "[VOX] media up: {} ({} Hz, {} ms)",
                profile.abbreviation(),
                profile.sample_rate_hz(),
                profile.frame_ms()
            ),
        )
        .await;
    }

    /// Close the devices. rsLXST stops its own streams on call end, so this only
    /// has to release the hardware — leaving the microphone open between calls
    /// is both a privacy problem and a way to hold the device from other apps.
    fn stop_media(&mut self) {
        self.capture = None;
        self.playback = None;
        self.streaming = None;
    }

    /// Push decoded audio to the speaker and update the receive meter.
    fn play(&mut self, frames: &[RawAudioFrame]) {
        let Some(playback) = &mut self.playback else {
            return;
        };
        for frame in frames {
            playback.play(frame);
        }
    }

    /// Publish the call state to the UI.
    async fn publish(&mut self, call: Option<Call>, events: &mpsc::Sender<NetEvent>) {
        if self.call == call {
            return;
        }
        self.call = call.clone();
        let _ = events.send(NetEvent::Voice(VoiceEvent::Call(call))).await;
    }

    /// Clear the line: close devices, drop the call, tell the UI why.
    async fn end(&mut self, reason: String, events: &mpsc::Sender<NetEvent>) {
        self.stop_media();
        self.pending_link = None;
        self.muted = false;
        self.last_levels = (0, 0);
        if self.call.take().is_some() {
            let _ = events.send(NetEvent::Voice(VoiceEvent::Call(None))).await;
        }
        let _ = events
            .send(NetEvent::Voice(VoiceEvent::Ended(reason)))
            .await;
    }

    /// The telephony service task ended — cleanly, or by panicking inside the
    /// codec on a packet from the far end. Either way the line is gone: close
    /// the devices, clear the call so the UI does not hold one it can never
    /// end, and say plainly what happened.
    async fn service_died(
        &mut self,
        err: Option<tokio::task::JoinError>,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let reason = match &err {
            // A panic is upstream's, not the operator's; name it as a fault in
            // the voice stack rather than as something they did, and point at
            // where the detail was written.
            Some(e) if e.is_panic() => {
                "voice stack faulted (see panic.log) — calls unavailable until restart".to_string()
            }
            Some(_) => "voice stack cancelled".to_string(),
            None => "voice stack stopped".to_string(),
        };
        if self.call.is_some() {
            self.end(reason.clone(), events).await;
        }
        self.stop_media();
        vox(events, format!("[VOX] [ERR] {reason}")).await;
    }

    /// Surface a device that failed mid-call. The realtime callbacks cannot
    /// report anything themselves, so they park the first error in a slot and
    /// this poll drains it — otherwise an unplugged microphone gives a call
    /// that is established, silent, and unexplained.
    async fn report_faults(&mut self, events: &mpsc::Sender<NetEvent>) {
        let faults: Vec<String> = [
            self.capture.as_ref().and_then(|c| c.take_fault()),
            self.playback.as_ref().and_then(|p| p.take_fault()),
        ]
        .into_iter()
        .flatten()
        .collect();
        for fault in faults {
            vox(events, format!("[VOX] [WRN] audio device failed — {fault}")).await;
        }
    }

    /// Publish the VU meters, but only when they actually moved.
    async fn report_levels(&mut self, events: &mpsc::Sender<NetEvent>) {
        let tx = self.capture.as_ref().map(|c| c.level()).unwrap_or(0);
        let rx = self.playback.as_ref().map(|p| p.level()).unwrap_or(0);
        if (tx, rx) == self.last_levels {
            return;
        }
        self.last_levels = (tx, rx);
        let _ = events
            .send(NetEvent::Voice(VoiceEvent::Levels { tx, rx }))
            .await;
    }

    /// Fold an `lxst.telephony` announce into the roster.
    async fn announce(&mut self, ev: &AnnounceHandlerEvent, events: &mpsc::Sender<NetEvent>) {
        // The announce names a *destination*; the identity behind it is what a
        // call addresses, and it is recoverable from the announced public key.
        let Some(pk) = ev.public_key else { return };
        let Ok(identity) = Identity::from_public_key(&pk) else {
            return;
        };
        // Guard against a destination that merely claims the aspect: only trust
        // it if it is genuinely derived from the announced key.
        if telephony_destination_hash(&identity.hash) != ev.destination_hash {
            return;
        }
        let hex_identity = hex::encode(identity.hash);
        if hex_identity == self.local {
            return; // our own announce, echoed back
        }
        // No name here on purpose: LXST's telephony announce carries no app
        // data, so anything in that field is another client's private
        // convention — not something to render as a peer's name. The Voice
        // roster gets its labels from `lxmf.delivery` announces instead, which
        // `foxhole-net` correlates onto the same identity.
        let _ = events
            .send(NetEvent::Voice(VoiceEvent::Peer {
                identity: hex_identity,
                name: None,
                hops: Some(ev.hops),
            }))
            .await;
    }

    /// Display label for a hex identity hash.
    fn label(&self, identity: &str) -> String {
        self.names
            .get(identity)
            .cloned()
            .unwrap_or_else(|| format!("{}\u{2026}", identity.get(..8).unwrap_or(identity)))
    }
}

/// Subscribe to `lxst.telephony` announces so the Voice roster fills itself.
async fn register_announces(
    transport: &mpsc::Sender<TransportMessage>,
) -> Result<mpsc::Receiver<AnnounceHandlerEvent>, String> {
    let (tx, rx) = mpsc::channel::<AnnounceHandlerEvent>(ANNOUNCE_CAPACITY);
    transport
        .send(TransportMessage::RegisterAnnounceHandler {
            aspect_filter: Some(TELEPHONY_ASPECT.to_string()),
            receive_path_responses: true,
            callback_tx: tx,
        })
        .await
        .map_err(|_| "transport closed".to_string())?;
    Ok(rx)
}

/// FoxHole profile → LXST profile.
fn to_lxst(profile: VoiceProfile) -> Profile {
    Profile::from_wire(profile.wire_value()).unwrap_or(Profile::DEFAULT)
}

/// LXST profile → FoxHole profile (`None` for one we don't model).
fn from_lxst(profile: Profile) -> Option<VoiceProfile> {
    VoiceProfile::from_wire(profile.wire_value())
}

/// LXST signalling status → the phase the HUD shows.
fn phase_of(status: SignallingStatus, answered: bool) -> CallPhase {
    match status {
        SignallingStatus::Calling | SignallingStatus::Available => CallPhase::Calling,
        // A ringing call we have already answered is on its way up, not still
        // asking to be picked up — showing RINGING there would have the operator
        // pressing Enter at a call they have answered.
        SignallingStatus::Ringing if answered => CallPhase::Connecting,
        SignallingStatus::Ringing => CallPhase::Ringing,
        SignallingStatus::Connecting => CallPhase::Connecting,
        SignallingStatus::Established => CallPhase::Established,
        SignallingStatus::Busy | SignallingStatus::Rejected => CallPhase::Ending,
    }
}

/// LXST call role → who placed the call. The names line up, but LXST's
/// `Incoming` describes the *call* while ours describes the direction, so the
/// mapping is spelled out rather than assumed.
fn direction_of(role: lxst_core::CallRole) -> CallDirection {
    match role {
        lxst_core::CallRole::Incoming => CallDirection::Incoming,
        lxst_core::CallRole::Outgoing => CallDirection::Outgoing,
    }
}

/// Human-readable reason for a terminated call.
fn terminated_reason(reason: Option<SignallingStatus>) -> String {
    match reason {
        Some(SignallingStatus::Busy) => "peer busy".to_string(),
        Some(SignallingStatus::Rejected) => "rejected".to_string(),
        Some(other) => format!("{other:?}").to_lowercase(),
        None => "hung up".to_string(),
    }
}

/// Parse a hex identity hash into the 16 bytes LXST addresses.
fn parse_identity(hex_hash: &str) -> Option<IdentityHash> {
    let bytes = hex::decode(hex_hash.trim()).ok()?;
    <[u8; 16]>::try_from(bytes.as_slice()).ok()
}

/// Current Unix time in whole seconds (UTC).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Report a voice-layer line to the UI.
async fn vox(events: &mpsc::Sender<NetEvent>, line: String) {
    let _ = events.send(NetEvent::Voice(VoiceEvent::Sys(line))).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_round_trip_through_lxst() {
        // The whole point of mirroring `Profile` in `foxhole-core` is that the
        // two agree; if a variant ever fails to survive the round trip, a call
        // would be negotiated on a profile the UI is not showing.
        for p in VoiceProfile::ALL {
            assert_eq!(from_lxst(to_lxst(p)), Some(p), "{p:?} did not round-trip");
        }
    }

    #[test]
    fn every_lxst_profile_maps_back() {
        for p in Profile::ORDER {
            assert!(from_lxst(p).is_some(), "{p:?} has no FoxHole equivalent");
        }
    }

    #[test]
    fn answered_ringing_reads_as_connecting() {
        // rsLXST keeps reporting RINGING briefly after an answer; showing that
        // verbatim would invite the operator to answer a call twice.
        assert_eq!(
            phase_of(SignallingStatus::Ringing, false),
            CallPhase::Ringing
        );
        assert_eq!(
            phase_of(SignallingStatus::Ringing, true),
            CallPhase::Connecting
        );
    }

    #[test]
    fn refused_statuses_end_the_call() {
        assert_eq!(phase_of(SignallingStatus::Busy, false), CallPhase::Ending);
        assert_eq!(
            phase_of(SignallingStatus::Rejected, false),
            CallPhase::Ending
        );
        assert_eq!(
            phase_of(SignallingStatus::Established, true),
            CallPhase::Established
        );
    }

    #[test]
    fn identity_parsing_rejects_wrong_lengths() {
        assert!(parse_identity(&"ab".repeat(16)).is_some());
        // Trailing whitespace is tolerated (it comes from operator input paths).
        assert!(parse_identity(&format!(" {} ", "ab".repeat(16))).is_some());
        assert!(parse_identity("abcd").is_none());
        assert!(parse_identity(&"ab".repeat(17)).is_none());
        assert!(parse_identity("zz".repeat(16).as_str()).is_none());
    }

    #[test]
    fn terminated_reasons_are_human_readable() {
        assert_eq!(terminated_reason(None), "hung up");
        assert_eq!(terminated_reason(Some(SignallingStatus::Busy)), "peer busy");
        assert_eq!(
            terminated_reason(Some(SignallingStatus::Rejected)),
            "rejected"
        );
    }
}
