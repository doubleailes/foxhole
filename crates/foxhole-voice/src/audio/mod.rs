//! Platform audio: microphone capture and speaker playback, converted to and
//! from the `RawAudioFrame`s rsLXST's Opus streams take.
//!
//! rsLXST draws its boundary here deliberately — "applications still own
//! capture/playback and resampling into `RawAudioFrame`" — so everything below
//! is FoxHole's, and it is the only part of the voice feature that touches a
//! device.
//!
//! **The device backend is behind the `audio` feature, and that is load
//! bearing for the whole workspace.** `cpal`'s Linux backend links ALSA through
//! `alsa-sys`, which needs `libasound2-dev` at build time — and because
//! `cargo build/test --workspace` builds *every* member regardless of the
//! binary's features, an unconditional `cpal` here would make that system
//! package a hard prerequisite for the dependency-light offline build too. It
//! is not: without the feature this module resolves to [`silent`], the pure
//! conversion helpers below stay compiled and tested, and voice degrades to the
//! signalling-only mode it already models as `AudioStatus::Unavailable` — the
//! headless-relay configuration the docs describe.
//!
//! What lives where:
//!
//!  * here — the device-free conversion maths ([`Resampler`], [`to_mono`],
//!    [`peak_level`]), unit-tested in every configuration;
//!  * `cpal_backend` — the real devices, `#[cfg(feature = "audio")]`;
//!  * `silent` — the same API, reporting that no backend was built.
//!
//! Two things make the backend more than a pair of `cpal` streams, and both are
//! documented there: callback discipline (a `cpal` data callback runs on a
//! realtime audio thread, so it must not block or allocate unboundedly) and
//! thread ownership (`cpal::Stream` is `!Send` on some hosts).
//!
//! A missing or unusable device is reported, never fatal: a call still signals
//! and connects with no audio path.

// The conversion maths is device-free, so it compiles and is tested whether or
// not a backend is built. Without one nothing calls it, hence the allow.
#![cfg_attr(not(feature = "audio"), allow(dead_code))]

#[cfg(feature = "audio")]
mod cpal_backend;
#[cfg(not(feature = "audio"))]
mod silent;

#[cfg(feature = "audio")]
pub(crate) use cpal_backend::{Capture, Playback, open_capture, open_playback, probe};
#[cfg(not(feature = "audio"))]
pub(crate) use silent::{Capture, Playback, open_capture, open_playback, probe};

use foxhole_core::app::AudioStatus;

/// Startup readiness of each direction, kept apart because the telephony task
/// opens them independently and runs with either one alone.
pub(crate) struct AudioProbe {
    /// Microphone readiness, or why not.
    pub(crate) capture: Result<(), String>,
    /// Speaker readiness, or why not.
    pub(crate) playback: Result<(), String>,
}

impl AudioProbe {
    /// Collapse to the status the UI shows — but only where the directions
    /// really do agree.
    pub(crate) fn into_status(self) -> AudioStatus {
        match (self.capture, self.playback) {
            (Ok(()), Ok(())) => AudioStatus::Ready,
            (Ok(()), Err(why)) => AudioStatus::TransmitOnly(why),
            (Err(why), Ok(())) => AudioStatus::ReceiveOnly(why),
            // Both gone: lead with the capture reason. They are usually the
            // same underlying cause (no card, no backend), and one is enough.
            (Err(capture), Err(_)) => AudioStatus::Unavailable(capture),
        }
    }
}

/// Linear-interpolation resampler for a single mono stream.
///
/// Voice-band linear interpolation is not audiophile resampling, but it is
/// cheap, allocation-free, and streams across callback boundaries — which is
/// what matters when the alternative is a resampling crate in the hot path of a
/// realtime thread. It carries the fractional read position and the previous
/// sample between calls, so consecutive chunks join without a click.
pub(crate) struct Resampler {
    /// Input samples consumed per output sample (`from / to`).
    step: f64,
    /// Fractional position within the segment from `prev` to the next input.
    pos: f64,
    /// Last input sample seen, the left end of the current segment.
    prev: f32,
}

impl Resampler {
    /// A resampler from `from_hz` to `to_hz`. Both must be non-zero; a zero rate
    /// (which a device should never report) degrades to pass-through rather
    /// than dividing by zero on the audio thread.
    pub(crate) fn new(from_hz: u32, to_hz: u32) -> Self {
        let step = if from_hz == 0 || to_hz == 0 {
            1.0
        } else {
            f64::from(from_hz) / f64::from(to_hz)
        };
        Self {
            step,
            pos: 0.0,
            prev: 0.0,
        }
    }

    /// Resample `input`, appending to `out`.
    pub(crate) fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &sample in input {
            // Each input advances virtual time by exactly 1; output samples are
            // spaced `step` apart, so emit every output landing in [prev, sample).
            while self.pos < 1.0 {
                let t = self.pos as f32;
                out.push(self.prev + (sample - self.prev) * t);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = sample;
        }
    }
}

/// Fold an interleaved device buffer down to one mono sample per frame.
/// Averaging (rather than taking channel 0) keeps a microphone wired to only the
/// right channel from coming through silent.
pub(crate) fn to_mono(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    for frame in interleaved.chunks_exact(channels) {
        out.push(frame.iter().sum::<f32>() / channels as f32);
    }
}

/// Peak level of a mono buffer as 0–100, for the VU meters. Peak rather than
/// RMS: a meter is being read at a glance to answer "is my microphone live and
/// am I clipping", and RMS lags both.
pub(crate) fn peak_level(samples: &[f32]) -> u8 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    (peak.clamp(0.0, 1.0) * 100.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_halves_rate() {
        // 48 kHz -> 24 kHz: one output per two inputs.
        let mut r = Resampler::new(48_000, 24_000);
        let mut out = Vec::new();
        r.process(&[0.0; 100], &mut out);
        assert_eq!(out.len(), 50);
    }

    #[test]
    fn resampler_doubles_rate() {
        let mut r = Resampler::new(24_000, 48_000);
        let mut out = Vec::new();
        r.process(&[0.0; 100], &mut out);
        assert_eq!(out.len(), 200);
    }

    #[test]
    fn resampler_streams_across_chunks_without_drift() {
        // The whole point of carrying `pos`/`prev` is that chunking the input
        // must not change the output count — otherwise the packet cadence
        // slowly slides against the device clock over a long call.
        let mut whole = Resampler::new(44_100, 24_000);
        let mut a = Vec::new();
        whole.process(&[0.5; 4410], &mut a);

        let mut chunked = Resampler::new(44_100, 24_000);
        let mut b = Vec::new();
        for chunk in [0.5f32; 4410].chunks(441) {
            chunked.process(chunk, &mut b);
        }
        assert_eq!(a.len(), b.len());
        assert_eq!(a, b);
    }

    #[test]
    fn resampler_interpolates_between_samples() {
        // Upsampling a step must produce the midpoint, not a repeat — that is
        // the difference between interpolation and nearest-neighbour.
        let mut r = Resampler::new(1_000, 2_000);
        let mut out = Vec::new();
        r.process(&[1.0], &mut out);
        // From prev=0.0 toward 1.0: emits 0.0 then 0.5.
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn resampler_survives_a_zero_rate() {
        // A device reporting 0 Hz would otherwise divide by zero on the audio
        // thread; degrade to pass-through instead.
        let mut r = Resampler::new(0, 48_000);
        let mut out = Vec::new();
        r.process(&[0.25; 10], &mut out);
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn mono_fold_averages_channels() {
        let mut out = Vec::new();
        to_mono(&[1.0, 0.0, 0.5, 0.5], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5]);

        // A one-channel device passes straight through.
        let mut mono = Vec::new();
        to_mono(&[0.1, 0.2], 1, &mut mono);
        assert_eq!(mono, vec![0.1, 0.2]);
    }

    #[test]
    fn mono_fold_keeps_a_single_hot_channel_audible() {
        // Taking channel 0 instead of averaging would render this silent.
        let mut out = Vec::new();
        to_mono(&[0.0, 1.0], 2, &mut out);
        assert_eq!(out, vec![0.5]);
    }

    #[test]
    fn peak_level_survives_non_finite_input() {
        // The capture path clamps before metering, but the meter must not
        // produce a nonsense reading if anything slips past: NaN compares
        // false against everything, which is how a max-fold silently keeps 0.
        assert_eq!(peak_level(&[f32::NAN]), 0);
        assert_eq!(peak_level(&[f32::INFINITY]), 100);
        assert_eq!(peak_level(&[f32::NEG_INFINITY]), 100);
    }

    #[test]
    fn peak_level_scales_and_clamps() {
        assert_eq!(peak_level(&[]), 0);
        assert_eq!(peak_level(&[0.0, 0.0]), 0);
        assert_eq!(peak_level(&[0.5, -0.25]), 50);
        // Negative peaks count, and over-unity input clamps rather than wrapping.
        assert_eq!(peak_level(&[-1.0]), 100);
        assert_eq!(peak_level(&[4.0]), 100);
    }
}
