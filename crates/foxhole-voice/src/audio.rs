//! Platform audio: microphone capture and speaker playback, converted to and
//! from the `RawAudioFrame`s rsLXST's Opus streams take.
//!
//! rsLXST draws its boundary here deliberately — "applications still own
//! capture/playback and resampling into `RawAudioFrame`" — so everything below
//! is FoxHole's, and it is the only part of the voice feature that touches a
//! device.
//!
//! Three things make this more than a pair of `cpal` streams:
//!
//!  * **Rate and channel conversion.** The negotiated LXST profile fixes the
//!    codec's sample rate (8/24/48 kHz) and channel count; the device offers
//!    whatever it offers. Both directions therefore run through [`Resampler`]
//!    and a mono fold, rather than assuming a 48 kHz stereo device.
//!  * **Callback discipline.** A `cpal` data callback runs on a realtime audio
//!    thread: it must not allocate unboundedly, block, or panic. Capture does
//!    its conversion into a buffer it owns and hands whole frames off with
//!    `try_send` (dropping, never blocking, if the consumer stalls); playback
//!    drains a bounded ring and pads with silence on underrun.
//!  * **Thread ownership.** `cpal::Stream` is `!Send` on some hosts, so each
//!    stream lives on its own `std::thread` that parks until its handle drops.
//!    Dropping the handle is what stops the device — there is no separate stop
//!    call to forget.
//!
//! A missing or unusable device is reported, never fatal: a call still signals
//! and connects with no audio path, which is what a headless relay wants.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use lxst_core::{Profile, RawAudioFrame};
use tokio::sync::mpsc;

/// How many captured frames may sit in the queue to the telephony task before
/// the audio callback starts dropping them. Small on purpose: a backlog here is
/// latency the far end hears, so if the task cannot keep up it is better to lose
/// a packet than to grow a delay that never recovers. `try_send` drops the frame
/// being offered — the newest — which costs one packet rather than resetting the
/// stream's continuity.
const CAPTURE_QUEUE_FRAMES: usize = 8;

/// Playback ring ceiling, in milliseconds of audio at the profile rate. Acts as
/// the jitter budget: below it, arriving frames absorb network jitter; above it
/// the oldest audio is discarded rather than letting one slow patch add
/// permanent delay to the rest of the call.
const PLAYBACK_MAX_MS: usize = 400;

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

/// A running audio device. Dropping it stops the stream and joins its thread.
struct DeviceThread {
    /// Dropped to signal the stream thread to tear down; the thread is blocked
    /// on the matching receiver.
    _stop: std::sync::mpsc::Sender<()>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DeviceThread {
    fn drop(&mut self) {
        // `_stop` is dropped with the struct, which wakes the thread's `recv`.
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Live microphone capture for one call. Dropping it closes the device.
pub(crate) struct Capture {
    _thread: DeviceThread,
    /// Set by the UI's mute key. Read on the audio thread each callback, which
    /// then emits silence — keeping the packet cadence and the negotiated
    /// profile intact, so unmuting resumes instantly instead of renegotiating.
    muted: Arc<AtomicBool>,
    /// Most recent capture peak, 0–100, published for the TX meter.
    level: Arc<AtomicU8>,
}

impl Capture {
    /// Mute or unmute the microphone.
    pub(crate) fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Latest transmit level, 0–100.
    pub(crate) fn level(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }
}

/// Live speaker playback for one call. Dropping it closes the device.
pub(crate) struct Playback {
    _thread: DeviceThread,
    /// Device-rate, device-channel interleaved samples awaiting the callback.
    ring: Arc<Mutex<VecDeque<f32>>>,
    /// Ceiling on `ring`, in samples — the jitter budget in concrete terms.
    capacity: usize,
    /// Conversion state for the feed side (profile rate → device rate).
    resampler: Resampler,
    device_channels: usize,
    /// Most recent playback peak, 0–100, published for the RX meter.
    level: Arc<AtomicU8>,
}

impl Playback {
    /// Queue one decoded frame for the speaker, converting it to the device's
    /// rate and channel count. The frame's peak is published through
    /// [`Playback::level`] for the RX meter.
    ///
    /// Over-full is handled by discarding the *oldest* audio: the newest is what
    /// the far end just said, and keeping it is what stops a jitter spike from
    /// becoming a permanent delay for the rest of the call.
    pub(crate) fn play(&mut self, frame: &RawAudioFrame) {
        // Frames arrive interleaved at the profile's channel count; fold to mono
        // first so one conversion path covers every profile/device pairing.
        let mut mono = Vec::with_capacity(frame.sample_frames());
        to_mono(
            &frame.samples,
            usize::from(frame.channels.max(1)),
            &mut mono,
        );
        let level = peak_level(&mono);
        self.level.store(level, Ordering::Relaxed);

        let mut resampled = Vec::with_capacity(mono.len() * 2);
        self.resampler.process(&mono, &mut resampled);

        if let Ok(mut ring) = self.ring.lock() {
            for s in resampled {
                // Duplicate mono across the device's channels.
                for _ in 0..self.device_channels {
                    ring.push_back(s);
                }
            }
            let overflow = ring.len().saturating_sub(self.capacity);
            if overflow > 0 {
                ring.drain(..overflow);
            }
        }
    }

    /// Latest receive level, 0–100.
    pub(crate) fn level(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }
}

/// Check that both a microphone and a speaker exist and report a usable default
/// configuration, without opening either.
///
/// Deliberately non-invasive: it answers "will a call have audio?" at startup
/// without holding a capture stream open on an idle terminal, which is both a
/// privacy question and a courtesy to whatever else wants the device.
pub(crate) fn probe() -> Result<(), String> {
    let host = cpal::default_host();
    host.default_output_device()
        .ok_or_else(|| "no output device".to_string())?
        .default_output_config()
        .map_err(|e| format!("output config: {e}"))?;
    host.default_input_device()
        .ok_or_else(|| "no input device".to_string())?
        .default_input_config()
        .map_err(|e| format!("input config: {e}"))?;
    Ok(())
}

/// Open the microphone for `profile`, returning the capture handle and the
/// stream of frames to hand to `TelephonyControl::StartOpusStream`.
pub(crate) fn open_capture(
    profile: Profile,
) -> Result<(Capture, mpsc::Receiver<RawAudioFrame>), String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no input device".to_string())?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("input config: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let device_rate = config.sample_rate.0;
    let device_channels = usize::from(config.channels).max(1);
    let profile_channels = usize::from(profile.channels()).max(1);
    // One packet's worth of *mono* samples at the codec rate.
    let frame_samples = profile.sample_frames_per_packet().max(1);

    let (tx, rx) = mpsc::channel::<RawAudioFrame>(CAPTURE_QUEUE_FRAMES);
    let muted = Arc::new(AtomicBool::new(false));
    let level = Arc::new(AtomicU8::new(0));

    // Everything the callback owns, moved onto the stream thread.
    let cb = CaptureState {
        resampler: Resampler::new(device_rate, profile.sample_rate_hz()),
        mono: Vec::with_capacity(2048),
        pending: Vec::with_capacity(frame_samples * 2),
        frame_samples,
        device_channels,
        profile_channels,
        tx,
        muted: muted.clone(),
        level: level.clone(),
    };

    let thread = spawn_stream("capture", move || match format {
        SampleFormat::I16 => build_input::<i16>(&device, &config, cb),
        SampleFormat::U16 => build_input::<u16>(&device, &config, cb),
        SampleFormat::F32 => build_input::<f32>(&device, &config, cb),
        other => Err(format!("unsupported input sample format {other}")),
    })?;

    Ok((
        Capture {
            _thread: thread,
            muted,
            level,
        },
        rx,
    ))
}

/// Open the speaker for `profile`.
pub(crate) fn open_playback(profile: Profile) -> Result<Playback, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no output device".to_string())?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("output config: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let device_rate = config.sample_rate.0;
    let device_channels = usize::from(config.channels).max(1);
    let capacity = (device_rate as usize / 1000) * PLAYBACK_MAX_MS * device_channels;

    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(capacity)));
    let level = Arc::new(AtomicU8::new(0));
    let cb_ring = ring.clone();

    let thread = spawn_stream("playback", move || match format {
        SampleFormat::I16 => build_output::<i16>(&device, &config, cb_ring),
        SampleFormat::U16 => build_output::<u16>(&device, &config, cb_ring),
        SampleFormat::F32 => build_output::<f32>(&device, &config, cb_ring),
        other => Err(format!("unsupported output sample format {other}")),
    })?;

    Ok(Playback {
        _thread: thread,
        ring,
        capacity,
        resampler: Resampler::new(profile.sample_rate_hz(), device_rate),
        device_channels,
        level,
    })
}

/// Everything the capture callback owns and mutates between invocations.
struct CaptureState {
    resampler: Resampler,
    /// Scratch for the device-channel fold; reused so the callback never
    /// allocates in steady state.
    mono: Vec<f32>,
    /// Codec-rate samples accumulated toward the next whole packet.
    pending: Vec<f32>,
    frame_samples: usize,
    device_channels: usize,
    profile_channels: usize,
    tx: mpsc::Sender<RawAudioFrame>,
    muted: Arc<AtomicBool>,
    level: Arc<AtomicU8>,
}

impl CaptureState {
    /// Convert one device buffer and emit any whole packets it completes.
    fn feed(&mut self, input: &[f32]) {
        self.mono.clear();
        to_mono(input, self.device_channels, &mut self.mono);

        if self.muted.load(Ordering::Relaxed) {
            // Keep the cadence: same sample count, no signal. The far end hears
            // silence rather than a stalled stream, and the meter reads 0.
            self.mono.iter_mut().for_each(|s| *s = 0.0);
            self.level.store(0, Ordering::Relaxed);
        } else {
            self.level.store(peak_level(&self.mono), Ordering::Relaxed);
        }

        let mono = std::mem::take(&mut self.mono);
        self.resampler.process(&mono, &mut self.pending);
        self.mono = mono;

        while self.pending.len() >= self.frame_samples {
            let packet: Vec<f32> = self.pending.drain(..self.frame_samples).collect();
            // Interleave up to the codec's channel count (only the stereo
            // profile needs it) by duplicating the mono capture.
            let samples = if self.profile_channels == 1 {
                packet
            } else {
                let mut out = Vec::with_capacity(packet.len() * self.profile_channels);
                for s in packet {
                    for _ in 0..self.profile_channels {
                        out.push(s);
                    }
                }
                out
            };
            let Ok(frame) = RawAudioFrame::new(self.profile_channels as u8, samples) else {
                // Only a channel/length mismatch can land here, which this code
                // constructs; drop rather than risk a panic on the audio thread.
                continue;
            };
            // Never block an audio callback: if the consumer is behind, the
            // freshest audio still gets through on the next packet.
            let _ = self.tx.try_send(frame);
        }
    }
}

/// Build a typed input stream that funnels into [`CaptureState`].
fn build_input<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut state: CaptureState,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut scratch: Vec<f32> = Vec::with_capacity(4096);
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| {
                scratch.clear();
                scratch.extend(data.iter().map(|s| f32::from_sample(*s)));
                state.feed(&scratch);
            },
            |err| {
                // Nothing useful to do from the audio thread; the operator sees
                // the call itself fail if the device really is gone.
                let _ = err;
            },
            None,
        )
        .map_err(|e| format!("build input stream: {e}"))
}

/// Build a typed output stream fed from the playback ring.
fn build_output<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    ring: Arc<Mutex<VecDeque<f32>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                let mut guard = ring.lock().ok();
                for slot in data.iter_mut() {
                    let sample = guard
                        .as_mut()
                        .and_then(|r| r.pop_front())
                        // Underrun (or a poisoned lock): silence, never stale
                        // audio and never a stalled device.
                        .unwrap_or(0.0);
                    *slot = T::from_sample(sample);
                }
            },
            |err| {
                let _ = err;
            },
            None,
        )
        .map_err(|e| format!("build output stream: {e}"))
}

/// Run `build` on a dedicated thread, keep the resulting stream alive there, and
/// hand back a handle whose drop tears it down.
///
/// The stream never leaves that thread: `cpal::Stream` is `!Send` on some hosts,
/// so building it inside the thread is what keeps this portable rather than
/// working only where the backend happens to be `Send`.
fn spawn_stream<F>(what: &'static str, build: F) -> Result<DeviceThread, String>
where
    F: FnOnce() -> Result<cpal::Stream, String> + Send + 'static,
{
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    let join = std::thread::Builder::new()
        .name(format!("foxhole-voice-{what}"))
        .spawn(move || {
            let stream = match build() {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(format!("start stream: {e}")));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            // Park until the handle drops (the sender goes with it, so `recv`
            // returns `Err`), then drop the stream and close the device.
            let _ = stop_rx.recv();
        })
        .map_err(|e| format!("spawn {what} thread: {e}"))?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(DeviceThread {
            _stop: stop_tx,
            join: Some(join),
        }),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            Err(format!("{what} thread died during setup"))
        }
    }
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
    fn peak_level_scales_and_clamps() {
        assert_eq!(peak_level(&[]), 0);
        assert_eq!(peak_level(&[0.0, 0.0]), 0);
        assert_eq!(peak_level(&[0.5, -0.25]), 50);
        // Negative peaks count, and over-unity input clamps rather than wrapping.
        assert_eq!(peak_level(&[-1.0]), 100);
        assert_eq!(peak_level(&[4.0]), 100);
    }
}
