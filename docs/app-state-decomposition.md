# Decomposing the `App` god-struct

**Status: done** (2026-08). Measured and deferred during the architecture
refactor of 2026-08 (`refactor(net)` / `refactor(core)` / `refactor: isolate the
feature-gated wiring`), then carried out as its own pass of seven commits, one
group each. This note keeps the measurement that motivated it — it is the record
of *why* the shape is what it is — and records what shipped, including the two
questions the measurement left open.

## The problem

`foxhole_core::app::App` was the single mutable state object for the whole
terminal, carrying **49 public fields** covering eight top-level tools, seven
modal overlays, four scroll positions, three outbound queues, and five bits of
cross-cutting bookkeeping.

Every field is `pub`, so `foxhole-tui` renders straight off them and `main`
drains them directly. That was the right call early — it keeps the render path
trivial and the logic unit-testable without accessor ceremony — but it meant the
struct had no internal seams at all.

### It was still growing

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

Roughly **+1.5 fields per feature**, with no counter-pressure. Nothing in that
shape made the next feature cheaper than the last.

### What it actually cost

Three concrete symptoms, in the order they bit:

1. **`App::new` was a 69-line flat initialiser** that every feature had to edit,
   and that had to be reviewed field-by-field because nothing grouped related
   defaults.
2. **Related state could drift apart.** `browser_selected` indexed `nomad_nodes`
   and `node_selected` indexed `nodes`, but nothing in the type system tied
   either pair together — the invariant lived only in the code that happened to
   maintain it. The same held for `selected`/`conversations`,
   `map_selected`/the marker list, and `note_selected`/`notes`.
3. **Everything read like it might be global.** A reader of
   `crates/foxhole-core/src/app/browser.rs` could not tell from the signature
   `fn handle_browser_key(&mut self, key: KeyEvent)` that the whole module
   touched eight fields and not forty-nine — nor which eight.

Note what was *not* on that list: the modal-overlay chain, which was the sharpest
edge, had already been fixed — `handle_key` routes through the `Modal` enum
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
therefore rewrite the tests and the code under test together — exactly the change
shape where a mechanical slip stops being caught. Hence one group per commit,
each compiling and testing clean on its own.

## What shipped

Seven groups, in the order the note prescribed (fewest test-suite accesses
first, `ConversationsState` last):

| Group | Fields | Lives in | `App` field |
| --- | ---: | --- | --- |
| `MapState` | 5 | `app/map.rs` | `map` |
| `NetworkState` | 6 | `app/network.rs` | `net` |
| `BrowserState` | 6 | `app/browser.rs` | `browser` |
| `IntelState` | 6 | `app/intel.rs` | `intel` |
| `Outbox` | 4 | `app/outbox.rs` | `outbox` |
| `Modals` | 3 | `app/mod.rs` | `modals` |
| `ConversationsState` | 5 | `app/conversations.rs` | `convs` |

That is the projected 35 of 49 fields, leaving `App` at **21 fields** (20 without
the `splash` feature): the seven groups plus `active`, `state`, `boot`, `config`,
`notes`, `note_selected`, `notes_dirty`, `syslog`, `log_scroll`, `guide_scroll`,
`local_address`, `sync_status`, `burn`, `should_quit`. `App::new` is now a list
of group defaults; the demo-peer seeding moved with the roster into
`ConversationsState::default`.

Each group defines its own module's state next to the `impl App` block that
drives it, so `app/browser.rs` now says in its first twenty lines exactly which
state the Browser owns.

### Decisions the measurement had left open

- **`IntelState` stayed a sibling of `MapState`, not part of it.** The note
  flagged the two as entangled (six of the thirteen cross-group methods are
  map↔intel) and asked whether one `MapState` should own the intel layer. It
  should not: the map *draws* the layer, but the layer's lifetime is the
  network's — it arrives from peers, is persisted encrypted by the binary, and is
  swept on a timer, none of which the viewport has a say in. Folding it in would
  have produced an eleven-field `MapState` of exactly the shape being
  decomposed, and would have buried the binary's cross-crate persistence handoff
  (`app.intel.live` / `.staged` / `.dirty` → `foxhole-net`'s `intel_store`)
  underneath the viewport. The six map↔intel methods stay on `App`, as the note
  predicted they would either way.
- **`Outbox` was grouped anyway.** It is touched from every tool by design —
  that is what a send queue is — so grouping it reduced no coupling, exactly as
  predicted. It earns its place by naming the four fields that move together and
  by making the id source private: `next_msg_id` is no longer a `pub` field
  anyone can bump, only `Outbox::next_id` hands one out.
- **The thirteen cross-group methods all stayed on `App`.** None was pushed
  onto a sub-struct, so none of them grew an extra hop.
- **`notes`/`note_selected`/`notes_dirty` stayed top-level.** They are a
  three-field tool with no modal and no queue; a group would be ceremony. The
  first Notes feature that adds a fourth is the trigger to reconsider.

### Conventions to keep

- **Fields on the groups stay `pub`, and call sites use direct paths**
  (`app.map.view`, `self.convs.list`). `&mut self.browser.page` alongside
  `&self.convs.list` borrow-checks fine; the same access behind
  `self.browser_mut()` and `self.convs()` does not. Accessor pairs would force
  clones the flat struct never needed.
- **A group owns its own modal.** `MapState::goto_mgrs`, `IntelState::review` /
  `share_zone` / `author` live with their tool; `Modals` holds only the three
  program-global overlays (New Conversation, burn confirmation, mnemonic view).
  Which is which is now visible from where the field lives.
- **A group owns its own scroll.** `BrowserState::scroll` and
  `ConversationsState::scroll` moved with their panes; `log_scroll` and
  `guide_scroll` belong to tools that are nothing but a scrollback.
- **`config` is not in any group.** It is read from the intel, map, and network
  modules and from the binary — operator settings are genuinely top-level state.
- **New state goes in the group it belongs to**, not on `App`. A field lands on
  `App` only when more than one tool genuinely owns it, and a *new* group is
  worth its own struct at roughly four fields. This is the counter-pressure the
  flat struct did not have.
