# xi-client Roadmap

**Status:** Phase 0–5 shipped. Phase 6–9 planned.
**Companion docs:** [`protocol.md`](protocol.md) for the wire details;
[`../README.md`](../README.md) for the project overview.

This document is a **living** roadmap. Phase 0–5 are kept as a compact
archive of what landed and what didn't; Phase 6 onward is where we're going.
If a phase's exit criterion was met, the phase is Done — even if some
stretch goals inside it were deferred to a later phase.

Each phase is independently shippable. Phase 6 is the next.

---

## Table of contents

- [Phase 0–5 (Done)](#phase-05-done)
- [Phase 6 — Movement & editing gaps](#phase-6--movement--editing-gaps)
- [Phase 7 — Full payloads](#phase-7--full-payloads)
- [Phase 8 — Async & performance](#phase-8--async--performance)
- [Phase 9 — IDE features (deferred)](#phase-9--ide-features-deferred)
- [Cross-cutting concerns](#cross-cutting-concerns)
- [Non-goals](#non-goals)
- [Open questions](#open-questions)

---

## Phase 0–5 (Done)

A short archive. For every phase: the exit criterion, what landed, what
didn't, and — when relevant — what we found while building it.

### Phase 0 — Validate against real `xi-core` [done]

**Exit criterion:** integration tests pass against a real `xi-core` binary,
gated by `XI_CORE`.

**Landed:**

- `tests/lifecycle.rs` with `XI_CORE` gating and `wait_until_doc` /
  `wait_for` / `assert_doc_state` helpers.
- Round-trip tests for `new_view`, `insert`, `shutdown` → `Exited`.
- `tests/README.md`.

**Not automated:** wire-shape sniffing (the original plan was to log every
inbound notification and diff against §11). It happened **manually** —
and that's how we found the two protocol bugs below. A formal harness is
not worth the maintenance; the lifecycle tests catch divergences in
practice.

**Protocol bugs found:** none in Phase 0 itself; two later, in Phase 4–5:

- `find_status` carries `matches` inside `queries[]`, not at the top level.
- `plugin` namespace uses `"command"`, not `"method"`.

Both were caught by lifecycle tests, both would have been invisible to
mocks. **This is the whole reason Phase 0 exists.** Treat lifecycle tests
as load-bearing, not nice-to-have.

### Phase 1 — Interactive editing [done]

**Exit criterion:** a simple GUI can drive cursor movement, selection,
typing, undo/redo, and clipboard.

**Landed:**

- Basic movement: `move_up/down/left/right` + `_and_modify_selection`.
- `select_all`, `collapse_selections`.
- Editing: `insert`, `insert_newline`, `insert_tab`, `delete_backward`,
  `delete_forward`, `delete_word_backward`, `delete_word_forward`, `undo`,
  `redo`, `transpose`, `duplicate_line`.
- Transforms: `uppercase`, `lowercase`, `capitalize`, `indent`, `outdent`,
  `reindent`.
- Viewport: `resize` (new), `scroll` (already existed).
- Mouse: `GestureType` + `Granularity`, `gesture()` with the new wire
  format only.
- Clipboard: `cut`, `copy` (blocking sync).

**Deferred to Phase 6:** word movement, paragraph movement, page
scrolling, add-selection-above/below, `selection_into_lines`, `paste`,
`goto_line`, `delete_to_end_of_paragraph`,
`delete_to_beginning_of_line`, `increase/decrease_number`.

**Design note (resolved):** the original plan considered a `MovementKind`
enum + single dispatcher. We kept flat methods (`move_up`, `move_left`,
…). Reason: 90% of the API is four cardinal directions; making them
enum-dispatch adds cognitive overhead for a UI that already has
keybindings mapped to them. The remaining 20+ methods in Phase 6 will
follow the same flat style.

### Phase 2 — Performance & large documents [done]

**Exit criterion:** open a 10 MB file, scroll from top to bottom without
stalls. _(Informally verified; no regression test yet — see Open
Questions.)_

**Landed:**

- `request_lines(view_id, first, last)` — fires `update` for out-of-viewport lines.
- `set_width_measure(callback)` / `clear_width_measure()` — callback runs
  on the reader thread; padded/truncated defensively.

**Deferred to Phase 8:** bounded event channel
(`XiClient::set_event_buffer`).

### Phase 3 — Config, lifecycle, spawn options [done]

**Exit criterion:** SDK usable in a headless service (spawn, configure,
open, read, wait, exit).

**Landed:**

- `SpawnOptions` (`config_dir`, `client_extras_dir`, `log_level`,
  `log_dir`, `log_file`, `capture_stderr`).
- `spawn_with_options(core_path, opts)` — sends `config_dir` /
  `client_extras_dir` in `client_started`; forwards `XI_LOG` and
  `-log-dir` / `-log-file`; drains stderr in a background thread when
  `capture_stderr = true`.
- `ConfigDomain::{General, Syntax(String), UserOverride(ViewId)}` +
  `modify_user_config`.
- `get_config(view_id) -> Result<Config>` (blocking).
- `wait_for_exit(timeout) -> Result<ExitStatus>` — idempotent, condvar-based.
- `view_ids() -> Vec<ViewId>`.

**Deferred to Phase 8:** `new_view_async`. The infrastructure
(`Inner::next_request_id`) is already in place, but `new_view` remains
blocking. Inside a `winit` / `egui` loop, offload to a worker thread.

**Implementation note:** `wait_for_exit` uses a `(Mutex<bool>, Condvar)`
reader-exit flag rather than storing a `JoinHandle`. Both work; the
condvar version is simpler and doesn't need `Drop` ordering gymnastics.

### Phase 4 — Search & replace [done]

**Exit criterion:** find bar with match count, next/prev, replace-all.

**Landed:**

- `FindQuery`, `FindOptions`, `ModifySelection` (builder pattern).
- `find`, `multi_find`, `find_next`, `find_previous`, `find_all`.
- `highlight_find`, `selection_for_find`.
- `replace`, `replace_next`, `replace_all`, `selection_for_replace`.
- `Event::ReplaceStatus { view_id, chars, preserve_case }`.

**Protocol bug fixed:** `find_status` sends `matches` inside
`queries[i]`, not at the top level. `on_find_status` now sums across
queries. This is exactly the class of bug Phase 0 exists to catch.

**Deferred to Phase 7:** full `queries` payload (per-query
`chars`/`case_sensitive`/`lines`). Needed for minimap markers and "F6"
navigation; only the summed match count is surfaced today.

**Gotchas documented:** `find` needs `resize` + `scroll` before it can
find anything; `replace_all` uses the currently-set replacement string
(call `replace(chars)` first). Both in `protocol.md` §6.2.6.

### Phase 5 — IDE features [done]

**Exit criterion:** hover, plugin lifecycle, and status items are
observable through the SDK.

**Landed:**

- `request_hover(view_id, request_id, Option<Position>)` +
  `Event::HoverShown { view_id, request_id, result }`.
- `Position`, `Hover` types.
- `plugin_start` / `plugin_stop` / `plugin_rpc` (using the correct
  `"command"` tag — see bug below).
- `Event::AvailablePlugins`, `PluginStarted`, `PluginStopped`,
  `PluginCommands`.
- `Event::StatusItemAdded` / `Updated` / `Removed`.

**Protocol bug fixed:** the `plugin` namespace uses `"command"` as its
inner tag, not `"method"`. Sending the wrong tag causes `xi-core` to
exit with `UnknownRequest("missing field command")`. Caught by
`plugin_start_unknown_plugin_does_not_crash`.

**Deferred to Phase 7:** full `ThemeSettings` payload on
`ThemeChanged`.

**Explicitly not implemented (see Open Questions):**
`debug_get_contents`, and the event-driven variants
`ConfigResponse` / `CutResult` / `CopyResult` / `DocumentContents`.

---

## Phase 6 — Movement & editing gaps

**Why now:** these are the gaps a Sublime-like UI hits on day one.
`Ctrl+→`, `Ctrl+Home`, `PageDown`, `Ctrl+G`, `Ctrl+V` — all missing.

**Exit criterion:** every keyboard shortcut in the Sublime default
keymap (Windows/Linux) can be implemented against the SDK without
reaching for raw RPC.

All methods are one-liners of the form
`self.send_edit(view_id, "<inner>", json!([]))`. No new types, no
protocol quirks beyond §6.2.1 and §6.2.2 of `protocol.md`.

### 6.1 Movement (word / line edges / document edges / pages)

| Method                                                                    | Effort  |
| ------------------------------------------------------------------------- | ------- |
| `move_word_left`, `move_word_right` (+ `_and_modify_selection`)           | XS each |
| `move_to_left_end_of_line`, `move_to_right_end_of_line` (+ variants)      | XS each |
| `move_to_beginning_of_document`, `move_to_end_of_document` (+ variants)   | XS each |
| `move_to_beginning_of_paragraph`, `move_to_end_of_paragraph` (+ variants) | XS each |
| `scroll_page_up`, `scroll_page_down` (+ variants)                         | XS each |

**Total:** 20 methods. **Effort:** S (~2 hours with tests).

### 6.2 Selection expansion

| Method                                       | Effort  |
| -------------------------------------------- | ------- |
| `add_selection_above`, `add_selection_below` | XS each |
| `selection_into_lines`                       | XS      |

### 6.3 Editing

| Method                                                      | Effort  | Notes                                                                                                                                                                               |
| ----------------------------------------------------------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `paste(view_id, chars)`                                     | S       | Multi-cursor semantics: one line per cursor if `lines.len() == cursors.len()`, else full string at every cursor. **Needs a dedicated lifecycle test** — `insert` is not equivalent. |
| `goto_line(view_id, line)`                                  | XS      | Triggers `Event::ScrollTo`; test that `ScrollTo` arrives.                                                                                                                           |
| `delete_to_end_of_paragraph`, `delete_to_beginning_of_line` | XS each |                                                                                                                                                                                     |
| `increase_number`, `decrease_number`                        | XS each |                                                                                                                                                                                     |

**Total (Phase 6):** ~27 methods, ~10 lifecycle smoke tests
(one per family — movement, selection, paste, goto, delete).

**Commit shape:** one PR, titled `feat(phase6): movement & editing
gaps`, with the body listing every method added.

---

## Phase 7 — Full payloads

**Why now:** the events work, but the payloads are truncated. A
Sublime-like UI can't render a minimap, apply a theme, or display
diagnostics without the data we're currently dropping.

**Exit criterion:** `Event` variants carry everything `xi-core` sends;
`DocumentSnapshot` reflects plugin annotations.

### 7.1 `Event::ConfigChanged { changes, raw }`

Add `raw: serde_json::Value` alongside the typed `Config`. This
preserves keys not modeled by `Config` (e.g. `scroll_past_end`).
`changes` remains the typed subset for consumers that don't care about
the raw diff.

**Effort:** XS. **Breaking:** yes (adds a field). Since the crate is
still pre-1.0, acceptable.

### 7.2 `Event::FindStatus { queries: Vec<QueryStatus> }`

Replace `matches: u64` with a `Vec<QueryStatus>` where:

```rust
pub struct QueryStatus {
    pub id: u64,
    pub chars: Option<String>,
    pub case_sensitive: Option<bool>,
    pub regex: Option<bool>,
    pub whole_words: Option<bool>,
    pub lines: Vec<u64>,       // lines with at least one match
    pub matches: u64,
}
```

**Effort:** M. **Breaking:** yes. Lifecycle test: find "foo" in a
3-line buffer with matches on lines 0 and 2; assert `queries[0].lines`
contains both. This is the exact payload we currently throw away.

### 7.3 `Event::ThemeChanged { name, theme: ThemeSettings }`

```rust
pub struct ThemeSettings {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub caret: Option<u32>,
    pub selection: Option<u32>,
    pub selection_foreground: Option<u32>,
    pub line_highlight: Option<u32>,
    pub find_highlight: Option<u32>,
    pub find_highlight_foreground: Option<u32>,
}
```

**Effort:** S. **Breaking:** yes. Wire shape: `theme_changed` carries a
`theme` object with these (all optional) keys. Lifecycle test: send
`set_theme(<known-theme>)`, wait for `ThemeChanged`, assert `theme`
is not all-`None`. **Requires a build that ships at least one theme.**

### 7.4 `DocumentSnapshot.annotations`

Add `annotations: Vec<AnnotationSlice>`. Currently we only extract
`type: "selection"`; diagnostics and other plugin annotations are on
the wire but not exposed.

```rust
pub struct AnnotationSlice {
    pub ann_type: String,       // "selection" | "find" | "diagnostic" | ...
    pub ranges: Vec<[u64; 4]>,  // [start_line, start_col, end_line, end_col]
    pub payloads: Vec<serde_json::Value>,
    pub n: u64,
}
```

**Effort:** M. **Breaking:** yes (new field on `DocumentSnapshot`).
Lifecycle test: insert text, trigger a find, assert that
`doc.annotations` contains a `find` slice with the right ranges.

**Total (Phase 7):** ~4 breaking changes, ~6 lifecycle tests.

---

## Phase 8 — Async & performance

**Why now:** the SDK works, but it's not friendly to real UI event
loops. `new_view` blocks the caller; `document()` clones every line;
the event channel is unbounded.

**Exit criterion:** a `winit`/`egui` app can call every method from the
main thread without blocking it (or is documented to offload specific
ones); large-file benchmark is within a documented budget.

### 8.1 `new_view_async` / `copy_async` / `cut_async`

```rust
pub fn new_view_async(&self, path: Option<PathBuf>) -> Result<u64>;
```

Returns a request id; caller awaits
`Event::ViewOpened { request_id, .. }`. The `Inner::next_request_id`
counter and `Event::ViewOpened` field are already in place.

For `copy_async` / `cut_async`: use `xi-rpc`'s `send_rpc_request_async`

- a callback channel. Alternatively, decide that these are always
  called off-thread and only document it. **This is a design decision to
  make before implementing.**

**Effort:** M for `new_view_async`; S each for copy/cut if we choose
the callback route.

### 8.2 Bounded event channel

`XiClient::set_event_buffer(size: usize)`. Enables `crossbeam`'s
bounded channel with a drop-oldest policy so a slow consumer can't OOM.
Default remains unbounded.

**Effort:** S.

### 8.3 `document()` allocation budget

Current `DocumentSnapshot` clones every `LineState` (and its `String`)
on every call. For a 10k-line file, that's ~10k allocations per render
frame.

Options:

1. **Benchmark first** — measure `document()` on a 10k-line file. If
   it's already under budget, stop.
2. `Arc<str>` in `LineState`.
3. Dirty-region API: `document_region(view_id, first_line, last_line)`.

**Effort:** S (benchmark), M (Arc/region). Do not start this before a
benchmark shows a problem.

### 8.4 Reader-thread contention

`Inner` is a single `Mutex`. All view reads block behind any writer. A
`DashMap<ViewId, Mutex<DocumentState>>` is the candidate. Same rule:
benchmark first.

**Total (Phase 8):** 4 tasks, one design decision
(`copy_async`/`cut_async`), one optional benchmark.

---

## Phase 9 — IDE features (deferred)

**Why deferred:** depends on an LSP plugin being present in the target
`xi-core` build, which is not guaranteed. None of these can be
properly lifecycle-tested until we have one.

Listed here so the API shape is planned, not so they're built next.

- **`request_completion`.** Not in the current `xi-core` `EditNotification`
  set, as far as we can tell. If it appears, plan a
  `request_completion(view_id, request_id, Option<Position>)` and
  `Event::CompletionShown { request_id, result: Vec<CompletionItem> }`.
- **Diagnostics.** Arrive as `annotations` with `type: "diagnostic"`.
  Once Phase 7.4 lands, plan a filtered `DocumentSnapshot::diagnostics()`
  helper and an `Event::DiagnosticsUpdated { view_id }` signal so
  frontends don't have to diff `annotations` every frame.
- **`Event::Alert` severity.** `xi-core` sends a bare string. Parsing
  for `"info"` / `"warning"` / `"error"` prefixes is fragile. Defer
  until `xi-core` itself tags alerts.

**Nothing in Phase 9 blocks 1.0.** 1.0 means "stable API for a Sublime-like
editor without an LSP plugin", and Phase 7 achieves that.

---

## Cross-cutting concerns

### Versioning

Current: `0.1.0`.

| Milestone     | Version | Rationale                                                                                                   |
| ------------- | ------- | ----------------------------------------------------------------------------------------------------------- |
| **Now**       | `0.2.0` | Phase 0–5 shipped; SDK is usable. Cut this release.                                                         |
| After Phase 6 | `0.3.0` | Every Sublime default keybinding works.                                                                     |
| After Phase 7 | `1.0.0` | `Event` and `DocumentSnapshot` are stable for a real editor. No more breaking changes without a major bump. |

`#[non_exhaustive]` on `Event` means **new variants** don't require
major bumps. Adding fields to existing variants **does**, which is why
Phase 7 is the last breaking window before 1.0.

### Testing strategy

Three layers, each load-bearing:

| Layer       | Location                                         | What it catches                    | Cost                    |
| ----------- | ------------------------------------------------ | ---------------------------------- | ----------------------- |
| Inline unit | `src/**/*.rs` `#[cfg(test)]`                     | Private helpers, invariants        | <1s                     |
| Integration | `tests/{types,config_merge,document,process}.rs` | Public API drift, wire round-trips | <1s                     |
| Lifecycle   | `tests/lifecycle.rs` (gated by `XI_CORE`)        | **Real protocol divergences**      | ~10s with `XI_CORE` set |

**Rule:** any new method that synthesizes JSON (movement, find, gesture)
gets **at least one** lifecycle test. Mocks cannot catch a
`find_status`-shaped bug; lifecycle tests can.

**Rule:** before every release, run
`XI_CORE=/path/to/xi-core cargo test --test lifecycle -- --test-threads=1`.
CI should do the same on a known-good `xi-core` build (see Open
Questions).

### Documentation

| File                          | Update cadence                                                                      |
| ----------------------------- | ----------------------------------------------------------------------------------- |
| `README.md`                   | On every phase completion.                                                          |
| `docs/protocol.md`            | On every wire discovery (new gotcha, new payload, changed event).                   |
| `docs/roadmap.md` (this file) | Move finished phases into the Done archive; adjust Phase 6–9 as decisions are made. |

### Maintenance

- **Bump `xi-core` pin** (currently `./xi-core` in the repo root, gitignored).
  Re-run lifecycle tests. If `find_status` or `plugin` shapes ever
  change again, we'll know.
- **Re-run `cargo fmt`** before every PR. CI should enforce
  `cargo fmt --check` once CI exists.
- **Re-read `docs/protocol.md` §10 (Known gaps)** at the start of every
  phase. It's the checklist of what's deferred.

---

## Non-goals

- **Auto-reconnect.** `xi-core` is single-shot; if it dies, respawn.
  Matches upstream behavior.
- **Raw `Value` escape hatch.** If a method isn't in `protocol.md` §5.1,
  add it there first — don't open a back door.
- **Plugin lifecycle orchestration.** Manifest parsing and process
  supervision stay in `xi-core`. The SDK speaks `plugin` RPCs and
  forwards events; that's it.
- **Multi-view synchronization.** `xi-core` handles it; the SDK just
  relays per-view events.
- **Trace collection** (`tracing_config`, `save_trace`). Out of scope;
  use the raw protocol.
- **`XiClient: Sync`.** Inherited limitation from `xi-rpc`'s
  `Box<dyn Peer>`. `Arc<Mutex<XiClient>>` is the sanctioned workaround.

---

## Open questions

These are decisions, not tasks. Each needs an answer before its phase.

1. **CI.** Do we set up CI now, or after Phase 6? The value: `cargo fmt
--check`, `cargo test`, and `XI_CORE=... cargo test --test lifecycle`
   on every push. The cost: a GitHub Actions file and a decision about
   pinning a `xi-core` build artifact. **Recommendation:** add it before
   Phase 6 starts.

2. **`new_view_async` design.** Do we ship a callback-based
   `send_rpc_request_async` wrapper, or do we document that "you call
   `new_view` from a worker thread"? **Recommendation:** the wrapper.
   It's ~30 lines and it removes a real footgun.

3. **`copy_async` / `cut_async`.** Same question. Same recommendation.

4. **`debug_get_contents`.** Do we expose it? It's a one-liner and
   useful for integration tests that want to assert on the full buffer
   without walking `DocumentSnapshot`. **Recommendation:** yes, expose
   it — it's cheaper than maintaining a `debug_get_contents` workaround
   in tests.

5. **Event-driven sync-request results.**
   `ConfigResponse` / `CutResult` / `CopyResult` / `DocumentContents`.
   Do any consumers actually want these as events, or is
   `Result<T>` from the blocking call enough? **Recommendation:** skip
   until a consumer asks. Adding them "just in case" doubles the
   `Event` surface for no clear user.

6. **Event ordering guarantee.** Today the SDK makes no promise about
   the order of events **relative to `document()` reads**. If a UI
   renders in a `DocumentUpdated` handler and then handles a
   `ConfigChanged` in the same tick, does the second read see both?
   Answer: yes if the reader thread has applied both before your handler
   ran, no otherwise. **Recommendation:** either (a) document the
   "pull, don't push" contract more firmly in `protocol.md` §7.2, or
   (b) add `Event::DocumentSnapshot(DocumentSnapshot)` as a variant
   that carries the state alongside the signal. Option (b) is a design
   change; decide in Phase 8.

7. **Public `Inner`?** No. Ever.
