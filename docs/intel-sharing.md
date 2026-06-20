# Intel Sharing — Design Note

**v2: Foxhole speaks CoT over Reticulum.**
Status: **draft / proposal** — pins the model and wire format before any code.

## 0. What changed from v1

v1 proposed a foxhole-native msgpack schema (`foxhole.intel/1`). That was
reinventing — badly — a standard that already exists: **CoT
(Cursor-on-Target)**, the open event format the ATAK/TAK ecosystem uses for
shared situational awareness. v2 **adopts a CoT subset** instead, gaining a
battle-tested model, MIL-STD-2525 symbology, built-in validity/expiry, and a path
to interoperate with ATAK / WinTAK / TAK Server / FreeTAKServer.

Telemetry stays as-is (Sideband-compatible `FIELD_TELEMETRY`); CoT covers the
**intel / marker / zone** layer. Foxhole becomes, in effect, a **CoT node on
Reticulum** — and a potential gateway between the Sideband/Reticulum and TAK
ecosystems.

## 1. Telemetry vs. intel (unchanged premise)

Two orthogonal map layers; conflating them is a category error:

| Layer | Answers | Source | Lifetime | Mechanism |
|-------|---------|--------|----------|-----------|
| Telemetry (`◆`) | "who is where" | self-reported per node | ephemeral | Sideband `FIELD_TELEMETRY` (done) |
| Intel (markers, zones) | "what is where" | operator-authored | validity window | **CoT over LXMF** (this note) |

## 2. Why CoT

- **It is the spec we were sketching**, only standardized. The v1 model maps 1:1:

  | v1 field | CoT |
  |---|---|
  | `id` | `uid` (latest `time` wins for a uid) |
  | `category`/severity | `type` (MIL-STD-2525 hierarchical code + affiliation) |
  | `lat`,`lon`,accuracy | `<point lat lon hae ce le>` |
  | `expiry`/TTL | `start` + **`stale`** (validity interval, built in) |
  | provenance hint | `how` |
  | `note`, radius, shape | `<detail>` children |

- Hands us, for free: markers, zones/drawing shapes, routes, GeoChat, MEDEVAC,
  and affiliation (friend/hostile/neutral/unknown).
- Interop: a real bridge to the TAK world is *possible* later (out of scope here,
  but the door stays open by speaking the wire format).

## 3. The CoT event model (the part foxhole uses)

A CoT message is one `<event>`:

```xml
<event version="2.0" uid="J-OP-1" type="a-h-G-U-C" how="h-g-i-g-o"
       time="2026-06-15T07:00:00Z" start="2026-06-15T07:00:00Z"
       stale="2026-06-15T13:00:00Z">
  <point lat="50.4000" lon="30.5000" hae="0" ce="9999999" le="9999999"/>
  <detail>
    <contact callsign="AO ALPHA"/>
    <remarks>shelling reported</remarks>
    <shape><ellipse major="400000" minor="400000" angle="0"/></shape>
  </detail>
</event>
```

- **`uid`** — globally unique object id; a newer event for the same uid replaces
  the prior one. (We additionally key by *source*, see §6.)
- **`type`** — hierarchical 2525 code. Affiliation is the 2nd token:
  `f`=friendly, `h`=hostile, `n`=neutral, `u`=unknown. Battle dimension is the
  3rd (`G`round/`A`ir/`S`ea/…). Examples we care about (authoritative strings come
  from the CoT type catalog / MIL-STD-2525):

  | Intent | Representative `type` |
  |---|---|
  | Friendly ground unit | `a-f-G-U-C` |
  | Hostile ground unit | `a-h-G-U-C` |
  | Neutral / unknown contact | `a-n-G`, `a-u-G` |
  | Generic point marker | `b-m-p-s-m` |
  | Drawn area / hazard zone (circle) | `u-d-c-c` (+ `<shape><ellipse>`) |
  | Route | `b-m-r` |
  | GeoChat (future) | `b-t-f` |

- **`point`** — `lat`/`lon` (deg), `hae` (height above ellipsoid, m), `ce`/`le`
  (circular/linear error, m; `9999999` = unknown).
- **`detail`** — extensible: `contact/@callsign`, `remarks`, `color`, and the
  `shape`/`ellipse` (or a radius) that turns a point into a **zone**. Foxhole reads
  the handful it renders and ignores the rest (never fatal).
- **`stale`** — when the event stops being valid → our expiry, for free.

## 4. The foxhole CoT subset

Foxhole is a monochrome TUI, not ATAK — it implements a pragmatic **subset**:

- **Consume**: `point`/`type`/`stale` + `detail.contact.callsign`,
  `detail.remarks`, and `shape.ellipse`/radius. Map `type` →
  `(affiliation, kind)`; render affiliation as a tint + glyph, kind as a category.
  Unknown `type`s render as a generic marker — display-only, never an error.
- **Produce**: markers (`a-{f,h,n,u}-G…`) and drawn hazard zones (`u-d-c-c` with an
  ellipse) — i.e. exactly today's `Zone` plus affiliated points.
- **Out of subset (for now)**: PLI (overlaps telemetry — kept on Sideband),
  GeoChat (overlaps LXMF messaging), routes, MEDEVAC. Reserved for later phases.

Today's `Zone` (`{label, center, radius_km}`) becomes a produced `u-d-c-c` event;
`zones.conf` stays the local authoring path and is rendered as *local* intel.

## 5. Transport over Reticulum

CoT rides inside an **LXMF message** via the sanctioned custom fields:

- `FIELD_CUSTOM_TYPE` (`0xFB`) = content tag — **`"cot/xml"`** (v1) or
  `"cot/proto"` (upgrade). The receiver matches this before decoding.
- `FIELD_CUSTOM_DATA` (`0xFC`) = the CoT event bytes (one `<event>` per message,
  per CoT convention; sharing several items = several messages).
- The LXMF **text body** SHOULD carry a one-line human summary
  (`"INTEL: AO ALPHA (hostile) @ 50.40,30.50 r400km, stale 13:00Z"`) so a
  non-foxhole/non-TAK client still shows something legible (graceful degradation).

**Bandwidth vs. MTU — an important nuance.** CoT XML is verbose (~1 KB/event),
but **Reticulum fragments/segments payloads itself** (links/resources), unlike raw
Meshtastic frames — so a CoT event does *not* need to fit one LoRa frame; Reticulum
delivers it. XML-vs-protobuf is therefore an **efficiency** choice over a slow
link, not a "does it fit" one:

- **v1 baseline — CoT XML** (`cot/xml`): simplest, fully standard,
  human-debuggable, no schema work. Recommended to start.
- **Upgrade — TAK Protocol v1 protobuf** (`cot/proto`): compact and directly
  interoperable with TAK servers; costs a protobuf dependency. Add when bandwidth
  or TAK-server interop demands it. (This is the same compaction the Meshtastic
  ATAK plugin uses for LoRa.)

## 6. Provenance, trust, lifecycle

- **Provenance is the LXMF signature**, not the CoT `uid`. The carrying message is
  signed by the sender's identity, so the *origin* of every event is
  cryptographically authenticated and unspoofable. Events are keyed internally by
  **`(source, uid)`**.
- **Trust gating** (reuses foxhole's per-peer `Trust`):
  - `Trusted` → auto-apply to the map.
  - `Unknown`/`Untrusted` → **staged** in an "incoming intel" review list; operator
    accepts or discards. (Config toggle may opt into auto-apply from all.)
  - `Compromised` → dropped silently.
- **Lifecycle**:
  - *Apply/update*: upsert by `(source, uid)`; newer `time` wins (CoT semantics).
  - *Expiry*: drop when `now > stale`; a periodic sweep enforces it. Missing/`0`
    stale ⇒ a config default TTL.
  - *Revoke*: a CoT event with `stale ≤ time` (or a TAK delete `t-x-d-d` event)
    removes the object.
  - *Conflict*: same `(source, uid)` ⇒ newest wins; same area from *different*
    sources ⇒ keep both, attributed (no silent merge).
- **No auto-relay** in v1: foxhole does not rebroadcast received CoT (no
  flooding/loops). Forwarding is an explicit operator action.

## 7. Rendering / UI

- **Map layers**, visually distinct so the operator always knows the source class:
  - *Local* intel (`zones.conf` / authored) — authoritative, brass.
  - *Received* CoT — tinted by **affiliation** (friend/hostile/neutral/unknown),
    tagged with the sender's short hash + a `stale` countdown.
  - *Telemetry* peers — `◆` (unchanged).
- **Affiliation → tint/glyph** (monochrome-safe via glyph; decision in §10):
  friendly / hostile / neutral / unknown each get a distinct glyph and color.
- **INTEL panel** (today's HAZARD AOs, generalized): lists events with
  callsign/`uid`, `type`, source, and time-to-stale.
- **Authoring** (later phase): place a marker/zone, pick affiliation + type +
  stale, then **share** (direct/group). v1 ships `zones.conf` authoring + a
  "share zone" action only.

## 8. Bridging to TAK (future, enabled-not-built)

Because foxhole speaks CoT on the wire, a **gateway node** can translate
LXMF-CoT ↔ a TAK Server stream (CoT/TAK Protocol over TCP/TLS), making an off-grid
Reticulum mesh a CoT feed into/out of standard TAK infrastructure. Explicitly out
of scope for this note, but the format choice is what makes it possible.

## 9. Security considerations

Received CoT is **attacker-controllable external input** (any peer can send it):

- **Authenticated source** via the LXMF signature (anti-spoof) — but a *trusted*
  peer can still send bad data, hence trust gating + staging.
- **Hardened XML parsing** is mandatory for the `cot/xml` path: a reader with
  **external entities / DTDs disabled** (no XXE), bounded nesting and total size.
  Prefer a small hardened parser (e.g. `quick-xml`) or a hand-rolled subset reader
  over the known element set. (The protobuf path sidesteps XML attack surface.)
- **Validate/clamp** every `point` (`GeoPos::new` for lat/lon, bound radius),
  truncate `callsign`/`remarks`, ignore unknown/active `detail`.
- **Bound volume**: cap events per message, per-source, and overall; enforce
  `stale` expiry — DoS / map-flooding defence.
- **Never execute**: CoT is display data only.

## 10. Open decisions

1. **Transport v1** — `cot/xml` (recommended) vs. `cot/proto` from the start.
2. **XML codec** — hardened library (`quick-xml`, entities off) vs. a hand-rolled
   subset parser/generator (smaller dep surface, only our element set).
3. **Subset scope v1** — markers + hazard zones only, with PLI/GeoChat deferred?
4. **Persistence** — store received CoT in the encrypted store (survives restart,
   burn-wiped) vs. ephemeral/in-memory for v1.
5. **Affiliation glyph/tint mapping** for the TUI (e.g. friendly `▲`/green,
   hostile `◆`/red, neutral `■`/grey, unknown `●`/amber).
6. **Crate placement** — CoT codec in the root binary's net layer, or a new
   dependency-light `foxhole-cot` member crate (keeps `foxhole-core` clean and the
   codec reusable/testable in isolation).

## 11. Non-goals (v1)

- Not full CoT/TAK compliance — a pragmatic, documented subset.
- Not a telemetry replacement (Sideband telemetry stays).
- No automatic multi-hop flooding/gossip (manual forward only).
- Not a TAK server, and no live TAK bridge yet (§8 is future).

## 12. Phased implementation

1. **P1 — CoT subset codec (pure, tested).** ✅ **Done** — `crates/foxhole-cot`:
   hardened XML subset parse/generate of `event`/`point`/`detail`, an
   affiliation+kind model, in-house ISO-8601↔epoch, round-trip + lenient-parse +
   malformed-input + XXE-rejection tests. `Zone` folds in as a produced
   `u-d-c-c`. Test vectors come from the reference injector (Appendix A).
2. **P2 — ingest + render.** ✅ **Done** — `net.rs` parses the custom fields →
   `NetEvent::Cot` → `foxhole-core`'s `app/intel.rs` applies it with trust gating
   + `stale` sweep; the World Map renders the affiliation-tinted layer and the
   generalized INTEL panel. Dev-tested live with the reference injector.
3. **P3 — share.** ✅ **Done** — Ctrl+G in Conversations shares a local zone as a
   `cot/xml` LXMF message (CoT event + human-readable summary body) to the peer.
4. **P4 — durability + reach.** *In progress.*
   - ✅ **Encrypted persistence** — `src/intel_store.rs` (`FXI1` blob, same
     AES-256-CBC+HMAC/atomic recipe as the conversation store); intel survives a
     restart and is burn-wiped.
   - ✅ **Revoke workflow** — Ctrl+G → `r` sends a `stale==time` revocation so the
     peer drops the object; `x` on the map removes a received/authored object
     locally.
   - ✅ **In-app authoring** — `a`/`e` on the World Map place/edit markers & zones
     of any affiliation into the live intel layer.
   - ⏳ **Remaining:** the protobuf transport (`cot/proto`) and a TAK gateway
     (plus PLI/GeoChat unification). See §13.

## 13. Remaining work (not yet built)

The interactive intel layer (P1–P4 above) is complete. Two larger, deliberately
separate efforts remain — each adds a dependency or an external attack surface,
so they are scoped here rather than rushed:

### 13.1 `cot/proto` — TAK Protocol v1 (protobuf) transport

An **efficiency** upgrade over `cot/xml`, not a correctness one (Reticulum already
fragments large payloads — §5). Carry the event as TAK Protocol v1 protobuf in
`FIELD_CUSTOM_DATA` with `FIELD_CUSTOM_TYPE = "cot/proto"`.

- **Why:** ~3–5× smaller than XML, the same compaction the Meshtastic ATAK plugin
  uses — worth it over a slow LoRa link, and directly interoperable with TAK
  servers that speak the protobuf stream.
- **Sketch:** add a `cot/proto` codec path to `foxhole-cot` (a small protobuf
  dependency, e.g. `prost`, or a hand-rolled encoder for the handful of fields).
  Producer picks the format (config/per-peer); `net.rs` matches the content tag
  and routes to the XML or proto decoder. The decoded `CotEvent` and everything
  above it (ingest, trust, render, store) are unchanged.
- **Cost:** a protobuf dependency (or hand-rolled varint code) — the first
  non-trivial dep in the dependency-light `foxhole-cot`. Gate it behind a feature
  if XML-only builds should stay dependency-free.

### 13.2 TAK gateway (bridge to TAK Server / FreeTAKServer)

A separate **gateway node** that translates LXMF-CoT ↔ a TAK Server stream
(CoT/TAK Protocol over TCP/TLS), making an off-grid Reticulum mesh a CoT feed
into/out of standard TAK infrastructure (§8). Explicitly out of the foxhole TUI;
likely a small standalone binary/service.

- **Reference:** FreeTAKTeam's **Reticulum-Community-Hub** already implements the
  mesh→TAK direction (its `atak_cot` module uses **PyTAK** to push CoT over
  `tcp/udp` to a TAK Server, default `tcp://127.0.0.1:8087`, optional TLS). It is
  *unidirectional* and uses its own on-mesh JSON envelope (`r3akt.*`), **not**
  foxhole's `cot/xml` — so it is a model to follow, not a drop-in peer.
- **foxhole's advantage:** foxhole already emits real CoT XML on the wire, so its
  gateway is mostly "frame the existing event onto a TCP/TLS socket" (no
  JSON→CoT translation), and can be **bidirectional** (TAK → mesh) more naturally.
- **Scope:** out of the `net` feature; a dedicated bridge process that subscribes
  to a foxhole node's LXMF delivery and relays to/from the TAK server. Pin the
  CoT type mappings (telemetry→`a-f-G-U-C`, GeoChat→`b-t-f`) and a trust/allowlist
  policy for what crosses the boundary.

### 13.3 Note on the shared `0xFB` namespace

`FIELD_CUSTOM_TYPE` (`0xFB`) is becoming a shared namespace in the
Reticulum/TAK space — foxhole uses `cot/xml`, Reticulum-Community-Hub uses
`r3akt.*`. They coexist safely (foxhole ignores non-`cot/*` tags), but anyone
adding a transport here should keep matching on the exact tag value.


## Appendix A — Reference injector (`cot_inject.py`)

A tiny Python sender that puts a CoT-bearing LXMF message on a Reticulum network,
serving two jobs:

- **Dev test fixture (offline):** `--dry-run` prints the CoT `<event>` XML — paste
  it straight into a P1 decoder unit test (exactly how captured telemetry hex
  became Rust test vectors).
- **Live ingest test:** with `--to <foxhole-hash>` it delivers the event over
  Reticulum, so P2 ingest can be exercised **without a second foxhole or ATAK**.
  The sender uses a fresh identity, so foxhole sees it as an *Unknown* peer — handy
  for testing the trust-gating / staging path.

It pins the on-wire framing this note specifies: `FIELD_CUSTOM_TYPE` (`0xFB`) =
`"cot/xml"`, `FIELD_CUSTOM_DATA` (`0xFC`) = the UTF-8 event bytes, plus a
human-readable summary in the message body.

Prereqs: `pip install rns lxmf`, and a Reticulum config that shares a network with
the foxhole node (the recipient must have announced so a path is known).

```python
#!/usr/bin/env python3
"""Inject a CoT-bearing LXMF message onto a Reticulum network.

Reference sender + dev fixture for foxhole's CoT-over-Reticulum intel ingest.

  # live send (marker)
  python cot_inject.py --to e0f0216ad3841468f909ff12fdbb250e \
      --type a-f-G-U-C --callsign OP-1 --lat 48.86 --lon 2.29

  # live send (hostile hazard zone, 400 km radius, 6 h stale)
  python cot_inject.py --to <hash> --type a-h-G-U-C --callsign "AO ALPHA" \
      --lat 50.40 --lon 30.50 --radius 400000 --stale 21600 \
      --remarks "shelling reported"

  # just print the CoT XML (no network) -> copy into a decoder test
  python cot_inject.py --dry-run --type a-u-G --callsign MARK-1 --lat 0 --lon 0
"""
import argparse, time
from datetime import datetime, timedelta, timezone
from xml.sax.saxutils import escape, quoteattr


def cot_event(uid, cot_type, lat, lon, callsign, stale_s, remarks, radius_m):
    now = datetime.now(timezone.utc)
    iso = lambda t: t.strftime("%Y-%m-%dT%H:%M:%S.000Z")
    detail = f"<contact callsign={quoteattr(callsign)}/>"
    if remarks:
        detail += f"<remarks>{escape(remarks)}</remarks>"
    if radius_m:  # a point with a radius == a circular zone
        detail += f'<shape><ellipse major="{radius_m}" minor="{radius_m}" angle="0"/></shape>'
    return (
        '<?xml version="1.0" standalone="yes"?>'
        f"<event version=\"2.0\" uid={quoteattr(uid)} type={quoteattr(cot_type)} "
        f'how="h-g-i-g-o" time="{iso(now)}" start="{iso(now)}" '
        f'stale="{iso(now + timedelta(seconds=stale_s))}">'
        f'<point lat="{lat:.6f}" lon="{lon:.6f}" hae="0.0" ce="9999999.0" le="9999999.0"/>'
        f"<detail>{detail}</detail></event>"
    ).encode("utf-8")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--to", help="recipient lxmf.delivery hash (hex)")
    ap.add_argument("--type", default="a-u-G", help="CoT type code")
    ap.add_argument("--callsign", default="MARK-1")
    ap.add_argument("--lat", type=float, required=True)
    ap.add_argument("--lon", type=float, required=True)
    ap.add_argument("--radius", type=int, default=0, help="zone radius in metres (0 = point)")
    ap.add_argument("--remarks", default="")
    ap.add_argument("--stale", type=int, default=21600, help="seconds until stale")
    ap.add_argument("--uid", default=None)
    ap.add_argument("--config", default=None, help="Reticulum config dir")
    ap.add_argument("--dry-run", action="store_true", help="print the CoT XML and exit")
    a = ap.parse_args()

    uid = a.uid or f"foxhole-{a.callsign}-{int(time.time())}"
    xml = cot_event(uid, a.type, a.lat, a.lon, a.callsign, a.stale, a.remarks, a.radius or None)
    summary = f"INTEL: {a.callsign} ({a.type}) @ {a.lat:.4f},{a.lon:.4f}" + (
        f" r{a.radius}m" if a.radius else "")

    if a.dry_run:
        print(xml.decode())
        return
    if not a.to:
        ap.error("--to is required unless --dry-run")

    import RNS, LXMF  # constants are 0xFB / 0xFC if your LXMF doesn't export the names
    RNS.Reticulum(a.config)
    router = LXMF.LXMRouter(storagepath="./.cot_inject_lxm")
    source = router.register_delivery_identity(RNS.Identity(), display_name="cot-inject")
    router.announce(source.hash)

    dest_hash = bytes.fromhex(a.to)
    if not RNS.Transport.has_path(dest_hash):
        RNS.Transport.request_path(dest_hash)
        deadline = time.time() + 15
        while not RNS.Transport.has_path(dest_hash) and time.time() < deadline:
            time.sleep(0.1)
    recipient = RNS.Identity.recall(dest_hash)
    if recipient is None:
        raise SystemExit("no path/identity yet — has the recipient announced?")

    dest = RNS.Destination(recipient, RNS.Destination.OUT, RNS.Destination.SINGLE,
                           "lxmf", "delivery")
    lxm = LXMF.LXMessage(dest, source, summary, "", fields={
        LXMF.FIELD_CUSTOM_TYPE: "cot/xml",
        LXMF.FIELD_CUSTOM_DATA: xml,
    }, desired_method=LXMF.LXMessage.DIRECT)
    router.handle_outbound(lxm)

    done = (LXMF.LXMessage.SENT, LXMF.LXMessage.DELIVERED, LXMF.LXMessage.FAILED)
    deadline = time.time() + 30
    while lxm.state not in done and time.time() < deadline:
        time.sleep(0.1)
    print("uid:", uid, "state:", lxm.state)


if __name__ == "__main__":
    main()
```

When P1 lands, this script (or its `--dry-run` output) graduates into a checked-in
test fixture under `tools/` so the decoder is always validated against a real
CoT-in-LXMF payload, not a hand-mocked one.
