//! Voice bridge: the link between the UI's [`VoiceCommand`]s and the LXST
//! telephony task in `foxhole-voice`.
//!
//! Two jobs, one of which exists whether or not the `voice` feature is on:
//!
//!  * **Address translation.** The Conversations roster dials by LXMF
//!    *destination* hash ([`VoiceCommand::CallPeer`]) because that is the only
//!    hash it holds; LXST addresses an *identity*. Both derive from the same
//!    key pair, so the announce-learned [`PeerCache`] can bridge them —
//!    `dest → public key → Identity::from_public_key().hash` — and that is done
//!    here, in the one place the key cache lives. It needs no audio stack, so it
//!    is compiled unconditionally: an offline build still explains *why* a call
//!    can't be placed rather than silently dropping it.
//!
//!  * **Task ownership.** Under `voice`, [`VoiceLink::spawn`] starts the
//!    telephony task on the transport this crate has already brought up and
//!    keeps the command channel to it. Without the feature the link is inert and
//!    every command is answered with one "offline" notice.

use tokio::sync::mpsc;

use foxhole_core::app::{NetEvent, VoiceCommand, VoiceEvent};
use rns_transport::messages::AnnounceHandlerEvent;

use super::peers::PeerCache;

/// Depth of the UI→telephony command queue. Small on purpose: these are
/// operator keystrokes (call, answer, hang up), so a backlog would mean the
/// telephony task is wedged, and queueing more of them past that point only
/// delays the hangup that matters.
#[cfg(feature = "voice")]
const VOICE_COMMAND_CAPACITY: usize = 16;

/// The link down to the LXST telephony task.
pub(crate) struct VoiceLink {
    /// Command channel to the task; `None` in a build without the `voice`
    /// feature (and after the task has gone away).
    #[cfg(feature = "voice")]
    commands: Option<mpsc::Sender<VoiceCommand>>,
}

impl VoiceLink {
    /// An inert link — what a build without the `voice` feature always has.
    pub(crate) fn offline() -> Self {
        Self {
            #[cfg(feature = "voice")]
            commands: None,
        }
    }

    /// Start the telephony task against the live transport and keep its command
    /// channel. `identity` is loaded separately from the same on-disk file the
    /// LXMF endpoint uses, because `Identity` is not `Clone` and the telephony
    /// service needs its own — the two register different aspects
    /// (`lxmf.delivery` vs `lxst.telephony`) of the same identity, which is
    /// exactly what lets one peer be reached both ways.
    #[cfg(feature = "voice")]
    pub(crate) fn spawn(
        transport: mpsc::Sender<rns_transport::messages::TransportMessage>,
        id_path: &std::path::Path,
        events: mpsc::Sender<NetEvent>,
    ) -> Result<Self, String> {
        let identity = rns_identity::identity::Identity::from_file(id_path)
            .map_err(|e| format!("load identity (voice): {e:?}"))?;
        let (tx, rx) = mpsc::channel::<VoiceCommand>(VOICE_COMMAND_CAPACITY);
        tokio::spawn(foxhole_voice::run(transport, identity, rx, events));
        Ok(Self { commands: Some(tx) })
    }

    /// Hand one command to the telephony task, resolving a
    /// [`VoiceCommand::CallPeer`] to an identity first.
    pub(crate) async fn command(
        &mut self,
        cmd: VoiceCommand,
        peers: &PeerCache,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let cmd = match cmd {
            VoiceCommand::CallPeer(dest) => match resolve_identity(&dest, peers) {
                Some(identity) => VoiceCommand::Call(identity),
                None => {
                    // The peer is in the roster but we've never heard its key —
                    // so we cannot derive the identity LXST needs. Say so
                    // precisely: "call failed" would send the operator hunting
                    // for a network problem that isn't there.
                    vox(
                        events,
                        format!(
                            "[VOX] [WRN] no identity key for {}\u{2026} yet — \
                             wait for an announce, or call from the Voice roster",
                            dest.get(..8).unwrap_or(&dest)
                        ),
                    )
                    .await;
                    return;
                }
            },
            other => other,
        };
        self.dispatch(cmd, events).await;
    }

    /// Push a resolved command down to the task (or report that there isn't one).
    #[cfg(feature = "voice")]
    async fn dispatch(&mut self, cmd: VoiceCommand, events: &mpsc::Sender<NetEvent>) {
        let Some(tx) = &self.commands else {
            vox(events, OFFLINE.to_string()).await;
            return;
        };
        if tx.send(cmd).await.is_err() {
            // The task ended (a fatal bring-up error it already reported). Drop
            // the channel so later commands take the offline path rather than
            // failing one at a time.
            self.commands = None;
            vox(
                events,
                "[VOX] [ERR] telephony task is not running".to_string(),
            )
            .await;
        }
    }

    /// Without the feature there is nothing to dispatch to.
    #[cfg(not(feature = "voice"))]
    async fn dispatch(&mut self, _cmd: VoiceCommand, events: &mpsc::Sender<NetEvent>) {
        vox(events, OFFLINE.to_string()).await;
    }
}

/// What the operator is told when the voice stack isn't in the build.
const OFFLINE: &str = "[VOX] [WRN] voice offline — rebuild with --features voice";

/// Report a display name heard on an `lxmf.delivery` announce against the
/// *identity* behind it, so the Voice roster can label a callable peer.
///
/// LXST's telephony announce carries no app data — there is no display name on
/// that aspect at all — but both aspects hang off one key pair, so the name a
/// peer publishes for messaging is the same peer. This is the only place that
/// correlation is cheap: the announce carries the public key the identity hash
/// derives from.
///
/// Note this is not a roster entry. Hearing an LXMF announce says nothing about
/// whether that node can take a call.
pub(crate) async fn learn_alias(ev: &AnnounceHandlerEvent, events: &mpsc::Sender<NetEvent>) {
    let Some(pk) = ev.public_key else { return };
    let Some(name) = ev
        .app_data
        .as_deref()
        .and_then(lxmf_core::handlers::display_name_from_app_data)
    else {
        return;
    };
    // Peer-supplied text reaching the UI: refuse anything with control
    // characters rather than letting it near the terminal.
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().any(|c| c.is_control()) {
        return;
    }
    let Ok(identity) = rns_identity::identity::Identity::from_public_key(&pk) else {
        return;
    };
    let _ = events
        .send(NetEvent::Voice(VoiceEvent::Alias {
            identity: hex::encode(identity.hash),
            name,
        }))
        .await;
}

/// Resolve an LXMF destination hash (hex) to the owning identity's hash (hex),
/// via the announce-learned public key. `None` when the hash is malformed or we
/// hold no key for it.
fn resolve_identity(dest_hex: &str, peers: &PeerCache) -> Option<String> {
    let dest = super::codec::parse_hash(dest_hex).ok()?;
    let key = peers.key_for(&dest)?;
    let identity = rns_identity::identity::Identity::from_public_key(key).ok()?;
    Some(hex::encode(identity.hash))
}

/// Report a voice-layer line to the UI (Voice call log + the Log tool).
async fn vox(events: &mpsc::Sender<NetEvent>, line: String) {
    let _ = events.send(NetEvent::Voice(VoiceEvent::Sys(line))).await;
}
