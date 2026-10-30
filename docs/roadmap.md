# xi-client Roadmap

## Guiding principle

The SDK is currently a **working transport with a partial façade**. The remaining work is additive, not corrective — except for one structural decision (validation), which should happen first because it may change what we build.

Each phase below is independently shippable. `docs/protocol.md` §5.1 is the master checklist; this document sequences it by _usefulness to a real frontend_, not by wire-protocol order.

---

## Phase 0 — Validate against real `xi-core` (blocker for everything else)

**Why first:** all current tests use mocks (`sh` in `process.rs`, hand-built JSON in `document.rs`). Not a single assertion has been made against a real `xi-core` subprocess. If the wire diverges anywhere (e.g. `config_changed` shapes, `find_status` variants, `view_id` casing in edge cases), we want to know _now_, before building 20 methods on top.

| Task                                              | Effort | Notes                                                                                                                  |
| ------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------- |
| Integration test harness (`tests/integration.rs`) | S      | Spawn `xi-core`, use `tempfile` for fixture files, drive via `XiClient`, assert on `Event` and `document()`.           |
| Round-trip test: open file → verify content       | S      | `new_view(Some(tmp))` → wait `DocumentUpdated` → assert `document().lines[0].text == contents`.                        |
| Round-trip test: `insert` → verify                | S      | `new_view(None)` → `insert("hi")` → wait `DocumentUpdated` → assert.                                                   |
| Round-trip test: `shutdown` → verify `Exited`     | XS     |                                                                                                                        |
| Wire-shape sniffing                               | S      | Log every inbound notification in a test build; compare to `docs/protocol.md` §11. Fix doc or code where they diverge. |

**Exit criterion:** three integration tests pass against a real `xi-core` binary on CI (or locally, with a `XI_CORE` env var gate).

**Deliverables:** `tests/integration.rs`, a documented `XI_CORE` env var, and a `docs/protocol.md` errata section for any wire quirks discovered.

---

## Phase 1 — Interactive editing (the MVP block)

Without this, users can only type at byte offset 0. This is the single most important phase after validation.

### 1.1 Movement & selection

All share the wire shape `edit.<inner>` with `[]` params.

| Task                                                         | Effort  |
| ------------------------------------------------------------ | ------- |
| `move_up`, `move_down`, `move_left`, `move_right`            | XS each |
| `move_word_left`, `move_word_right`                          | XS each |
| `move_to_left_end_of_line`, `move_to_right_end_of_line`      | XS each |
| `move_to_beginning_of_document`, `move_to_end_of_document`   | XS each |
| `move_to_beginning_of_paragraph`, `move_to_end_of_paragraph` | XS each |
| `scroll_page_up`, `scroll_page_down`                         | XS each |
| `*_and_modify_selection` variants of the above               | XS each |
| `select_all`, `collapse_selections`                          | XS each |
| `add_selection_above`, `add_selection_below`                 | XS each |
| `selection_into_lines`                                       | XS      |

**Design note:** rather than 30 near-identical `pub fn move_up(&self, view_id: ViewId)`, consider a small `MovementKind` enum + one dispatcher:

```rust
pub fn move_cursor(&self, view_id: ViewId, kind: Movement, extend_selection: bool) -> Result<()>;
```

This keeps the public surface small. Discuss before implementing — it's a public-API decision.

**Effort (either design):** S–M total.

### 1.2 Editing basics

| Task                                                                | Effort  |
| ------------------------------------------------------------------- | ------- |
| `undo` / `redo`                                                     | XS each |
| `insert_newline` / `insert_tab`                                     | XS each |
| `delete_word_backward` / `delete_word_forward`                      | XS each |
| `delete_to_end_of_paragraph` / `delete_to_beginning_of_line`        | XS each |
| `transpose`, `duplicate_line`, `increase_number`, `decrease_number` | XS each |
| `uppercase` / `lowercase` / `capitalize`                            | XS each |
| `indent` / `outdent` / `reindent`                                   | XS each |

### 1.3 Viewport feedback

| Task                                            | Effort | Notes                         |
| ----------------------------------------------- | ------ | ----------------------------- |
| `resize(view_id, width, height)`                | XS     | px/points, not device pixels. |
| `scroll` already exists — verify against §6.2.4 | —      |                               |

### 1.4 Mouse / touch — `gesture`

The only non-trivial piece of Phase 1. Requires new types in `types/`.

| Task                                                                | Effort |
| ------------------------------------------------------------------- | ------ |
| `GestureType` + `Granularity` enums (new module `types/gesture.rs`) | S      |
| Custom `Serialize` to new wire format (§6.2.5)                      | S      |
| `gesture(view_id, line, col, ty)`                                   | XS     |
| Tests: serialize `Select`, `SelectExtend`, `Drag` to expected JSON  | S      |

**Important:** the new format is required; the legacy format is deprecated. Do **not** accept legacy as input.

### 1.5 Clipboard — synchronous requests

First use of `send_rpc_request` from the app thread (already works via `new_view`).

| Task                                      | Effort |
| ----------------------------------------- | ------ |
| `cut(view_id) -> Result<Option<String>>`  | S      |
| `copy(view_id) -> Result<Option<String>>` | S      |

**Design note:** empty selection → `None`. If `xi-core` dies mid-request, caller gets `Error::CoreNotRunning` (already wired via `From<xi_rpc::Error>` in `lib.rs`). No retry.

**Phase 1 exit criterion:** a simple GUI can drive cursor movement, selection, typing, undo/redo, and clipboard. That is the threshold for "editor" rather than "viewer".

---

## Phase 2 — Performance & large documents

Without these, files over a few screens become sluggish or stop rendering.

| Task                                  | Effort | Notes                                                                                                                                            |
| ------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `request_lines(view_id, first, last)` | XS     | Triggers `update` for out-of-viewport lines. Critical for scroll responsiveness.                                                                 |
| Document when to call it              | —      | Follow `xi-core`'s guidance: after `scroll`, before the viewport enters blank territory.                                                         |
| `set_width_measure(callback)`         | M      | Optional. `CodepointMono` is fine for MVP. When added, callback runs on the reader thread → must not re-enter `XiClient` (document this loudly). |
| Bounded event channel option          | S      | `XiClient::set_event_buffer(usize)`. Prevents unbounded memory in headless consumers.                                                            |

**Phase 2 exit criterion:** open a 10 MB file, scroll from top to bottom without stalls.

---

## Phase 3 — Config, lifecycle, spawn options

Production-readiness items that don't block MVP but are needed before shipping to anyone else.

| Task                                                                                                     | Effort | Notes                                                                                                                              |
| -------------------------------------------------------------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------- |
| `SpawnOptions` (`config_dir`, `client_extras_dir`, `log_level`, `log_dir`, `log_file`, `capture_stderr`) | M      | Today `client_started` sends `{}` → **file-based config in xi-core is disabled**. Need a `client_extras_dir` for real deployments. |
| `spawn_with_options(core_path, opts)`                                                                    | S      |                                                                                                                                    |
| `ConfigDomain` enum (`General`, `Syntax`, `UserOverride`)                                                | S      |
| `modify_user_config(domain, changes: Value)`                                                             | S      | Rejects `General`/`Syntax` when `config_dir` is set — surface as `Error::Rpc`.                                                     |
| `get_config(view_id) -> Result<Config>`                                                                  | S      | Blocking. Mostly useful for tests.                                                                                                 |
| `wait_for_exit(Duration) -> Result<ExitStatus>`                                                          | S      | Needs `XiProcess::wait` (already in progress) + a `JoinHandle` on the reader thread stored in `XiClient`.                          |
| Store `JoinHandle<()>` for reader thread                                                                 | XS     | Needed for `wait_for_exit`, and for `Drop` to `join` instead of racing.                                                            |
| `new_view_async(path) -> Result<u64>`                                                                    | S      | Returns `request_id`; caller awaits `Event::ViewOpened { request_id, .. }`.                                                        |
| `view_ids() -> Vec<ViewId>`                                                                              | XS     |                                                                                                                                    |

**Phase 3 exit criterion:** SDK can be used in a headless service (spawn, configure, open, read, wait, exit).

---

## Phase 4 — Search & replace

Not a blocker for MVP, but a table-stakes editor feature.

| Task                                                                    | Effort | Notes                                                   |
| ----------------------------------------------------------------------- | ------ | ------------------------------------------------------- |
| `FindQuery` / `FindOptions` / `ModifySelection` types (`types/find.rs`) | S      | See §6.2.6.                                             |
| `find`, `multi_find`, `find_next`, `find_previous`, `find_all`          | M      |                                                         |
| `highlight_find`, `selection_for_find`                                  | S      |                                                         |
| `replace`, `replace_next`, `replace_all`, `selection_for_replace`       | M      |                                                         |
| `Event::ReplaceStatus` (new variant)                                    | S      | Payload: `{ view_id, chars, preserve_case }`.           |
| Full `find_status` payload (`queries`, not just `matches`)              | M      | Schema is "in flux"; coordinate with `xi-core` version. |

**Phase 4 exit criterion:** find bar with match count, next/prev, replace-all.

---

## Phase 5 — Polish & optional features

| Task                                                                             | Effort  | Notes                                                                                |
| -------------------------------------------------------------------------------- | ------- | ------------------------------------------------------------------------------------ |
| `request_hover` + `Event::HoverShown`                                            | M       | Needs `Position` type, plugin cooperation.                                           |
| Full `theme_changed` payload (`ThemeSettings`)                                   | S       | Currently only `name` is surfaced.                                                   |
| `show_hover` correlation with caller `request_id`                                | S       | Blocked by hover.                                                                    |
| `debug_get_contents(view_id) -> Result<String>`                                  | XS      | Useful for tests; cheap.                                                             |
| `Event::ViewClosed` — already added                                              | —       |                                                                                      |
| Non-exhaustive `Event` — already added                                           | —       |                                                                                      |
| `plugin_start` / `plugin_stop` / `plugin_rpc`                                    | M       | Full plugin story is a separate design.                                              |
| `Event::PluginStarted` / `PluginStopped` / `PluginCommands` / `AvailablePlugins` | M       | Blocked by plugin methods.                                                           |
| `Event::StatusItem*` (added/updated/removed)                                     | M       | Blocked by plugin methods.                                                           |
| `Event::ConfigResponse` / `CutResult` / `CopyResult` / `DocumentContents`        | XS each | Parallel to `Result<T>` from sync methods; only if event-driven consumers need them. |

---

## Cross-cutting concerns

### API stability

- **Breaking changes allowed through Phase 3.** The crate is `0.1.0` and unused externally; use this window to finalize signatures.
- **After Phase 3:** anything in `Event` or `XiClient` becomes semver-sensitive. `#[non_exhaustive]` on `Event` (already applied) buys us room for new variants.
- **`XiClient` is `Send` but not `Sync`** (inherited from `xi-rpc::Peer`). Document this; if a user needs shared access, `Arc<Mutex<XiClient>>`. Do **not** try to change this — it's a `xi-rpc` design.

### Documentation

- `docs/protocol.md` needs updates at each phase:
  - §3.1 (remove references to internal `RpcReader`/`RpcWriter`, point at `xi-rpc`).
  - §4.3 (document `SharedStdin`-based shutdown).
  - §8.4 (mark as resolved — delta decoding is correct).
  - §10 (move resolved bugs to a "fixed" subsection).
- `README.md` should have a **"Quick start"** with a working 20-line example, plus a compatibility matrix (SDK version ↔ `xi-core` build).

### Testing strategy

- **Unit tests** (already in place for `types/`, `document.rs`, `process.rs`, `client.rs` helpers).
- **Integration tests** (Phase 0 — spawn real `xi-core`). Add one per new method family.
- **Wire-shape tests** — for methods where the SDK synthesizes JSON (e.g. `gesture`, `find`), assert the exact JSON matches §6.2.x. This catches the case where `xi-rpc`'s serde re-exports drift.

### Versioning & release

- Bump to `0.2.0` after Phase 1 (first phase that adds user-facing methods).
- `0.3.0` after Phase 3 (production-ready).
- `1.0.0` when the `Event` enum is stable (post-Phase 5) and at least one external consumer uses it.

---

## At-a-glance summary

| Phase | Content                               | Effort | Unlocks                    |
| ----- | ------------------------------------- | ------ | -------------------------- |
| **0** | Validate against real `xi-core`       | **S**  | Confidence to build on top |
| **1** | Movement, editing, gesture, clipboard | **M**  | A usable text editor       |
| **2** | Viewport, `request_lines`             | **S**  | Large files                |
| **3** | Config, spawn options, lifecycle      | **M**  | Headless / production use  |
| **4** | Find / replace                        | **M**  | Feature-complete editor    |
| **5** | Hover, plugins, polish                | **L**  | IDE features               |

**Effort key:** XS < 1h, S ≈ half day, M ≈ 1–3 days, L ≈ 1+ week (all estimates assume one developer familiar with the codebase).

**Critical path:** Phase 0 → Phase 1. Nothing else matters until those are done.

---

## What is explicitly _not_ in scope

- **Auto-reconnect.** `xi-core` is single-shot; if it dies, user re-spawns. Matches upstream behavior.
- **Raw `Value` escape hatch.** If a method isn't in `docs/protocol.md` §5.1, it doesn't belong in the façade — add it to §5.1 first.
- **Plugin lifecycle orchestration.** Only the transport RPCs (`plugin_start` etc.); manifest parsing and process supervision stay in `xi-core`.
- **Multi-view synchronization.** `xi-core` already handles it; the SDK just relays events per view.
- **Trace collection** (`tracing_config`, `save_trace`). Out of scope; use raw protocol if needed.
