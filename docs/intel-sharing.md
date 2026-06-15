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

1. **P1 — CoT subset codec (pure, tested).** Parse/generate the foxhole subset of
   CoT `event`/`point`/`detail` (XML first, hardened). An `Annotation`/`CotEvent`
   model with affiliation+kind, round-trip + lenient-parse + malformed-input +
   XXE-rejection tests. Lives in `foxhole-cot` (proposed) or the binary; `Zone`
   folds in as a produced `u-d-c-c`.
2. **P2 — ingest + render.** Parse the custom fields in `net.rs` → `NetEvent::Cot`
   → core applies to the received-intel layer with trust gating + `stale` sweep.
   Render the affiliation-tinted layer; generalize the HAZARD panel to INTEL.
3. **P3 — share.** Generate a CoT event + human-readable body, send via the LXMF
   custom field; trigger from an in-app "share" action (direct/group).
4. **P4 — durability + reach.** In-app authoring, encrypted persistence, revoke
   workflow, the protobuf transport (`cot/proto`), and — separately — a TAK gateway
   and PLI/GeoChat unification.
