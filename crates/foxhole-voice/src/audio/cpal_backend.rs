//! The `cpal` device backend: real microphone capture and speaker playback.
//!
//! Built only under the `audio` feature — see the module docs in `mod.rs` for
//! why that gate exists rather than depending on `cpal` unconditionally.
//!
//! Two constraints shape everything here:
//!
//!  * **Callback discipline.** A `cpal` data callback runs on a realtime audio
//!    thread: it must not allocate unboundedly, block, or panic. Capture does
//!    its conversion into buffers it owns and hands whole frames off with
//!    `try_send` (dropping, never blocking, if the consumer stalls); playback
//!    drains a bounded ring and pads with silence on underrun.
//!  * **Thread ownership.** `cpal::Stream` is `!Send` on some hosts, so each
//!    stream lives on its own `std::thread` that parks until its handle drops.
//!    Dropping the handle is what stops the device — there is no separate stop
//!    call to forget, and it is why the microphone is open only during a call.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use lxst_core::{Profile, RawAudioFrame};
use tokio::sync::mpsc;

use super::{Resampler, peak_level, to_mono};

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

/// Silences file descriptor 2 for as long as it is alive, restoring it on drop.
///
/// ALSA's C library writes its diagnostics straight to stderr, bypassing every
/// Rust logging path — and on a machine with no sound card, an unusual
/// `.asoundrc`, or inside a container, opening the default device produces
/// half a screen of them. FoxHole runs full-screen on the alternate buffer, so
/// that output lands directly on top of the console and corrupts the display,
/// with no redraw to clean it up. Losing those lines is the right trade: they
/// describe a condition the operator is already told about, in the Voice tool's
/// audio status, in terms that mean something.
///
/// Wrapped around the device calls only, not installed process-wide, so nothing
/// else FoxHole writes is affected. It does briefly redirect a process-global
/// descriptor, but the window is one device open and the TUI writes to stdout,
/// never stderr. On non-Unix this is a no-op — WASAPI and CoreAudio do not do
/// this.
struct QuietStderr {
    /// The saved descriptor to restore, if the redirect took.
    #[cfg(unix)]
    saved: Option<libc::c_int>,
}

impl QuietStderr {
    #[cfg(unix)]
    fn new() -> Self {
        // Every step is best-effort: failing to mute stderr must never stop a
        // call from being placed.
        let saved = unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved < 0 {
                return Self { saved: None };
            }
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if null < 0 {
                libc::close(saved);
                return Self { saved: None };
            }
            libc::dup2(null, libc::STDERR_FILENO);
            libc::close(null);
            Some(saved)
        };
        Self { saved }
    }

    #[cfg(not(unix))]
    fn new() -> Self {
        Self {}
    }
}

impl Drop for QuietStderr {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(saved) = self.saved.take() {
            unsafe {
                libc::dup2(saved, libc::STDERR_FILENO);
                libc::close(saved);
            }
        }
    }
}
/// A running audio device. Dropping it stops the stream and joins its thread.
struct DeviceThread {
    /// Closing this is what tells the stream thread to tear down — it is parked
    /// on the matching receiver, which only returns once every sender is gone.
    ///
    /// `Option` so [`Drop`] can *take* it: struct fields are dropped after the
    /// `Drop::drop` body runs, so simply holding a `Sender` here and joining in
    /// the body would park the joiner on a thread that is itself waiting for
    /// that very sender to close. See the regression test at the bottom.
    stop: Option<std::sync::mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DeviceThread {
    fn drop(&mut self) {
        // Order matters: close the channel *first* so the thread's `recv`
        // returns, and only then wait for it.
        drop(self.stop.take());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// A slot the realtime error callback can drop a message into without
/// blocking, for the telephony task to pick up on its next poll.
///
/// A device that dies mid-call (unplugged, backend restarted) otherwise leaves
/// a call that is established, silent, and gives the operator no reason —
/// indistinguishable from a peer who simply stopped talking. Same rationale as
/// the mesh stack's delivery-proof tracing: an unreported failure is worse than
/// a noisy one.
pub(crate) type FaultSlot = Arc<Mutex<Option<String>>>;

/// Record the first fault only. Later errors on a dying device are usually the
/// same cause repeating, and the first one is the diagnostic.
fn record_fault(slot: &FaultSlot, what: &str, err: cpal::StreamError) {
    if let Ok(mut guard) = slot.lock()
        && guard.is_none()
    {
        *guard = Some(format!("{what}: {err}"));
    }
}

/// Live microphone capture for one call. Dropping it closes the device.
pub(crate) struct Capture {
    _thread: DeviceThread,
    /// First runtime stream error, if the device has failed.
    fault: FaultSlot,
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

    /// Take the first runtime stream error, if the microphone has failed.
    pub(crate) fn take_fault(&self) -> Option<String> {
        self.fault.lock().ok()?.take()
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
    /// First runtime stream error, if the device has failed.
    fault: FaultSlot,
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

        // Interleave to the device's channel count *before* taking the lock.
        // The output callback contends for this mutex on a realtime thread, so
        // everything that can happen outside the critical section must: what is
        // left is one extend and a bounded drain.
        let mut interleaved = Vec::with_capacity(resampled.len() * self.device_channels);
        for s in resampled {
            for _ in 0..self.device_channels {
                interleaved.push(s);
            }
        }

        if let Ok(mut ring) = self.ring.lock() {
            ring.extend(interleaved);
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

    /// Take the first runtime stream error, if the speaker has failed.
    pub(crate) fn take_fault(&self) -> Option<String> {
        self.fault.lock().ok()?.take()
    }
}

/// Check whether a microphone and a speaker exist and report a usable default
/// configuration, without opening either.
///
/// Deliberately non-invasive: it answers "will a call have audio?" at startup
/// without holding a capture stream open on an idle terminal, which is both a
/// privacy question and a courtesy to whatever else wants the device.
///
/// Each direction is reported on its own — one missing device does not stop the
/// other from carrying a call.
pub(crate) fn probe() -> super::AudioProbe {
    let _quiet = QuietStderr::new();
    let host = cpal::default_host();
    super::AudioProbe {
        capture: host
            .default_input_device()
            .ok_or_else(|| "no input device".to_string())
            .and_then(|d| {
                d.default_input_config()
                    .map(|_| ())
                    .map_err(|e| format!("input config: {e}"))
            }),
        playback: host
            .default_output_device()
            .ok_or_else(|| "no output device".to_string())
            .and_then(|d| {
                d.default_output_config()
                    .map(|_| ())
                    .map_err(|e| format!("output config: {e}"))
            }),
    }
}

/// Open the microphone for `profile`, returning the capture handle and the
/// stream of frames to hand to `TelephonyControl::StartOpusStream`.
pub(crate) fn open_capture(
    profile: Profile,
) -> Result<(Capture, mpsc::Receiver<RawAudioFrame>), String> {
    // Held across `spawn_stream` too — it blocks until the stream thread has
    // built and started the device, so the guard covers that work as well.
    let _quiet = QuietStderr::new();
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
    let fault: FaultSlot = Arc::new(Mutex::new(None));
    let cb_fault = fault.clone();

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
        SampleFormat::I16 => build_input::<i16>(&device, &config, cb, cb_fault),
        SampleFormat::U16 => build_input::<u16>(&device, &config, cb, cb_fault),
        SampleFormat::F32 => build_input::<f32>(&device, &config, cb, cb_fault),
        other => Err(format!("unsupported input sample format {other}")),
    })?;

    Ok((
        Capture {
            _thread: thread,
            fault,
            muted,
            level,
        },
        rx,
    ))
}

/// Open the speaker for `profile`.
pub(crate) fn open_playback(profile: Profile) -> Result<Playback, String> {
    let _quiet = QuietStderr::new();
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
    let fault: FaultSlot = Arc::new(Mutex::new(None));
    let cb_ring = ring.clone();
    let cb_fault = fault.clone();

    let thread = spawn_stream("playback", move || match format {
        SampleFormat::I16 => build_output::<i16>(&device, &config, cb_ring, cb_fault),
        SampleFormat::U16 => build_output::<u16>(&device, &config, cb_ring, cb_fault),
        SampleFormat::F32 => build_output::<f32>(&device, &config, cb_ring, cb_fault),
        other => Err(format!("unsupported output sample format {other}")),
    })?;

    Ok(Playback {
        _thread: thread,
        ring,
        capacity,
        resampler: Resampler::new(profile.sample_rate_hz(), device_rate),
        device_channels,
        level,
        fault,
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

        // Opus is specified on normalised samples, and a host is not obliged to
        // hand us any: a boosted input, a loopback device, or a glitching driver
        // can deliver magnitudes past 1.0 or a stray NaN. Clamping here means
        // the codec only ever sees what it is defined for, and costs one pass
        // over a buffer we have just touched anyway.
        for s in self.mono.iter_mut() {
            *s = if s.is_finite() {
                s.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }

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
    fault: FaultSlot,
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
            move |err| record_fault(&fault, "microphone", err),
            None,
        )
        .map_err(|e| format!("build input stream: {e}"))
}

/// Build a typed output stream fed from the playback ring.
fn build_output<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    ring: Arc<Mutex<VecDeque<f32>>>,
    fault: FaultSlot,
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
            move |err| record_fault(&fault, "speaker", err),
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
            stop: Some(stop_tx),
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
    fn device_thread_drop_does_not_deadlock() {
        // The teardown handshake is easy to get exactly backwards: struct fields
        // drop *after* the `Drop::drop` body, so joining before releasing the
        // stop sender parks the dropper on a thread waiting for that sender to
        // close. It cost a hang on every hangup once; this pins it.
        //
        // No device involved — this exercises the handshake alone, which is the
        // part that was wrong.
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let join = std::thread::spawn(move || {
            let _ = stop_rx.recv();
        });
        let device = DeviceThread {
            stop: Some(stop_tx),
            join: Some(join),
        };

        // Drop on a worker so a regression fails the test by timeout instead of
        // hanging the whole suite.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(device);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok(),
            "dropping a DeviceThread deadlocked: the stop sender must close before the join"
        );
    }
}
