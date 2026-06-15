# Intel Sharing — Design Note

Status: **draft / proposal**. No code yet — this pins the model and wire format
before implementation, the same discipline that made the telemetry formats stick.

## 1. Motivation

Foxhole's World Map has two distinct kinds of geographic data, and conflating
them is a category error we want to avoid:

- **Telemetry** — a node reporting **itself**: *"I am at X, battery Y."*
  Self-authored, live, ephemeral. Already implemented (Sideband-compatible
  `FIELD_TELEMETRY`, plotted as `◆` peer markers).
- **Intel** — an operator annotating **the world**: *"that area is a hazard,"*
  *"this point is an objective."* Authored about third parties/areas, shared
  deliberately, persistent with provenance and expiry.

Telemetry cannot carry intel (it only describes the sender). Intel is therefore
its own concept with its own transport. The existing hazard-AO overlay
(`zones.conf`) is the seed of the intel layer; this note specifies how to make it
**shareable peer-to-peer over LXMF**.

The two are orthogonal layers on the same map:

| Layer | Answers | Source | Lifetime |
|-------|---------|--------|----------|
| Telemetry (`◆`) | "who is where" | self-reported per node | ephemeral |
| Intel (`⚠`, waypoints) | "what is where" | operator-authored | persistent + expiry |

## 2. Data model

A shared unit is an **Annotation**, a superset of today's `Zone`:

```
Annotation {
    id:      String,        // stable id, unique per source (e.g. short random)
    kind:    AnnotationKind, // Zone | Point
    label:   String,        // short display name ("AO ALPHA", "OP-1")
    lat: f64, lon: f64,     // WGS-84 decimal degrees (clamped on ingest)
    radius_km: Option<f64>, // Zone only
    category: Category,      // Hazard | Conflict | Objective | Contact | Waypoint | Note
    severity: u8,            // 0..=3 (0 = info, 3 = critical); advisory only
    expiry:   Option<u64>,   // Unix seconds; None => default TTL
    note:     String,        // short free text (bounded)
}
```

- `Zone` (today's `{label, center, radius_km}`) becomes the `Zone` kind of an
  Annotation. `zones.conf` keeps working as the *local* authoring path.
- `Point` is a zero-radius marker (objective, contact, waypoint).
- Identity across the net is `(source, id)` — two sources may reuse `id`s without
  colliding, and a source can only update/revoke its own annotations.

## 3. Wire format (LXMF custom fields)

Intel travels as an **LXMF message** carrying two custom fields (the sanctioned
app-extension slots — distinct from telemetry's `0x02`):

- `FIELD_CUSTOM_TYPE` (`0xFB`) = msgpack string **`"foxhole.intel/1"`** — the
  schema id a receiver matches before decoding.
- `FIELD_CUSTOM_DATA` (`0xFC`) = msgpack payload:

```
{
  "ann": [ <annotation>, ... ],   // additions / updates
  "rm":  [ "<id>", ... ]          // revocations (optional)
}
```

Annotation encoding (string keys for readability; intel volume is low):

```
{ "id":"z7f3", "k":"zone",  "label":"AO ALPHA", "lat":50.4, "lon":30.5,
  "r":400.0, "cat":"hazard", "sev":3, "exp":1781600000, "note":"shelling" }

{ "id":"p12",  "k":"point", "label":"OP-1", "lat":48.86, "lon":2.29,
  "cat":"contact", "sev":1, "exp":0, "note":"" }
```

- Coordinates are **float degrees** (operator-authored, human-edited — no need
  for telemetry's `×1e6` fixed-point). Clamped/wrapped on ingest via `GeoPos::new`.
- `r` present only for zones. `exp` of `0`/absent ⇒ apply the default TTL.
- Unknown keys ignored; malformed annotations skipped (one bad row never drops the
  batch) — mirrors the lenient parsing already used for `zones.conf` and telemetry.

**Graceful degradation:** the message SHOULD also set a human-readable text body
(e.g. `"INTEL: AO ALPHA hazard @ 50.4,30.5 r400km"`) so a non-foxhole client
(Sideband) at least shows something legible while foxhole reads the structured
field.

## 4. Provenance & trust

The carrying LXMF message is **signed by the sender's identity** — so the source
of every annotation is *cryptographically authenticated*. We do not put a source
field in the payload; the message `source_hash` is the provenance, and it can't be
spoofed.

Foxhole already has per-peer `Trust` (`Unknown` / `Untrusted` / `Trusted` /
`Compromised`). Proposed gating:

- **Trusted** source → auto-apply to the map.
- **Unknown / Untrusted** → **staged**: held in an "incoming intel" review list;
  the operator accepts (applies) or discards. (A config toggle may opt into
  auto-applying from all.)
- **Compromised** → dropped silently.

## 5. Lifecycle

- **Apply**: an annotation upserts into the received-intel layer keyed by
  `(source, id)`. A later message from the same source with the same `id` updates
  it in place; `rm` removes it.
- **Expiry**: each annotation carries an absolute `expiry`; a periodic sweep drops
  expired ones. No `expiry` ⇒ default TTL (config, e.g. 24 h) so stale intel fades.
- **Dedup / conflict**: same `(source, id)` ⇒ newest wins. Same area from
  *different* sources ⇒ keep both, attributed (operators reconcile, the app
  doesn't merge silently).
- **No auto-relay** in v1: foxhole does not rebroadcast received intel (avoids
  loops/amplification). Forwarding is an explicit operator action.

## 6. Distribution

- **Direct** — to one peer (an LXMF message with the custom fields).
- **Group** — via `FIELD_GROUP` to a shared net.
- **Broadcast** — N direct sends across the roster (LXMF has no native broadcast),
  or to a designated group destination.

## 7. UI

- **Map layers**, visually distinct so the operator always knows the source class:
  - *Local* intel (`zones.conf` / in-app authored) — authoritative, brass.
  - *Received* intel — tinted by trust, tagged with the sender's short hash + an
    expiry hint.
  - *Telemetry* peers — `◆` (unchanged).
- **HAZARD AOs panel** extends to show, for received items, `source` + time-to-expiry.
- **Authoring** (later phase): create/edit a zone or point at a coordinate, set
  category/severity/expiry, then **share** to peer/group/roster. v1 can ship with
  `zones.conf` authoring + a "share current zones" action only.

## 8. Persistence

Received intel MAY be persisted in the existing **encrypted store** (like
conversations) keyed by source, so it survives a restart and is destroyed by a
burn — consistent with the off-grid posture. v1 may keep received intel
**in-memory** (rebuilt as messages re-arrive) to stay small; persistence is a
later phase. Local authored intel stays in `zones.conf`.

## 9. Security considerations

Received intel is **attacker-controllable external input** (any peer can send it).
Required handling:

- **Authenticated source** via the LXMF signature (anti-spoof) — but a *trusted*
  peer can still send bad data, hence trust gating + staging.
- **Clamp/validate** every coordinate (`GeoPos::new`), bound `radius_km`, truncate
  `note`/`label`.
- **Bound volume**: cap annotations per message (e.g. 64), cap total stored
  per-source and overall, enforce expiry — DoS / map-flooding defence.
- **Never execute**: intel is data only; it influences nothing but the display.

## 10. Non-goals (v1)

- Not telemetry, not live tracking.
- No automatic multi-hop intel flooding/gossip (manual forward only).
- No new routing/transport layer — rides plain LXMF.
- Not a real-time C2 system.

## 11. Open decisions

1. **Auto-apply policy** — trusted-auto + others-staged (recommended), or a global
   auto-apply toggle?
2. **Default TTL** — 24 h (recommended), with per-annotation `exp` override?
3. **Persistence in v1** — ephemeral/in-memory (recommended) vs. encrypted-store now?
4. **Coordinate encoding** — float degrees (recommended) vs. `i32 ×1e6`.
5. **Categories** — fix the initial enum (Hazard / Conflict / Objective / Contact /
   Waypoint / Note) or keep it an open string?

## 12. Phased implementation

1. **P1 — model + codec (pure, tested).** `Annotation`/`AnnotationKind`/`Category`
   in `foxhole-core`; the `foxhole.intel/1` msgpack encode/decode. No I/O, no UI.
   Round-trip + lenient-parse unit tests. (`Zone` folds in as the `Zone` kind.)
2. **P2 — ingest + render.** Parse the custom fields in `net.rs` → a
   `NetEvent::Intel` → core applies to the received-intel layer with trust gating
   and an expiry sweep. Render the new layer distinctly on the Map; extend the
   HAZARD panel.
3. **P3 — share.** Build an LXMF message with the custom fields (+ human-readable
   body) and send it; trigger from an in-app "share" action (direct/group).
4. **P4 — authoring + durability.** In-app create/edit of zones/points, encrypted
   persistence, revocation, and the staging/review workflow for untrusted sources.
