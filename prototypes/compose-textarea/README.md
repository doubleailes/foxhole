# compose-textarea — `tui-textarea` evaluation prototype

A throwaway, standalone binary that rebuilds foxhole's **TRANSMIT BUFFER** pane on
top of [`tui-textarea`](https://github.com/rhysd/tui-textarea) so the compose UX
can be felt and compared against the current editor in
`crates/foxhole-core/src/app/conversations.rs` (which is `String::push` on each
`KeyCode::Char(c)` and `String::pop` on `Backspace` — no caret motion, no
wrapping, no kill/yank, no undo).

## Run it

```sh
cargo run --manifest-path prototypes/compose-textarea/Cargo.toml
```

Tab switches TITLE/BODY · Ctrl+S "transmits" (echoes a `[TX]` line) · Esc quits.
The right-hand panel lists the editing motions you get for free.

## Why it lives outside the workspace

`tui-textarea` 0.7 (latest) — and even its `main` branch — pin **ratatui ^0.29 /
crossterm ^0.28**, while foxhole is on **ratatui 0.30 / crossterm 0.29**. The two
ratatui majors don't interoperate, so this crate is `exclude`d from the root
workspace and resolves its own older dependency graph. That version lag is the
headline cost of adopting `tui-textarea` today and is the main thing to weigh.

## If we adopt it for real

The editing state (`TextArea`) is a ratatui type, so it **cannot** live in
`foxhole-core` without breaking the compiler-enforced "core depends only on
crossterm + foxhole-micron" boundary. The realistic integration:

- Keep `Conversation::{draft, draft_title}` (`String`) in core as the persisted
  source of truth.
- Own a `TextArea` per active field in the **TUI/runtime layer**; route key
  events to it while `Pane::Transmit` is focused, then sync `.lines()` back into
  the core draft on edit (and reload the draft into the `TextArea` on
  conversation switch).
- Gate the whole thing behind a `textarea` feature so the dependency-light
  default build is unaffected — and only once `tui-textarea` ships ratatui-0.30
  support.
