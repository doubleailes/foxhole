//! The no-backend stand-in, used when the `audio` feature is off.
//!
//! Mirrors `cpal_backend`'s API exactly so the telephony task carries no `cfg`s
//! of its own: it opens devices, is told none exist, and reports
//! `AudioStatus::Unavailable` — the same path a machine with no sound card
//! takes. Calls still signal, connect and tear down; they simply carry no
//! audio.
//!
//! This is not only a build-plumbing convenience. It is a configuration worth
//! having: a headless node that should be reachable and answerable without
//! pulling a platform audio stack in at all.

use lxst_core::{Profile, RawAudioFrame};
use tokio::sync::mpsc;

/// What every entry point here reports. Phrased to distinguish "this build has
/// no audio support" from "this machine has no usable device" — otherwise an
/// operator would go hunting for a missing microphone that was never the issue.
const NO_BACKEND: &str = "built without audio support (rebuild with --features voice)";

/// Stand-in for a microphone. Never constructed — [`open_capture`] always fails
/// — so its methods exist only to satisfy the shared API.
pub(crate) struct Capture;

impl Capture {
    pub(crate) fn set_muted(&self, _muted: bool) {}

    pub(crate) fn level(&self) -> u8 {
        0
    }
}

/// Stand-in for a speaker. Never constructed; see [`Capture`].
pub(crate) struct Playback;

impl Playback {
    pub(crate) fn play(&mut self, _frame: &RawAudioFrame) {}

    pub(crate) fn level(&self) -> u8 {
        0
    }
}

pub(crate) fn probe() -> Result<(), String> {
    Err(NO_BACKEND.to_string())
}

pub(crate) fn open_capture(
    _profile: Profile,
) -> Result<(Capture, mpsc::Receiver<RawAudioFrame>), String> {
    Err(NO_BACKEND.to_string())
}

pub(crate) fn open_playback(_profile: Profile) -> Result<Playback, String> {
    Err(NO_BACKEND.to_string())
}
