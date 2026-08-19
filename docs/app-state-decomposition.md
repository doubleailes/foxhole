# Decomposing the `App` god-struct

**Status: done (2026-08).** Carried out as the seven single-group commits
`refactor(core): group …` (`MapState` → `NetworkState` → `BrowserState` →
`Modals` → `Outbox` → `IntelState` → `ConversationsState`), in the order and
shape this note prescribes; every commit compiled and passed the full workspace
test suite (offline and `--features net`). See **Outcome** at the end for the
decisions taken where this note left a question open. The measurement below is
kept as written — it is the record of why the work took this shape.

## The problem

`foxhole_core::app::App` is the single mutable state object for the whole
terminal. It currently carries **49 public fields** covering eight top-level
tools, seven modal overlays, four scroll positions, three outbound queues, and
five bits of cross-cutting bookkeeping.

Every field is `pub`, so `foxhole-tui` renders straight off them and `main`
drains them directly. That was the right call early — it keeps the render path
trivial and the logic unit-testable without accessor ceremony — but it means the
struct has no internal seams at all.

### It is still growing

Field count at each commit that changed it:

| Date | Fields | Change |
| --- | --- | --- |
| 2026-06-14 | 34 | mnemonic addresses + trust level |
| 2026-06-14 | 37 | ten-slot note buffer |
| 2026-06-14 | 38 | Ctrl+T message title |
| 2026-06-14 | 40 | World Map tool |
| 2026-06-14 | 41 | war-zone hazard overlay |
| 2026-06-15 | 44 | intel sharing P2 (CoT ingest) |
| 2026-06-19 | 45 | intel sharing P3 (share a zone) |
| 2026-06-20 | 46 | intel sharing P4 (persistence) |
| 2026-06-20 | 47 | intel sharing P4 (authoring) |
| 2026-06-20 | 48 | capitals/cities layer |
| 2026-06-20 | 49 | MGRS support |

Roughly **+1.5 fields per feature**, with no counter-pressure. Nothing in the
current shape makes the next feature cheaper than the last.

### What it actually costs

Three concrete symptoms, in the order they bite:

1. **`App::new` is a 69-line flat initialiser** that has to be edited by every
   feature, and reviewed field-by-field because nothing groups related defaults.
2. **Related state can drift apart.** `browser_selected` indexes `nomad_nodes`
   and `node_selected` indexes `nodes`, but nothing in the type system ties
   either pair together — the invariant lives only in the code that happens to
   maintain it. The same holds for `selected`/`conversations`,
   `map_selected`/the marker list, and `note_selected`/`notes`.
3. **Everything reads like it might be global.** A reader of
   `crates/foxhole-core/src/app/browser.rs` cannot tell from the signature
   `fn handle_browser_key(&mut self, key: KeyEvent)` that the whole module
   touches eight fields and not forty-nine — nor which eight. (Six of them are
   the Browser's own; the other two are `active` and `commands`, which is itself
   worth knowing.)

Note what is *not* on this list: the modal-overlay chain, which was the sharpest
edge, has already been fixed — `handle_key` now routes through the `Modal` enum
rather than seven hand-ordered `is_some()` branches.

## Why it was not done in the same pass

The refactor it was deferred from was verifiable by construction: every change
was a move or a re-owning within one crate, so the compiler and the existing
tests proved equivalence. Decomposing `App` is not that shape — it rewrites call
sites in **four** crates, including the two test suites that are the safety net
for everything else.

Measured call sites, counting `self.<field>` inside `foxhole-core` and
`app.<field>` elsewhere:

| Location | Field accesses |
| --- | --- |
| `foxhole-core` (lib) | 289 |
| `foxhole-core` (`app/tests.rs`) | 269 |
| `foxhole-tui` | 94 |
| `foxhole` (binary) | 26 |
| **Total** | **678** |

Roughly 40% of those live in the test suite. A single sweeping commit would
therefore rewrite the tests and the code under test together — exactly the
change shape where a mechanical slip stops being caught, and exactly what a
reviewer cannot check by reading a diff.

## Proposed grouping

Seven sub-structs, sized by the call sites each would move:

| Group | Fields | core | core tests | tui | bin |
| --- | ---: | ---: | ---: | ---: | ---: |
| `ConversationsState` | `conversations`, `selected`, `focus`, `transmit_field`, `thread_scroll` | 60 | 96 | 20 | 1 |
| `IntelState` | `intel`, `intel_staged`, `intel_dirty`, `intel_review`, `share_zone`, `author` | 62 | 2 | 11 | 7 |
| `MapState` | `map`, `map_selected`, `map_cities`, `zones`, `goto_mgrs` | 32 | 0 | 13 | 1 |
| `BrowserState` | `nomad_nodes`, `browser_selected`, `browser_pane`, `page`, `history`, `page_scroll` | 31 | 41 | 10 | 0 |
| `NetworkState` | `nodes`, `node_selected`, `net_col`, `path_probes`, `interfaces`, `link_count` | 19 | 27 | 13 | 0 |
| `Outbox` | `commands`, `outbound`, `dirty`, `next_msg_id` | 15 | 27 | 1 | 4 |
| `Modals` | `new_conv`, `burn_confirm`, `mnemonic_view` | 16 | 11 | 3 | 0 |

That accounts for 35 of the 49. The remaining 14 are genuinely top-level:
`active`, `state`, `boot`, `config`, `notes`, `note_selected`, `notes_dirty`,
`syslog`, `log_scroll`, `guide_scroll`, `local_address`, `sync_status`, `burn`,
`should_quit`.

### Two of these groups may not survive contact

The cross-group method scan below is worth doing *before* accepting the table
above, because it says something the field list alone does not:

- **`MapState` and `IntelState` are entangled.** Six of the thirteen cross-group
  methods are map↔intel, and that is not accidental: intel objects *are* map
  objects. Authoring reads the map centre and writes an intel record; removing
  one reads the map selection; the share picker spans conversations, intel, and
  map. Splitting them into two groups buys separation the code does not actually
  have. Consider one `MapState` that owns the intel layer, or accept that those
  six methods stay on `App`.
- **`Outbox` is touched from every tool by design.** Five cross-group methods
  involve it, from conversations, network, browser, and share alike. That is what
  a send queue *is* — the entanglement is the feature, not the debt. Grouping it
  is still worthwhile (it names the four fields that move together), but do not
  expect it to reduce coupling.

The other four groups (`ConversationsState`, `NetworkState`, `BrowserState`,
`Modals`) are clean: each is a tool's own state, and the methods that reach
outside them reach only into `Outbox` or `active`.

### Order to do them in

`MapState` first, then `NetworkState`, then `BrowserState` — they are the three
with the fewest test-suite accesses (0, 27, 41), so the first pass exercises the
pattern where a mistake is cheapest to spot. `ConversationsState` last: it is the
largest by call sites *and* by test coverage, and it benefits most from the
pattern being settled by then.

`IntelState` is a special case twice over: its 7 accesses in the binary are the
`intel`/`intel_staged`/`intel_dirty` handoff to `foxhole-net`'s encrypted store,
so grouping it changes a cross-crate interface rather than just a field path —
and per the section above, it may want to merge with `MapState` rather than
stand alone. Settle that question on paper before writing any of it.

## Hazards worth knowing before starting

- **Thirteen methods already span more than one proposed group** (a lower bound
  — the scan that produced this is a crude one). They belong at the composition
  root; pushing them onto a sub-struct would re-create the coupling with an extra
  hop. The full list is worth reading before committing to the grouping above,
  because two of the clusters are informative rather than incidental:

  | Method | Groups spanned |
  | --- | --- |
  | `conversations.rs::upsert_peer` | convs, net |
  | `conversations.rs::handle_conversations_key` | convs, outbox |
  | `conversations.rs::transmit` | convs, outbox |
  | `network.rs::handle_network_key` | convs, net, outbox |
  | `browser.rs::fetch_page` | browser, outbox |
  | `author.rs::open_author` | intel, map |
  | `author.rs::remove_selected_intel` | intel, map |
  | `share.rs::open_share_zone` | convs, intel, map |
  | `share.rs::handle_share_zone_key` | intel, map |
  | `share.rs::share_zone` | convs, map, outbox |
  | `share.rs::revoke_shared_zone` | convs, map, outbox |
  | `mod.rs::open_modal` | intel, map, modals |
  | `mod.rs::active_scroll` | browser, convs |

  Plus `map_markers()`, which reads `config`, `conversations`, and `intel`.
- **Disjoint borrows still work, but only through direct field paths.**
  `&mut self.browser.page` alongside `&self.convs.conversations` borrow-checks
  fine; the same access behind `self.browser_mut()` and `self.conversations()`
  does not. Prefer `pub` fields on the sub-structs over accessor pairs, or the
  borrow checker will force clones that the current flat struct does not need.
- **`foxhole-tui` is the cheapest place to validate the split.** Its 94 accesses
  are almost all reads in render functions, so if a grouping makes the renderer
  read awkwardly, that is real feedback about the grouping.
- **Do not fold `config` into a group.** It is read from the intel, map, and
  network modules and from the binary — operator settings are genuinely
  top-level state, not any one tool's.

## Suggested approach

One group per commit, each self-contained: move the fields, update the call
sites, keep the tests passing at every step. Seven commits that each compile and
test clean are reviewable; one 678-site commit is not.

Do **not** stage it through temporary accessor methods on `App`. That doubles
the number of call-site edits (once to the accessor, once to the field path) and
leaves the codebase in a state where both spellings are valid, which is worse
than either endpoint.

## When to actually do it

This is maintainability debt, not a correctness or security problem — nothing
here can strand a message, mis-plot a marker, or leak intel. The trigger to
schedule it is the next feature that would add fields to a group that already
exists on this list, since that is the point where the decomposition pays for
itself immediately instead of eventually.

## Outcome

What was actually built, including the calls made where the note above left a
question open:

- **All seven groups landed**, one commit each, in the prescribed order. Each
  sub-struct lives in its tool's module (`ConversationsState` in
  `conversations.rs`, `NetworkState` in `network.rs`, and so on; `Modals` and
  `Outbox` in `mod.rs`), is re-exported from `app`, and owns its defaults in a
  `new()` — `App::new` now composes groups instead of listing 49 fields.
- **Map and Intel stayed two groups.** The other sanctioned option — accepting
  that the six map↔intel methods stay on `App` — was taken instead of the
  merge. Those methods are composition-root methods either way (pushing them
  down would re-create the coupling with an extra hop, per the hazard list),
  and a standalone `IntelState` keeps the binary's encrypted-store handoff a
  clean `intel.live`/`intel.staged`/`intel.dirty` interface. All thirteen
  cross-group methods remain on `App`, unmoved.
- **Fields stayed `pub`, no accessors were staged** — both as prescribed.
  Disjoint borrows (`&mut self.browser.page` alongside `&self.convs.items`)
  work through the direct field paths.
- **Names drop the old prefixes** now the group provides the context:
  `nomad_nodes`/`browser_selected`/`browser_pane`/`page_scroll` became
  `browser.{nodes,selected,pane,scroll}`, `map`/`map_selected`/`map_cities`
  became `map.{view,selected,cities}`, `net_col` became `net.column`,
  `intel`/`intel_staged`/`intel_dirty` became `intel.{live,staged,dirty}`,
  and `conversations` became `convs.items`. `BrowserPane` moved next to
  `BrowserState` (still re-exported from `app`).
- `App` is down to **21 fields**: the seven groups plus the 14 genuinely
  top-level ones listed above (unchanged, `config` still not folded in).
- The renderer validated the split as predicted: `foxhole-tui` reads
  `app.convs.items`, `app.map.view`, `app.net.nodes` etc. without a single
  clone being forced.

The counter-pressure this note asked for now exists: a new feature's state
either belongs to a group — and goes there, with its default beside its
siblings — or it argues explicitly for being top-level.
