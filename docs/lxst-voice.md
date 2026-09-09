# LXST Voice — Integration Note

**FoxHole speaks LXST telephony over the same Reticulum transport it messages on.**
Status: **implemented** behind the `voice` feature (off by default).

The messaging binding is documented in [`lxmf-integration.md`](lxmf-integration.md);
this note covers the voice one. Read that first if you want the transport
bring-up story — voice starts where it ends.

## 1. What this is

[LXST](https://github.com/markqvist/LXST) (Lightweight Extensible Signal
Transport) is Reticulum's real-time media layer — the thing behind Sideband's
voice calls. [rsLXST](https://github.com/ratspeak/rsLXST) is the Rust
implementation, from the same authors as the `rsReticulum`/`rsLXMF` stack
FoxHole already links. It targets *interoperable Opus telephony* against Python
LXST rather than full parity, and is explicitly experimental.

FoxHole uses it for one thing: **point-to-point voice calls between operators**,
off-grid, over whatever interfaces the mesh already has.

## 2. Why it rides the existing transport

`foxhole-net` brings up Reticulum and holds `transport_tx`. The voice task takes
a clone of it rather than calling `reticulum::init` again.

This is not just tidiness. The peer you message and the peer you call are the
same node reached over the same interfaces. A second Reticulum instance would:

- announce a second set of paths for one physical node, doubling announce
  traffic on a link where bandwidth is the scarce resource;
- hold a second set of interface sockets (an `AutoInterface` peer discovery
  round per instance);
- and give the two stacks separate views of path state for the same peers.

So `foxhole-net` owns the voice task, because `net::run_inner` is the only place
where a live `transport_tx` and the loaded identity both exist.

```
                   ┌──────────────── foxhole-net ────────────────┐
  reticulum::init ─┤ transport_tx ──┬─→ LXMF endpoint            │
                   │                │   (lxmf.delivery)          │
                   │                └─→ foxhole-voice task       │
                   │                    (lxst.telephony)         │
                   └─────────────────────────────────────────────┘
```

Both aspects are registered against **the same identity file**, loaded twice
because `rns_identity::Identity` is not `Clone`. That shared identity is what
makes a peer reachable both ways, and it is what the addressing section below
turns on.

## 3. Addressing: destination hash vs. identity hash

The single most confusable thing here, and the source of a whole class of "the
call never connects" bugs.

| | Messaging | Voice |
|---|---|---|
| Aspect | `lxmf.delivery` | `lxst.telephony` |
| Roster key | **destination** hash | **identity** hash |
| Placed by | `Outbound { peer: dest_hex, … }` | `VoiceCommand::Call(identity_hex)` |

Both destinations derive from one key pair:

```
identity_hash    = sha256(public_key)[..16]
lxmf.delivery    = Destination::hash_from_name_and_identity("lxmf.delivery",  identity_hash)
lxst.telephony   = Destination::hash_from_name_and_identity("lxst.telephony", identity_hash)
```

They are therefore *derivable in one direction only*: given a peer's announced
public key you can compute the identity hash and thus either destination, but
you cannot go from one destination hash to the other without that key.

FoxHole exploits exactly that:

- The **Voice roster** is built from `lxst.telephony` announces, so it holds
  identity hashes directly, and its Enter key sends `VoiceCommand::Call`.
- The **Conversations roster** only holds destination hashes, so its Ctrl+V
  sends `VoiceCommand::CallPeer(dest)`. `foxhole-net`'s `net/voice.rs` resolves
  it against the announce-learned key cache — `dest → public key →
  Identity::from_public_key().hash` — because that cache is the only place the
  bridge is possible. If no key has been heard for that peer yet, the operator
  is told precisely that rather than shown a failed call.

Inbound announces are checked the other way round before they enter the roster:
the announced destination hash must actually equal
`telephony_destination_hash(identity_of(public_key))`, so a destination merely
*claiming* the aspect does not get listed as callable.

### Names

LXST's telephony announce carries **no app data at all** — there is no display
name on that aspect. Anything found in that field is some other client's private
convention, so FoxHole does not render it as a name.

Instead `foxhole-net` correlates: every `lxmf.delivery` announce *does* carry a
display name, and its public key yields the identity hash, so the name is
reported as `VoiceEvent::Alias { identity, name }`. The Voice tool keeps those
separately from the roster and applies them to a roster row (or a call already in
progress) when they match. An alias never *creates* a roster entry — hearing a
peer's LXMF announce says nothing about whether it can take a call.

Peer-supplied names are rejected if they contain control characters, on the same
grounds as every other piece of remote text that reaches the terminal.

Note that the Voice roster is deliberately **not** the Conversations roster. A
node running FoxHole without `voice` — or Sideband, or `lxmd` — announces
`lxmf.delivery` and cannot take a call. Listing those would be a phone book of
numbers that never ring.

## 4. The rsLXST seam

rsLXST's `api/README.md` names `TelephonyService` as *the* application boundary
and marks `TelephonyRnsEndpoint`, `TelephonyRuntimeCore`, `TelephonyCommand` and
friends as implementation-level SPI. FoxHole sits on the service:

```rust
let parts = TelephonyService::registered(transport_tx, &identity)?;
tokio::spawn(parts.service.run());
// parts.control_tx : mpsc::Sender<TelephonyControl>
// parts.event_rx   : mpsc::Receiver<TelephonyServiceEvent>
```

That buys call state, caller policy, destination registration, announce
discovery, outgoing link establishment, exact-link binding, timeouts and
teardown. Reimplementing any of it would mean re-deriving call state from raw
Reticulum traffic, which upstream warns against for good reason: rsLXST binds a
call to the exact interface that established the Link, and rejects packets
arriving on a different one *before* decryption or call accounting. That
property is not reconstructible from outside the service.

`foxhole-voice` is therefore a translator, not a protocol implementation:

| FoxHole | rsLXST |
|---|---|
| `VoiceCommand::Call` | `TelephonyControl::Call { remote_identity, profile, discovery_timeout }` |
| `VoiceCommand::Answer` | `request_answer(&control, expected_link_id)` |
| `VoiceCommand::Hangup` | `TelephonyControl::Hangup { ring_timeout: false }` |
| `VoiceCommand::SetProfile` | `TelephonyControl::SwitchProfile` (only while established) |
| `VoiceEvent::Call` | `TelephonyServiceEvent::Snapshot(…).active_call` |
| `VoiceEvent::Ended` | `CallTerminated` / `OutgoingCallFailed` |

**The snapshot is the authority.** Phase comes from
`ActiveCallSnapshot::{status, answered}`, never from inferring it out of the
discrete events; the discrete events supply the link id, the remote identity and
log lines. `App` in turn never invents call state — it only ever adopts
`VoiceEvent::Call`. That one-way rule is what keeps the HUD from offering
"answer" for a call rsLXST has already torn down.

Two mappings are not one-to-one and are worth knowing:

- **Answered-but-still-RINGING reads as CONNECTING.** rsLXST keeps reporting
  `Ringing` briefly after an answer; shown verbatim it invites the operator to
  answer the same call twice.
- **`Discovering` is FoxHole's, not LXST's.** Outgoing path/announce resolution
  runs asynchronously inside the service before there is any link to signal on,
  and on a multi-hop mesh it can take seconds. Without a phase for it the HUD
  would sit blank and look wedged.

## 5. Audio

rsLXST draws its boundary above the devices — *"applications still own
capture/playback and resampling into `RawAudioFrame`"* — so
`foxhole-voice/src/audio.rs` is entirely FoxHole's, built on `cpal`.

**Rate and channel conversion.** The negotiated profile fixes the codec rate
(8/24/48 kHz) and channel count; the device offers whatever it offers. Both
directions run through a linear resampler that carries its fractional read
position and previous sample *across callback boundaries*. That streaming
property is the whole point: if chunking the input changed the output count, the
packet cadence would slide against the device clock over a long call. There is a
test pinning it (`resampler_streams_across_chunks_without_drift`).

Linear interpolation is not audiophile resampling. For voice band, in the hot
path of a realtime thread, against the alternative of a resampling crate and its
allocations, it is the right trade.

**Callback discipline.** A `cpal` data callback runs on a realtime audio thread,
so it must not block or allocate unboundedly:

- capture converts into buffers it owns and offers whole frames with
  `try_send` — under back-pressure it drops the frame being offered rather than
  blocking, costing one packet instead of stalling the device;
- playback drains a bounded ring and pads with silence on underrun, and discards
  the *oldest* audio when over-full, so a jitter spike does not become permanent
  added delay.

**Thread ownership.** `cpal::Stream` is `!Send` on some hosts, so each stream is
built and kept on its own `std::thread` that parks until its handle drops.
Dropping the handle is what closes the device — there is no stop call to forget.
Devices are held only for the duration of a call, and the startup readiness
probe queries the devices *without opening them*, so an idle FoxHole never holds
a microphone open.

**No `StartOpusReceiveStream`.** rsLXST emits `OpusFramesReceived` carrying the
decoded audio unconditionally; a registered receive stream is an *additional*
delivery path fed from the same decode. Since the select loop already owns the
playback device, registering one would clone every audio frame into a channel
this task would immediately drain and discard.

**Mute sends silence** rather than stopping the stream, so packet cadence and the
negotiated profile survive it and unmuting is instant.

**Stderr is muted around the device calls.** ALSA's C library writes its
diagnostics straight to stderr, bypassing every Rust logging path — and with no
sound card, an unusual `.asoundrc`, or inside a container, opening the default
device produces half a screen of them. FoxHole runs full-screen on the alternate
buffer, so that output lands on top of the console and corrupts the display with
no redraw to clean it up. `audio::QuietStderr` redirects fd 2 to `/dev/null` for
the duration of each device call and restores it after. Losing those lines is the
right trade: they describe a condition the operator is already told about, in the
Voice tool's audio status, in terms that mean something.

A missing or unusable device is reported, never fatal — a call still signals and
connects with no audio path, which is what a headless relay wants.

## 6. Profiles

`foxhole-core` carries its own `VoiceProfile` mirroring `lxst_core::Profile`
rather than depending on it, so the core stays buildable without rsLXST. The
mirror is pinned to LXST's **wire bytes** and round-trip tested on both sides
(`profile_wire_values_round_trip`, `profiles_round_trip_through_lxst`), so a
drift fails a test instead of silently mis-negotiating a call.

| Profile | Codec | Rate | Frame | Ceiling | Usable |
|---|---|---|---|---|---|
| ULBW / VLBW / LBW | Codec2 700C/1600/3200 | 8 kHz | 400/320/200 ms | 0.7–3.2 kbps | **no** |
| MQ *(default)* | Opus voice | 24 kHz | 60 ms | 8 kbps | yes |
| HQ | Opus voice | 48 kHz | 60 ms | 16 kbps | yes |
| SHQ | Opus stereo | 48 kHz | 60 ms | 32 kbps | yes |
| LL | Opus voice | 24 kHz | 20 ms | 8 kbps | yes |
| ULL | Opus voice | 24 kHz | 10 ms | 8 kbps | yes |

The Codec2 profiles are signalling-only: rsLXST's first release ships Opus only.
The `p` key cycles in LXST's own order but **skips them**, so the operator can
never select a profile that cannot carry audio.

Mid-call, `p` renegotiates. rsLXST stops the existing streams on a profile
switch, so `foxhole-voice` reopens the devices at the new rate when it sees the
negotiated profile change — a capture still producing at the previous rate would
feed a stream that no longer exists, giving a call that looks connected and is
silent.

## 7. Operating it

Build (note the extra system dependency — the ALSA backend needs headers):

```bash
sudo apt install -y libasound2-dev    # Debian / Ubuntu / Raspberry Pi OS
cargo build --release --features voice
```

`voice` implies `net`. The default build stays offline and pulls neither rsLXST,
the bundled Opus encoder, nor an audio backend; the Voice tool is still there and
says the stack is not in the build.

In the console, Ctrl+N to the **Voice** tab:

| Key | Action |
|---|---|
| Up/Down | move the roster |
| Enter | call the selection — or **answer**, when a call is ringing |
| `h` / Esc | hang up, reject, or abandon a call being placed |
| `m` | mute / unmute the microphone |
| `p` | cycle profile (renegotiates during a call) |
| `a` | re-announce `lxst.telephony` now |

Ctrl+V in **Conversations** dials the selected peer and jumps here.

The tool's header shows **this node's identity hash** — that, not the
`lxmf.delivery` address shown in the Network tab, is what a peer needs in order
to call you.

## 8. Interoperability and limits

- **Against Python LXST / Sideband:** rsLXST targets wire interoperability for
  Opus telephony and tests against upstream, but it is experimental and FoxHole
  has not been verified against a live Sideband call. Treat cross-client calling
  as unproven until someone tries it in the field.
- **One call at a time.** `TelephonyRuntimeCore` is a single-line runtime; the
  UI refuses a second call rather than letting the task reject it silently.
- **No Codec2**, so the sub-4-kbps profiles that would matter most on a
  bandwidth-starved LoRa link are not yet usable. That is the gap to watch: it is
  upstream work, not FoxHole's.
- **No caller policy surfaced.** rsLXST has `CallerAccessPolicy`; FoxHole does
  not yet bind it to the peer trust levels, so any peer that can reach you can
  ring you. The obvious next step is to gate `Compromised`/`Untrusted` peers the
  way received intel already is.
- **Voice bypasses the encrypted stores.** Calls are not recorded and nothing
  about them lands on disk beyond the `[VOX]` lines in the log — which BURN
  destroys along with everything else.
